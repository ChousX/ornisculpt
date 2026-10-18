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

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{
    AttachSlot, Form, FormPatch, HardKind, HardParams, Lever, PartKind, PartNode, PartPath,
};
use ornisculpt::mob::{
    BodyReady, BoneScale, BoneScales, GenCache, GeneratedBody, GraftPart, Mob, MobBody, MobSeed, MobTier, Part,
    QualityProfile, SeverPart, TierCamera,
};

fn main() {
    let count: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(64);
    App::new()
        .add_plugins((DefaultPlugins, OrnisculptPlugin))
        .insert_resource(Count(count))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .init_resource::<Orbit>()
        .add_observer(on_ready)
        .add_systems(Startup, setup)
        .add_systems(Update, (controls, orbit_camera, report, screenshot))
        .run();
}

#[derive(Resource)]
struct Count(usize);

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
            .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_default();
        match v[..] {
            [yaw, pitch, distance, x, y, z] => Self { yaw, pitch, distance, target: Vec3::new(x, y, z) },
            _ => Self { yaw: 0.6, pitch: 0.45, distance: 12.0, target: Vec3::ZERO },
        }
    }
}

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
    commands.spawn((
        DirectionalLight { shadow_maps_enabled: true, illuminance: 8000.0, ..default() },
        Transform::from_xyz(3.0, 6.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(200.0, 200.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color: Color::srgb(0.32, 0.36, 0.3), perceptual_roughness: 1.0, ..default() })),
    ));

    let body = assets.load("mobs/quadruped.body.ron");
    let side = (count.0 as f32).sqrt().ceil() as usize;
    for i in 0..count.0 {
        // A handful of distinct builds, so the cache shares bodies between mobs.
        let variant = i % 4;
        let k = variant as f32 / 3.0;
        let form = Form {
            overall_scale: 0.85 + 0.3 * k,
            patches: vec![
                FormPatch { part: PartPath("torso/limb.front".into()), lever: Lever::Length, value: 0.9 + 0.3 * k },
                FormPatch { part: PartPath("torso/limb.hind".into()), lever: Lever::Length, value: 0.9 + 0.3 * k },
                FormPatch { part: PartPath("torso".into()), lever: Lever::Girth, value: 1.2 - 0.3 * k },
                FormPatch { part: PartPath("torso".into()), lever: Lever::Muscle, value: 0.5 + k },
            ],
        };
        let (x, z) = ((i % side) as f32, (i / side) as f32);
        let offset = (side as f32 - 1.0) * 0.5 * 2.5;
        commands.spawn((
            Mob::new(body.clone()),
            form,
            MobSeed(i as u64),
            Index(i),
            Transform::from_xyz(x * 2.5 - offset, 0.0, z * 2.5 - offset)
                .with_rotation(Quat::from_rotation_y(i as f32 * 0.7)),
        ));
    }
    info!("controls: left-drag orbit, scroll zoom, S sever tail, H graft horns, L stretch necks, Q quality");
}

fn on_ready(ev: On<BodyReady>, mobs: Query<&Index>) {
    if let Ok(i) = mobs.get(ev.entity) {
        if i.0 == 0 {
            info!("first body ready");
        }
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
            kind: PartKind::Hard(HardParams { kind: HardKind::Horn, size: 0.14, curve: 0.6, twist: 0.3 }),
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
    if keys.just_pressed(KeyCode::KeyQ) {
        *quality = (*quality + 1) % 3;
        *profile = [QualityProfile::MEDIUM, QualityProfile::HIGH, QualityProfile::LOW][*quality].clone();
        info!("quality: {}", ["medium", "high", "low"][*quality]);
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
        orbit.yaw -= motion.delta.x * 0.008;
        orbit.pitch = (orbit.pitch + motion.delta.y * 0.008).clamp(-1.4, 1.4);
    }
    orbit.distance = (orbit.distance * (1.0 - scroll.delta.y * 0.1)).clamp(0.5, 200.0);
    let rot = Quat::from_euler(EulerRot::YXZ, orbit.yaw, -orbit.pitch, 0.0);
    for mut t in &mut camera {
        *t = Transform::from_translation(orbit.target + rot * Vec3::Z * orbit.distance).looking_at(orbit.target, Vec3::Y);
    }
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
    let mut counts = [0; 3];
    for t in &tiers {
        counts[t.0 as usize] += 1;
    }
    info!(
        "{} mobs, {} part entities | {} bodies cached, {} generating | tiers hero/near/far = {:?}",
        mobs.iter().count(),
        parts.iter().count(),
        cache.bodies.len(),
        cache.in_flight(),
        counts
    );
}

fn screenshot(mut commands: Commands, mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT") else { return };
    *frame += 1;
    if *frame == 300 {
        commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
    }
    if *frame == 320 {
        exit.write(AppExit::Success);
    }
}
