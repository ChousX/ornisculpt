//! Crowd stress test: thousands of wandering mobs across several body plans.
//!
//! ```text
//! cargo run --release --example crowd [count]
//! ```
//!
//! Hero-tier mobs (closest on screen) get joints and full procedural animation; the rest are
//! single entities skinned on the GPU from baked motion. Frame time and tier counts are logged.
//! Controls: left-drag orbit · scroll zoom · Q cycle quality profile.
//! `ORNISCULPT_COATS=1` dresses each body in its own coat, a quarter left plain
//! (`=<name>` dresses every mob in `coats/<name>.coat.ron`);
//! `ORNISCULPT_OUTFITS=1` dresses the men and women (peasant/warrior) and the oxen (harness);
//! `ORNISCULPT_MOTION=1` gives every mob the standard motion graph (idles and actions play in
//! crowd tiers from baked clips; a mob triggers an action every few seconds);
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits
//! (`ORNISCULPT_SHOT_FRAME=n` picks the frame);
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::window::PresentMode;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::clothing::MobOutfit;
use ornisculpt::coat::MobCoat;
use ornisculpt::mob::{CrowdUploads, GenCache, Mob, MobTier, QualityProfile, TierCamera};
use ornisculpt::motion::{MobMotion, MotionGraph, MotionParams, library};
use ornisculpt::num::{Saturate, ToF32};

mod common;
use common::crowd::{self, BODIES, ROOTS};
use common::{Orbit, cycle_quality, default_plugins, orbit_camera, screenshot_frames, spawn_stage, tier_counts};

fn main() {
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(4000);
    App::new()
        .add_plugins((
            default_plugins(Window { present_mode: PresentMode::AutoNoVsync, ..default() }),
            FrameTimeDiagnosticsPlugin::default(),
            OrnisculptPlugin,
        ))
        .insert_resource(Count(count))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .insert_resource(Orbit::from_env(0.6, 0.5, 30.0, Vec3::ZERO, (0.5, 400.0)))
        .init_resource::<UploadStats>()
        .add_systems(Startup, setup)
        .add_systems(Update, (crowd::wander, act, orbit_camera, controls, report, screenshot_frames(600)))
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

/// `ORNISCULPT_MOTION=1`: a mob with the standard graph, triggering `action` every `every` seconds.
#[derive(Component)]
struct Actor {
    action: &'static str,
    every: f32,
    next: f32,
}

fn setup(
    mut commands: Commands,
    count: Res<Count>,
    assets: Res<AssetServer>,
    mut graphs: ResMut<Assets<MotionGraph>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 400.0, Vec3::new(3.0, 6.0, 2.0));
    let side = count.0.to_f32().sqrt().ceil().sat_usize();
    let graph = std::env::var("ORNISCULPT_MOTION").is_ok().then(|| graphs.add(library::standard()));
    for (i, (path, root)) in BODIES.iter().zip(ROOTS).cycle().take(count.0).enumerate() {
        let mut mob = crowd::spawn(&mut commands, &assets, i, side, (*path, root));
        if let Some(graph) = &graph {
            let action = ["attack", "shake", "wave", "rear"].get(i % 4).copied().unwrap_or("attack");
            mob.insert((MobMotion::new(graph.clone()), Actor { action, every: 6.0, next: (i % 60).to_f32().mul_add(0.1, 3.0) }));
        }
        // `ORNISCULPT_COATS=1`: each body its own coat, a quarter plain.
        // `=<name>` puts that coat on every mob instead.
        match std::env::var("ORNISCULPT_COATS").as_deref() {
            Ok("1") => {
                if let Some(coat) = crowd::coat(i) {
                    mob.insert(MobCoat(assets.load(coat)));
                }
            }
            Ok(name) => {
                mob.insert(MobCoat(assets.load(format!("coats/{name}.coat.ron"))));
            }
            Err(_) => {}
        }
        // `ORNISCULPT_OUTFITS=1`: men and women wear peasant or warrior clothes, oxen a harness.
        if std::env::var("ORNISCULPT_OUTFITS").is_ok() {
            let outfit = match *path {
                "mobs/man.body.ron" | "mobs/woman.body.ron" => Some(if (i / BODIES.len()) % 2 == 0 { "outfits/peasant.outfit.ron" } else { "outfits/warrior.outfit.ron" }),
                "mobs/ox.body.ron" => Some("outfits/harness.outfit.ron"),
                _ => None,
            };
            if let Some(o) = outfit {
                mob.insert(MobOutfit(assets.load(o)));
            }
        }
    }
    info!("spawned {} mobs; controls: left-drag orbit, scroll zoom, Q quality", count.0);
}

/// Actors trigger their action on schedule (whether it fits their body is the graph's business).
fn act(time: Res<Time>, mut actors: Query<(&mut Actor, &mut MotionParams)>) {
    let t = time.elapsed_secs();
    for (mut actor, mut params) in &mut actors {
        if t >= actor.next {
            actor.next = t + actor.every;
            params.trigger(actor.action);
        }
    }
}

fn controls(keys: Res<ButtonInput<KeyCode>>, mut profile: ResMut<QualityProfile>, mut quality: Local<usize>) {
    cycle_quality(&keys, &mut profile, &mut quality);
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
    let counts = tier_counts(&tiers);
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
