//! Body plan → `*.body.glb`.

use bevy::math::Affine3A;
use bevy::prelude::*;
use serde_json::{Map, Value, json};
use thiserror::Error;

use super::authored::{authored, base_frame, fingerprint};
use crate::clothing::{Outfit, fit_first_piece, garment_field};
use super::{FORMAT, GlbError, KEY, write_glb};
use crate::anatomy::{Anatomy, BodyPlan, PartAssets, PartNode, PartPath, path_matches};
use crate::coat::Coat;
use crate::surface::{BodyMeshData, MaterialSlot};

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("part `{0}` isn't in the anatomy (was it built from this plan?)")]
    NotBuilt(PartPath),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Glb(#[from] GlbError),
}

/// What [`export_body`] writes.
pub struct BodyExport<'a> {
    pub plan: &'a BodyPlan,
    /// Built from `plan` in its default form (`Anatomy::from_plan`) with `assets`: the part
    /// frames come from it.
    pub anatomy: &'a Anatomy,
    /// The meshes the plan and the outfit reference.
    pub assets: &'a PartAssets,
    pub coat: Option<&'a Coat>,
    /// Garments to export fitted on the body (each on the first part it wraps).
    pub outfit: Option<&'a Outfit>,
    /// A hero mesh of the body, exported skinned to the skeleton as reference.
    pub skin: Option<&'a BodyMeshData>,
}

/// Writes a body as a `*.body.glb`.
///
/// The file holds an empty per authored part at its frame, carrying the part, and, when given,
/// the reference content: the skeleton in rest pose and the skin skinned to it. The coat rides
/// along on the root. A part with an authored mesh (a sculpt, a mesh hard part) is a mesh node
/// instead, with its mesh, scaled, in its frame. Each garment is a mesh node fitted on the first
/// part it wraps, in the rest pose, carrying its [`Garment`](crate::clothing::Garment).
///
/// # Errors
///
/// When a part of the plan isn't in the anatomy.
pub fn export_body(body: &BodyExport) -> Result<Vec<u8>, ExportError> {
    let BodyExport { plan, anatomy, assets, coat, outfit, skin } = *body;
    let mut doc = Doc::default();
    let mut extras = Map::new();
    extras.insert("format".into(), json!(FORMAT));
    extras.insert("tone".into(), serde_json::to_value(plan.tone)?);
    if let Some(coat) = coat {
        extras.insert("coat".into(), strip_nulls(serde_json::to_value(coat)?));
    }
    let root = doc.node(json!({ "name": KEY, "extras": { KEY: extras } }));

    let cx = PartCx { anatomy, assets };
    let part = part_node(&mut doc, &cx, &plan.root, &PartPath::root(&plan.root.name), Affine3A::IDENTITY, 0)?;
    doc.child(root, part);

    if let Some(outfit) = outfit.filter(|o| !o.garments.is_empty()) {
        let field = garment_field(anatomy);
        for (order, g) in outfit.garments.iter().enumerate() {
            let Some(fitted) = assets.meshes.get(&g.mesh).and_then(|m| fit_first_piece(anatomy, &field, g, m)) else {
                warn!("garment `{}`: no mesh or no part to wrap; not exported", g.name);
                continue;
            };
            let garment = strip_nulls(serde_json::to_value(g)?);
            let print = format!("{:016x}", fingerprint(&fitted.positions, fitted.indices.len() / 3));
            let mesh = doc.mesh(&g.name, &fitted.positions, &[], &fitted.indices, MaterialSlot::Cloth);
            let name = if g.name.is_empty() { format!("garment.{order}") } else { g.name.clone() };
            let n = doc.node(json!({ "name": name, "mesh": mesh, "extras": { KEY: { "garment": garment, "order": order, "mesh_print": print } } }));
            doc.child(root, n);
        }
    }

    if let Some(skin) = skin {
        let reference = reference(&mut doc, anatomy, skin);
        doc.child(root, reference);
    }

    let gltf = json!({
        "asset": { "version": "2.0", "generator": concat!("ornisculpt ", env!("CARGO_PKG_VERSION")) },
        "scene": 0,
        "scenes": [{ "nodes": [root] }],
        "nodes": doc.nodes,
        "meshes": doc.meshes,
        "skins": doc.skins,
        "materials": doc.materials,
        "accessors": doc.accessors,
        "bufferViews": doc.views,
        "buffers": if doc.bin.is_empty() { json!([]) } else { json!([{ "byteLength": doc.bin.len() }]) },
    });
    Ok(write_glb(&drop_empty(gltf), &doc.bin)?)
}

/// The empty for authored part `node` at `path` (and its children's), placed relative to its
/// parent's frame `parent` (model space). `order` is its place among its siblings, which DCCs
/// don't keep (Blender sorts by name) but the build order depends on.
fn part_node(doc: &mut Doc, cx: &PartCx, node: &PartNode, path: &PartPath, parent: Affine3A, order: usize) -> Result<usize, ExportError> {
    // The first copy is the logical one: `.l` before `.r`, `.0` before the rest.
    let built = cx.anatomy.parts.iter().find(|p| path_matches(path, &p.path)).ok_or_else(|| ExportError::NotBuilt(path.clone()))?;
    let frame = base_frame(&node.kind, built.attach).compute_affine() * node.offset.to_transform().compute_affine();
    let local = parent.inverse() * frame;
    let data = PartNode { children: Vec::new(), ..node.clone() };
    let mut part = strip_nulls(serde_json::to_value(&data)?);
    if let Some(p) = part.as_object_mut() {
        p.remove("children");
    }
    let mut n = trs(&local);
    n.insert("name".into(), json!(node.name));
    let mut extras = Map::new();
    extras.insert("part".into(), part);
    extras.insert("frame".into(), json!(Mat4::from(frame).to_cols_array()));
    extras.insert("order".into(), json!(order));
    if let Some((mesh_path, _, scale)) = authored(&node.kind)
        && let Some(mesh) = cx.assets.meshes.get(mesh_path)
    {
        let positions: Vec<Vec3> = mesh.positions.iter().map(|p| *p * scale).collect();
        // Import keeps the mesh's path while its geometry is unchanged (see `fingerprint`).
        extras.insert("mesh_print".into(), json!(format!("{:016x}", fingerprint(&positions, mesh.indices.len() / 3))));
        let index = doc.mesh(&node.name, &positions, &mesh.normals, &mesh.indices, MaterialSlot::Hard);
        n.insert("mesh".into(), json!(index));
    }
    n.insert("extras".into(), json!({ KEY: extras }));
    let index = doc.node(Value::Object(n));
    for (order, child) in node.children.iter().enumerate() {
        let c = part_node(doc, cx, child, &path.child(&child.name), frame, order)?;
        doc.child(index, c);
    }
    Ok(index)
}

/// What parts are exported from.
struct PartCx<'a> {
    anatomy: &'a Anatomy,
    assets: &'a PartAssets,
}

/// The reference skeleton (joints in rest pose) and the skin bound to it, under one empty that
/// import skips.
fn reference(doc: &mut Doc, anatomy: &Anatomy, skin: &BodyMeshData) -> usize {
    let holder = doc.node(json!({ "name": "reference", "extras": { KEY: { "reference": true } } }));
    let bones = &anatomy.skeleton.bones;
    let joints: Vec<usize> = bones
        .iter()
        .map(|b| {
            let mut n = trs(&b.rest.compute_affine());
            n.insert("name".into(), json!(b.name));
            doc.node(Value::Object(n))
        })
        .collect();
    for (b, &j) in bones.iter().zip(&joints) {
        match b.parent.and_then(|p| joints.get(p.index())) {
            Some(&parent) => doc.child(parent, j),
            None => doc.child(holder, j),
        }
    }
    let binds: Vec<f32> = anatomy.skeleton.inverse_bind.iter().flat_map(Mat4::to_cols_array).collect();
    let binds = doc.accessor(bytemuck::cast_slice(&binds), FLOAT, "MAT4", binds.len() / 16, None);
    doc.skins.push(json!({ "joints": joints, "inverseBindMatrices": binds }));

    let (lo, hi) = skin.positions.iter().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let n = skin.positions.len();
    let position = doc.accessor(bytemuck::cast_slice(&skin.positions), FLOAT, "VEC3", n, Some((lo, hi)));
    let normal = doc.accessor(bytemuck::cast_slice(&skin.normals), FLOAT, "VEC3", n, None);
    let joint = doc.accessor(bytemuck::cast_slice(&skin.joints), UNSIGNED_SHORT, "VEC4", n, None);
    let weight = doc.accessor(bytemuck::cast_slice(&skin.weights), FLOAT, "VEC4", n, None);
    let attributes = json!({ "POSITION": position, "NORMAL": normal, "JOINTS_0": joint, "WEIGHTS_0": weight });
    let mut primitives = Vec::new();
    for s in &skin.submeshes {
        let indices = skin.submesh_indices(s);
        let accessor = doc.accessor(bytemuck::cast_slice(indices), UNSIGNED_INT, "SCALAR", indices.len(), None);
        let material = doc.material(s.slot);
        primitives.push(json!({ "attributes": attributes, "indices": accessor, "material": material }));
    }
    doc.meshes.push(json!({ "name": "skin", "primitives": primitives }));
    let mesh = doc.node(json!({ "name": "skin", "mesh": doc.meshes.len() - 1, "skin": doc.skins.len() - 1 }));
    doc.child(holder, mesh);
    holder
}

const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;

/// The glTF document as it's assembled: arrays of JSON objects and the one binary buffer.
#[derive(Default)]
struct Doc {
    nodes: Vec<Value>,
    meshes: Vec<Value>,
    skins: Vec<Value>,
    materials: Vec<Value>,
    accessors: Vec<Value>,
    views: Vec<Value>,
    bin: Vec<u8>,
    /// Material index per slot, made on first use.
    slots: Vec<(MaterialSlot, usize)>,
}

impl Doc {
    fn node(&mut self, n: Value) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    fn child(&mut self, parent: usize, child: usize) {
        let Some(n) = self.nodes.get_mut(parent).and_then(Value::as_object_mut) else { return };
        if let Some(list) = n.entry("children").or_insert_with(|| json!([])).as_array_mut() {
            list.push(json!(child));
        }
    }

    /// Appends `bytes` as a buffer view with one accessor over it.
    fn accessor(&mut self, bytes: &[u8], component: u32, kind: &str, count: usize, bounds: Option<(Vec3, Vec3)>) -> usize {
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        self.views.push(json!({ "buffer": 0, "byteOffset": self.bin.len(), "byteLength": bytes.len() }));
        self.bin.extend_from_slice(bytes);
        let mut a = Map::new();
        a.insert("bufferView".into(), json!(self.views.len() - 1));
        a.insert("componentType".into(), json!(component));
        a.insert("type".into(), json!(kind));
        a.insert("count".into(), json!(count));
        if let Some((lo, hi)) = bounds {
            a.insert("min".into(), json!(lo.to_array()));
            a.insert("max".into(), json!(hi.to_array()));
        }
        self.accessors.push(Value::Object(a));
        self.accessors.len() - 1
    }

    /// A one-primitive mesh of `positions` (and `normals`, when there is one per position).
    fn mesh(&mut self, name: &str, positions: &[Vec3], normals: &[Vec3], indices: &[u32], slot: MaterialSlot) -> usize {
        let (lo, hi) = positions.iter().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        let n = positions.len();
        let mut attributes = Map::new();
        attributes.insert("POSITION".into(), json!(self.accessor(bytemuck::cast_slice(positions), FLOAT, "VEC3", n, Some((lo, hi)))));
        if normals.len() == n {
            attributes.insert("NORMAL".into(), json!(self.accessor(bytemuck::cast_slice(normals), FLOAT, "VEC3", n, None)));
        }
        let indices = self.accessor(bytemuck::cast_slice(indices), UNSIGNED_INT, "SCALAR", indices.len(), None);
        let material = self.material(slot);
        self.meshes.push(json!({ "name": name, "primitives": [{ "attributes": attributes, "indices": indices, "material": material }] }));
        self.meshes.len() - 1
    }

    /// A plain material per slot, so the reference reads as skin, horn, eye and cloth.
    fn material(&mut self, slot: MaterialSlot) -> usize {
        if let Some((_, m)) = self.slots.iter().find(|(s, _)| *s == slot) {
            return *m;
        }
        let (name, colour, roughness) = match slot {
            MaterialSlot::Skin => ("skin", [0.8, 0.62, 0.52], 0.7),
            MaterialSlot::Hard => ("hard", [0.85, 0.8, 0.68], 0.4),
            MaterialSlot::Eye => ("eye", [0.1, 0.07, 0.05], 0.1),
            MaterialSlot::Cloth => ("cloth", [0.35, 0.4, 0.55], 0.9),
            MaterialSlot::Wing => ("wing", [0.6, 0.5, 0.45], 0.6),
        };
        let [r, g, b] = colour;
        self.materials.push(json!({
            "name": name,
            "doubleSided": slot == MaterialSlot::Wing,
            "pbrMetallicRoughness": { "baseColorFactor": [r, g, b, 1.0], "metallicFactor": 0.0, "roughnessFactor": roughness },
        }));
        let m = self.materials.len() - 1;
        self.slots.push((slot, m));
        m
    }
}

/// A node's `translation`, `rotation` and `scale` (identity ones left out).
fn trs(m: &Affine3A) -> Map<String, Value> {
    let (scale, rotation, translation) = m.to_scale_rotation_translation();
    let mut n = Map::new();
    if translation != Vec3::ZERO {
        n.insert("translation".into(), json!(translation.to_array()));
    }
    if rotation != Quat::IDENTITY {
        n.insert("rotation".into(), json!(rotation.normalize().to_array()));
    }
    if scale != Vec3::ONE {
        n.insert("scale".into(), json!(scale.to_array()));
    }
    n
}

/// Removes `null`s, which some DCCs can't store as custom properties (an absent `Option` reads
/// back as `None`).
fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k, strip_nulls(v))).collect()),
        Value::Array(a) => Value::Array(a.into_iter().map(strip_nulls).collect()),
        v => v,
    }
}

/// Leaves out the document's empty top-level arrays (glTF forbids them).
fn drop_empty(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(_, v)| !v.as_array().is_some_and(Vec::is_empty)).collect()),
        v => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{LodLevel, PartLodRules, test_assets};
    use crate::surface::{AdaptiveContour, SurfaceSettings, mesh_lod};

    fn sample(name: &str) -> BodyPlan {
        let path = format!("{}/assets/mobs/{name}.body.ron", env!("CARGO_MANIFEST_DIR"));
        ron::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn built(plan: &BodyPlan) -> Anatomy {
        Anatomy::from_plan(plan, &PartLodRules::default(), &test_assets(plan)).unwrap()
    }

    /// Every sample plan exports, and the glTF reader takes the file: one empty per authored
    /// part, each with its part and frame.
    #[test]
    fn every_sample_plan_exports() {
        for (name, plan) in crate::interchange::sample_plans() {
            let bytes = export_body(&BodyExport { plan: &plan, anatomy: &built(&plan), assets: &test_assets(&plan), coat: None, outfit: None, skin: None }).unwrap_or_else(|e| panic!("{name}: {e}"));
            let gltf = gltf::Gltf::from_slice(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            let parts = gltf
                .nodes()
                .filter(|n| n.extras().as_ref().is_some_and(|x| x.get().contains("\"part\"")))
                .count();
            let mut authored = 0;
            plan.visit(|_, _| authored += 1);
            assert_eq!(parts, authored, "{name}");
        }
    }

    /// The reference skin is skinned to a joint per bone, and its file is valid glTF.
    #[test]
    fn reference_skin_exports() {
        let plan = sample("quadruped");
        let anatomy = built(&plan);
        let settings = SurfaceSettings::default();
        let skin = mesh_lod(&AdaptiveContour, &anatomy, LodLevel::Far, settings.get(LodLevel::Far), 0).unwrap();
        let bytes = export_body(&BodyExport { plan: &plan, anatomy: &anatomy, assets: &test_assets(&plan), coat: Some(&Coat::default()), outfit: None, skin: Some(&skin) }).unwrap();
        let gltf = gltf::Gltf::from_slice(&bytes).unwrap();
        let s = gltf.skins().next().unwrap();
        assert_eq!(s.joints().count(), anatomy.skeleton.len());
        let mesh = gltf.meshes().next().unwrap();
        assert_eq!(mesh.primitives().count(), skin.submeshes.len());
        let root = gltf.nodes().find(|n| n.name() == Some(KEY)).unwrap();
        assert!(root.extras().as_ref().unwrap().get().contains("\"coat\""));
    }
}
