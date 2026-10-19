//! Crowd stress test: thousands of wandering mobs across several body plans.
//!
//! ```text
//! cargo run --release --example crowd [count]
//! ```
//!
//! Hero-tier mobs (closest on screen) get joints and full procedural animation; the rest are
//! single entities skinned on the GPU from baked motion. Frame time and tier counts are logged.
//! Controls: left-drag orbit · scroll zoom · Q cycle quality profile.
//! `ORNISCULPT_COATS=1` dresses a quarter each plain, scaly, furry and feathered
//! (`=lizard|furry|bird` dresses every mob in one coat);
//! `ORNISCULPT_OUTFITS=1` dresses bipeds (peasant/warrior) and quadrupeds (harness);
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::window::PresentMode;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{Form, FormPatch, Lever, LodLevel, PartPath};
use ornisculpt::clothing::MobOutfit;
use ornisculpt::coat::MobCoat;
use ornisculpt::mob::{CrowdUploads, GenCache, Mob, MobSeed, MobTier, QualityProfile, TierCamera};
use ornisculpt::motion::MobIntent;
use ornisculpt::num::{Saturate, ToF32, usize_to_u64};

const COATS: [Option<&str>; 4] = [None, Some("coats/lizard.coat.ron"), Some("coats/furry.coat.ron"), Some("coats/bird.coat.ron")];

const BODIES: [&str; 4] = ["mobs/quadruped.body.ron", "mobs/beast.body.ron", "mobs/biped.body.ron", "mobs/hexapod.body.ron"];
/// Each body's root part (the trunk the girth patch widens), in `BODIES` order.
const ROOTS: [&str; 4] = ["torso", "torso", "torso", "thorax"];

fn main() {
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(4000);
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window { present_mode: PresentMode::AutoNoVsync, ..default() }),
                ..default()
            }),
            FrameTimeDiagnosticsPlugin::default(),
            OrnisculptPlugin,
        ))
        .insert_resource(Count(count))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .init_resource::<Orbit>()
        .init_resource::<UploadStats>()
        .add_systems(Startup, setup)
        .add_systems(Update, (wander, orbit_camera, controls, report, screenshot))
        .add_systems(Last, count_uploads)
        .run();
}

#[derive(Resource)]
struct Count(usize);

/// Running totals of crowd instance uploads (slots changed per frame).
#[derive(Resource, Default)]
struct UploadStats {
    frames: u32,
    slots: usize,
}

fn count_uploads(uploads: Res<CrowdUploads>, mut stats: ResMut<UploadStats>) {
    stats.frames += 1;
    stats.slots += uploads.instances.len();
}

#[derive(Component)]
struct Wander {
    centre: Vec3,
    radius: f32,
    speed: f32,
    offset: f32,
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
            _ => Self { yaw: 0.6, pitch: 0.5, distance: 30.0, target: Vec3::ZERO },
        }
    }
}

fn setup(
    mut commands: Commands,
    count: Res<Count>,
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
        Mesh3d(meshes.add(Plane3d::default().mesh().size(400.0, 400.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
    let side = count.0.to_f32().sqrt().ceil().sat_usize();
    let spacing = 2.2;
    for (i, (path, root)) in BODIES.iter().zip(ROOTS).cycle().take(count.0).enumerate() {
        // Three builds per body plan, so the cache shares bodies.
        let build = (i / BODIES.len()) % 3;
        let form = Form {
            overall_scale: 0.15f32.mul_add(build.to_f32(), 0.85),
            patches: vec![FormPatch { part: PartPath(root.into()), lever: Lever::Girth, value: 0.15f32.mul_add(build.to_f32(), 0.9) }],
        };
        let (x, z) = ((i % side).to_f32(), (i / side).to_f32());
        let centre = Vec3::new(side.to_f32().mul_add(-0.5, x) * spacing, 0.0, side.to_f32().mul_add(-0.5, z) * spacing);
        let mut mob = commands.spawn((
            Mob::new(assets.load(*path)),
            form,
            MobSeed(usize_to_u64(i)),
            Wander { centre, radius: 0.6, speed: (i % 7).to_f32().mul_add(0.12, 0.5), offset: i.to_f32() * 1.7 },
            Transform::from_translation(centre),
        ));
        // `ORNISCULPT_COATS=1`: a quarter each plain, scaly, furry and feathered.
        // `=lizard|furry|bird` puts that coat on every mob instead.
        match std::env::var("ORNISCULPT_COATS").as_deref() {
            Ok("1") => {
                if let Some(Some(coat)) = COATS.get((i / 7) % COATS.len()) {
                    mob.insert(MobCoat(assets.load(*coat)));
                }
            }
            Ok(name) => {
                mob.insert(MobCoat(assets.load(format!("coats/{name}.coat.ron"))));
            }
            Err(_) => {}
        }
        // `ORNISCULPT_OUTFITS=1`: bipeds wear peasant or warrior clothes, quadrupeds a harness.
        if std::env::var("ORNISCULPT_OUTFITS").is_ok() {
            let outfit = match *path {
                "mobs/biped.body.ron" => Some(if (i / BODIES.len()) % 2 == 0 { "outfits/peasant.outfit.ron" } else { "outfits/warrior.outfit.ron" }),
                "mobs/quadruped.body.ron" => Some("outfits/harness.outfit.ron"),
                _ => None,
            };
            if let Some(o) = outfit {
                mob.insert(MobOutfit(assets.load(o)));
            }
        }
    }
    info!("spawned {} mobs; controls: left-drag orbit, scroll zoom, Q quality", count.0);
}

fn wander(time: Res<Time>, mut mobs: Query<(&Wander, &Transform, &mut MobIntent)>) {
    let t = time.elapsed_secs();
    mobs.par_iter_mut().for_each(|(w, tf, mut intent)| {
        let a = f32::mul_add(t * w.speed / w.radius.max(0.1), 0.5, w.offset);
        let target = w.centre + Vec3::new(a.cos(), 0.0, a.sin()) * w.radius;
        let pause = f32::from(u8::from(f32::mul_add(t, 0.15, w.offset).sin() > 0.8));
        intent.velocity = ((target - tf.translation) * 1.5).clamp_length_max(w.speed) * (1.0 - pause);
    });
}

fn controls(keys: Res<ButtonInput<KeyCode>>, mut profile: ResMut<QualityProfile>, mut quality: Local<usize>) {
    if keys.just_pressed(KeyCode::KeyQ) {
        *quality = (*quality + 1) % 3;
        let presets = [(QualityProfile::MEDIUM, "medium"), (QualityProfile::HIGH, "high"), (QualityProfile::LOW, "low")];
        if let Some((preset, name)) = presets.get(*quality) {
            *profile = preset.clone();
            info!("quality: {name}");
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
    orbit.distance = (orbit.distance * scroll.delta.y.mul_add(-0.1, 1.0)).clamp(0.5, 400.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn report(
    time: Res<Time>,
    mut next: Local<f32>,
    diagnostics: Res<DiagnosticsStore>,
    cache: Res<GenCache>,
    mut uploads: ResMut<UploadStats>,
    tiers: Query<&MobTier>,
    mobs: Query<(), With<Mob>>,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    let counts = LodLevel::ALL.map(|level| tiers.iter().filter(|t| t.0 == level).count());
    let frame_ms = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(bevy::diagnostic::Diagnostic::smoothed)
        .unwrap_or(0.0);
    let per_frame = uploads.slots.to_f32() / uploads.frames.max(1).to_f32();
    *uploads = default();
    info!(
        "{} mobs | frame {:.2} ms ({:.0} fps) | bodies {} cached, {} generating | hero/near/far {:?} | {:.0} slots ({:.1} KB) uploaded/frame",
        mobs.iter().count(),
        frame_ms,
        1000.0 / frame_ms.max(0.001),
        cache.bodies.len(),
        cache.in_flight(),
        counts,
        per_frame,
        per_frame * 32.0 / 1024.0,
    );
}

fn screenshot(mut commands: Commands, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    *frame += 1;
    if *frame == 600 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frame == 620 {
        exit.write(AppExit::Success);
    }
}
