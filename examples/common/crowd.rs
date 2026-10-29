//! The crowd workload: `examples/crowd.rs` shows it, `examples/bench.rs` measures it (a change
//! here is a change to the benchmark: bump its `BENCH_VERSION`).

use bevy::prelude::*;
use ornisculpt::anatomy::{Form, FormPatch, Lever, PartPath};
use ornisculpt::mob::{Mob, MobSeed};
use ornisculpt::motion::MobIntent;
use ornisculpt::num::{ToF32, usize_to_u64};

pub const BODIES: [&str; 4] = ["mobs/man.body.ron", "mobs/woman.body.ron", "mobs/ox.body.ron", "mobs/spider.body.ron"];
/// Each body's root part (the trunk the girth patch widens), in `BODIES` order.
pub const ROOTS: [&str; 4] = ["torso", "torso", "torso", "prosoma"];
/// Each body's own coat, in `BODIES` order (see [`coat`]).
pub const COATS: [&str; 4] = ["coats/skin_fair.coat.ron", "coats/skin_woman.coat.ron", "coats/hereford.coat.ron", "coats/wolf_spider.coat.ron"];
const SPACING: f32 = 2.2;

/// Walks a circle.
#[derive(Component)]
pub struct Wander {
    pub centre: Vec3,
    pub radius: f32,
    pub speed: f32,
    pub offset: f32,
}

/// Spawns crowd mob `i` of a square of `side` by `side`: body plan `path`, in one of three builds
/// per plan (so the cache shares bodies), wandering round its own spot.
pub fn spawn<'a>(commands: &'a mut Commands, assets: &AssetServer, i: usize, side: usize, (path, root): (&'static str, &str)) -> EntityCommands<'a> {
    let build = (i / BODIES.len()) % 3;
    let form = Form {
        overall_scale: 0.15f32.mul_add(build.to_f32(), 0.85),
        patches: vec![FormPatch { part: PartPath(root.into()), lever: Lever::Girth, value: 0.15f32.mul_add(build.to_f32(), 0.9) }],
    };
    let (x, z) = ((i % side).to_f32(), (i / side).to_f32());
    let centre = Vec3::new(side.to_f32().mul_add(-0.5, x) * SPACING, 0.0, side.to_f32().mul_add(-0.5, z) * SPACING);
    commands.spawn((
        Mob::new(assets.load(path)),
        form,
        MobSeed(usize_to_u64(i)),
        Wander { centre, radius: 0.6, speed: (i % 7).to_f32().mul_add(0.12, 0.5), offset: i.to_f32() * 1.7 },
        Transform::from_translation(centre),
    ))
}

/// Mob `i`'s coat in the mixed crowd: its body's own, on all but every fourth build (those stay
/// plain, so coated and bare bodies share the scene).
pub fn coat(i: usize) -> Option<&'static str> {
    COATS.get(i % BODIES.len()).copied().filter(|_| (i / BODIES.len()) % 4 != 3)
}

pub fn wander(time: Res<Time>, mut mobs: Query<(&Wander, &Transform, &mut MobIntent)>) {
    let t = time.elapsed_secs();
    mobs.par_iter_mut().for_each(|(w, tf, mut intent)| {
        let a = f32::mul_add(t * w.speed / w.radius.max(0.1), 0.5, w.offset);
        let target = w.centre + Vec3::new(a.cos(), 0.0, a.sin()) * w.radius;
        let pause = f32::from(u8::from(f32::mul_add(t, 0.15, w.offset).sin() > 0.8));
        intent.velocity = ((target - tf.translation) * 1.5).clamp_length_max(w.speed) * (1.0 - pause);
    });
}
