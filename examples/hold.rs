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

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::window::PresentMode;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{LodLevel, Side, SocketKind};
use ornisculpt::equip::{HeldBy, Holding, SocketSel};
use ornisculpt::mob::{BodyReady, ForceTier, GeneratedBody, Mob, MobBody, TierCamera};
use ornisculpt::motion::MobIntent;
use ornisculpt::num::{Saturate, ToF32};

const BODIES: [&str; 4] = ["mobs/biped.body.ron", "mobs/quadruped.body.ron", "mobs/biped.body.ron", "mobs/hexapod.body.ron"];

fn main() {
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(16);
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window { present_mode: PresentMode::AutoNoVsync, ..default() }),
                ..default()
            }),
            OrnisculptPlugin,
            bevy::diagnostic::FrameTimeDiagnosticsPlugin::default(),
            bevy::diagnostic::LogDiagnosticsPlugin::default(),
        ))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .insert_resource(Count(count))
        .init_resource::<Orbit>()
        .add_systems(Startup, setup)
        .add_systems(Update, (wander, keys, orbit_camera, screenshot))
        .add_observer(equip)
        .run();
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
            _ => Self { yaw: 0.5, pitch: 0.35, distance: 8.0, target: Vec3::new(0.0, 0.6, 0.0) },
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
        Mesh3d(meshes.add(Plane3d::default().mesh().size(200.0, 200.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));
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

    let forced = match std::env::var("ORNISCULPT_TIER").as_deref() {
        Ok("hero") => Some(LodLevel::Hero),
        Ok("near") => Some(LodLevel::Near),
        Ok("far") => Some(LodLevel::Far),
        _ => None,
    };
    let side = count.0.to_f32().sqrt().ceil().sat_usize();
    for (i, path) in BODIES.iter().cycle().take(count.0).enumerate() {
        let centre = Vec3::new((i % side).to_f32() * 2.5, 0.0, (i / side).to_f32() * 2.5) - Vec3::new(side.to_f32() * 1.25, 0.0, side.to_f32() * 1.25);
        let mut mob = commands.spawn((
            Mob::new(assets.load(*path)),
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
    for (key, tier) in [(KeyCode::Digit1, LodLevel::Hero), (KeyCode::Digit2, LodLevel::Near), (KeyCode::Digit3, LodLevel::Far)] {
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
    orbit.distance = (orbit.distance * scroll.delta.y.mul_add(-0.1, 1.0)).clamp(0.5, 200.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
}

fn screenshot(mut commands: Commands, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    *frame += 1;
    if *frame == 360 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frame == 380 {
        exit.write(AppExit::Success);
    }
}
