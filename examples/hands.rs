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

use bevy::prelude::*;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::LodLevel;
use ornisculpt::mob::{ForceTier, Mob, TierCamera};
use ornisculpt::motion::{Hand, HandPose};
use ornisculpt::num::ToF32;

mod common;
use common::{Orbit, default_plugins, orbit_camera, screenshot_at, spawn_stage};

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
        .add_plugins((default_plugins(Window::default()), OrnisculptPlugin))
        .insert_resource(GlobalAmbientLight { brightness: 400.0, ..default() })
        // The right hand, from the front and a little to its side.
        .insert_resource(Orbit::from_env(-0.5, 0.15, 0.75, Vec3::new(-0.22, 0.7, 0.0), (0.2, 50.0)))
        .init_resource::<Mode>()
        .add_systems(Startup, setup)
        .add_systems(Update, (keys, piano, orbit_camera, screenshot_at(240)))
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

fn setup(mut commands: Commands, assets: Res<AssetServer>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((Camera3d::default(), TierCamera));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 20.0, Vec3::new(-2.0, 5.0, 4.0));
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
