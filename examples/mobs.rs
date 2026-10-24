//! A field of mobs spawned through the plugin: async generation, shared bodies, tiers by
//! on-screen size, and runtime levers.
//!
//! ```text
//! cargo run --release --example mobs [count]
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · S sever a tail · H graft horns · L stretch necks ·
//! Q cycle quality profile.
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits;
//! `ORNISCULPT_DEMO=1` triggers sever/graft/stretch automatically;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use bevy::prelude::*;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::num::{Saturate, ToF32, usize_to_u64};
use ornisculpt::anatomy::{
    AttachSlot, Form, FormPatch, HardKind, HardParams, Lever, PartKind, PartNode, PartPath,
};
use ornisculpt::mob::{
    BodyReady, BoneScale, BoneScales, GenCache, GeneratedBody, GraftPart, Mob, MobBody, MobSeed, MobTier, Part,
    QualityProfile, SeverPart, TierCamera,
};

mod common;
use common::{Orbit, cycle_quality, default_plugins, orbit_camera, screenshot_at, spawn_stage, tier_counts};

fn main() {
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(64);
    App::new()
        .add_plugins((default_plugins(Window::default()), OrnisculptPlugin))
        .insert_resource(Count(count))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .insert_resource(Orbit::from_env(0.6, 0.45, 12.0, Vec3::ZERO, (0.5, 200.0)))
        .add_observer(on_ready)
        .add_systems(Startup, setup)
        .add_systems(Update, (controls, orbit_camera, report, screenshot_at(300)))
        .run();
}

#[derive(Resource)]
struct Count(usize);

/// Index of a spawned mob, used to pick demo targets deterministically.
#[derive(Component)]
struct Index(usize);

fn setup(
    mut commands: Commands,
    count: Res<Count>,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 200.0, Vec3::new(3.0, 6.0, 2.0));

    let body = assets.load("mobs/quadruped.body.ron");
    let side = count.0.to_f32().sqrt().ceil().sat_usize();
    for i in 0..count.0 {
        // A handful of distinct builds, so the cache shares bodies between mobs.
        let variant = i % 4;
        let k = variant.to_f32() / 3.0;
        let form = Form {
            overall_scale: 0.3f32.mul_add(k, 0.85),
            patches: vec![
                FormPatch { part: PartPath("torso/limb.front".into()), lever: Lever::Length, value: 0.3f32.mul_add(k, 0.9) },
                FormPatch { part: PartPath("torso/limb.hind".into()), lever: Lever::Length, value: 0.3f32.mul_add(k, 0.9) },
                FormPatch { part: PartPath("torso".into()), lever: Lever::Girth, value: 0.3f32.mul_add(-k, 1.2) },
                FormPatch { part: PartPath("torso".into()), lever: Lever::Muscle, value: 0.5 + k },
            ],
        };
        let (x, z) = ((i % side).to_f32(), (i / side).to_f32());
        let offset = (side.to_f32() - 1.0) * 0.5 * 2.5;
        commands.spawn((
            Mob::new(body.clone()),
            form,
            MobSeed(usize_to_u64(i)),
            Index(i),
            Transform::from_xyz(f32::mul_add(x, 2.5, -offset), 0.0, f32::mul_add(z, 2.5, -offset))
                .with_rotation(Quat::from_rotation_y(i.to_f32() * 0.7)),
        ));
    }
    info!("controls: left-drag orbit, scroll zoom, S sever tail, H graft horns, L stretch necks, Q quality");
}

fn on_ready(ev: On<BodyReady>, mobs: Query<&Index>) {
    if let Ok(i) = mobs.get(ev.entity)
        && i.0 == 0 {
            info!("first body ready");
        }
}

fn controls(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut profile: ResMut<QualityProfile>,
    bodies: Res<Assets<GeneratedBody>>,
    mut mobs: Query<(Entity, &Index, &MobBody, &mut BoneScales)>,
    mut quality: Local<usize>,
    mut frame: Local<u32>,
) {
    // `ORNISCULPT_DEMO=1` presses S, H and L once bodies have had time to generate.
    *frame += 1;
    let demo = std::env::var("ORNISCULPT_DEMO").is_ok() && *frame == 150;
    let pressed = |k: KeyCode| keys.just_pressed(k) || demo;
    if pressed(KeyCode::KeyS) {
        for (e, i, ..) in &mobs {
            if i.0 % 3 == 0 {
                commands.trigger(SeverPart { entity: e, path: PartPath("torso/tail".into()) });
            }
        }
        info!("severed tails on every third mob");
    }
    if pressed(KeyCode::KeyH) {
        let horn = PartNode {
            name: "horn".into(),
            kind: PartKind::Hard(HardParams { kind: HardKind::Horn, size: 0.14, curve: 0.6, twist: 0.3, ..default() }),
            attach: AttachSlot::Horn,
            offset: default(),
            mirror: true,
            repeat: default(),
            side: default(),
            detail: default(),
            tone: default(),
            stamps: default(),
            density: 1.0,
            children: vec![],
        };
        for (e, i, ..) in &mobs {
            if i.0 % 3 == 1 {
                commands.trigger(GraftPart { entity: e, parent: PartPath("torso/neck/head".into()), node: horn.clone() });
            }
        }
        info!("grafted horns on every third mob");
    }
    if pressed(KeyCode::KeyL) {
        for (_, i, body, mut scales) in &mut mobs {
            if i.0 % 3 != 2 {
                continue;
            }
            let Some(body) = bodies.get(&body.0) else { continue };
            let sk = &body.anatomy.skeleton;
            scales.0 = if scales.0.is_empty() {
                (0..3).filter_map(|n| sk.find(&format!("neck.{n}"))).map(|bone| BoneScale { bone, length: 1.5, girth: 0.85 }).collect()
            } else {
                default()
            };
        }
        info!("toggled long necks on every third mob");
    }
    cycle_quality(&keys, &mut profile, &mut quality);
}

fn report(
    time: Res<Time>,
    mut next: Local<f32>,
    cache: Res<GenCache>,
    tiers: Query<&MobTier>,
    mobs: Query<(), With<Mob>>,
    parts: Query<(), With<Part>>,
) {
    if time.elapsed_secs() < *next {
        return;
    }
    *next = time.elapsed_secs() + 2.0;
    let counts = tier_counts(&tiers);
    info!(
        "{} mobs, {} part entities | {} bodies cached, {} generating | tiers hero/near/far = {:?}",
        mobs.iter().count(),
        parts.iter().count(),
        cache.bodies.len(),
        cache.in_flight(),
        counts
    );
}
