//! The motion API: every body runs the same standard graph (`motion::library::standard`) and
//! plays only the cycles that fit it.
//!
//! ```text
//! cargo run --release --example motion
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · Space cycle speed (stand / walk / trot / gallop) ·
//! 1 attack · 2 shake · 3 rear · 4 wave · 5 flare · H happy (tail wag) · Y flying (flap).
//! Tab follows the next mob, Escape frees the camera.
//! `ORNISCULPT_SPEED=n` starts at speed step n, `ORNISCULPT_FOCUS=n` follows mob n,
//! `ORNISCULPT_DIAG=1` logs frame times,, `ORNISCULPT_ACTION=name` triggers an action
//! repeatedly, `ORNISCULPT_PARAMS=happy,flying` sets bools,
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits,
//! `ORNISCULPT_SHOT_FRAME=n` picks the frame, `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets
//! the camera.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::mob::{GeneratedBody, Mob, MobBody, TierCamera};
use ornisculpt::num::{Saturate, ToF32};
use ornisculpt::motion::{Footstep, MobIntent, MobMotion, MotionEvent, MotionGraph, MotionParams, library};

const BODIES: [&str; 6] = [
    "mobs/biped.body.ron",
    "mobs/quadruped.body.ron",
    "mobs/horse.body.ron",
    "mobs/dragon.body.ron",
    "mobs/bird.body.ron",
    "mobs/hexapod.body.ron",
];
/// Leg lengths per second.
const SPEEDS: [(&str, f32); 4] = [("stand", 0.0), ("walk", 1.0), ("trot", 2.5), ("gallop", 4.5)];
const ACTIONS: [(KeyCode, &str); 5] =
    [(KeyCode::Digit1, "attack"), (KeyCode::Digit2, "shake"), (KeyCode::Digit3, "rear"), (KeyCode::Digit4, "wave"), (KeyCode::Digit5, "flare")];

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, OrnisculptPlugin))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .init_resource::<Orbit>()
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, (controls, drive, orbit_camera.after(drive), screenshot, frame_stats))
        .add_observer(|e: On<MotionEvent>| info!("{:?}: {} ({})", e.entity, e.name, e.cycle))
        .add_observer(|_: On<Footstep>, mut steps: ResMut<Demo>| steps.footsteps += 1)
        .run();
}

#[derive(Resource)]
struct Demo {
    speed: usize,
    /// Mob the camera follows (Tab cycles, Escape frees it).
    focus: Option<usize>,
    happy: bool,
    flying: bool,
    footsteps: u32,
}

impl Default for Demo {
    fn default() -> Self {
        let params = std::env::var("ORNISCULPT_PARAMS").unwrap_or_default();
        Self {
            speed: std::env::var("ORNISCULPT_SPEED").ok().and_then(|s| s.parse().ok()).unwrap_or(1).min(SPEEDS.len() - 1),
            focus: std::env::var("ORNISCULPT_FOCUS").ok().and_then(|s| s.parse().ok()),
            happy: params.contains("happy"),
            flying: params.contains("flying"),
            footsteps: 0,
        }
    }
}

#[derive(Component)]
struct Walker {
    index: usize,
    centre: Vec3,
}

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
            _ => Self { yaw: 0.3, pitch: 0.35, distance: 13.0, target: Vec3::new(0.0, 0.6, 0.0) },
        }
    }
}

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut graphs: ResMut<Assets<MotionGraph>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 8000.0, ..default() },
        // `ORNISCULPT_LIGHT_YAW=deg` swings the light round (to light a mob's back, say).
        Transform::from_translation(
            Quat::from_rotation_y(std::env::var("ORNISCULPT_LIGHT_YAW").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0).to_radians())
                * Vec3::new(3.0, 6.0, 2.0),
        )
        .looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(80.0, 80.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
    // One graph for every body.
    let graph = graphs.add(library::standard());
    for (i, path) in BODIES.iter().enumerate() {
        let centre = Vec3::new(i.to_f32().mul_add(4.5, -11.25), 0.0, 0.0);
        commands.spawn((
            Mob::new(assets.load(*path)),
            MobMotion::new(graph.clone()),
            Walker { index: i, centre },
            Transform::from_translation(centre + Vec3::X * 1.6),
        ));
    }
    info!("Space speed, 1-5 actions (attack shake rear wave flare), H happy, Y flying");
}

fn controls(keys: Res<ButtonInput<KeyCode>>, mut demo: ResMut<Demo>, mut mobs: Query<&mut MotionParams>, mut frame: Local<u32>) {
    *frame += 1;
    if keys.just_pressed(KeyCode::Space) {
        demo.speed = (demo.speed + 1) % SPEEDS.len();
        info!("speed: {}", SPEEDS.get(demo.speed).map_or("?", |s| s.0));
    }
    if keys.just_pressed(KeyCode::Tab) {
        demo.focus = Some(demo.focus.map_or(0, |f| (f + 1) % BODIES.len()));
        info!("following {}", BODIES.get(demo.focus.unwrap_or(0)).unwrap_or(&"?"));
    }
    if keys.just_pressed(KeyCode::Escape) {
        demo.focus = None;
    }
    demo.happy ^= keys.just_pressed(KeyCode::KeyH);
    demo.flying ^= keys.just_pressed(KeyCode::KeyY);
    let scripted = std::env::var("ORNISCULPT_ACTION").ok().filter(|_| *frame % 150 == 60);
    for mut p in &mut mobs {
        p.set_bool("happy", demo.happy).set_bool("flying", demo.flying);
        for (key, action) in ACTIONS {
            if keys.just_pressed(key) {
                p.trigger(action);
            }
        }
        if let Some(a) = &scripted {
            p.trigger(a);
        }
    }
}

fn drive(
    time: Res<Time>,
    demo: Res<Demo>,
    bodies: Res<Assets<GeneratedBody>>,
    mut walkers: Query<(&Walker, &Transform, &MobBody, &mut MobIntent)>,
) {
    let t = time.elapsed_secs();
    for (w, tf, body, mut intent) in &mut walkers {
        let Some(b) = bodies.get(&body.0) else { continue };
        let legs = &b.anatomy.legs;
        let leg = if legs.is_empty() { 0.5 } else { legs.iter().map(|l| l.length).sum::<f32>() / legs.len().to_f32() };
        // Around a circle, pulled back onto it.
        let to_centre = w.centre - tf.translation;
        let tangent = Vec3::new(-to_centre.z, 0.0, to_centre.x).normalize_or_zero();
        let pull = to_centre.normalize_or_zero() * (to_centre.length() - 1.6);
        let speed = SPEEDS.get(demo.speed).map_or(0.0, |s| s.1) * leg;
        intent.velocity = (tangent + pull * 0.5).normalize_or_zero() * speed;
        if speed == 0.0 {
            intent.facing = Some(Dir3::new(Vec3::new(f32::mul_add(t, 0.1, w.index.to_f32()).sin(), 0.0, 1.0)).unwrap_or(Dir3::Z));
        } else {
            intent.facing = None;
        }
    }
}

fn orbit_camera(
    demo: Res<Demo>,
    walkers: Query<(&Walker, &Transform), Without<Camera3d>>,
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
    orbit.distance = (orbit.distance * scroll.delta.y.mul_add(-0.1, 1.0)).clamp(0.5, 100.0);
    if let Some(f) = demo.focus
        && let Some((_, tf)) = walkers.iter().find(|(w, _)| w.index == f) {
            orbit.target = tf.translation + Vec3::Y * 0.6;
        }
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn screenshot(mut commands: Commands, demo: Res<Demo>, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    // `ORNISCULPT_SHOT_FRAME=360` or several, `300,302,304` (saved as `<path>.<frame>.png`).
    let frames: Vec<u32> = std::env::var("ORNISCULPT_SHOT_FRAME").unwrap_or_else(|_| "360".into()).split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let last = frames.iter().copied().max().unwrap_or(360);
    *frame += 1;
    if frames.contains(&*frame) {
        info!("footsteps so far: {}", demo.footsteps);
        let path = if frames.len() > 1 { format!("{path}.{}.png", *frame) } else { path };
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frame == last + 20 {
        exit.write(AppExit::Success);
    }
}

/// `ORNISCULPT_DIAG=1`: every two seconds, frame time percentiles and how many frames hitched
/// (took over twice the median).
fn frame_stats(time: Res<Time>, mut frames: Local<Vec<f32>>, mut since: Local<f32>) {
    if std::env::var("ORNISCULPT_DIAG").is_err() {
        return;
    }
    frames.push(time.delta_secs() * 1000.0);
    *since += time.delta_secs();
    if *since < 2.0 {
        return;
    }
    *since = 0.0;
    let mut sorted = std::mem::take(&mut *frames);
    sorted.sort_by(f32::total_cmp);
    let at = |q: f32| sorted.get((sorted.len().saturating_sub(1).to_f32() * q).sat_usize()).copied().unwrap_or(0.0);
    let median = at(0.5);
    let hitches = sorted.iter().filter(|t| **t > median * 2.0).count();
    info!("frame ms: p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}, {hitches} hitches in {} frames", median, at(0.95), at(0.99), at(1.0), sorted.len());
}

