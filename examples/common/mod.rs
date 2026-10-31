//! Shared by the examples: the orbit camera, the stage they stand on, headless screenshots, the
//! crowd workload (`crowd` and `bench`), and small helpers. Each example uses only part of it.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bevy::app::{PluginGroupBuilder, ScheduleRunnerPlugin};
use bevy::camera::{ImageRenderTarget, RenderTarget};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::render::view::window::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::window::{ExitCondition, PresentMode};
use bevy::winit::WinitPlugin;
use ornisculpt::anatomy::{Anatomy, BodyPlan, LodLevel, PartAssets, PartLodRules, TriMesh, referenced_assets};
use ornisculpt::mob::{BodyFailed, MobTier, QualityProfile};

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

/// A shadowed, slightly warm sun at `light` looking at the origin, a cool unshadowed fill from the
/// sun's other side, lower, so shaded sides, eyes under their lids and the underside keep their
/// shape, an overcast backdrop, and a square of ground `ground` metres across.
pub fn spawn_stage(commands: &mut Commands, meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>, ground: f32, light: Vec3) {
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 9000.0, color: Color::srgb(1.0, 0.96, 0.9), ..default() },
        Transform::from_translation(light).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    let fill = Vec3::new(-light.x, light.y * 0.35, light.z);
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: false, illuminance: 2600.0, color: Color::srgb(0.78, 0.86, 1.0), ..default() },
        Transform::from_translation(fill).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(ClearColor(Color::srgb(0.46, 0.5, 0.55)));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(ground, ground))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.36, 0.37, 0.33), perceptual_roughness: 1.0, ..default() })),
    ));
}

/// `DefaultPlugins` with `window`, or, when `ORNISCULPT_SCREENSHOT` is set, headless as
/// ornithochory's grove: no window, every camera renders into a fixed [`SHOT_SIZE`] image
/// ([`ShotTarget`]) and screenshots copy out of it. The loop runs at 60 Hz, as a vsynced window
/// would, or uncapped if `window` doesn't wait for vsync.
pub fn default_plugins(window: Window) -> PluginGroupBuilder {
    if std::env::var_os("ORNISCULPT_SCREENSHOT").is_none() {
        return DefaultPlugins.set(WindowPlugin { primary_window: Some(window), ..default() });
    }
    let wait = match window.present_mode {
        PresentMode::AutoNoVsync | PresentMode::Immediate => Duration::ZERO,
        _ => Duration::from_secs_f64(1.0 / 60.0),
    };
    DefaultPlugins
        .set(WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..default() })
        .disable::<WinitPlugin>()
        // With a window plugin, DefaultPlugins leaves the runner to winit.
        .add(ScheduleRunnerPlugin::run_loop(wait))
        .add(ShotTargetPlugin)
}

/// The size headless screenshots render at (the default window's).
pub const SHOT_SIZE: UVec2 = UVec2::new(1280, 720);

/// What every camera renders into when headless.
#[derive(Resource)]
pub struct ShotTarget(pub Handle<Image>);

struct ShotTargetPlugin;

impl Plugin for ShotTargetPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(retarget);
    }

    // Images only exist once the render plugins are built.
    fn finish(&self, app: &mut App) {
        let mut image = Image::new_target_texture(SHOT_SIZE.x, SHOT_SIZE.y, TextureFormat::Rgba8UnormSrgb, None);
        image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let handle = app.world_mut().resource_mut::<Assets<Image>>().add(image);
        app.insert_resource(ShotTarget(handle));
    }
}

/// Points a window camera at the shot target, and the UI with it.
fn retarget(add: On<Add<Camera>>, mut commands: Commands, target: Res<ShotTarget>, cameras: Query<&RenderTarget>) {
    if let Ok(RenderTarget::Window(_)) = cameras.get(add.entity) {
        commands
            .entity(add.entity)
            .insert((RenderTarget::Image(ImageRenderTarget { handle: target.0.clone(), scale_factor: 1.0 }), IsDefaultUiCamera));
    }
}

/// A screenshot of what the camera shows: the shot target when headless, else the window.
pub fn screenshot(target: Option<&ShotTarget>) -> Screenshot {
    target.map_or_else(Screenshot::primary_window, |t| Screenshot::image(t.0.clone()))
}

/// `ORNISCULPT_SCREENSHOT=out.png`: saves a screenshot at `frame` and exits once it is saved.
pub fn screenshot_at(frame: u32) -> impl FnMut(Commands, Option<Res<ShotTarget>>, MessageWriter<AppExit>) {
    screenshots(vec![frame], frame)
}

/// As [`screenshot_at`], at each frame of `ORNISCULPT_SHOT_FRAME=360` or several, `300,302,304`
/// (saved as `<path>.<frame>.png`); `default` without it.
pub fn screenshot_frames(default: u32) -> impl FnMut(Commands, Option<Res<ShotTarget>>, MessageWriter<AppExit>) {
    let frames = shot_frames(default);
    let last = frames.iter().copied().max().unwrap_or(default);
    screenshots(frames, last)
}

/// The frames of `ORNISCULPT_SHOT_FRAME`, `default` without it.
pub fn shot_frames(default: u32) -> Vec<u32> {
    std::env::var("ORNISCULPT_SHOT_FRAME").unwrap_or_else(|_| default.to_string()).split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

/// Exits once every shot is saved: the capture arrives some frames after the request, more when
/// frames are fast or a body is generating, so a fixed frame count can quit before it lands.
fn screenshots(frames: Vec<u32>, last: u32) -> impl FnMut(Commands, Option<Res<ShotTarget>>, MessageWriter<AppExit>) {
    let mut frame = 0u32;
    let saved = Arc::new(AtomicUsize::new(0));
    move |mut commands: Commands, target: Option<Res<ShotTarget>>, mut exit: MessageWriter<AppExit>| {
        let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
        frame += 1;
        if frames.contains(&frame) {
            let path = if frames.len() > 1 { format!("{path}.{frame}.png") } else { path };
            let saved = saved.clone();
            commands
                .spawn(screenshot(target.as_deref()))
                .observe(save_to_disk(path))
                .observe(move |_: On<ScreenshotCaptured>| _ = saved.fetch_add(1, Ordering::Relaxed));
        }
        if frame > last && saved.load(Ordering::Relaxed) >= frames.len() {
            exit.write(AppExit::Success);
        }
    }
}

/// A headless shot (`ORNISCULPT_SCREENSHOT`) exits non-zero, with the error, when a body fails:
/// a plan, coat or outfit that doesn't parse would otherwise leave the run waiting out its
/// timeout. With a window the failure is only logged, and the example stays up.
pub fn exit_on_body_failed(ev: On<BodyFailed>, mut exit: MessageWriter<AppExit>) {
    if std::env::var_os("ORNISCULPT_SCREENSHOT").is_none() {
        return;
    }
    error!("mob {} failed, no screenshot: {}", ev.entity, ev.error);
    exit.write(AppExit::error());
}

/// Counts `frames` since the body became ready: a screenshot to `path` at 120 (pipelines have
/// compiled by then), exit at 140.
pub fn shoot_when_ready(commands: &mut Commands, target: Option<&ShotTarget>, exit: &mut MessageWriter<AppExit>, frames: &mut u32, path: String) {
    *frames += 1;
    if *frames == 120 {
        commands.spawn(screenshot(target)).observe(save_to_disk(path));
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

/// The checkout examples read from: this one, or `ORNISCULPT_BENCH_ROOT` for a binary built on
/// another machine (the bench on the Steam Deck runs toth's build).
pub fn root() -> std::path::PathBuf {
    std::env::var_os("ORNISCULPT_BENCH_ROOT").map_or_else(|| env!("CARGO_MANIFEST_DIR").into(), std::path::PathBuf::from)
}

/// The authored part meshes `plan` references, read from `assets/` without the asset server:
/// the labelled primitive's positions, normals and indices from an embedded little-endian buffer,
/// as `tools/` write them. A part that can't be read is left out (building then says which).
pub fn part_assets(plan: &BodyPlan) -> PartAssets {
    let mut assets = PartAssets::default();
    for path in referenced_assets(plan) {
        if let Some(mesh) = read_part(&path) {
            assets.insert(path, Arc::new(mesh));
        }
    }
    assets
}

/// `file#MeshN/PrimitiveM` (`Mesh0/Primitive0` without a label).
fn read_part(path: &str) -> Option<TriMesh> {
    use base64::Engine;
    let (file, label) = path.split_once('#').unwrap_or((path, ""));
    let number = |prefix: &str| label.split('/').find_map(|l| l.strip_prefix(prefix)?.parse::<usize>().ok()).unwrap_or(0);
    let text = std::fs::read_to_string(root().join("assets").join(file)).ok()?;
    let gltf: serde_json::Value = serde_json::from_str(&text).ok()?;
    let at = |pointer: String| gltf.pointer(&pointer);
    let (_, data) = at("/buffers/0/uri".into())?.as_str()?.split_once(',')?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let index = |v: &serde_json::Value| v.as_u64().and_then(|i| usize::try_from(i).ok());
    let view = |accessor: &serde_json::Value| -> Option<(&[u8], u64)> {
        let acc = at(format!("/accessors/{}", index(accessor)?))?;
        let v = at(format!("/bufferViews/{}", index(acc.get("bufferView")?)?))?;
        let o = v.get("byteOffset").and_then(index).unwrap_or(0);
        let l = index(v.get("byteLength")?)?;
        Some((bytes.get(o..o + l)?, acc.get("componentType")?.as_u64()?))
    };
    let prim = at(format!("/meshes/{}/primitives/{}", number("Mesh"), number("Primitive")))?;
    let f32s = |c: &[u8; 12]| c.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<_>>();
    let vec3s = |name: &str| -> Option<Vec<Vec3>> {
        let (b, _) = view(prim.pointer(&format!("/attributes/{name}"))?)?;
        Some(b.as_chunks::<12>().0.iter().map(|c| Vec3::from_slice(&f32s(c))).collect())
    };
    let positions = vec3s("POSITION")?;
    let normals = vec3s("NORMAL").unwrap_or_default();
    let (ib, kind) = view(prim.get("indices")?)?;
    let indices = match kind {
        5125 => ib.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect(),
        _ => ib.as_chunks::<2>().0.iter().map(|c| u32::from(u16::from_le_bytes(*c))).collect(),
    };
    Some(TriMesh::new(positions, normals, indices))
}

/// The anatomy of `plan` in its default form.
pub fn build_plan(plan: &BodyPlan, rules: PartLodRules, assets: &PartAssets) -> Result<Anatomy, String> {
    Anatomy::from_plan(plan, &rules, assets).map_err(|e| e.to_string())
}
