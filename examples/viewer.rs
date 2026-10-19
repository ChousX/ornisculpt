//! Mob viewer: one mob spawned through the plugin, with LOD, tone and rig debugging.
//!
//! ```text
//! cargo run --release --example viewer [mobs/plan.body.ron] [coats/look.coat.ron] [outfits/wear.outfit.ron]
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · 1/2/3 LOD (hero/near/far) · G skeleton gizmos ·
//! W wiggle bones · B hold joints bent (shows muscle flex) · M muscle flex on/off · T cycle tone preset ·
//! R regenerate.
//! The body plan and any authored part meshes hot-reload when saved.
//!
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot once the body is ready and exits;
//! `ORNISCULPT_LOD=near|far`, `ORNISCULPT_WIGGLE=1`, `ORNISCULPT_TONE=<preset>` and
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` set the starting state; `ORNISCULPT_BEND=<degrees>`
//! holds the limbs' inner joints bent about their x axes, `ORNISCULPT_MUSCLE=<gain>` scales muscle flex (0 = off) and
//! `ORNISCULPT_GIZMOS=0` hides the skeleton.

use bevy::asset::AssetPlugin;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{LodLevel, Tone};
use ornisculpt::clothing::MobOutfit;
use ornisculpt::coat::{MobCoat, MobMaterial};
use ornisculpt::mob::{BodyReady, ForceTier, GenRequest, GeneratedBody, LiveBody, Mob, MobBody, MobJoint, MobSet, MobSkeleton};

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "mobs/quadruped.body.ron".into());
    let path = path.strip_prefix("assets/").map(str::to_owned).unwrap_or(path);
    let coat = std::env::args().nth(2).map(|c| c.strip_prefix("assets/").map(str::to_owned).unwrap_or(c));
    App::new()
        .add_plugins((
            DefaultPlugins
                .set(WindowPlugin { primary_window: Some(Window { title: "Ornisculpt viewer".into(), ..default() }), ..default() })
                .set(AssetPlugin { watch_for_changes_override: Some(true), ..default() }),
            OrnisculptPlugin,
        ))
        .insert_resource(Viewer::new(path, coat))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .init_resource::<Orbit>()
        .add_observer(on_ready)
        .add_systems(Startup, setup)
        .add_systems(Update, (controls, orbit_camera, wiggle.after(MobSet::Motion), muscle_flex, draw_skeleton, screenshot))
        .run();
}

#[derive(Resource)]
struct Viewer {
    path: String,
    coat: Option<String>,
    mob: Entity,
    lod: LodLevel,
    gizmos: bool,
    wiggle: bool,
    /// Fixed bend of every joint (radians), instead of the rest pose.
    bend: Option<f32>,
    /// Muscles bulge as they shorten (the coat's muscle flex gain).
    muscles: bool,
    /// Muscle flex gain while on (`ORNISCULPT_MUSCLE=<gain>`; 1 = as baked).
    muscle_gain: f32,
    tone: usize,
    /// Frames since the latest body became ready (for screenshots).
    ready_frames: Option<u32>,
}

impl Viewer {
    fn new(path: String, coat: Option<String>) -> Self {
        let env = |k: &str| std::env::var(k).ok();
        let lod = match env("ORNISCULPT_LOD").as_deref() {
            Some("near") => LodLevel::Near,
            Some("far") => LodLevel::Far,
            _ => LodLevel::Hero,
        };
        let tone = env("ORNISCULPT_TONE").and_then(|t| TONE_PRESETS.iter().position(|(n, _)| *n == t)).unwrap_or(0);
        Self {
            path,
            coat,
            mob: Entity::PLACEHOLDER,
            lod,
            gizmos: env("ORNISCULPT_GIZMOS").as_deref() != Some("0"),
            wiggle: env("ORNISCULPT_WIGGLE").is_some(),
            bend: env("ORNISCULPT_BEND").and_then(|d| d.parse::<f32>().ok()).map(f32::to_radians),
            muscles: env("ORNISCULPT_MUSCLE").as_deref() != Some("0"),
            muscle_gain: env("ORNISCULPT_MUSCLE").and_then(|g| g.parse::<f32>().ok()).filter(|g| *g > 0.0).unwrap_or(1.0),
            tone,
            ready_frames: None,
        }
    }
}

/// (name, [muscle, fat, folds, landmarks]); `authored` keeps the RON's tone.
/// Joint bend held by B (degrees).
const BEND_DEGREES: f32 = 50.0;

const TONE_PRESETS: [(&str, [f32; 4]); 5] = [
    ("authored", [0.0; 4]),
    ("smooth", [0.0, 0.0, 0.0, 0.0]),
    ("lean", [0.9, 0.05, 0.6, 1.0]),
    ("muscular", [1.4, 0.15, 0.5, 0.6]),
    ("heavy", [0.4, 1.2, 0.8, 0.2]),
];

#[derive(Resource)]
struct Orbit {
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: Vec3,
}

impl Default for Orbit {
    fn default() -> Self {
        let v: Vec<f32> = std::env::var("ORNISCULPT_CAM")
            .map_or_default(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect());
        match v[..] {
            [yaw, pitch, distance, x, y, z] => Self { yaw, pitch, distance, target: Vec3::new(x, y, z) },
            _ => Self { yaw: 0.9, pitch: 0.35, distance: 3.0, target: Vec3::new(0.0, 0.5, 0.0) },
        }
    }
}

fn setup(
    mut commands: Commands,
    mut viewer: ResMut<Viewer>,
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
    viewer.mob = commands.spawn((Mob::new(assets.load(viewer.path.clone())), ForceTier(viewer.lod))).id();
    if let Some(outfit) = std::env::args().nth(3).map(|c| c.strip_prefix("assets/").map(str::to_owned).unwrap_or(c)) {
        commands.entity(viewer.mob).insert(MobOutfit(assets.load(outfit)));
    }
    if let Some(coat) = viewer.coat.as_ref().filter(|c| c.as_str() != "-") {
        commands.entity(viewer.mob).insert(MobCoat(assets.load(coat.clone())));
    }
    info!("viewing {}", viewer.path);
    info!("controls: left-drag orbit, scroll zoom, 1/2/3 LOD, G gizmos, W wiggle, T tone preset, R regenerate");
}

/// Applies the tone preset to a freshly resolved body, and counts frames for screenshots.
fn on_ready(
    ev: On<BodyReady>,
    mut commands: Commands,
    mut viewer: ResMut<Viewer>,
    mut lives: Query<&mut LiveBody>,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<&MobBody>,
) {
    if ev.entity != viewer.mob {
        return;
    }
    if apply_tone(&mut lives, viewer.mob, viewer.tone) {
        commands.entity(viewer.mob).insert(GenRequest);
        return;
    }
    viewer.ready_frames = Some(0);
    if let Some(body) = mobs.get(viewer.mob).ok().and_then(|b| bodies.get(&b.0)) {
        let a = &body.anatomy;
        info!(
            "ready: {} bones ({} crowd), {} primitives, {} hard parts, {} parts",
            a.skeleton.len(),
            a.skeleton.crowd.bones.len(),
            a.primitives.len(),
            a.hard_parts.len(),
            a.parts.len(),
        );
    }
}

/// Sets the preset's tone on the mob's live body. Returns whether anything changed.
fn apply_tone(lives: &mut Query<&mut LiveBody>, mob: Entity, preset: usize) -> bool {
    let Some(&(name, [muscle, fat, folds, landmarks])) = TONE_PRESETS.get(preset) else { return false };
    let Ok(mut live) = lives.get_mut(mob) else { return false };
    if name == "authored" {
        return false;
    }
    let tone = Tone { muscle, fat, folds, landmarks, ..live.0.tone };
    if live.0.tone == tone {
        return false;
    }
    live.0.tone = tone;
    true
}

fn controls(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut viewer: ResMut<Viewer>,
    mut lives: Query<&mut LiveBody>,
) {
    for (key, level) in [(KeyCode::Digit1, LodLevel::Hero), (KeyCode::Digit2, LodLevel::Near), (KeyCode::Digit3, LodLevel::Far)] {
        if keys.just_pressed(key) {
            viewer.lod = level;
            commands.entity(viewer.mob).insert(ForceTier(level));
            info!("LOD {level:?}");
        }
    }
    if keys.just_pressed(KeyCode::KeyG) {
        viewer.gizmos = !viewer.gizmos;
    }
    if keys.just_pressed(KeyCode::KeyW) {
        viewer.wiggle = !viewer.wiggle;
    }
    if keys.just_pressed(KeyCode::KeyB) {
        viewer.bend = if viewer.bend.is_some() { None } else { Some(BEND_DEGREES.to_radians()) };
    }
    if keys.just_pressed(KeyCode::KeyM) {
        viewer.muscles = !viewer.muscles;
        info!("muscle flex {}", if viewer.muscles { "on" } else { "off" });
    }
    if keys.just_pressed(KeyCode::KeyR) {
        // Re-resolve from the asset (picks up the authored tone again).
        commands.entity(viewer.mob).remove::<LiveBody>().insert(GenRequest);
    }
    if keys.just_pressed(KeyCode::KeyT) {
        viewer.tone = (viewer.tone + 1) % TONE_PRESETS.len();
        let name = TONE_PRESETS.get(viewer.tone).map_or("authored", |p| p.0);
        info!("tone preset: {name}");
        if name == "authored" {
            commands.entity(viewer.mob).remove::<LiveBody>().insert(GenRequest);
        } else if apply_tone(&mut lives, viewer.mob, viewer.tone) {
            commands.entity(viewer.mob).insert(GenRequest);
        }
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

fn wiggle(
    viewer: Res<Viewer>,
    time: Res<Time>,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<&MobBody>,
    mut joints: Query<(&MobJoint, &mut Transform)>,
) {
    // Otherwise the motion pose shows.
    if !viewer.wiggle && viewer.bend.is_none() {
        return;
    }
    let t = time.elapsed_secs();
    // The bend holds the limbs' inner joints (knees, elbows, ...) only: the body stays upright.
    let bent_limb = |joint: &MobJoint| {
        let body = mobs.get(joint.mob).ok().and_then(|b| bodies.get(&b.0));
        let bone = body.and_then(|b| b.anatomy.skeleton.get(joint.bone));
        bone.is_some_and(|b| b.name.starts_with("limb") && !b.name.ends_with(".0"))
    };
    for (joint, mut tf) in &mut joints {
        let i = f32::from(joint.bone.0);
        if viewer.wiggle && joint.bone.0 > 0 {
            tf.rotation = joint.rest.rotation * Quat::from_rotation_x(f32::mul_add(t, 3.0, i).sin() * 0.35);
        } else if let Some(bend) = viewer.bend.filter(|_| bent_limb(joint)) {
            tf.rotation = joint.rest.rotation * Quat::from_rotation_x(bend);
        }
    }
}

/// Applies the muscle toggle to every mob material (new bodies come with flex on).
fn muscle_flex(viewer: Res<Viewer>, mut materials: ResMut<Assets<MobMaterial>>) {
    let gain = if viewer.muscles { viewer.muscle_gain } else { 0.0 };
    let stale: Vec<_> = materials.iter().filter(|(_, m)| m.extension.coat.muscle.x.to_bits() != f32::to_bits(gain)).map(|(id, _)| id).collect();
    for id in stale {
        if let Some(mut m) = materials.get_mut(id) {
            m.extension.coat.muscle.x = gain;
        }
    }
}

fn draw_skeleton(
    viewer: Res<Viewer>,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<(&MobBody, &MobSkeleton)>,
    joints: Query<&GlobalTransform, With<MobJoint>>,
    mut gizmos: Gizmos,
) {
    let Ok((body, skeleton)) = mobs.get(viewer.mob) else { return };
    let Some(body) = bodies.get(&body.0) else { return };
    if !viewer.gizmos {
        return;
    }
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

fn screenshot(mut commands: Commands, mut viewer: ResMut<Viewer>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    let Some(frames) = viewer.ready_frames.as_mut() else { return };
    *frames += 1;
    // Give pipelines time to compile before capturing.
    if *frames == 120 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frames == 140 {
        exit.write(AppExit::Success);
    }
}
