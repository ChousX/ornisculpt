//! Procedural locomotion: a dog-like quadruped, the authored-part bear, a biped, an insect, a
//! horse, and a curvy and a thick woman (soft tissue with `--features soft-tissue`) walking.
//!
//! ```text
//! cargo run --release --example walk
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · Tab select mob · WASD drive the selected mob
//! (Shift = faster) · L heads look at the camera · F foot gizmos.
//! Unselected mobs wander in circles.
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera; `ORNISCULPT_FOCUS=n` follows mob n
//! with the yaw relative to its heading (`π` looks at its back); `ORNISCULPT_SHOT_FRAME=a,b,..`
//! saves several frames (as `<path>.<frame>.png`); `ORNISCULPT_STILL=1` keeps every mob standing;
//! `ORNISCULPT_GIZMOS=1` starts with the foot gizmos on; `ORNISCULPT_VIABILITY=1` logs each body's
//! viability report and sole heights.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::num::ToF32;
use ornisculpt::mob::{Mob, TierCamera};
use ornisculpt::motion::{Feet, MobIntent};

mod common;
use common::{Orbit, default_plugins, screenshot_frames, spawn_stage};

const BODIES: [&str; 7] = [
    "mobs/quadruped.body.ron",
    "mobs/beast.body.ron",
    "mobs/biped.body.ron",
    "mobs/hexapod.body.ron",
    "mobs/horse.body.ron",
    "mobs/curvy.body.ron",
    "mobs/thick.body.ron",
];

fn main() {
    App::new()
        .add_plugins((default_plugins(Window::default()), OrnisculptPlugin))
        .insert_resource(GlobalAmbientLight { brightness: 450.0, ..default() })
        .insert_resource(Orbit::from_env(0.5, 0.4, 9.0, Vec3::new(0.0, 0.4, 0.0), (0.5, 100.0)))
        .init_resource::<Demo>()
        .add_systems(Startup, setup)
        .add_systems(Update, (drive, orbit_camera, draw_feet, screenshot_frames(360)))
        .add_observer(report_viability)
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

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 60.0, Vec3::new(3.0, 6.0, 2.0));
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
        if std::env::var_os("ORNISCULPT_STILL").is_some() {
            intent.velocity = Vec3::ZERO;
        }
        intent.look_at = demo.look.then_some(cam.translation);
    }
}

fn draw_feet(demo: Res<Demo>, feet: Query<&Feet>, mut gizmos: Gizmos) {
    if !demo.gizmos && std::env::var_os("ORNISCULPT_GIZMOS").is_none() {
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
    walkers: Query<(&Walker, &Transform), Without<Camera3d>>,
) {
    let focus: Option<usize> = std::env::var("ORNISCULPT_FOCUS").ok().and_then(|s| s.parse().ok());
    let followed = walkers.iter().find(|(w, _)| Some(w.index) == focus).map(|(_, tf)| {
        let f = tf.rotation * Vec3::Z;
        (tf.translation + Vec3::Y * orbit.target.y, f.x.atan2(f.z))
    });
    orbit.step(buttons.pressed(MouseButton::Left), motion.delta, scroll.delta.y, true);
    let (target, heading) = followed.unwrap_or((orbit.target, 0.0));
    let view = orbit.transform_at(target, heading);
    for mut t in &mut camera {
        *t = view;
    }
}

/// `ORNISCULPT_VIABILITY=1`: each body's viability report, and how high each sole stands at rest.
fn report_viability(ready: On<ornisculpt::mob::BodyReady>, mobs: Query<(&Walker, &ornisculpt::mob::MobBody)>, bodies: Res<Assets<ornisculpt::mob::GeneratedBody>>) {
    use ornisculpt::anatomy::{ViabilityChecks, sole_heights};
    if std::env::var_os("ORNISCULPT_VIABILITY").is_none() {
        return;
    }
    let Ok((w, body)) = mobs.get(ready.entity) else { return };
    let Some(b) = bodies.get(&body.0) else { return };
    let soles: Vec<String> = sole_heights(&b.anatomy).iter().map(|s| format!("{s:.3}")).collect();
    info!("{}: soles at rest [{}]", BODIES.get(w.index).unwrap_or(&"?"), soles.join(", "));
    for (name, v) in ViabilityChecks::with_builtins().run(&b.anatomy).results {
        info!("  {name}: {v:?}");
    }
}
