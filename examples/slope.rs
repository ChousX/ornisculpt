//! Crowd tiers on uneven ground: a row of mobs on a ramp and a flight of steps, each pinned to
//! a crowd tier, with the ground hits a game's physics would write. Their feet sit on the slope
//! through the crowd ground fit (`motion::CrowdGround`), not the hero tier's leg IK.
//!
//! ```text
//! cargo run --release --example slope
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · G toggles the ground hits (off, every mob stands as
//! on flat ground) · 1/2/3 force hero/near/far.
//! `ORNISCULPT_TIER=hero|near|far` picks the tier (near by default); `ORNISCULPT_GROUND=0`
//! starts with the hits off; `ORNISCULPT_STILL=1` has every mob stand facing up the ramp; `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few
//! seconds and exits; `ORNISCULPT_SHOT_FRAME=n` picks the frame;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::LodLevel;
use ornisculpt::coat::MobCoat;
use ornisculpt::mob::{ForceTier, Mob, TierCamera};
use ornisculpt::motion::{FootGround, FootProbes, MobIntent, MobMotion, MotionGraph, MotionParams, library};
use ornisculpt::num::{Clamp, Saturate, ToF32};

mod common;
use common::{Orbit, TIER_KEYS, default_plugins, orbit_camera, screenshot_frames, spawn_stage, tier_env};

/// (body plan, coat, grazes).
const BODIES: [(&str, &str, bool); 4] = [
    ("mobs/horse.body.ron", "coats/bay.coat.ron", true),
    ("mobs/ox.body.ron", "coats/hereford.coat.ron", true),
    ("mobs/man.body.ron", "coats/skin_fair.coat.ron", false),
    ("mobs/spider.body.ron", "coats/wolf_spider.coat.ron", false),
];
/// Where the terrain stops being flat: the ramp rises ahead of `RAMP_Z`, the steps climb behind
/// `STEPS_Z`.
const RAMP_Z: f32 = 2.0;
const RAMP_SLOPE: f32 = 0.32;
const STEPS_Z: f32 = -3.0;
const STEP_RUN: f32 = 1.6;
const STEP_RISE: f32 = 0.3;

fn main() {
    App::new()
        .add_plugins((default_plugins(Window::default()), OrnisculptPlugin))
        .insert_resource(GlobalAmbientLight { brightness: 450.0, ..default() })
        .insert_resource(Orbit::from_env(2.6, 0.25, 22.0, Vec3::new(0.0, 1.0, 0.0), (0.5, 100.0)))
        .insert_resource(Ground(std::env::var("ORNISCULPT_GROUND").as_deref() != Ok("0")))
        .add_systems(Startup, setup)
        .add_systems(Update, (controls, wander, graze, orbit_camera, screenshot_frames(420)))
        .add_systems(Update, follow_terrain.after(ornisculpt::mob::MobSet::Motion))
        .run();
}

/// Whether the mobs get ground hits.
#[derive(Resource)]
struct Ground(bool);

#[derive(Component)]
struct Walker {
    centre: Vec3,
    phase: f32,
    grazes: bool,
    next_graze: f32,
}

/// The terrain's height at `(x, z)`: flat in the middle, a ramp ahead, steps behind.
fn height(x: f32, z: f32) -> f32 {
    let _ = x;
    if z > RAMP_Z {
        (z - RAMP_Z) * RAMP_SLOPE
    } else if z < STEPS_Z {
        ((STEPS_Z - z) / STEP_RUN).floor() * STEP_RISE
    } else {
        0.0
    }
}

/// A grid mesh over the terrain, fine enough that the steps keep near-vertical risers.
fn terrain(size: f32, cell: f32) -> Mesh {
    let n = (size / cell).ceil().sat_usize().max(1);
    let at = |i: usize| i.to_f32().mul_add(cell, -size * 0.5);
    let mut positions = Vec::with_capacity((n + 1) * (n + 1));
    let mut uvs = Vec::with_capacity((n + 1) * (n + 1));
    for j in 0..=n {
        for i in 0..=n {
            let (x, z) = (at(i), at(j));
            positions.push([x, height(x, z), z]);
            uvs.push([i.to_f32() / n.to_f32(), j.to_f32() / n.to_f32()]);
        }
    }
    let mut indices = Vec::with_capacity(n * n * 6);
    for j in 0..n {
        for i in 0..n {
            let near_left = (j * (n + 1) + i).clamp_u32();
            let near_right = near_left + 1;
            let far_left = near_left + (n + 1).clamp_u32();
            let far_right = far_left + 1;
            indices.extend_from_slice(&[near_left, far_left, near_right, near_right, far_left, far_right]);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    mesh.compute_normals();
    mesh
}

fn setup(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut graphs: ResMut<Assets<MotionGraph>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 0.0, Vec3::new(3.0, 6.0, -2.0));
    commands.spawn((
        Mesh3d(meshes.add(terrain(60.0, 0.2))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.36, 0.4, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
    let graph = graphs.add(library::standard());
    let tier = tier_env("ORNISCULPT_TIER").unwrap_or(LodLevel::Near);
    // Three rows: on the ramp, on the flat, on the steps.
    for (row, z) in [(0, 7.0), (1, 0.0), (2, -7.0)] {
        for (i, (path, coat, grazes)) in BODIES.iter().enumerate() {
            let centre = Vec3::new(i.to_f32().mul_add(4.0, -6.0), 0.0, z);
            commands.spawn((
                Mob::new(assets.load(*path)),
                MobCoat(assets.load(*coat)),
                MobMotion::new(graph.clone()),
                ForceTier(tier),
                FootGround::default(),
                Walker { centre, phase: (row * 4 + i).to_f32() * 1.3, grazes: *grazes, next_graze: (row * 4 + i).to_f32().mul_add(0.7, 4.0) },
                Transform::from_translation(centre + Vec3::Z * 0.8),
            ));
        }
    }
    info!("G toggles the ground hits; 1/2/3 force hero/near/far");
}

fn controls(keys: Res<ButtonInput<KeyCode>>, mut ground: ResMut<Ground>, mut commands: Commands, mobs: Query<Entity, With<Walker>>) {
    if keys.just_pressed(KeyCode::KeyG) {
        ground.0 = !ground.0;
        info!("ground hits {}", if ground.0 { "on" } else { "off" });
    }
    for (key, level) in TIER_KEYS {
        if keys.just_pressed(key) {
            for mob in &mobs {
                commands.entity(mob).insert(ForceTier(level));
            }
        }
    }
}

/// Each mob ambles round its spot, pausing now and then (or stands facing up the ramp under
/// `ORNISCULPT_STILL=1`).
fn wander(time: Res<Time>, mut mobs: Query<(&Walker, &Transform, &mut MobIntent)>, mut still: Local<Option<bool>>) {
    let still = *still.get_or_insert_with(|| std::env::var_os("ORNISCULPT_STILL").is_some());
    let t = time.elapsed_secs();
    for (w, tf, mut intent) in &mut mobs {
        if still {
            intent.velocity = Vec3::ZERO;
            intent.facing = Some(Dir3::Z);
            continue;
        }
        let a = t.mul_add(0.25, w.phase);
        let target = w.centre + Vec3::new(a.cos(), 0.0, a.sin()) * 0.8;
        let pause = f32::from(u8::from(t.mul_add(0.11, w.phase).sin() > 0.5));
        intent.velocity = ((target - tf.translation) * 1.5).clamp_length_max(0.5) * (1.0 - pause);
    }
}

/// Grazers eat every few seconds.
fn graze(time: Res<Time>, mut mobs: Query<(&mut Walker, &mut MotionParams)>) {
    let t = time.elapsed_secs();
    for (mut w, mut params) in &mut mobs {
        if w.grazes && t >= w.next_graze {
            w.next_graze = t + 9.0;
            params.trigger("eat");
        }
    }
}

/// The game's part: the mob's root rides the terrain, and each foot probe hits it.
fn follow_terrain(ground: Res<Ground>, mut mobs: Query<(&mut Transform, &FootProbes, &mut FootGround), With<Walker>>) {
    for (mut tf, probes, mut hits) in &mut mobs {
        let y = height(tf.translation.x, tf.translation.z);
        if tf.translation.y.to_bits() != y.to_bits() {
            tf.translation.y = y;
        }
        hits.0.clear();
        if ground.0 {
            hits.0.extend(probes.0.iter().map(|p| Some(Vec3::new(p.x, height(p.x, p.z), p.z))));
        }
    }
}
