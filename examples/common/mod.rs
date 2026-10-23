//! Shared by the examples: the orbit camera, the stage they stand on, screenshots, the crowd
//! workload (`crowd` and `bench`), and small helpers. Each example uses only part of it.
#![allow(dead_code)]

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::anatomy::{Anatomy, BodyPlan, Form, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};
use ornisculpt::mob::{MobTier, QualityProfile};

pub mod crowd;

/// The camera circles `target` at `distance`; left-drag turns it, scroll zooms within `zoom`.
#[derive(Resource)]
pub struct Orbit {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: Vec3,
    /// Nearest and farthest zoom.
    pub zoom: (f32, f32),
}

impl Orbit {
    /// `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z`, or the given view.
    pub fn from_env(yaw: f32, pitch: f32, distance: f32, target: Vec3, zoom: (f32, f32)) -> Self {
        let (yaw, pitch, distance, target) = cam_env().unwrap_or((yaw, pitch, distance, target));
        Self { yaw, pitch, distance, target, zoom }
    }

    /// Turns by the mouse's `motion` if `rotate`, and zooms by `scroll` lines if `zoom`.
    pub fn step(&mut self, rotate: bool, motion: Vec2, scroll: f32, zoom: bool) {
        if rotate {
            self.yaw = motion.x.mul_add(-0.008, self.yaw);
            self.pitch = motion.y.mul_add(0.008, self.pitch).clamp(-1.4, 1.4);
        }
        if zoom {
            self.distance = (self.distance * scroll.mul_add(-0.1, 1.0)).clamp(self.zoom.0, self.zoom.1);
        }
    }

    /// The camera looking at `target`, its yaw turned by `heading`.
    pub fn transform_at(&self, target: Vec3, heading: f32) -> Transform {
        let rot = Quat::from_euler(EulerRot::YXZ, self.yaw + heading, -self.pitch, 0.0);
        Transform::from_translation(target + rot * Vec3::Z * self.distance).looking_at(target, Vec3::Y)
    }
}

/// The six values of `ORNISCULPT_CAM`, when all are given.
pub fn cam_env() -> Option<(f32, f32, f32, Vec3)> {
    let v: Vec<f32> = std::env::var("ORNISCULPT_CAM").map_or_default(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect());
    match v[..] {
        [yaw, pitch, distance, x, y, z] => Some((yaw, pitch, distance, Vec3::new(x, y, z))),
        _ => None,
    }
}

/// Left-drag orbits, scroll zooms.
pub fn orbit_camera(
    mut orbit: ResMut<Orbit>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    orbit.step(buttons.pressed(MouseButton::Left), motion.delta, scroll.delta.y, true);
    let view = orbit.transform_at(orbit.target, 0.0);
    for mut t in &mut camera {
        *t = view;
    }
}

/// A shadowed sun at `light` looking at the origin, and a square of ground `ground` metres across.
pub fn spawn_stage(commands: &mut Commands, meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>, ground: f32, light: Vec3) {
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 8000.0, ..default() },
        Transform::from_translation(light).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(ground, ground))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
}

/// `ORNISCULPT_SCREENSHOT=out.png`: saves a screenshot at `frame` and exits 20 frames later.
pub fn screenshot_at(frame: u32) -> impl FnMut(Commands, MessageWriter<AppExit>) {
    screenshots(vec![frame], frame)
}

/// As [`screenshot_at`], at each frame of `ORNISCULPT_SHOT_FRAME=360` or several, `300,302,304`
/// (saved as `<path>.<frame>.png`); `default` without it.
pub fn screenshot_frames(default: u32) -> impl FnMut(Commands, MessageWriter<AppExit>) {
    let frames = shot_frames(default);
    let last = frames.iter().copied().max().unwrap_or(default);
    screenshots(frames, last)
}

/// The frames of `ORNISCULPT_SHOT_FRAME`, `default` without it.
pub fn shot_frames(default: u32) -> Vec<u32> {
    std::env::var("ORNISCULPT_SHOT_FRAME").unwrap_or_else(|_| default.to_string()).split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

fn screenshots(frames: Vec<u32>, last: u32) -> impl FnMut(Commands, MessageWriter<AppExit>) {
    let mut frame = 0u32;
    move |mut commands: Commands, mut exit: MessageWriter<AppExit>| {
        let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
        frame += 1;
        if frames.contains(&frame) {
            let path = if frames.len() > 1 { format!("{path}.{frame}.png") } else { path };
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
        }
        if frame == last + 20 {
            exit.write(AppExit::Success);
        }
    }
}

/// Counts `frames` since the body became ready: a screenshot to `path` at 120 (pipelines have
/// compiled by then), exit at 140.
pub fn shoot_when_ready(commands: &mut Commands, exit: &mut MessageWriter<AppExit>, frames: &mut u32, path: String) {
    *frames += 1;
    if *frames == 120 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frames == 140 {
        exit.write(AppExit::Success);
    }
}

/// Command-line argument `n` as an asset path (a leading `assets/` dropped).
pub fn asset_arg(n: usize) -> Option<String> {
    std::env::args().nth(n).map(|a| a.strip_prefix("assets/").map(str::to_owned).unwrap_or(a))
}

/// `hero`, `near` or `far` from the environment variable `var`.
pub fn tier_env(var: &str) -> Option<LodLevel> {
    match std::env::var(var).as_deref() {
        Ok("hero") => Some(LodLevel::Hero),
        Ok("near") => Some(LodLevel::Near),
        Ok("far") => Some(LodLevel::Far),
        _ => None,
    }
}

/// 1/2/3 force hero/near/far.
pub const TIER_KEYS: [(KeyCode, LodLevel); 3] = [(KeyCode::Digit1, LodLevel::Hero), (KeyCode::Digit2, LodLevel::Near), (KeyCode::Digit3, LodLevel::Far)];

/// Q cycles the quality profile (medium, high, low); `quality` is the step.
pub fn cycle_quality(keys: &ButtonInput<KeyCode>, profile: &mut QualityProfile, quality: &mut usize) {
    if keys.just_pressed(KeyCode::KeyQ) {
        *quality = (*quality + 1) % 3;
        let presets = [(QualityProfile::MEDIUM, "medium"), (QualityProfile::HIGH, "high"), (QualityProfile::LOW, "low")];
        if let Some((preset, name)) = presets.get(*quality) {
            *profile = preset.clone();
            info!("quality: {name}");
        }
    }
}

/// Mobs at each tier, hero first.
pub fn tier_counts(tiers: &Query<&MobTier>) -> [usize; 3] {
    LodLevel::ALL.map(|level| tiers.iter().filter(|t| t.0 == level).count())
}

/// The anatomy of `plan` in its default form.
pub fn build_plan(plan: &BodyPlan, rules: PartLodRules, assets: &PartAssets) -> Result<Anatomy, String> {
    let live = apply_form(plan, &Form::default()).map_err(|e| e.to_string())?;
    build_anatomy(&live, &rules, assets).map_err(|e| e.to_string())
}
