//! Held tools: every mob carries what its sockets allow (spear and club in grips, a pack on the
//! back, a pouch on the hip, a box in the saddle) while wandering, at hero and crowd tiers.
//!
//! ```text
//! cargo run --release --example hold [count]
//! ```
//!
//! Controls: left-drag orbit · scroll zoom · H drop / pick up everything · 1/2/3 force every mob
//! to hero/near/far tier · 0 automatic tiers.
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few seconds and exits;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera; `ORNISCULPT_TIER=hero|near|far`
//! forces a tier from the start; `ORNISCULPT_STILL=1` keeps the mobs standing. Frame time is
//! logged every second.

use bevy::prelude::*;
use bevy::window::PresentMode;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{Side, SocketKind};
use ornisculpt::coat::MobCoat;
use ornisculpt::equip::{HeldBy, Holding, SocketSel};
use ornisculpt::mob::{BodyReady, ForceTier, GeneratedBody, Mob, MobBody, TierCamera};
use ornisculpt::motion::MobIntent;
use ornisculpt::num::{Saturate, ToF32};

mod common;
use common::{Orbit, TIER_KEYS, default_plugins, orbit_camera, screenshot_at, spawn_stage, tier_env};

/// (body plan, coat).
const BODIES: [(&str, &str); 4] = [
    ("mobs/man.body.ron", "coats/skin_fair.coat.ron"),
    ("mobs/horse.body.ron", "coats/bay.coat.ron"),
    ("mobs/woman.body.ron", "coats/skin_woman.coat.ron"),
    ("mobs/spider.body.ron", "coats/wolf_spider.coat.ron"),
];

fn main() -> AppExit {
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(16);
    App::new()
        .add_plugins((
            default_plugins(Window { present_mode: PresentMode::AutoNoVsync, ..default() }),
            OrnisculptPlugin,
            bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
            bevy::diagnostic::LogDiagnosticsPlugin::default(),
        ))
        .insert_resource(GlobalAmbientLight { brightness: 450.0, ..default() })
        .insert_resource(Count(count))
        .insert_resource(Orbit::from_env(0.5, 0.35, 8.0, Vec3::new(0.0, 0.6, 0.0), (0.5, 200.0)))
        .add_systems(Startup, setup)
        .add_systems(Update, (wander, keys, orbit_camera, screenshot_at(360)))
        .add_observer(equip)
        .add_observer(common::exit_on_body_failed)
        .run()
}

#[derive(Resource)]
struct Count(usize);

#[derive(Resource)]
struct Kit {
    spear: (Handle<Mesh>, Handle<StandardMaterial>),
    club: (Handle<Mesh>, Handle<StandardMaterial>),
    pack: (Handle<Mesh>, Handle<StandardMaterial>),
    pouch: (Handle<Mesh>, Handle<StandardMaterial>),
}

#[derive(Component)]
struct Wanderer {
    centre: Vec3,
    seed: f32,
}

/// Remembers where a dropped tool came from.
#[derive(Component, Clone)]
struct Owned(HeldBy);

fn setup(
    mut commands: Commands,
    count: Res<Count>,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((Camera3d::default(), TierCamera));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 200.0, Vec3::new(3.0, 6.0, 2.0));
    let mut mat = |r, g, b, rough| materials.add(StandardMaterial { base_color: Color::srgb(r, g, b), perceptual_roughness: rough, ..default() });
    let (wood, metal, leather) = (mat(0.45, 0.3, 0.17, 0.8), mat(0.7, 0.72, 0.75, 0.35), mat(0.35, 0.22, 0.12, 0.9));
    // Tools are authored with their main axis along +Y and the grip at the origin.
    let spear = Mesh::from(Cylinder::new(0.012, 1.6)).translated_by(Vec3::Y * 0.5);
    let club = Mesh::from(Cone { radius: 0.045, height: 0.55 })
        .rotated_by(Quat::from_rotation_x(std::f32::consts::PI))
        .translated_by(Vec3::Y * 0.22);
    let pack = Mesh::from(Cuboid::new(0.24, 0.1, 0.3)).translated_by(Vec3::Y * 0.05);
    let pouch = Mesh::from(Cuboid::new(0.05, 0.08, 0.1)).translated_by(Vec3::Y * 0.04);
    commands.insert_resource(Kit {
        spear: (meshes.add(spear), metal),
        club: (meshes.add(club), wood),
        pack: (meshes.add(pack), leather.clone()),
        pouch: (meshes.add(pouch), leather),
    });

    let forced = tier_env("ORNISCULPT_TIER");
    let side = count.0.to_f32().sqrt().ceil().sat_usize();
    for (i, (path, coat)) in BODIES.iter().cycle().take(count.0).enumerate() {
        let centre = Vec3::new((i % side).to_f32() * 3.5, 0.0, (i / side).to_f32() * 3.5) - Vec3::new(side.to_f32() * 1.75, 0.0, side.to_f32() * 1.75);
        let mut mob = commands.spawn((
            Mob::new(assets.load(*path)),
            MobCoat(assets.load(*coat)),
            Wanderer { centre, seed: i.to_f32() * 1.7 },
            Transform::from_translation(centre),
        ));
        if let Some(tier) = forced {
            mob.insert(ForceTier(tier));
        }
    }
    info!("controls: left-drag orbit, scroll zoom, H drop/pick up, 1/2/3 force tier, 0 auto tiers");
}

/// Hands each mob the tools its sockets take, once its first body is ready.
fn equip(
    ev: On<BodyReady>,
    mut commands: Commands,
    kit: Res<Kit>,
    mobs: Query<(&MobBody, Has<Holding>)>,
    bodies: Res<Assets<GeneratedBody>>,
) {
    let Ok((body, holding)) = mobs.get(ev.entity) else { return };
    let Some(body) = bodies.get(&body.0) else { return };
    if holding {
        return;
    }
    let sockets = &body.anatomy.sockets;
    let has = |kind, side| sockets.iter().any(|s| s.kind == kind && s.side == side);
    let mob = ev.entity;
    let mut give = |tool: &(Handle<Mesh>, Handle<StandardMaterial>), socket: SocketSel, grip: Transform| {
        let held = HeldBy::new(mob, socket).with_grip(grip);
        commands.spawn((Mesh3d(tool.0.clone()), MeshMaterial3d(tool.1.clone()), Transform::default(), Owned(held.clone()), held));
    };
    if has(SocketKind::Grip, Side::Right) {
        // Held a third of the way up the shaft, point forward and up.
        give(&kit.spear, SocketSel::grip(Side::Right), Transform::from_xyz(0.0, 0.0, 0.0).with_rotation(Quat::from_rotation_x(0.6)));
    }
    if has(SocketKind::Grip, Side::Left) {
        give(&kit.club, SocketSel::grip(Side::Left), Transform::IDENTITY);
    }
    if has(SocketKind::Grip, Side::Left) && has(SocketKind::Back, Side::Center) {
        give(&kit.pack, SocketSel::Kind(SocketKind::Back, Side::Center), Transform::IDENTITY);
    } else if has(SocketKind::Saddle, Side::Center) {
        give(&kit.pack, SocketSel::Kind(SocketKind::Saddle, Side::Center), Transform::IDENTITY);
    }
    if has(SocketKind::Hip, Side::Left) {
        give(&kit.pouch, SocketSel::Kind(SocketKind::Hip, Side::Left), Transform::IDENTITY);
    }
}

fn wander(time: Res<Time>, mut mobs: Query<(&Wanderer, &Transform, &mut MobIntent)>) {
    if std::env::var("ORNISCULPT_STILL").is_ok() {
        return;
    }
    let t = time.elapsed_secs();
    for (w, tf, mut intent) in &mut mobs {
        let to_centre = w.centre - tf.translation;
        let tangent = Vec3::new(-to_centre.z, 0.0, to_centre.x).normalize_or_zero();
        let pull = to_centre.normalize_or_zero() * (to_centre.length() - 0.8);
        let pause = f32::from(u8::from(f32::mul_add(t, 0.2, w.seed).sin() > 0.8));
        intent.velocity = (tangent * 0.7 + pull) * (1.0 - pause);
    }
}

fn keys(
    mut commands: Commands,
    input: Res<ButtonInput<KeyCode>>,
    mut dropped: Local<bool>,
    tools: Query<(Entity, &Owned)>,
    mobs: Query<Entity, With<Mob>>,
) {
    if input.just_pressed(KeyCode::KeyH) {
        *dropped = !*dropped;
        for (tool, owned) in &tools {
            if *dropped {
                commands.entity(tool).remove::<HeldBy>();
            } else {
                commands.entity(tool).insert(owned.0.clone());
            }
        }
        info!("{}", if *dropped { "dropped" } else { "picked up" });
    }
    for (key, tier) in TIER_KEYS {
        if input.just_pressed(key) {
            for mob in &mobs {
                commands.entity(mob).insert(ForceTier(tier));
            }
            info!("forced {tier:?}");
        }
    }
    if input.just_pressed(KeyCode::Digit0) {
        for mob in &mobs {
            commands.entity(mob).remove::<ForceTier>();
        }
        info!("automatic tiers");
    }
}
