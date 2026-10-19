//! Procedural locomotion: a dog-like quadruped, the authored-part bear, a biped, an insect and a
//! horse walking.
//!
//! ```text
//! cargo run --release --example walk
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · Tab select mob · WASD drive the selected mob
//! (Shift = faster) · L heads look at the camera · F foot gizmos.
//! Unselected mobs wander in circles.
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::num::ToF32;
use ornisculpt::mob::{Mob, TierCamera};
use ornisculpt::motion::{Feet, MobIntent};

const BODIES: [&str; 5] = ["mobs/quadruped.body.ron", "mobs/beast.body.ron", "mobs/biped.body.ron", "mobs/hexapod.body.ron", "mobs/horse.body.ron"];

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, OrnisculptPlugin))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .init_resource::<Orbit>()
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, (drive, orbit_camera, draw_feet, screenshot))
        .run();
}

#[derive(Resource, Default)]
struct Demo {
    selected: usize,
    look: bool,
    gizmos: bool,
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
            _ => Self { yaw: 0.5, pitch: 0.4, distance: 9.0, target: Vec3::new(0.0, 0.4, 0.0) },
        }
    }
}

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 8000.0, ..default() },
        Transform::from_xyz(3.0, 6.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(60.0, 60.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
    for (i, path) in BODIES.iter().enumerate() {
        let centre = Vec3::new(i.to_f32().mul_add(4.0, -8.0), 0.0, 0.0);
        commands.spawn((
            Mob::new(assets.load(*path)),
            Walker { index: i, centre },
            Transform::from_translation(centre + Vec3::X * 1.5),
        ));
    }
    info!("controls: left-drag orbit, scroll zoom, Tab select, WASD drive (Shift faster), L look at camera, F foot gizmos");
}

fn drive(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut demo: ResMut<Demo>,
    camera: Query<&Transform, With<Camera3d>>,
    mut walkers: Query<(&Walker, &Transform, &mut MobIntent), Without<Camera3d>>,
) {
    if keys.just_pressed(KeyCode::Tab) {
        demo.selected = (demo.selected + 1) % BODIES.len();
        info!("driving {}", BODIES.get(demo.selected).unwrap_or(&"?"));
    }
    if keys.just_pressed(KeyCode::KeyL) {
        demo.look = !demo.look;
    }
    if keys.just_pressed(KeyCode::KeyF) {
        demo.gizmos = !demo.gizmos;
    }
    let Ok(cam) = camera.single() else { return };
    let t = time.elapsed_secs();
    for (w, tf, mut intent) in &mut walkers {
        let manual = w.index == demo.selected
            && [KeyCode::KeyW, KeyCode::KeyA, KeyCode::KeyS, KeyCode::KeyD].iter().any(|k| keys.pressed(*k));
        intent.velocity = if manual {
            let fwd = Vec3::new(cam.forward().x, 0.0, cam.forward().z).normalize_or_zero();
            let right = Vec3::new(cam.right().x, 0.0, cam.right().z).normalize_or_zero();
            let mut dir = Vec3::ZERO;
            for (k, d) in [(KeyCode::KeyW, fwd), (KeyCode::KeyS, -fwd), (KeyCode::KeyD, right), (KeyCode::KeyA, -right)] {
                if keys.pressed(k) {
                    dir += d;
                }
            }
            let speed = if keys.pressed(KeyCode::ShiftLeft) { 2.5 } else { 0.9 };
            dir.normalize_or_zero() * speed
        } else {
            // Wander: walk around a circle, pausing now and then.
            let to_centre = w.centre - tf.translation;
            let tangent = Vec3::new(-to_centre.z, 0.0, to_centre.x).normalize_or_zero();
            let pull = to_centre.normalize_or_zero() * (to_centre.length() - 1.5);
            let pause = f32::from(u8::from(f32::mul_add(t, 0.2, w.index.to_f32()).sin() > 0.85));
            (tangent * 0.8 + pull) * (1.0 - pause)
        };
        intent.look_at = demo.look.then_some(cam.translation);
    }
}

fn draw_feet(demo: Res<Demo>, feet: Query<&Feet>, mut gizmos: Gizmos) {
    if !demo.gizmos {
        return;
    }
    for s in &feet {
        for f in &s.0 {
            let c = if f.planted { Color::srgb(0.2, 1.0, 0.3) } else { Color::srgb(1.0, 0.5, 0.1) };
            gizmos.sphere(Isometry3d::from_translation(f.position), 0.025, c);
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
    orbit.distance = (orbit.distance * scroll.delta.y.mul_add(-0.1, 1.0)).clamp(0.5, 100.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn screenshot(mut commands: Commands, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    *frame += 1;
    if *frame == 360 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frame == 380 {
        exit.write(AppExit::Success);
    }
}
