//! Motion systems: binding, root movement, then the whole pose (see [`stages`](super::stages))
//! on every mob in parallel, then the joints.

use std::f32::consts::{PI, TAU};

use bevy::ecs::query::QueryData;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::heft::Heft;
use super::stages::{self, Frame, PoseInput, PoseState, leg_length};
use super::state::{ActiveStep, CyclePose, Feet, GaitClock, GaitOverride, Locomotion, LookState, MobPose, SpringState, TorsoState, MotionTime};
use super::{DefaultGaits, ExternalRoot, FootGround, FootProbes, Footstep, Gait, HandPose, MobGait, MobIntent};
use crate::anatomy::Anatomy;
use crate::mob::{BodyReady, GeneratedBody, MobBody, MobJoint, MobSkeleton};

pub fn gait_for<'a>(handle: Option<&MobGait>, gaits: &'a Assets<Gait>, defaults: &'a DefaultGaits, a: &Anatomy) -> &'a Gait {
    handle.and_then(|h| gaits.get(&h.0)).unwrap_or_else(|| defaults.for_legs(a.legs.len()))
}

/// The assets and clock every motion system reads.
#[derive(SystemParam)]
pub(super) struct MotionAssets<'w> {
    time: Res<'w, MotionTime>,
    bodies: Res<'w, Assets<GeneratedBody>>,
    gaits: Res<'w, Assets<Gait>>,
    defaults: Res<'w, DefaultGaits>,
}

impl MotionAssets<'_> {
    /// This frame's context for a mob, once its body is loaded.
    fn frame(&self, body: &MobBody, gait: Option<&MobGait>, tf: &Transform) -> Option<Frame<'_>> {
        let a = &self.bodies.get(&body.0)?.anatomy;
        Some(Frame {
            a,
            gait: gait_for(gait, &self.gaits, &self.defaults, a),
            root: tf.compute_affine(),
            dt: self.time.dt,
            time: self.time.elapsed,
            leg: leg_length(a),
        })
    }
}

fn yaw_of(q: Quat) -> f32 {
    let f = q * Vec3::Z;
    f.x.atan2(f.z)
}

fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

/// The turn rate the body bends and leans with, eased so it doesn't flicker frame to frame.
fn ease_turn(loco: &mut Locomotion, rate: f32, dt: f32) {
    loco.turn_rate = (rate - loco.turn_rate).mul_add(1.0 - (-12.0 * dt).exp(), loco.turn_rate);
}

/// A (re)generated body: fresh feet, springs and torso, and its weight.
#[allow(clippy::type_complexity)]
pub(super) fn bind_body(
    ready: On<BodyReady>,
    bodies: Res<Assets<GeneratedBody>>,
    mut mobs: Query<(&Transform, &MobBody, &mut Feet, &mut SpringState, &mut LookState, &mut TorsoState, &mut GaitClock, &mut Heft)>,
) {
    let Ok((tf, body, mut feet, mut springs, mut look, mut torso, mut clock, mut heft)) = mobs.get_mut(ready.entity) else { return };
    let Some(b) = bodies.get(&body.0) else { return };
    stages::bind(&b.anatomy, tf.compute_affine(), &mut feet, &mut springs, &mut look, &mut torso, &mut clock);
    *heft = Heft::of(&b.anatomy);
}

/// Moves mob roots toward their intent (or reads externally driven roots).
#[allow(clippy::type_complexity)]
pub(super) fn move_roots(
    assets: MotionAssets,
    mut mobs: Query<(&mut Transform, &MobIntent, &mut Locomotion, &Heft, &MobBody, Option<&MobGait>, Has<ExternalRoot>)>,
) {
    let dt = assets.time.dt;
    mobs.par_iter_mut().for_each(|(mut tf, intent, mut loco, heft, body, gait, external)| {
        let Some(f) = assets.frame(body, gait, &tf) else { return };
        let (leg, gait) = (f.leg, f.gait);
        let agility = gait.heft.agility.sample(heft.heaviness);
        loco.prev_velocity = loco.velocity;
        if external {
            if let Some((pos, rot)) = loco.last_root {
                loco.velocity = (tf.translation - pos) / dt;
                ease_turn(&mut loco, wrap(yaw_of(tf.rotation) - yaw_of(rot)) / dt, dt);
            }
        } else {
            let want = Vec3::new(intent.velocity.x, 0.0, intent.velocity.z);
            let dv = (want - loco.velocity).clamp_length_max(gait.accel * agility * leg * dt);
            loco.velocity += dv;
            tf.translation += loco.velocity * dt;

            let facing = intent.facing.map(|d| d.as_vec3()).or_else(|| (loco.velocity.length() > 0.05 * leg).then_some(loco.velocity));
            let turned = if let Some(f) = facing {
                let delta = wrap(f.x.atan2(f.z) - yaw_of(tf.rotation));
                let max = gait.turn_rate_deg.to_radians() * agility * dt;
                let step = delta.clamp(-max, max);
                tf.rotation = Quat::from_rotation_y(step) * tf.rotation;
                step / dt
            } else {
                0.0
            };
            ease_turn(&mut loco, turned, dt);
        }
        loco.last_root = Some((tf.translation, tf.rotation));
    });
}

/// Every tier: the gait mode for the speed and the step it takes.
#[allow(clippy::type_complexity)]
pub(super) fn choose_steps(
    assets: MotionAssets,
    mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &Locomotion, &Heft, &GaitOverride, &mut GaitClock, &mut ActiveStep)>,
) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, loco, heft, cycle, mut clock, mut step)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::choose_step(&f, loco, heft, cycle, &mut clock, &mut step);
    });
}

/// Every tier: advance the gait phase. Hero mobs' feet keep stepping until they catch up.
#[allow(clippy::type_complexity)]
pub(super) fn advance_clocks(
    assets: MotionAssets,
    mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &Locomotion, &Heft, &ActiveStep, &Feet, Has<MobSkeleton>, &mut GaitClock)>,
) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, loco, heft, step, feet, hero, mut clock)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::advance_clock(&f, loco, heft, step, hero.then_some(feet), &mut clock);
    });
}

/// Rays down from above each foot's rest spot, for the game's ground probe: hero mobs, and crowd
/// mobs the game gave a [`FootGround`] (they sit on the hits through their
/// [`CrowdGround`](super::CrowdGround)).
#[allow(clippy::type_complexity)]
pub(super) fn write_probes(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, &mut FootProbes), Or<(With<MobSkeleton>, With<FootGround>)>>) {
    mobs.par_iter_mut().for_each(|(tf, body, mut probes)| {
        let Some(f) = assets.frame(body, None, tf) else { return };
        probes.0.clear();
        probes.0.extend(f.a.legs.iter().map(|l| f.root.transform_point3(l.rest_foot) + Vec3::Y * f.leg));
    });
}

/// One hero mob, as [`pose_heroes`] reads and writes it.
#[derive(QueryData)]
#[query_data(mutable)]
pub(super) struct HeroPose {
    entity: Entity,
    tf: &'static Transform,
    body: &'static MobBody,
    skeleton: &'static MobSkeleton,
    gait: Option<&'static MobGait>,
    intent: &'static MobIntent,
    loco: &'static Locomotion,
    clock: &'static GaitClock,
    step: &'static ActiveStep,
    heft: &'static Heft,
    cycles: &'static CyclePose,
    hands: Option<&'static HandPose>,
    ground: Option<&'static FootGround>,
    feet: &'static mut Feet,
    springs: &'static mut SpringState,
    look: &'static mut LookState,
    torso: &'static mut TorsoState,
    pose: &'static mut MobPose,
}

/// Every hero mob's pose, all stages ([`stages::pose_frame`]), in parallel. The pose starts from
/// rest with the joints' scales (set by levers); a foot touching down triggers [`Footstep`].
pub(super) fn pose_heroes(assets: MotionAssets, mut mobs: Query<HeroPose>, joints: Query<&Transform, With<MobJoint>>, commands: ParallelCommands) {
    mobs.par_iter_mut().for_each(|mut m| {
        let Some(f) = assets.frame(m.body, m.gait, m.tf) else { return };
        let scales = m.skeleton.0.iter().map(|e| joints.get(*e).map_or(Vec3::ONE, |t| t.scale));
        let input = PoseInput {
            loco: m.loco,
            clock: m.clock,
            step: m.step,
            heft: m.heft,
            cycles: m.cycles.0.as_ref(),
            hands: m.hands,
            ground: m.ground.map_or(&[][..], |g| &g.0[..]),
            look_at: m.intent.look_at,
        };
        let state = PoseState { feet: &mut m.feet, springs: &mut m.springs, look: &mut m.look, torso: &mut m.torso, pose: &mut m.pose };
        let landed = stages::pose_frame(&f, &input, scales, state);
        if !landed.is_empty() {
            commands.command_scope(|mut commands| {
                for leg in landed {
                    if let Some(foot) = m.feet.0.get(leg) {
                        commands.trigger(Footstep { entity: m.entity, leg, position: foot.position });
                    }
                }
            });
        }
    });
}

/// Copies each mob's pose onto its joint entities.
pub(super) fn write_joints(mut joints: Query<(&MobJoint, &mut Transform)>, poses: Query<&MobPose>) {
    joints.par_iter_mut().for_each(|(joint, mut tf)| {
        let Some(l) = poses.get(joint.mob).ok().and_then(|p| p.local.get(joint.bone.index())) else { return };
        if tf.translation != l.translation || tf.rotation != l.rotation {
            tf.translation = l.translation;
            tf.rotation = l.rotation;
        }
    });
}

/// Steps every hero mob's soft-tissue springs on its final pose.
#[cfg(feature = "soft-tissue")]
pub(super) fn soft_tissue(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, &MobPose, &Feet, &mut crate::soft::SoftState), With<MobSkeleton>>) {
    let dt = assets.time.dt;
    mobs.par_iter_mut().for_each(|(tf, body, pose, feet, mut soft)| {
        let Some(b) = assets.bodies.get(&body.0) else { return };
        crate::soft::step(&b.anatomy, &b.soft.hero, tf.compute_affine(), &pose.local, &feet.planted(), dt, &mut soft);
    });
}

/// Moves each soft joint by its region's offset (in its bone's frame).
#[cfg(feature = "soft-tissue")]
pub(super) fn write_soft_joints(mut joints: Query<(&crate::mob::SoftJoint, &mut Transform)>, mobs: Query<&crate::soft::SoftState>) {
    joints.par_iter_mut().for_each(|(joint, mut tf)| {
        let Some(&offset) = mobs.get(joint.mob).ok().and_then(|s| s.local.get(joint.region)) else { return };
        if tf.translation != offset {
            tf.translation = offset;
        }
    });
}
