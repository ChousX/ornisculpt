//! Mob viewer: one mob spawned through the plugin, with LOD, tone and rig debugging.
//!
//! ```text
//! cargo run --release --example viewer [mobs/plan.body.ron] [coats/look.coat.ron] [outfits/wear.outfit.ron]
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · 1/2/3 LOD (hero/near/far) · G skeleton gizmos ·
//! W wiggle bones · T cycle tone preset · R regenerate.
//! The body plan and any authored part meshes hot-reload when saved.
//!
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot once the body is ready and exits;
//! `ORNISCULPT_LOD=near|far`, `ORNISCULPT_WIGGLE=1`, `ORNISCULPT_TONE=<preset>` and
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` set the starting state.

use bevy::asset::AssetPlugin;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{LodLevel, Tone};
use ornisculpt::clothing::MobOutfit;
use ornisculpt::coat::MobCoat;
use ornisculpt::mob::{BodyReady, ForceTier, GenRequest, GeneratedBody, LiveBody, Mob, MobBody, MobJoint, MobSkeleton};

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
        .add_systems(Update, (controls, orbit_camera, wiggle, draw_skeleton, screenshot))
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
        Self { path, coat, mob: Entity::PLACEHOLDER, lod, gizmos: true, wiggle: env("ORNISCULPT_WIGGLE").is_some(), tone, ready_frames: None }
    }
}

/// (name, [muscle, fat, folds, landmarks]); `authored` keeps the RON's tone.
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
            .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_default();
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
    let (name, [muscle, fat, folds, landmarks]) = TONE_PRESETS[preset];
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
    if keys.just_pressed(KeyCode::KeyR) {
        // Re-resolve from the asset (picks up the authored tone again).
        commands.entity(viewer.mob).remove::<LiveBody>().insert(GenRequest);
    }
    if keys.just_pressed(KeyCode::KeyT) {
        viewer.tone = (viewer.tone + 1) % TONE_PRESETS.len();
        info!("tone preset: {}", TONE_PRESETS[viewer.tone].0);
        if TONE_PRESETS[viewer.tone].0 == "authored" {
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
        orbit.yaw -= motion.delta.x * 0.008;
        orbit.pitch = (orbit.pitch + motion.delta.y * 0.008).clamp(-1.4, 1.4);
    }
    orbit.distance = (orbit.distance * (1.0 - scroll.delta.y * 0.1)).clamp(0.3, 30.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn wiggle(viewer: Res<Viewer>, time: Res<Time>, mut joints: Query<(&MobJoint, &mut Transform)>) {
    let t = time.elapsed_secs();
    for (joint, mut tf) in &mut joints {
        let i = joint.bone.0 as f32;
        tf.rotation = if viewer.wiggle && joint.bone.0 > 0 {
            joint.rest.rotation * Quat::from_rotation_x((t * 3.0 + i).sin() * 0.35)
        } else {
            joint.rest.rotation
        };
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
    for (i, bone) in a.skeleton.bones.iter().enumerate() {
        let Ok(g) = joints.get(skeleton.0[i]) else { continue };
        gizmos.line(g.translation(), g.transform_point(Vec3::Y * bone.length), Color::srgb(1.0, 0.85, 0.1));
        gizmos.sphere(Isometry3d::from_translation(g.translation()), 0.008, Color::srgb(1.0, 0.4, 0.1));
    }
    for socket in &a.sockets {
        let Ok(g) = joints.get(skeleton.0[socket.bone.index()]) else { continue };
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
