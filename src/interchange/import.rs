//! `*.body.glb` → body plan.
//!
//! The part tree is the nesting of nodes carrying `extras.ornisculpt.part`. A node still at the
//! frame it was exported at keeps its attach and offset exactly; a moved one slides over its
//! parent (`Along` and `Surface` attaches) and its offset takes up the rest of the move.

use bevy::math::Affine3A;
use bevy::prelude::*;
use serde_json::Value;
use thiserror::Error;

use super::{GlbError, KEY, axis_coords, gltf_document};
use crate::anatomy::{
    Anatomy, AnatomyError, AttachSlot, BodyPlan, PartAssets, PartLodRules, PartNode, PartOffset, PartPath, Repeat, Tone, path_matches,
};
use crate::coat::Coat;

/// How far (metres, and matrix entries) a part node may be from its exported frame and still
/// count as unmoved.
const MOVED: f32 = 1e-4;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error(transparent)]
    Glb(#[from] GlbError),
    #[error("not a glTF file")]
    NotGltf,
    #[error("no part nodes (nothing carries `extras.{KEY}.part`)")]
    NoParts,
    #[error("{0} top-level part nodes: a body has one root part, the others nest under it")]
    Roots(usize),
    #[error("node `{node}`: {message}")]
    Part { node: String, message: String },
    #[error("root `{KEY}` node: {0}")]
    Root(String),
    #[error("moved part `{0}` didn't build")]
    NotBuilt(PartPath),
    #[error(transparent)]
    Anatomy(#[from] AnatomyError),
}

/// A body read back from glTF.
#[derive(Debug)]
pub struct ImportedBody {
    pub plan: BodyPlan,
    /// The coat exported with the body, if one was.
    pub coat: Option<Coat>,
    /// Parts whose node was moved, rotated or scaled since export (or never had a frame).
    pub moved: Vec<PartPath>,
    /// Things kept as they were or approximated (a non-uniform scale on a part node).
    pub warnings: Vec<String>,
}

/// Reads a body from glTF (`.glb` or `.gltf` text), placing moved parts where their nodes now
/// are. `assets` holds the meshes the plan references (see
/// [`referenced_assets`](crate::anatomy::referenced_assets)); it is only used when a part moved.
///
/// # Errors
///
/// When the file isn't glTF, holds no single root part, a part's data doesn't parse, or a moved
/// part's plan doesn't build.
pub fn import_body(bytes: &[u8], assets: &PartAssets) -> Result<ImportedBody, ImportError> {
    place_moved(read_body(bytes)?, assets)
}

/// The plan as the file states it, before moved parts are placed: what [`place_moved`] finishes.
#[derive(Debug)]
pub struct ReadBody {
    pub body: ImportedBody,
    /// Model-space transform of each moved part's node, parents first.
    nodes: Vec<(PartPath, Affine3A)>,
}

/// Reads the plan, its coat and which parts moved; no building.
///
/// # Errors
///
/// As [`import_body`], except for building.
pub fn read_body(bytes: &[u8]) -> Result<ReadBody, ImportError> {
    let (doc, _) = gltf_document(bytes).ok_or(ImportError::NotGltf)?;
    let empty = Vec::new();
    let nodes = doc.get("nodes").and_then(Value::as_array).unwrap_or(&empty);
    let node = |i: usize| nodes.get(i);
    let children = |n: &Value| -> Vec<usize> {
        n.get("children").and_then(Value::as_array).map_or_else(Vec::new, |c| c.iter().filter_map(index).collect())
    };

    // Model space is the root node's: moving the whole file in the DCC moves nothing.
    let root = nodes.iter().position(|n| ornisculpt(n).is_some_and(|o| o.get("format").is_some()));
    let scene_roots = || {
        let scene = doc.get("scene").and_then(index).unwrap_or(0);
        let roots = doc.pointer(&format!("/scenes/{scene}/nodes")).and_then(Value::as_array);
        (roots.map_or_else(Vec::new, |r| r.iter().filter_map(index).collect()), None)
    };
    let (starts, root_extras) = root.and_then(node).map_or_else(scene_roots, |r| (children(r), ornisculpt(r)));

    let mut found = Vec::new();
    for s in starts {
        collect(nodes, s, Affine3A::IDENTITY, &mut found, 0)?;
    }
    let root_part = match found.len() {
        0 => return Err(ImportError::NoParts),
        1 => found.swap_remove(0),
        n => return Err(ImportError::Roots(n)),
    };

    let tone = match root_extras.and_then(|o| o.get("tone")) {
        Some(t) => serde_json::from_value::<Tone>(t.clone()).map_err(|e| ImportError::Root(format!("tone: {e}")))?,
        None => Tone::default(),
    };
    let coat = match root_extras.and_then(|o| o.get("coat")) {
        Some(c) => Some(serde_json::from_value::<Coat>(c.clone()).map_err(|e| ImportError::Root(format!("coat: {e}")))?),
        None => None,
    };

    let mut moved = Vec::new();
    root_part.moved(&PartPath::root(&root_part.data.name), &mut moved);
    let plan = BodyPlan { root: root_part.into_node(), tone };
    let body = ImportedBody { plan, coat, moved: moved.iter().map(|(p, _)| p.clone()).collect(), warnings: Vec::new() };
    Ok(ReadBody { body, nodes: moved })
}

/// Places every moved part where its node is, parents first: an `Along` or `Surface` part slides
/// to the point of its parent nearest the node, then its offset takes up the rest.
///
/// # Errors
///
/// When the plan, as moved so far, doesn't build.
pub fn place_moved(read: ReadBody, assets: &PartAssets) -> Result<ImportedBody, ImportError> {
    let ReadBody { mut body, nodes } = read;
    let rules = PartLodRules::default();
    for (path, model) in nodes {
        let mut anatomy = Anatomy::from_plan(&body.plan, &rules, assets)?;
        let built = |a: &Anatomy| a.parts.iter().position(|p| path_matches(&path, &p.path));
        let mut at = built(&anatomy).ok_or_else(|| ImportError::NotBuilt(path.clone()))?;
        let parent = anatomy.parts.get(at).and_then(|p| p.parent).and_then(|p| anatomy.parts.get(usize::from(p)));
        let on_parent = parent.and_then(|p| axis_coords(&p.axis, model.translation.into()));
        if let (Some((t, around)), Some(node)) = (on_parent, body.plan.node_mut(&path))
            && slide(node, t, around)
        {
            anatomy = Anatomy::from_plan(&body.plan, &rules, assets)?;
            at = built(&anatomy).ok_or_else(|| ImportError::NotBuilt(path.clone()))?;
        }
        let attach = anatomy.parts.get(at).map_or_default(|p| p.attach);
        let (offset, uniform) = offset_of(&(attach.compute_affine().inverse() * model));
        if !uniform {
            body.warnings.push(format!("`{path}`: a part scales evenly; its node's scale was averaged"));
        }
        if let Some(node) = body.plan.node_mut(&path) {
            node.offset = offset;
        }
    }
    Ok(body)
}

/// Moves `node`'s attach to `t` (and `around` degrees, on the skin) along its parent. A series
/// moves as a whole, within the parent. `false` for slots that don't slide.
fn slide(node: &mut PartNode, t: f32, around: f32) -> bool {
    // Rounded as offsets are (see `offset_of`).
    let t = ((t * 1e6).round() / 1e6).clamp(0.0, 1.0);
    let around = (around * 1e4).round() / 1e4;
    match (&mut node.attach, &mut node.repeat) {
        (AttachSlot::Along(_) | AttachSlot::Surface { .. }, Repeat::Series { from, to, .. }) => {
            let shift = (t - *from).clamp(-from.min(*to), 1.0 - from.max(*to));
            *from += shift;
            *to += shift;
        }
        (AttachSlot::Along(at) | AttachSlot::Surface { t: at, .. }, _) => *at = t,
        _ => return false,
    }
    if let AttachSlot::Surface { around_deg, .. } = &mut node.attach {
        *around_deg = around;
    }
    true
}

/// The offset that is `m`, and whether `m`'s scale was even (an offset only scales evenly).
fn offset_of(m: &Affine3A) -> (PartOffset, bool) {
    let (scale, rotation, translation) = m.to_scale_rotation_translation();
    let (x, y, z) = rotation.to_euler(EulerRot::XYZ);
    let even = scale.element_sum() / 3.0;
    // Recomputed through a DCC's floats: round off the noise (a micrometre, a ten-thousandth of
    // a degree), so an untouched child of a moved part comes back as it was.
    let tidy = |v: Vec3, step: f32| (v / step).round() * step + Vec3::ZERO;
    let offset = PartOffset {
        translate: tidy(translation, 1e-6),
        rotate_deg: tidy(Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI), 1e-4),
        scale: if (even - 1.0).abs() < 1e-5 { 1.0 } else { even },
    };
    (offset, (scale - Vec3::splat(even)).abs().max_element() < 0.01 * even.abs().max(1e-6))
}

/// A part node found in the file.
struct Found {
    data: PartNode,
    /// Its frame at export, when it has one.
    frame: Option<Affine3A>,
    /// Where it is now (model space).
    model: Affine3A,
    /// Its place among its siblings at export (`None` for a part added since).
    order: Option<u64>,
    children: Vec<Self>,
}

impl Found {
    fn moved(&self, path: &PartPath, out: &mut Vec<(PartPath, Affine3A)>) {
        if !self.frame.is_some_and(|f| close(&f, &self.model)) {
            out.push((path.clone(), self.model));
        }
        for c in &self.children {
            c.moved(&path.child(&c.data.name), out);
        }
    }

    fn into_node(self) -> PartNode {
        PartNode { children: self.children.into_iter().map(Self::into_node).collect(), ..self.data }
    }
}

/// Collects the part nodes at and under node `i`, whose parent is at `parent` (model space).
/// Nodes without a part are walked through; reference content is skipped.
fn collect(nodes: &[Value], i: usize, parent: Affine3A, out: &mut Vec<Found>, depth: usize) -> Result<(), ImportError> {
    // glTF nodes form a forest, but a broken file could loop.
    if depth > 256 {
        return Ok(());
    }
    let Some(n) = nodes.get(i) else { return Ok(()) };
    let model = parent * local(n);
    let extras = ornisculpt(n);
    if extras.and_then(|o| o.get("reference")).and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let kids = n.get("children").and_then(Value::as_array).map_or_else(Vec::new, |c| c.iter().filter_map(index).collect::<Vec<_>>());
    let Some(part) = extras.and_then(|o| o.get("part")) else {
        for c in kids {
            collect(nodes, c, model, out, depth + 1)?;
        }
        return Ok(());
    };
    let name = n.get("name").and_then(Value::as_str).map(str::to_owned);
    let mut data = parse_part(part, name.as_deref()).map_err(|message| ImportError::Part { node: name.clone().unwrap_or_else(|| format!("#{i}")), message })?;
    if let Some(name) = name {
        data.name = name;
    }
    let frame = extras.and_then(|o| o.get("frame")).and_then(matrix);
    let order = extras.and_then(|o| o.get("order")).and_then(Value::as_u64);
    let mut children = Vec::new();
    for c in kids {
        collect(nodes, c, model, &mut children, depth + 1)?;
    }
    // Siblings in their exported order (DCCs reorder them), parts added since after them.
    children.sort_by_key(|c| c.order.unwrap_or(u64::MAX));
    out.push(Found { data, frame, model, order, children });
    Ok(())
}

/// A part from its extras: a JSON object, or a string holding JSON or RON. The node's name
/// stands in for a missing `name`.
fn parse_part(v: &Value, name: Option<&str>) -> Result<PartNode, String> {
    let with_name = |mut v: Value| {
        if let (Some(o), Some(name)) = (v.as_object_mut(), name) {
            o.entry("name").or_insert_with(|| Value::String(name.to_owned()));
        }
        v
    };
    match v {
        Value::String(s) => serde_json::from_str::<Value>(s).map_or_else(
            |_| {
                // RON names its fields too: a missing name comes from the node.
                let named = match (name, s.trim_start().strip_prefix('(')) {
                    (Some(name), Some(rest)) if !s.contains("name:") => format!("(name: {name:?}, {rest}"),
                    _ => s.clone(),
                };
                ron::from_str(&named).map_err(|e| e.to_string())
            },
            |json| serde_json::from_value(with_name(json)).map_err(|e| e.to_string()),
        ),
        v => serde_json::from_value(with_name(v.clone())).map_err(|e| e.to_string()),
    }
}

/// `extras.ornisculpt` of a node.
fn ornisculpt(n: &Value) -> Option<&serde_json::Map<String, Value>> {
    n.get("extras")?.get(KEY)?.as_object()
}

fn index(v: &Value) -> Option<usize> {
    v.as_u64().and_then(|i| usize::try_from(i).ok())
}

/// A node's local transform: its `matrix`, or its `translation`, `rotation` and `scale`.
fn local(n: &Value) -> Affine3A {
    if let Some(m) = n.get("matrix").and_then(matrix) {
        return m;
    }
    let floats = |k: &str| -> Option<Vec<f32>> { n.get(k)?.as_array()?.iter().map(|x| x.as_f64().map(narrow)).collect() };
    let v3 = |k: &str, d: Vec3| floats(k).and_then(|f| <[f32; 3]>::try_from(f).ok()).map_or(d, Vec3::from_array);
    let rotation = floats("rotation").and_then(|f| <[f32; 4]>::try_from(f).ok()).map_or(Quat::IDENTITY, |q| Quat::from_array(q).normalize());
    Affine3A::from_scale_rotation_translation(v3("scale", Vec3::ONE), rotation, v3("translation", Vec3::ZERO))
}

/// A column-major 4×4 matrix.
fn matrix(v: &Value) -> Option<Affine3A> {
    let f: Vec<f32> = v.as_array()?.iter().map(|x| x.as_f64().map(narrow)).collect::<Option<_>>()?;
    Some(Affine3A::from_mat4(Mat4::from_cols_array(&<[f32; 16]>::try_from(f).ok()?)))
}

#[expect(clippy::cast_possible_truncation, clippy::as_conversions, reason = "glTF floats are single precision")]
const fn narrow(x: f64) -> f32 {
    x as f32
}

fn close(a: &Affine3A, b: &Affine3A) -> bool {
    Mat4::from(*a).abs_diff_eq(Mat4::from(*b), MOVED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::test_assets;
    use crate::interchange::{export_body, read_glb, write_glb};

    fn sample(name: &str) -> BodyPlan {
        let path = format!("{}/assets/mobs/{name}.body.ron", env!("CARGO_MANIFEST_DIR"));
        ron::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn export(plan: &BodyPlan) -> Vec<u8> {
        let anatomy = Anatomy::from_plan(plan, &PartLodRules::default(), &test_assets(plan)).unwrap();
        export_body(plan, Some(&Coat::default()), &anatomy, None).unwrap()
    }

    fn ron(plan: &BodyPlan) -> String {
        ron::to_string(plan).unwrap()
    }

    /// Anything a plan can say survives a trip through glTF: every sample plan, exported and read
    /// back, is the same plan, with nothing moved.
    #[test]
    fn round_trip_every_sample_plan() {
        for (name, plan) in crate::interchange::sample_plans() {
            let back = import_body(&export(&plan), &test_assets(&plan)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(back.moved.is_empty(), "{name}: moved {:?}", back.moved);
            assert_eq!(ron(&back.plan), ron(&plan), "{name}");
            assert_eq!(back.coat, Some(Coat::default()), "{name}");
        }
    }

    /// The JSON document of `bytes`, edited by `edit`, written back.
    fn edited(bytes: &[u8], edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let (mut doc, bin) = read_glb(bytes).unwrap();
        edit(&mut doc);
        write_glb(&doc, bin).unwrap()
    }

    fn node_named<'a>(doc: &'a mut Value, name: &str) -> &'a mut Value {
        doc["nodes"].as_array_mut().unwrap().iter_mut().find(|n| n["name"] == name).unwrap()
    }

    /// The model-space frame `export_body` gives the part at `path` of `plan`.
    fn frame(plan: &BodyPlan, path: &PartPath) -> Affine3A {
        let anatomy = Anatomy::from_plan(plan, &PartLodRules::default(), &test_assets(plan)).unwrap();
        let built = anatomy.parts.iter().find(|p| path_matches(path, &p.path)).unwrap();
        built.attach.compute_affine() * plan.node(path).unwrap().offset.to_transform().compute_affine()
    }

    /// A part node moved over its parent slides its attach and lands where the node was put.
    #[test]
    fn moved_part_lands_where_its_node_is() {
        let plan = sample("quadruped");
        let mut target = None;
        plan.visit(|path, node| {
            if target.is_none() && matches!(node.attach, AttachSlot::Surface { .. } | AttachSlot::Along(_)) && path.parent().is_some() {
                target = Some((path.clone(), node.name.clone(), node.attach));
            }
        });
        let (path, name, before) = target.unwrap();
        let parent = path.parent().unwrap();
        // 4 cm along the parent's local Y, which runs along a segmented part.
        let shift = Vec3::new(0.0, 0.04, 0.0);
        let original = frame(&plan, &path);
        let bytes = edited(&export(&plan), |doc| {
            let n = node_named(doc, &name);
            let t = n.get("translation").map_or(Vec3::ZERO, |t| Vec3::from_array(std::array::from_fn(|i| narrow(t[i].as_f64().unwrap()))));
            n["translation"] = serde_json::json!((t + shift).to_array());
        });
        let back = import_body(&bytes, &test_assets(&plan)).unwrap();
        // Its children (if any) moved with it, after it.
        assert_eq!(back.moved.first(), Some(&path));
        let node = back.plan.node(&path).unwrap();
        assert_ne!(node.attach, before, "a part on its parent's axis or skin slides");
        // The node's translation moved by `shift` in its parent's frame.
        let parent_frame = frame(&plan, &parent);
        let want = parent_frame * Affine3A::from_translation(shift) * parent_frame.inverse() * original;
        let got = frame(&back.plan, &path);
        assert!(Vec3::from(got.translation).distance(Vec3::from(want.translation)) < 1e-3, "{got:?} vs {want:?}");
    }

    /// DCCs reorder siblings (Blender sorts them by name); the plan keeps its order.
    #[test]
    fn sibling_order_survives_reordering() {
        let plan = sample("quadruped");
        let bytes = edited(&export(&plan), |doc| {
            for n in doc["nodes"].as_array_mut().unwrap() {
                if let Some(c) = n.get_mut("children").and_then(Value::as_array_mut) {
                    c.reverse();
                }
            }
        });
        let back = import_body(&bytes, &test_assets(&plan)).unwrap();
        assert_eq!(ron(&back.plan), ron(&plan));
    }

    /// A part written by hand in a DCC: a RON string, its name from the node.
    #[test]
    fn part_as_ron_string() {
        let v = Value::String("(kind: Hard(()), attach: Horn)".into());
        let part = parse_part(&v, Some("horn")).unwrap();
        assert_eq!(part.name, "horn");
        assert_eq!(part.attach, AttachSlot::Horn);
    }
}
