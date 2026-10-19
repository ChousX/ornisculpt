//! Hand posing: one standing biped whose hands take [`HandPose`] presets, single fingers, or a
//! rippling "piano" of every digit in turn, over its idle animation.
//!
//! ```text
//! cargo run --release --example hands
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · 1 relaxed · 2 open · 3 fist · 4 grip · 5 point ·
//! 6 pinch · Q/W/E/R/T bend or straighten one digit of the right hand (thumb first) · P piano ·
//! 0 back to the animation.
//! `ORNISCULPT_HAND=relaxed|open|fist|grip|point|pinch|piano` starts in that pose;
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::LodLevel;
use ornisculpt::mob::{ForceTier, Mob, TierCamera};
use ornisculpt::motion::{Hand, HandPose};
use ornisculpt::num::ToF32;

const PRESETS: [(&str, KeyCode, Hand); 6] = [
    ("relaxed", KeyCode::Digit1, Hand::RELAXED),
    ("open", KeyCode::Digit2, Hand::OPEN),
    ("fist", KeyCode::Digit3, Hand::FIST),
    ("grip", KeyCode::Digit4, Hand::GRIP),
    ("point", KeyCode::Digit5, Hand::POINT),
    ("pinch", KeyCode::Digit6, Hand::PINCH),
];
const DIGIT_KEYS: [KeyCode; 5] = [KeyCode::KeyQ, KeyCode::KeyW, KeyCode::KeyE, KeyCode::KeyR, KeyCode::KeyT];

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, OrnisculptPlugin))
        .insert_resource(GlobalAmbientLight { brightness: 400.0, ..default() })
        .init_resource::<Orbit>()
        .init_resource::<Mode>()
        .add_systems(Startup, setup)
        .add_systems(Update, (keys, piano, orbit_camera, screenshot))
        .run();
}

/// What drives the hands.
#[derive(Resource, Clone, Copy, PartialEq)]
enum Mode {
    /// The idle animation only.
    Animated,
    Preset(Hand),
    /// Each digit curls and opens in turn.
    Piano,
}

impl Default for Mode {
    fn default() -> Self {
        match std::env::var("ORNISCULPT_HAND").as_deref() {
            Ok("piano") => Self::Piano,
            Ok(name) => PRESETS.iter().find(|(n, ..)| *n == name).map_or(Self::Animated, |(.., h)| Self::Preset(*h)),
            Err(_) => Self::Animated,
        }
    }
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
            // The right hand, from the front and a little to its side.
            _ => Self { yaw: -0.5, pitch: 0.15, distance: 0.75, target: Vec3::new(-0.22, 0.7, 0.0) },
        }
    }
}

fn setup(mut commands: Commands, assets: Res<AssetServer>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((Camera3d::default(), TierCamera));
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 8000.0, ..default() },
        Transform::from_xyz(-2.0, 5.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(20.0, 20.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
    commands.spawn((Mob::new(assets.load("mobs/biped.body.ron")), ForceTier(LodLevel::Hero), HandPose::default(), Transform::default()));
    info!("controls: 1-6 presets (relaxed, open, fist, grip, point, pinch), Q/W/E/R/T one digit, P piano, 0 animation");
}

fn keys(input: Res<ButtonInput<KeyCode>>, mut mode: ResMut<Mode>, mut mobs: Query<&mut HandPose>) {
    for (name, key, hand) in PRESETS {
        if input.just_pressed(key) {
            *mode = Mode::Preset(hand);
            info!("{name}");
        }
    }
    if input.just_pressed(KeyCode::KeyP) {
        *mode = Mode::Piano;
        info!("piano");
    }
    if input.just_pressed(KeyCode::Digit0) {
        *mode = Mode::Animated;
        info!("animation");
    }
    // One digit of the right hand: toggle between straight and closed.
    for (i, key) in DIGIT_KEYS.iter().enumerate() {
        if input.just_pressed(*key) {
            let base = match *mode {
                Mode::Preset(h) => h,
                Mode::Animated | Mode::Piano => Hand::RELAXED,
            };
            let curl = if base.curl.get(i).copied().unwrap_or(0.0) > 0.5 { 0.0 } else { 1.0 };
            *mode = Mode::Preset(base.with_digit(i, curl));
            info!("digit {i}: curl {curl}");
        }
    }
    let pose = match *mode {
        Mode::Animated => HandPose::default(),
        Mode::Preset(h) => HandPose::both(h),
        Mode::Piano => return,
    };
    for mut hands in &mut mobs {
        *hands = pose;
    }
}

/// Every digit curls and opens in turn, a quarter period apart.
fn piano(time: Res<Time>, mode: Res<Mode>, mut mobs: Query<&mut HandPose>) {
    if *mode != Mode::Piano {
        return;
    }
    let t = time.elapsed_secs() * 3.0;
    let mut hand = Hand::OPEN;
    for (i, c) in hand.curl.iter_mut().enumerate() {
        *c = 0.5f32.mul_add(i.to_f32().mul_add(-0.8, t).sin(), 0.5);
    }
    for mut hands in &mut mobs {
        *hands = HandPose::both(hand);
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
    orbit.distance = (orbit.distance * scroll.delta.y.mul_add(-0.1, 1.0)).clamp(0.2, 50.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn screenshot(mut commands: Commands, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    *frame += 1;
    if *frame == 240 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frame == 260 {
        exit.write(AppExit::Success);
    }
}
