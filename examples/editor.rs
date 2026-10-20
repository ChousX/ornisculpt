//! Live body plan editor: one mob with every value of its plan on screen, adjusted from the
//! keyboard, regenerated as you go and written back to the RON on save.
//!
//! ```text
//! cargo run --release --example editor [mobs/plan.body.ron]
//! ```
//!
//! Controls: Tab / Shift+Tab part · Up / Down field (`PageUp` / `PageDown` jump) · Left / Right
//! adjust (Shift x10, Ctrl x0.1; toggles bools, cycles enum variants) · Delete remove the part ·
//! Ctrl+Z undo · Ctrl+S save · left-drag orbit · scroll zoom · G skeleton gizmos.
//!
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot once the body is ready and exits.
//!
//! The fields come from reflection over the plan, so every number, flag and unit enum variant in a
//! part is editable; strings and `None` are shown but fixed. Mirrored and repeated parts are one
//! entry (the authored plan, not the expanded one). Saving rewrites the file from the edited plan,
//! so its comments and formatting are lost. The file is watched: a save from another editor
//! replaces unsaved edits here.

use std::fmt::Write as _;

use bevy::asset::AssetPlugin;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::reflect::enums::{DynamicEnum, DynamicVariant, VariantInfo, VariantType};
use bevy::reflect::{GetPath, PartialReflect, ReflectMut, ReflectRef, TypeInfo};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{BodyPlan, PartNode, PartPath, Repeat};
use ornisculpt::mob::{BodyReady, GeneratedBody, Mob, MobBody, MobJoint, MobSkeleton};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "mobs/quadruped.body.ron".into());
    let path = path.strip_prefix("assets/").map(str::to_owned).unwrap_or(path);
    App::new()
        .add_plugins((
            DefaultPlugins
                .set(WindowPlugin { primary_window: Some(Window { title: "Ornisculpt editor".into(), ..default() }), ..default() })
                .set(AssetPlugin { watch_for_changes_override: Some(true), ..default() }),
            OrnisculptPlugin,
        ))
        .insert_resource(Editor::new(path))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .init_resource::<Orbit>()
        .init_resource::<KeyRepeat>()
        .add_observer(on_ready)
        .add_systems(Startup, setup)
        .add_systems(Update, (sync_from_asset, controls, push_edits, orbit_camera, draw_skeleton, render_panel, screenshot).chain())
        .run();
}

/// Seconds an edit waits for the next before the plan goes to the asset (one regeneration per
/// burst of key presses).
const PUSH_DELAY: f32 = 0.3;
/// Edits kept for undo.
const UNDO_DEPTH: usize = 200;
/// Fields shown at once.
const FIELD_WINDOW: usize = 32;

const HELP: &str = "Tab part  Up/Down field  Left/Right adjust (Shift x10, Ctrl x0.1)  Delete part  Ctrl+Z undo  Ctrl+S save  G skeleton";

#[derive(Resource)]
struct Editor {
    /// Asset path (`mobs/quadruped.body.ron`).
    path: String,
    handle: Handle<BodyPlan>,
    mob: Entity,
    /// The plan being edited: the asset as loaded, plus unsaved edits.
    plan: Option<BodyPlan>,
    parts: Vec<PartEntry>,
    part: usize,
    fields: Vec<Field>,
    field: usize,
    undo: Vec<BodyPlan>,
    /// Seconds until the edited plan goes to the asset.
    push_in: Option<f32>,
    /// Asset events before this time (seconds since start) are our own writes: not reloaded.
    ignore_until: f64,
    generating: bool,
    status: String,
    gizmos: bool,
    /// Frames since the body became ready (for screenshots).
    ready_frames: Option<u32>,
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
            plan: None,
            parts: Vec::new(),
            part: 0,
            fields: Vec::new(),
            field: 0,
            undo: Vec::new(),
            push_in: None,
            ignore_until: 0.0,
            generating: true,
            status: "loading".into(),
            gizmos: false,
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

    /// Records the plan before an edit, for undo.
    fn snapshot(&mut self) {
        if let Some(plan) = &self.plan {
            if self.undo.len() >= UNDO_DEPTH {
                self.undo.remove(0);
            }
            self.undo.push(plan.clone());
        }
    }

    /// After an edit: refresh the panel and schedule the regeneration.
    fn edited(&mut self) {
        self.rebuild();
        self.push_in = Some(PUSH_DELAY);
    }

    fn select_part(&mut self, part: usize) {
        self.part = part;
        self.field = 0;
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
        if self.undo.len() >= UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.undo.push(before);
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
        self.select_part(self.part.saturating_sub(1));
        self.push_in = Some(PUSH_DELAY);
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
        out.push(PartEntry { path, reflect: reflect.clone(), depth, kind, copies: if copies > 1 { format!("x{copies}") } else { String::new() } });
        for (i, child) in node.children.iter().enumerate() {
            go(child, out.last().map_or_else(|| PartPath::root(&child.name), |p| p.path.child(&child.name)), format!("{reflect}.children[{i}]"), depth + 1, out);
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
    try_ints!(u8, u16, u32, u64, usize, i8, i16, i32, i64);
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
    try_ints!(u8, u16, u32, u64, usize, i8, i16, i32, i64);
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
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: Vec3,
}

impl Default for Orbit {
    fn default() -> Self {
        Self { yaw: 0.9, pitch: 0.35, distance: 3.0, target: Vec3::new(0.0, 0.5, 0.0) }
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
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn(Camera3d::default());
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 8000.0, ..default() },
        Transform::from_xyz(3.0, 6.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(20.0, 20.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
    editor.handle = assets.load(editor.path.clone());
    editor.mob = commands.spawn(Mob::new(editor.handle.clone())).id();
    commands
        .spawn((
            Node { position_type: PositionType::Absolute, left: Val::Px(8.0), top: Val::Px(8.0), padding: UiRect::all(Val::Px(8.0)), ..default() },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        ))
        .with_child((Panel, Text::new("loading"), TextFont { font_size: FontSize::Px(13.0), ..default() }, TextColor(Color::srgb(0.92, 0.92, 0.9))));
    info!("editing {}", editor.path);
    info!("controls: {HELP}");
}

/// Takes the plan from the asset when it loads or changes on disk (not after our own writes).
fn sync_from_asset(mut events: MessageReader<AssetEvent<BodyPlan>>, plans: Res<Assets<BodyPlan>>, mut editor: ResMut<Editor>, time: Res<Time>) {
    let id = editor.handle.id();
    let changed = events.read().any(|ev| match ev {
        AssetEvent::Added { id: i } | AssetEvent::Modified { id: i } | AssetEvent::LoadedWithDependencies { id: i } => *i == id,
        AssetEvent::Removed { .. } | AssetEvent::Unused { .. } => false,
    });
    if !changed || (editor.plan.is_some() && time.elapsed_secs_f64() < editor.ignore_until) {
        return;
    }
    let Some(plan) = plans.get(id) else { return };
    let first = editor.plan.is_none();
    if !first {
        editor.snapshot();
    }
    editor.plan = Some(plan.clone());
    editor.push_in = None;
    editor.generating = true;
    editor.status = if first { "loaded".into() } else { "reloaded from disk".into() };
    editor.rebuild();
}

#[allow(clippy::too_many_lines, reason = "one key per branch")]
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
        editor.undo();
        return;
    }
    if keys.just_pressed(KeyCode::KeyG) {
        editor.gizmos = !editor.gizmos;
    }
    if keys.just_pressed(KeyCode::Delete) {
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

/// Once edits pause, the plan goes to the asset; the plugin regenerates every mob using it.
fn push_edits(mut editor: ResMut<Editor>, mut plans: ResMut<Assets<BodyPlan>>, time: Res<Time>) {
    let Some(left) = editor.push_in else { return };
    let left = left - time.delta_secs();
    if left > 0.0 {
        editor.push_in = Some(left);
        return;
    }
    editor.push_in = None;
    let Some(plan) = editor.plan.clone() else { return };
    let Some(mut asset) = plans.get_mut(&editor.handle) else { return };
    *asset = plan;
    editor.ignore_until = time.elapsed_secs_f64() + 0.5;
    editor.generating = true;
    editor.status = "generating".into();
}

fn on_ready(ev: On<BodyReady>, mut editor: ResMut<Editor>, bodies: Res<Assets<GeneratedBody>>, mobs: Query<&MobBody>) {
    if ev.entity != editor.mob {
        return;
    }
    editor.generating = false;
    editor.ready_frames = Some(0);
    if let Some(body) = mobs.get(editor.mob).ok().and_then(|b| bodies.get(&b.0)) {
        let a = &body.anatomy;
        editor.status = format!("ready: {} bones, {} primitives, {} hard parts", a.skeleton.len(), a.primitives.len(), a.hard_parts.len());
    }
}

fn render_panel(editor: Res<Editor>, mut panels: Query<&mut Text, With<Panel>>) {
    if !editor.is_changed() {
        return;
    }
    let mut s = String::new();
    let _ = writeln!(s, "{}{}", editor.path, if editor.generating { "   [generating]" } else { "" });
    let _ = writeln!(s, "{}", editor.status);
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

fn screenshot(mut commands: Commands, mut editor: ResMut<Editor>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    let Some(frames) = editor.ready_frames.as_mut() else { return };
    *frames += 1;
    // Give pipelines time to compile before capturing.
    if *frames == 120 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frames == 140 {
        exit.write(AppExit::Success);
    }
}

fn orbit_camera(
    mut orbit: ResMut<Orbit>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    if buttons.pressed(MouseButton::Left) {
        orbit.yaw = motion.delta.x.mul_add(-0.008, orbit.yaw);
        orbit.pitch = motion.delta.y.mul_add(0.008, orbit.pitch).clamp(-1.4, 1.4);
    }
    orbit.distance = (orbit.distance * scroll.delta.y.mul_add(-0.1, 1.0)).clamp(0.3, 30.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn draw_skeleton(
    editor: Res<Editor>,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<(&MobBody, &MobSkeleton)>,
    joints: Query<&GlobalTransform, With<MobJoint>>,
    mut gizmos: Gizmos,
) {
    if !editor.gizmos {
        return;
    }
    let Ok((body, skeleton)) = mobs.get(editor.mob) else { return };
    let Some(body) = bodies.get(&body.0) else { return };
    let a = &body.anatomy;
    for (bone, &joint) in a.skeleton.bones.iter().zip(&skeleton.0) {
        let Ok(g) = joints.get(joint) else { continue };
        gizmos.line(g.translation(), g.transform_point(Vec3::Y * bone.length), Color::srgb(1.0, 0.85, 0.1));
        gizmos.sphere(Isometry3d::from_translation(g.translation()), 0.008, Color::srgb(1.0, 0.4, 0.1));
    }
    for socket in &a.sockets {
        let Some(Ok(g)) = skeleton.0.get(socket.bone.index()).map(|&joint| joints.get(joint)) else { continue };
        gizmos.sphere(Isometry3d::from_translation(g.transform_point(socket.local.translation)), 0.02, Color::srgb(0.2, 0.8, 1.0));
    }
}
