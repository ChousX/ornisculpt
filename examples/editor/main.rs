//! Live body plan editor: one mob with every value of its plan on screen, shaped with the mouse
//! or adjusted from the keyboard, shown as you go and written back to the RON on save.
//!
//! ```text
//! cargo run --release --example editor [mobs/plan.body.ron]
//! ```
//!
//! Mouse: click a part to select it · drag it over its parent's skin (parts on fixed slots move
//! freely, in the view plane) · Shift+drag turns it · scroll over it to scale it · drag empty
//! space or right-drag to orbit · scroll elsewhere to zoom.
//!
//! Keys: Tab / Shift+Tab part · Up / Down field (`PageUp` / `PageDown` jump) · Left / Right
//! adjust (Shift x10, Ctrl x0.1; toggles bools, cycles enum variants) · Delete remove the part ·
//! Ctrl+Z undo · Ctrl+S save · G skeleton gizmos. The selected part's bones are always drawn
//! (every copy of a mirrored or repeated part).
//!
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot once the body is ready and exits;
//! `ORNISCULPT_VIEW=yaw,pitch[,distance]` sets the camera for it and hides the panel.
//! `ORNISCULPT_EDITOR_TEST=1` first steps through a few edits by itself (held key, a drag over
//! the parent, a pause, undo), logs what each one cost, then takes the screenshot of the final
//! body. `ORNISCULPT_PREVIEW_FULL_BAKE=1` bakes every preview brick after every edit rather than
//! only those that changed (for checking the two look the same).
//!
//! The fields come from reflection over the plan, so every number, flag and unit enum variant in a
//! part is editable; strings and `None` are shown but fixed. Mirrored and repeated parts are one
//! entry (the authored plan, not the expanded one). Saving rewrites the file from the edited plan,
//! so its comments and formatting are lost. The file is watched: a save from another editor
//! replaces unsaved edits here.
//!
//! Every edit shows on the next frame: the anatomy is rebuilt (about a millisecond) and its
//! primitives are ray-marched on the GPU (`SdfPreview`), clay coloured, the selected part tinted,
//! wing sheets drawn as the anatomy's own triangles, the coat left out. Once edits pause the body
//! is meshed at full quality in the background and replaces the preview. With one body to build it gets every core
//! but one, and bodies not shown recently are dropped from the generation cache so a long session
//! doesn't fill memory.

use std::collections::VecDeque;
use std::fmt::Write as _;

use bevy::asset::AssetPlugin;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::math::{Affine3A, Vec3A};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::reflect::enums::{DynamicEnum, DynamicVariant, VariantInfo, VariantType};
use bevy::reflect::{GetPath, PartialReflect, ReflectMut, ReflectRef, TypeInfo};
use bevy::window::{PresentMode, PrimaryWindow};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{
    Anatomy, AttachSlot, BodyPlan, Form, LodLevel, PartAssets, PartLodRules, PartNode, PartPath, Repeat, apply_form, mirror_point, path_matches,
    referenced_assets, sample_axis,
};
use ornisculpt::num::Idx;
use ornisculpt::mob::{BodyFailed, BodyKey, BodyReady, GenBudget, GenCache, GenRequest, GeneratedBody, LiveBody, Mob, MobBody, PartAssetCache};
use ornisculpt::surface::{BodyField, BodyFieldOptions, Field as _, SdfPreview, SdfPreviewAssets, SdfPreviewPlugin};

#[path = "../common/mod.rs"]
mod common;
mod script;

use script::{Script, scripted_edits};

fn main() {
    let path = common::asset_arg(1).unwrap_or_else(|| "mobs/quadruped.body.ron".into());
    let test = std::env::var_os("ORNISCULPT_EDITOR_TEST").is_some();
    let mut app = App::new();
    app.add_plugins((
        common::default_plugins(Window {
            title: "Ornisculpt editor".into(),
            // The self-test's frame times are a measure: don't wait for the display.
            present_mode: if test { PresentMode::AutoNoVsync } else { PresentMode::default() },
            ..default()
        })
        .set(AssetPlugin { watch_for_changes_override: Some(true), ..default() }),
        OrnisculptPlugin,
        SdfPreviewPlugin,
    ))
    .insert_resource(Editor::new(path))
    // One body at a time: the meshing stages get every core but one (the main and render
    // threads share it; the default keeps two for a crowd's worth of bodies).
    .insert_resource(GenBudget { reserved_threads: 1, ..default() })
    .insert_resource(GlobalAmbientLight { brightness: 450.0, ..default() })
    .init_resource::<Orbit>()
    .init_resource::<KeyRepeat>()
    .add_observer(on_ready)
    .add_observer(on_failed)
    .add_systems(Startup, setup)
    .add_systems(
        Update,
        (sync_from_asset, pointer, controls, update_preview, drive_generation, swap_views, orbit_camera, draw_skeleton, render_panel, screenshot).chain(),
    );
    if test {
        app.init_resource::<Script>().add_systems(Update, scripted_edits.after(pointer).before(update_preview));
    }
    app.run();
}

/// Seconds after the last edit before the full-quality mesh is built. Short, so a pause shows the
/// real body soon; not zero, so a run of key presses doesn't start a build between each pair.
const REFINE_DELAY: f32 = 0.25;
/// Generated bodies kept in the cache: the ones shown most recently, so undo and a step back
/// come up at once. Everything older is dropped with its meshes.
const KEEP_BODIES: usize = 12;
/// Edits kept for undo.
const UNDO_DEPTH: usize = 200;
/// Fields shown at once.
const FIELD_WINDOW: usize = 32;
/// Scale step per scroll line over a part.
const SCROLL_SCALE: f32 = 1.06;
/// Degrees a part turns per pixel of Shift+drag.
const TURN_PER_PIXEL: f32 = 0.4;
/// 1 / φ, for golden-section searches.
const GOLDEN: f32 = 0.618_034;
/// The preview's clay.
const CLAY: Color = Color::srgb(0.78, 0.66, 0.55);

const HELP: &str = "click part, drag it, Shift+drag turn, scroll scale, right-drag orbit\n\
Tab part  Up/Down field  Left/Right adjust (Shift x10, Ctrl x0.1)  Delete part  Ctrl+Z undo  Ctrl+S save  G skeleton";

#[derive(Resource)]
struct Editor {
    /// Asset path (`mobs/quadruped.body.ron`).
    path: String,
    handle: Handle<BodyPlan>,
    mob: Entity,
    preview: Entity,
    /// The plan being edited: the asset as loaded, plus unsaved edits.
    plan: Option<BodyPlan>,
    parts: Vec<PartEntry>,
    part: usize,
    fields: Vec<Field>,
    field: usize,
    undo: Vec<BodyPlan>,
    /// Counts edits (a load is one): what the preview and the mesh are compared by.
    version: u64,
    /// When the preview of the latest version went up (seconds since start).
    edited_at: f64,
    /// Version and selected part the preview shows.
    previewed: Option<(u64, usize)>,
    /// The anatomy and field the preview was drawn from (bones for the gizmos, field for picking).
    body: Option<PreviewBody>,
    /// The full rebuild that hasn't come back yet (`BodyReady` or `BodyFailed`): its version and
    /// when it went. Superseded submissions never come back, so this is always the latest.
    submitted: Option<(u64, f64)>,
    /// Version of the mesh the mob has.
    shown: Option<u64>,
    /// Version whose full rebuild failed.
    full_failed: Option<u64>,
    /// Last preview build (anatomy, field, packing) and full rebuild (submit to attach), in ms.
    preview_ms: Option<f64>,
    full_ms: Option<f64>,
    /// Bricks of the last preview baked anew, of all its bricks.
    rebaked: (u32, u32),
    /// The mouse is shaping a part.
    drag: Option<Drag>,
    /// Keys of the bodies shown most recently, newest last (see `KEEP_BODIES`).
    recent: VecDeque<BodyKey>,
    /// Asset events before this time (seconds since start) are our own writes: not reloaded.
    ignore_until: f64,
    status: String,
    gizmos: bool,
    /// `ORNISCULPT_PREVIEW_FULL_BAKE`: rebake every brick of the preview after every edit.
    full_bake: bool,
    /// Frames since the body became ready (for screenshots).
    ready_frames: Option<u32>,
}

/// What the preview was built from.
struct PreviewBody {
    anatomy: Anatomy,
    field: BodyField,
    /// Idle pose (as the field), model space, per bone.
    model: Vec<Affine3A>,
}

/// A part being shaped with the mouse. The undo entry is taken at the first change.
struct Drag {
    /// The plan when the drag began.
    start: BodyPlan,
    changed: bool,
    mode: DragMode,
}

enum DragMode {
    /// Slides an `Along` or `Surface` attach over the parent: the change in the parent's
    /// coordinates under the cursor since the grab.
    Slide { parent: usize, mirrored: bool, grab: (f32, f32) },
    /// Moves the offset: the cursor's motion on the view plane through the grab point, through
    /// the inverse of how the offset moves the part's root (measured at the grab).
    Move { grab: Vec3, normal: Vec3, inverse: Mat3 },
    /// Shift: turns the offset by the cursor's motion.
    Turn { cursor: Vec2 },
}

/// One part of the authored plan.
struct PartEntry {
    path: PartPath,
    /// Reflect path from the plan: `root.children[1].children[0]`, or `tone`.
    reflect: String,
    depth: usize,
    /// Variant of the part kind, or what the pseudo-part is.
    kind: String,
    /// `x2` for a mirrored part, `x3` for three copies, `x6` for both.
    copies: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Float,
    Int,
    Bool,
    /// A unit variant of an enum: cycles through the enum's unit variants.
    Variant,
    Fixed,
}

/// One editable value of the selected part.
struct Field {
    label: String,
    /// Reflect path from the plan.
    reflect: String,
    kind: FieldKind,
    value: String,
}

impl Editor {
    fn new(path: String) -> Self {
        Self {
            path,
            handle: Handle::default(),
            mob: Entity::PLACEHOLDER,
            preview: Entity::PLACEHOLDER,
            plan: None,
            parts: Vec::new(),
            part: 0,
            fields: Vec::new(),
            field: 0,
            undo: Vec::new(),
            version: 0,
            edited_at: 0.0,
            previewed: None,
            body: None,
            submitted: None,
            shown: None,
            full_failed: None,
            preview_ms: None,
            full_ms: None,
            rebaked: (0, 0),
            drag: None,
            recent: VecDeque::new(),
            ignore_until: 0.0,
            status: "loading".into(),
            gizmos: false,
            full_bake: std::env::var_os("ORNISCULPT_PREVIEW_FULL_BAKE").is_some(),
            ready_frames: None,
        }
    }

    /// Recomputes the part list and the selected part's fields, keeping the selection in range.
    fn rebuild(&mut self) {
        let Some(plan) = &self.plan else {
            self.parts.clear();
            self.fields.clear();
            return;
        };
        self.parts = collect_parts(plan);
        self.part = self.part.min(self.parts.len().saturating_sub(1));
        self.fields = self.parts.get(self.part).map_or_else(Vec::new, |p| collect_fields(plan, p));
        self.field = self.field.min(self.fields.len().saturating_sub(1));
    }

    /// Records `plan` (the plan before an edit) for undo.
    fn push_undo(&mut self, plan: BodyPlan) {
        if self.undo.len() >= UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.undo.push(plan);
    }

    /// Records the plan before an edit, for undo.
    fn snapshot(&mut self) {
        if let Some(plan) = self.plan.clone() {
            self.push_undo(plan);
        }
    }

    /// After an edit: refresh the panel; `update_preview` shows it this frame.
    fn edited(&mut self) {
        self.rebuild();
        self.version += 1;
    }

    fn select_part(&mut self, part: usize) {
        if part != self.part {
            self.part = part;
            self.field = 0;
        }
        self.rebuild();
    }

    /// Steps the selected field: `dir` is -1 or 1, `scale` the key modifier.
    fn adjust_selected(&mut self, dir: f32, scale: f32) {
        let Some(field) = self.fields.get(self.field) else { return };
        if field.kind == FieldKind::Fixed {
            return;
        }
        let degrees = field.label.contains("deg");
        let reflect = field.reflect.clone();
        let Some(plan) = self.plan.as_mut() else { return };
        let before = plan.clone();
        let Ok(value) = plan.reflect_path_mut(reflect.as_str()) else { return };
        if !adjust(value, dir, scale, degrees) {
            return;
        }
        self.push_undo(before);
        self.edited();
    }

    /// Removes the selected part (and everything attached to it).
    fn delete_selected(&mut self) {
        let Some(path) = self.parts.get(self.part).map(|e| e.path.clone()) else { return };
        let (Some(parent), Some(name)) = (path.parent(), path.segments().last()) else {
            self.status = "the root part and the body tone stay".into();
            return;
        };
        let name = name.to_owned();
        self.snapshot();
        let Some(node) = self.plan.as_mut().and_then(|p| p.node_mut(&parent)) else { return };
        node.children.retain(|c| c.name != name);
        self.status = format!("removed {path}");
        self.part = self.part.saturating_sub(1);
        self.field = 0;
        self.edited();
    }

    fn undo(&mut self) {
        let Some(plan) = self.undo.pop() else {
            self.status = "nothing to undo".into();
            return;
        };
        self.plan = Some(plan);
        self.status = format!("undo ({} left)", self.undo.len());
        self.edited();
    }

    /// Writes the plan back to its file. The watcher then reloads it, which we ignore.
    fn save(&mut self, now: f64) {
        let Some(plan) = &self.plan else { return };
        let file = format!("{}/assets/{}", env!("CARGO_MANIFEST_DIR"), self.path);
        let written = ron::ser::to_string_pretty(plan, ron::ser::PrettyConfig::new())
            .map_err(|e| e.to_string())
            .and_then(|text| std::fs::write(&file, text).map_err(|e| e.to_string()));
        self.status = match written {
            Ok(()) => {
                self.ignore_until = now + 2.0;
                format!("saved {file}")
            }
            Err(e) => format!("save failed: {e}"),
        };
    }

    /// The authored entry an expanded part (`torso/limb.front.l`) was made from.
    fn entry_of(&self, path: &PartPath) -> Option<usize> {
        self.parts.iter().position(|e| e.reflect != "tone" && path_matches(&e.path, path))
    }

    /// The part the ray (model space) first hits on the preview, with the hit point.
    fn pick(&self, ray: Ray3d) -> Option<(usize, Vec3)> {
        let body = self.body.as_ref()?;
        let hit = march(|p| body.field.distance(p), ray)?;
        let part = body.field.part_at(hit.into())?;
        Some((usize::from(part), hit))
    }

    /// Starts shaping the grabbed copy `copy`, hit at `hit` (model space). Shift turns it.
    fn begin_drag(&mut self, copy: usize, hit: Vec3, ray: Ray3d, cursor: Vec2, turn: bool, ctx: &BuildCtx) {
        let (Some(plan), Some(body)) = (&self.plan, &self.body) else { return };
        let Some(built) = body.anatomy.parts.get(copy) else { return };
        let Some(node) = self.parts.get(self.part).and_then(|e| plan.node(&e.path)) else { return };
        if node.name == plan.root.name {
            // The root has no attach to move; turning and scaling it are the panel's.
            return;
        }
        let parent = built.parent.map(usize::from);
        let slide = matches!(node.attach, AttachSlot::Along(_) | AttachSlot::Surface { .. });
        let mode = if turn {
            DragMode::Turn { cursor }
        } else if let (true, Some(parent)) = (slide, parent)
            && let Some(grab) = self.surface_coords(parent, built.mirrored, ray)
        {
            DragMode::Slide { parent, mirrored: built.mirrored, grab }
        } else {
            let inverse = self.move_inverse(copy, ctx).unwrap_or(Mat3::IDENTITY);
            DragMode::Move { grab: hit, normal: ray.direction.as_vec3(), inverse }
        };
        self.drag = Some(Drag { start: plan.clone(), changed: false, mode });
    }

    /// Continues the drag for the cursor's `ray` (model space).
    fn continue_drag(&mut self, ray: Ray3d, cursor: Vec2) {
        let Some(drag) = self.drag.as_ref() else { return };
        let Some(path) = self.parts.get(self.part).map(|e| e.path.clone()) else { return };
        let Some(start) = drag.start.node(&path).cloned() else { return };
        let mut node = start.clone();
        match drag.mode {
            DragMode::Slide { parent, mirrored, grab } => {
                let Some((t, around)) = self.surface_coords(parent, mirrored, ray) else { return };
                let (dt, da) = (t - grab.0, wrap_degrees(around - grab.1));
                match (&mut node.attach, &mut node.repeat) {
                    (AttachSlot::Along(_) | AttachSlot::Surface { .. }, Repeat::Series { from, to, .. }) => {
                        let shift = dt.clamp(-from.min(*to), 1.0 - from.max(*to));
                        *from += shift;
                        *to += shift;
                    }
                    (AttachSlot::Along(at), _) => *at = (*at + dt).clamp(0.0, 1.0),
                    (AttachSlot::Surface { t, .. }, _) => *t = (*t + dt).clamp(0.0, 1.0),
                    _ => {}
                }
                if let AttachSlot::Surface { around_deg, .. } = &mut node.attach {
                    *around_deg = wrap_degrees(*around_deg + da);
                }
            }
            DragMode::Move { grab, normal, inverse } => {
                let Some(t) = ray.intersect_plane(grab, InfinitePlane3d::new(normal)) else { return };
                node.offset.translate = start.offset.translate + inverse * (ray.get_point(t) - grab);
            }
            DragMode::Turn { cursor: from } => {
                let d = (cursor - from) * TURN_PER_PIXEL;
                node.offset.rotate_deg.y = wrap_degrees(start.offset.rotate_deg.y + d.x);
                node.offset.rotate_deg.x = wrap_degrees(start.offset.rotate_deg.x + d.y);
            }
        }
        self.set_node(&path, node);
    }

    fn end_drag(&mut self) {
        self.drag = None;
    }

    /// Replaces the authored node at `path` (keeping its children) during a drag.
    fn set_node(&mut self, path: &PartPath, node: PartNode) {
        let Some(target) = self.plan.as_mut().and_then(|p| p.node_mut(path)) else { return };
        if target.attach == node.attach && target.offset == node.offset && target.repeat == node.repeat {
            return;
        }
        target.attach = node.attach;
        target.offset = node.offset;
        target.repeat = node.repeat;
        if let Some(drag) = self.drag.as_mut()
            && !drag.changed
        {
            drag.changed = true;
            let start = drag.start.clone();
            self.push_undo(start);
        }
        self.edited();
    }

    /// Scales the selected part's offset by `factor`.
    fn scale_selected(&mut self, factor: f32) {
        let Some(path) = self.parts.get(self.part).map(|e| e.path.clone()) else { return };
        let Some(plan) = self.plan.as_mut() else { return };
        let before = plan.clone();
        let Some(node) = plan.node_mut(&path) else { return };
        node.offset.scale = (node.offset.scale * factor).clamp(0.05, 20.0);
        self.push_undo(before);
        self.edited();
    }

    /// Where `ray` (model space) meets the parent copy `parent`'s own skin, as its attach
    /// coordinates: `t` along it and degrees around it (from the back, toward the left side).
    /// A mirrored copy is measured in its logical (left) frame.
    fn surface_coords(&self, parent: usize, mirrored: bool, ray: Ray3d) -> Option<(f32, f32)> {
        let body = self.body.as_ref()?;
        let part = body.anatomy.parts.get(parent)?;
        let prims = part.primitives.start.idx()..part.primitives.end.idx();
        let hit = march(|p| body.field.distance_among(prims.clone(), p), ray)?;
        let hit = if mirrored { mirror_point(hit) } else { hit };
        let axis = &part.axis;
        // The nearest point on the parent's axis: the best of 64 samples, then narrowed down
        // between its neighbours (golden section), so the part glides rather than steps.
        let d = |t: f32| sample_axis(axis, t).map_or(f32::INFINITY, |s| s.centre().distance_squared(hit));
        let coarse = (0..=64u8).map(|i| f32::from(i) / 64.0).min_by(|a, b| d(*a).total_cmp(&d(*b)))?;
        let (mut lo, mut hi) = ((coarse - 1.0 / 64.0).max(0.0), (coarse + 1.0 / 64.0).min(1.0));
        for _ in 0..24 {
            let (a, b) = ((hi - lo).mul_add(-GOLDEN, hi), (hi - lo).mul_add(GOLDEN, lo));
            if d(a) < d(b) {
                hi = b;
            } else {
                lo = a;
            }
        }
        let t = f32::midpoint(lo, hi);
        let s = sample_axis(axis, t)?;
        let out = hit - s.centre();
        let dorsal = s.model.rotation * Vec3::NEG_Z;
        let side = s.model.rotation * Vec3::X;
        Some((t, out.dot(side).atan2(out.dot(dorsal)).to_degrees()))
    }

    /// How the selected part's offset moves the root bone of copy `copy`: rebuilds the anatomy
    /// with the offset nudged along each axis, and inverts the result.
    fn move_inverse(&self, copy: usize, ctx: &BuildCtx) -> Option<Mat3> {
        const NUDGE: f32 = 0.01;
        let plan = self.plan.as_ref()?;
        let body = self.body.as_ref()?;
        let path = &self.parts.get(self.part)?.path;
        let built = body.anatomy.parts.get(copy)?;
        let root = |a: &Anatomy| -> Option<Vec3> {
            let part = a.part(&built.path)?;
            let bone = usize::from(part.bones.start);
            a.skeleton.model_from(&a.idle_pose()).get(bone).map(|m| Vec3::from(m.translation))
        };
        let base = root(&body.anatomy)?;
        let mut columns = [Vec3::ZERO; 3];
        for (axis, column) in columns.iter_mut().enumerate() {
            let mut nudged = plan.clone();
            let node = nudged.node_mut(path)?;
            node.offset.translate += Vec3::AXES.get(axis).copied()? * NUDGE;
            *column = (root(&ctx.build(&nudged).ok()?)? - base) / NUDGE;
        }
        let jacobian = Mat3::from_cols(columns[0], columns[1], columns[2]);
        (jacobian.determinant().abs() > 1e-6).then(|| jacobian.inverse())
    }
}

/// Signed degrees in `-180..180`.
fn wrap_degrees(d: f32) -> f32 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

/// First point where `ray` meets the zero set of `distance` (sphere tracing), within 50 m.
fn march(distance: impl Fn(Vec3A) -> f32, ray: Ray3d) -> Option<Vec3> {
    let mut t = 0.0;
    for _ in 0..256 {
        let p = ray.get_point(t);
        let d = distance(p.into());
        if d < 2e-4 {
            return Some(p);
        }
        t = d.max(1e-4).mul_add(0.9, t);
        if t > 50.0 {
            return None;
        }
    }
    None
}

/// What building an anatomy needs from the app.
struct BuildCtx<'a> {
    rules: &'a PartLodRules,
    assets: &'a PartAssets,
}

impl BuildCtx<'_> {
    fn build(&self, plan: &BodyPlan) -> Result<Anatomy, String> {
        common::build_plan(plan, *self.rules, self.assets)
    }
}

/// The authored parts, depth first, after a pseudo-part for the body tone.
fn collect_parts(plan: &BodyPlan) -> Vec<PartEntry> {
    fn go(node: &PartNode, path: PartPath, reflect: String, depth: usize, out: &mut Vec<PartEntry>) {
        let repeat = match node.repeat {
            Repeat::Once => 1,
            Repeat::Series { count, .. } | Repeat::Radial { count } => usize::from(count),
        };
        let copies = repeat * if node.mirror { 2 } else { 1 };
        let kind = match node.kind.reflect_ref() {
            ReflectRef::Enum(e) => e.variant_name().to_owned(),
            _ => String::new(),
        };
        let children: Vec<(PartPath, String)> =
            node.children.iter().enumerate().map(|(i, child)| (path.child(&child.name), format!("{reflect}.children[{i}]"))).collect();
        out.push(PartEntry { path, reflect, depth, kind, copies: if copies > 1 { format!("x{copies}") } else { String::new() } });
        for (child, (path, reflect)) in node.children.iter().zip(children) {
            go(child, path, reflect, depth + 1, out);
        }
    }
    let mut out = vec![PartEntry { path: PartPath::root("tone"), reflect: "tone".into(), depth: 0, kind: "body tone".into(), copies: String::new() }];
    go(&plan.root, PartPath::root(&plan.root.name), "root".into(), 0, &mut out);
    out
}

/// Every leaf value of a part, in declaration order.
fn collect_fields(plan: &BodyPlan, entry: &PartEntry) -> Vec<Field> {
    let mut out = Vec::new();
    let Ok(value) = plan.reflect_path(entry.reflect.as_str()) else { return out };
    let skip: &[&str] = if entry.reflect == "tone" { &[] } else { &["name", "children", "side"] };
    walk(value, &entry.reflect, "", skip, &mut out);
    out
}

/// Flattens `value` into fields. `reflect` is its path from the plan, `label` its display name.
fn walk(value: &dyn PartialReflect, reflect: &str, label: &str, skip: &[&str], out: &mut Vec<Field>) {
    let sub = |name: &str| if label.is_empty() { name.to_owned() } else { format!("{label}.{name}") };
    match value.reflect_ref() {
        ReflectRef::Struct(s) => {
            for i in 0..s.field_len() {
                let (Some(name), Some(f)) = (s.name_at(i), s.field_at(i)) else { continue };
                if !skip.contains(&name) {
                    walk(f, &format!("{reflect}.{name}"), &sub(name), &[], out);
                }
            }
        }
        ReflectRef::TupleStruct(t) => {
            for i in 0..t.field_len() {
                let Some(f) = t.field(i) else { continue };
                let l = if t.field_len() == 1 { label.to_owned() } else { sub(&i.to_string()) };
                walk(f, &format!("{reflect}.{i}"), &l, &[], out);
            }
        }
        ReflectRef::Tuple(t) => {
            for i in 0..t.field_len() {
                let Some(f) = t.field(i) else { continue };
                walk(f, &format!("{reflect}.{i}"), &sub(&i.to_string()), &[], out);
            }
        }
        ReflectRef::List(l) => {
            for i in 0..l.len() {
                let Some(f) = l.get(i) else { continue };
                walk(f, &format!("{reflect}[{i}]"), &format!("{label}[{i}]"), &[], out);
            }
        }
        ReflectRef::Array(a) => {
            for i in 0..a.len() {
                let Some(f) = a.get(i) else { continue };
                walk(f, &format!("{reflect}[{i}]"), &format!("{label}[{i}]"), &[], out);
            }
        }
        ReflectRef::Enum(e) => {
            let variant = e.variant_name();
            match e.variant_type() {
                VariantType::Unit => {
                    let kind = if variant == "None" { FieldKind::Fixed } else { FieldKind::Variant };
                    out.push(Field { label: label.to_owned(), reflect: reflect.to_owned(), kind, value: variant.to_owned() });
                }
                VariantType::Tuple => {
                    let inner = match (label, variant) {
                        ("kind", _) => variant.to_owned(),
                        (_, "Some") => label.to_owned(),
                        _ => format!("{label}:{variant}"),
                    };
                    for i in 0..e.field_len() {
                        let Some(f) = e.field_at(i) else { continue };
                        let l = if e.field_len() == 1 { inner.clone() } else { format!("{inner}.{i}") };
                        walk(f, &format!("{reflect}.{i}"), &l, &[], out);
                    }
                }
                VariantType::Struct => {
                    for i in 0..e.field_len() {
                        let (Some(name), Some(f)) = (e.name_at(i), e.field_at(i)) else { continue };
                        walk(f, &format!("{reflect}.{name}"), &format!("{label}:{variant}.{name}"), &[], out);
                    }
                }
            }
        }
        ReflectRef::Opaque(v) => {
            let (kind, value) = leaf(v);
            out.push(Field { label: label.to_owned(), reflect: reflect.to_owned(), kind, value });
        }
        _ => {}
    }
}

/// Calls `$m!` with every integer type a plan holds.
macro_rules! with_int_types {
    ($m:ident) => {
        $m!(u8, u16, u32, u64, usize, i8, i16, i32, i64)
    };
}

/// How a leaf value is edited, and how it reads.
fn leaf(v: &dyn PartialReflect) -> (FieldKind, String) {
    if let Some(x) = v.try_downcast_ref::<f32>() {
        return (FieldKind::Float, fmt_float(*x));
    }
    if let Some(x) = v.try_downcast_ref::<bool>() {
        return (FieldKind::Bool, x.to_string());
    }
    macro_rules! try_ints {
        ($($t:ty),*) => {$(
            if let Some(x) = v.try_downcast_ref::<$t>() {
                return (FieldKind::Int, x.to_string());
            }
        )*};
    }
    with_int_types!(try_ints);
    if let Some(x) = v.try_downcast_ref::<String>() {
        return (FieldKind::Fixed, format!("{x:?}"));
    }
    (FieldKind::Fixed, format!("<{}>", v.reflect_short_type_path()))
}

/// Up to four decimals, trailing zeros dropped.
fn fmt_float(v: f32) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{s}0") } else { s.to_owned() }
}

/// A tenth of the value's leading decimal place: 0.72 steps by 0.01, 25 by 1, 0.0035 by 0.0001.
fn step_of(v: f32) -> f32 {
    let a = v.abs();
    if a < 1e-9 {
        return 0.01;
    }
    // The largest power of ten at most a tenth of the value, within 1e-6..=1e6.
    (-6..=6).rev().map(|e| 10f32.powi(e)).find(|s| s * 10.0 <= a).unwrap_or(1e-6)
}

/// Steps a leaf value in place. Returns whether it changed.
fn adjust(value: &mut dyn PartialReflect, dir: f32, scale: f32, degrees: bool) -> bool {
    if let Some(v) = value.try_downcast_mut::<f32>() {
        let step = if degrees { 1.0 } else { step_of(*v) } * scale;
        *v = (dir.mul_add(step, *v) / step).round() * step;
        return true;
    }
    if let Some(v) = value.try_downcast_mut::<bool>() {
        *v = !*v;
        return true;
    }
    macro_rules! try_ints {
        ($($t:ty),*) => {$(
            if let Some(v) = value.try_downcast_mut::<$t>() {
                let next = if dir > 0.0 { v.saturating_add(1) } else { v.saturating_sub(1) };
                let changed = next != *v;
                *v = next;
                return changed;
            }
        )*};
    }
    with_int_types!(try_ints);
    let ReflectMut::Enum(e) = value.reflect_mut() else { return false };
    if e.variant_type() != VariantType::Unit {
        return false;
    }
    let Some(TypeInfo::Enum(info)) = e.get_represented_type_info() else { return false };
    let units: Vec<&str> = info.iter().filter(|v| matches!(v, VariantInfo::Unit(_))).map(VariantInfo::name).collect();
    let Some(i) = units.iter().position(|n| *n == e.variant_name()) else { return false };
    let next = if dir > 0.0 { (i + 1) % units.len() } else { (i + units.len() - 1) % units.len() };
    let Some(name) = units.get(next) else { return false };
    if next == i {
        return false;
    }
    e.apply(&DynamicEnum::new(*name, DynamicVariant::Unit));
    true
}

#[derive(Component)]
struct Panel;

#[derive(Resource)]
struct Orbit {
    view: common::Orbit,
    /// A left-drag that began on empty space orbits.
    dragging: bool,
    /// This frame's scroll scaled a part instead of zooming.
    scroll_used: bool,
}

impl Default for Orbit {
    fn default() -> Self {
        let view = common::Orbit { yaw: 0.9, pitch: 0.35, distance: 3.0, target: Vec3::new(0.0, 0.5, 0.0), zoom: (0.3, 30.0) };
        Self { view, dragging: false, scroll_used: false }
    }
}

impl Orbit {
    /// Frames a grounded body of this radius (the first body only; after that the view is yours).
    fn fit(&mut self, radius: f32) {
        self.view.target = Vec3::new(0.0, radius * 0.5, 0.0);
        self.view.distance = (radius * 2.6).clamp(0.3, 30.0);
    }

    /// `ORNISCULPT_VIEW=yaw,pitch[,distance]` (radians, metres): a fixed first view, for
    /// screenshots. Distance defaults to the fitted one.
    fn view_from_env(&mut self) {
        let Ok(view) = std::env::var("ORNISCULPT_VIEW") else { return };
        let values: Vec<f32> = view.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if let [yaw, pitch, rest @ ..] = values.as_slice() {
            (self.view.yaw, self.view.pitch) = (*yaw, pitch.clamp(-1.4, 1.4));
            if let Some(distance) = rest.first() {
                self.view.distance = distance.clamp(0.3, 30.0);
            }
        }
    }
}

/// Held arrow keys repeat after a delay.
#[derive(Resource, Default)]
struct KeyRepeat {
    key: Option<KeyCode>,
    next: f32,
}

impl KeyRepeat {
    fn fires(&mut self, keys: &ButtonInput<KeyCode>, dt: f32, key: KeyCode) -> bool {
        if keys.just_pressed(key) {
            self.key = Some(key);
            self.next = 0.35;
            return true;
        }
        if self.key != Some(key) {
            return false;
        }
        if !keys.pressed(key) {
            self.key = None;
            return false;
        }
        self.next -= dt;
        if self.next > 0.0 {
            return false;
        }
        self.next = 0.05;
        true
    }
}

fn setup(
    mut commands: Commands,
    mut editor: ResMut<Editor>,
    assets: Res<AssetServer>,
    mut preview_assets: SdfPreviewAssets,
    mut gizmo_config: ResMut<GizmoConfigStore>,
) {
    // Bones are inside the body: draw them over it.
    gizmo_config.config_mut::<DefaultGizmoConfigGroup>().0.depth_bias = -1.0;
    commands.spawn(Camera3d::default());
    common::spawn_stage(&mut commands, &mut preview_assets.meshes, &mut preview_assets.standard, 20.0, Vec3::new(3.0, 6.0, 2.0));
    editor.handle = assets.load(editor.path.clone());
    editor.mob = commands.spawn(Mob::new(editor.handle.clone())).id();
    // The preview shares the mob's model space: both sit at the origin.
    editor.preview = commands.spawn(SdfPreview::bundle(&mut preview_assets, CLAY)).id();
    // A scripted view is for screenshots of the body alone.
    let panel = if std::env::var_os("ORNISCULPT_VIEW").is_some() { Visibility::Hidden } else { Visibility::Inherited };
    commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(8.0), top: Val::Px(8.0), padding: UiRect::all(Val::Px(8.0)), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
            panel,
        ))
        .with_child((Panel, Text::new("loading"), TextFont { font_size: FontSize::Px(13.0), ..default() }, TextColor(Color::srgb(0.92, 0.92, 0.9))));
    info!("editing {}", editor.path);
    info!("controls: {HELP}");
}

/// Takes the plan from the asset when it loads or changes on disk (not after our own writes).
fn sync_from_asset(mut events: MessageReader<AssetEvent<BodyPlan>>, plans: Res<Assets<BodyPlan>>, mut editor: ResMut<Editor>, time: Res<Time>) {
    let id = editor.handle.id();
    let first = editor.plan.is_none();
    // Loading raises `Added` and `LoadedWithDependencies` (frames apart); only a change on disk
    // is a reload once the plan is here.
    let changed = events.read().any(|ev| match ev {
        AssetEvent::Added { id: i } | AssetEvent::LoadedWithDependencies { id: i } => first && *i == id,
        AssetEvent::Modified { id: i } => *i == id,
        AssetEvent::Removed { .. } | AssetEvent::Unused { .. } => false,
    });
    if !changed || (!first && time.elapsed_secs_f64() < editor.ignore_until) {
        return;
    }
    let Some(plan) = plans.get(id) else { return };
    if !first {
        editor.snapshot();
    }
    editor.plan = Some(plan.clone());
    editor.status = if first { "loaded".into() } else { "reloaded from disk".into() };
    editor.drag = None;
    editor.edited();
    if first {
        // The plugin builds the first body itself, from the asset.
        editor.submitted = Some((editor.version, time.elapsed_secs_f64()));
    }
}

/// The mouse: picks, drags, turns and scales parts; leaves the rest to `orbit_camera`.
#[allow(clippy::too_many_arguments)]
fn pointer(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Res<AccumulatedMouseScroll>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform), With<Camera3d>>,
    mobs: Query<&GlobalTransform, With<Mob>>,
    rules: Res<PartLodRules>,
    cache: Res<PartAssetCache>,
    mut editor: ResMut<Editor>,
    mut orbit: ResMut<Orbit>,
) {
    orbit.scroll_used = false;
    if buttons.just_released(MouseButton::Left) {
        editor.bypass_change_detection().end_drag();
        orbit.dragging = false;
    }
    let Some(cursor) = window.cursor_position() else { return };
    let (camera, camera_transform) = *camera;
    let Ok(world_ray) = camera.viewport_to_world(camera_transform, cursor) else { return };
    // Into the mob's model space (where the preview's field lives).
    let model_from_world = mobs.get(editor.mob).map_or(Affine3A::IDENTITY, |g| g.affine().inverse());
    let Ok(direction) = Dir3::new(model_from_world.transform_vector3(world_ray.direction.as_vec3())) else { return };
    let ray = Ray3d::new(model_from_world.transform_point3(world_ray.origin), direction);
    let ctx = BuildCtx { rules: &rules, assets: &cache.assets };

    if editor.drag.is_some() && buttons.pressed(MouseButton::Left) {
        editor.continue_drag(ray, cursor);
        return;
    }
    let picked = editor.pick(ray).and_then(|(copy, hit)| {
        let path = &editor.body.as_ref()?.anatomy.parts.get(copy)?.path;
        Some((copy, hit, editor.entry_of(path)?))
    });
    if buttons.just_pressed(MouseButton::Left) {
        if let Some((copy, hit, entry)) = picked {
            editor.select_part(entry);
            let turn = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
            editor.begin_drag(copy, hit, ray, cursor, turn, &ctx);
        } else {
            orbit.dragging = true;
        }
    }
    // Scroll over a part scales it (and selects it); anywhere else it zooms.
    if scroll.delta.y != 0.0
        && let Some((_, _, entry)) = picked
    {
        editor.select_part(entry);
        editor.scale_selected(SCROLL_SCALE.powf(scroll.delta.y));
        orbit.scroll_used = true;
    }
}

fn controls(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut repeat: ResMut<KeyRepeat>,
    mut editor: ResMut<Editor>,
) {
    let dt = time.delta_secs();
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    if ctrl && keys.just_pressed(KeyCode::KeyS) {
        editor.save(time.elapsed_secs_f64());
        return;
    }
    if ctrl && keys.just_pressed(KeyCode::KeyZ) {
        editor.end_drag();
        editor.undo();
        return;
    }
    if keys.just_pressed(KeyCode::KeyG) {
        editor.gizmos = !editor.gizmos;
    }
    if keys.just_pressed(KeyCode::Delete) {
        editor.end_drag();
        editor.delete_selected();
    }
    if keys.just_pressed(KeyCode::Tab) {
        let n = editor.parts.len().max(1);
        let part = if shift { (editor.part + n - 1) % n } else { (editor.part + 1) % n };
        editor.select_part(part);
    }
    let n = editor.fields.len().max(1);
    let field = editor.field;
    if repeat.fires(&keys, dt, KeyCode::ArrowUp) {
        editor.field = (field + n - 1) % n;
    } else if repeat.fires(&keys, dt, KeyCode::ArrowDown) {
        editor.field = (field + 1) % n;
    } else if repeat.fires(&keys, dt, KeyCode::PageUp) {
        editor.field = field.saturating_sub(10);
    } else if repeat.fires(&keys, dt, KeyCode::PageDown) {
        editor.field = (field + 10).min(n - 1);
    }
    let scale = if shift { 10.0 } else if ctrl { 0.1 } else { 1.0 };
    if repeat.fires(&keys, dt, KeyCode::ArrowRight) {
        editor.adjust_selected(1.0, scale);
    } else if repeat.fires(&keys, dt, KeyCode::ArrowLeft) {
        editor.adjust_selected(-1.0, scale);
    }
}

/// Rebuilds the anatomy and the ray-marched preview whenever the plan or the selection changed:
/// the same frame as the edit.
fn update_preview(
    mut editor: ResMut<Editor>,
    rules: Res<PartLodRules>,
    cache: Res<PartAssetCache>,
    mut previews: Query<&mut SdfPreview>,
    mut preview_assets: SdfPreviewAssets,
    time: Res<Time>,
) {
    let key = (editor.version, editor.part);
    if editor.plan.is_none() || editor.previewed == Some(key) {
        return;
    }
    // Authored meshes (sculpted parts) load with the mob's first generation; wait for them.
    if let Some(plan) = &editor.plan
        && referenced_assets(plan).iter().any(|path| !cache.assets.meshes.contains_key(path))
    {
        editor.bypass_change_detection().status = "loading part meshes".into();
        return;
    }
    let new_version = editor.previewed.is_none_or(|(v, _)| v != editor.version);
    editor.previewed = Some(key);
    let started = Instant::now();
    let ctx = BuildCtx { rules: &rules, assets: &cache.assets };
    let Some(plan) = &editor.plan else { return };
    let anatomy = match ctx.build(plan) {
        Ok(a) => a,
        Err(e) => {
            editor.status = format!("failed: {e}");
            return;
        }
    };
    // In the idle pose (wings folded by their rest fold), as the mob's mesh stands.
    let pose = anatomy.idle_pose();
    let field = BodyField::with_pose(&anatomy, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false }, &pose);
    // The selected authored part matches its expanded copies (`leg` matches `leg.l` and `leg.r`).
    let selected = editor.parts.get(editor.part).filter(|p| p.reflect != "tone").map(|p| &p.path);
    let packed = field.preview(&anatomy, |part| {
        selected.is_some_and(|sel| anatomy.parts.get(usize::from(part)).is_some_and(|p| path_matches(sel, &p.path)))
    });
    let Ok(mut preview) = previews.get_mut(editor.preview) else { return };
    preview.rebake_all = editor.full_bake;
    preview.show(&packed, &mut preview_assets);
    editor.rebaked = (preview.rebaked, u32::try_from(packed.bricks.len()).unwrap_or(u32::MAX));
    let model = anatomy.skeleton.model_from(&pose);
    editor.preview_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    editor.body = Some(PreviewBody { anatomy, field, model });
    if new_version {
        editor.edited_at = time.elapsed_secs_f64();
    }
}

/// Builds the full mesh once edits pause (not mid-drag). A full rebuild in progress is
/// superseded by the next; the plugin finishes it in the background and the result is only cached.
fn drive_generation(mut commands: Commands, mut editor: ResMut<Editor>, time: Res<Time>) {
    let now = time.elapsed_secs_f64();
    let version = editor.version;
    let pending = editor.submitted.is_some_and(|(v, _)| v == version);
    if editor.plan.is_none()
        || editor.shown == Some(version)
        || pending
        || editor.full_failed == Some(version)
        || editor.drag.is_some()
        || now - editor.edited_at < f64::from(REFINE_DELAY)
    {
        return;
    }
    let Some(plan) = &editor.plan else { return };
    let live = match apply_form(plan, &Form::default()) {
        Ok(live) => live,
        Err(e) => {
            editor.full_failed = Some(version);
            editor.status = format!("failed: {e}");
            return;
        }
    };
    commands.entity(editor.mob).insert((LiveBody(live), GenRequest));
    editor.submitted = Some((version, now));
}

/// Shows the mesh when it is of the latest plan, the preview otherwise.
fn swap_views(editor: Res<Editor>, mut visibility: Query<&mut Visibility>) {
    // `ORNISCULPT_SHOW_PREVIEW`: keep the preview up over the mesh (screenshots of the preview).
    let mesh = editor.shown == Some(editor.version) && std::env::var_os("ORNISCULPT_SHOW_PREVIEW").is_none();
    let preview = !mesh && editor.body.is_some();
    for (entity, show) in [(editor.mob, mesh), (editor.preview, preview)] {
        if let Ok(mut v) = visibility.get_mut(entity) {
            v.set_if_neq(if show { Visibility::Inherited } else { Visibility::Hidden });
        }
    }
}

fn on_ready(
    ev: On<BodyReady>,
    mut editor: ResMut<Editor>,
    mut cache: ResMut<GenCache>,
    mut orbit: ResMut<Orbit>,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<&MobBody>,
    time: Res<Time>,
) {
    if ev.entity != editor.mob {
        return;
    }
    let first = editor.shown.is_none() && editor.full_ms.is_none();
    // A body we didn't ask for (the plugin's own hot reload) is of no version we know.
    editor.shown = editor.submitted.take().map(|(version, at)| {
        editor.full_ms = Some((time.elapsed_secs_f64() - at) * 1000.0);
        version
    });
    if editor.shown == Some(editor.version) {
        editor.ready_frames = Some(0);
    }
    let Some(body) = mobs.get(editor.mob).ok().and_then(|b| bodies.get(&b.0)) else { return };
    if first {
        orbit.fit(body.radius);
        orbit.view_from_env();
    }
    let a = &body.anatomy;
    editor.status = format!("{} bones, {} primitives, {} hard parts", a.skeleton.len(), a.primitives.len(), a.hard_parts.len());
    // Keep the bodies shown lately (undo comes back to them at once); drop the rest with their
    // meshes. Superseded generations land in the cache too and go at the next attach.
    let key = body.key;
    editor.recent.retain(|k| *k != key);
    editor.recent.push_back(key);
    while editor.recent.len() > KEEP_BODIES {
        editor.recent.pop_front();
    }
    cache.bodies.retain(|k, _| editor.recent.contains(k));
}

fn on_failed(ev: On<BodyFailed>, mut editor: ResMut<Editor>) {
    if ev.entity != editor.mob {
        return;
    }
    editor.full_failed = editor.submitted.take().map(|(v, _)| v);
    editor.status = format!("failed: {}", ev.error);
}

fn render_panel(editor: Res<Editor>, mut panels: Query<&mut Text, With<Panel>>) {
    if !editor.is_changed() {
        return;
    }
    let mut s = String::new();
    let state = if editor.shown == Some(editor.version) {
        ""
    } else if editor.submitted.is_some_and(|(v, _)| v == editor.version) {
        "   [preview, meshing]"
    } else {
        "   [preview]"
    };
    let _ = writeln!(s, "{}{state}", editor.path);
    let ms = |t: Option<f64>| t.map_or_else(|| "-".to_owned(), |t| format!("{t:.1} ms"));
    let _ = writeln!(
        s,
        "{}   preview {} ({} of {} bricks baked), full {}",
        editor.status,
        ms(editor.preview_ms),
        editor.rebaked.0,
        editor.rebaked.1,
        ms(editor.full_ms)
    );
    let _ = writeln!(s, "{HELP}\n");
    for (i, p) in editor.parts.iter().enumerate() {
        let mark = if i == editor.part { '>' } else { ' ' };
        let name = p.path.segments().last().unwrap_or_default();
        let _ = writeln!(s, "{mark} {:indent$}{name} {}  {}", "", p.copies, p.kind, indent = p.depth * 2);
    }
    s.push('\n');
    let total = editor.fields.len();
    let start = editor.field.saturating_sub(FIELD_WINDOW / 2).min(total.saturating_sub(FIELD_WINDOW));
    if start > 0 {
        let _ = writeln!(s, "  ... {start} above");
    }
    for (i, f) in editor.fields.iter().enumerate().skip(start).take(FIELD_WINDOW) {
        let mark = if i == editor.field { '>' } else { ' ' };
        let fixed = if f.kind == FieldKind::Fixed { "  (fixed)" } else { "" };
        let _ = writeln!(s, "{mark} {:<34} {}{fixed}", f.label, f.value);
    }
    let shown = start + FIELD_WINDOW;
    if total > shown {
        let _ = writeln!(s, "  ... {} below", total - shown);
    }
    for mut text in &mut panels {
        text.0.clone_from(&s);
    }
}

fn screenshot(
    mut commands: Commands,
    mut editor: ResMut<Editor>,
    script: Option<Res<Script>>,
    target: Option<Res<common::ShotTarget>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    if script.is_some_and(|s| !s.done) {
        return;
    }
    if let Some(frames) = editor.ready_frames.as_mut() {
        common::shoot_when_ready(&mut commands, target.as_deref(), &mut exit, frames, path);
    }
}

fn orbit_camera(
    mut orbit: ResMut<Orbit>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    let rotate = (orbit.dragging && buttons.pressed(MouseButton::Left)) || buttons.pressed(MouseButton::Right);
    let zoom = !orbit.scroll_used;
    orbit.view.step(rotate, motion.delta, scroll.delta.y, zoom);
    let view = orbit.view.transform_at(orbit.view.target, 0.0);
    for mut t in &mut camera {
        *t = view;
    }
}

/// The selected part's bones (in every copy of it) in cyan, always; the whole skeleton and the
/// sockets in yellow with `G`. Drawn from the preview's anatomy, so they follow every edit.
fn draw_skeleton(editor: Res<Editor>, mobs: Query<&GlobalTransform, With<Mob>>, mut gizmos: Gizmos) {
    let Some(body) = &editor.body else { return };
    let world = mobs.get(editor.mob).map_or(Affine3A::IDENTITY, GlobalTransform::affine);
    let a = &body.anatomy;
    let selected = editor.parts.get(editor.part).filter(|p| p.reflect != "tone").map(|p| &p.path);
    let highlighted = |bone: usize| {
        selected.is_some_and(|sel| {
            a.parts.iter().any(|p| path_matches(sel, &p.path) && p.bones.contains(&u8::try_from(bone).unwrap_or(u8::MAX)))
        })
    };
    for (i, (bone, model)) in a.skeleton.bones.iter().zip(&body.model).enumerate() {
        let (line, dot) = if highlighted(i) {
            (Color::srgb(0.2, 0.95, 1.0), Color::srgb(0.6, 1.0, 1.0))
        } else if editor.gizmos {
            (Color::srgb(1.0, 0.85, 0.1), Color::srgb(1.0, 0.4, 0.1))
        } else {
            continue;
        };
        let g = world * *model;
        let start = Vec3::from(g.translation);
        gizmos.line(start, g.transform_point3(Vec3::Y * bone.length), line);
        gizmos.sphere(Isometry3d::from_translation(start), 0.008, dot);
    }
    if !editor.gizmos {
        return;
    }
    for socket in &a.sockets {
        let Some(model) = body.model.get(socket.bone.index()) else { continue };
        gizmos.sphere(Isometry3d::from_translation((world * *model).transform_point3(socket.local.translation)), 0.02, Color::srgb(0.2, 0.8, 1.0));
    }
}
