//! Motion systems: binding, root movement, then each pose stage (see [`stages`](super::stages))
//! on every mob in parallel, then the joints.

use std::f32::consts::{PI, TAU};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::heft::Heft;
use super::stages::{self, Frame, leg_length};
use super::state::{ActiveStep, CyclePose, Feet, GaitClock, GaitOverride, Locomotion, LookState, MobPose, SpringState, TorsoState, MotionTime};
use super::{DefaultGaits, ExternalRoot, FootGround, FootProbes, Footstep, Gait, MobGait, MobIntent};
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

/// Rays down from above each foot's rest spot, for the game's ground probe.
pub(super) fn write_probes(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, &mut FootProbes), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(tf, body, mut probes)| {
        let Some(f) = assets.frame(body, None, tf) else { return };
        probes.0.clear();
        probes.0.extend(f.a.legs.iter().map(|l| f.root.transform_point3(l.rest_foot) + Vec3::Y * f.leg));
    });
}

/// Starts each hero pose from rest, keeping the joints' scales (set by levers).
pub(super) fn rest_poses(
    assets: MotionAssets,
    mut mobs: Query<(&MobBody, &MobSkeleton, &mut MobPose), With<MobSkeleton>>,
    joints: Query<&Transform, With<MobJoint>>,
) {
    mobs.par_iter_mut().for_each(|(body, skeleton, mut pose)| {
        let Some(b) = assets.bodies.get(&body.0) else { return };
        let scales = skeleton.0.iter().map(|e| joints.get(*e).map_or(Vec3::ONE, |t| t.scale));
        stages::rest_pose(&b.anatomy, scales, &mut pose);
    });
}

pub(super) fn fold_wings(assets: MotionAssets, mut mobs: Query<(&MobBody, &CyclePose, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(body, cycles, mut pose)| {
        let Some(b) = assets.bodies.get(&body.0) else { return };
        stages::fold_wings(&b.anatomy, cycles.0.as_ref(), &mut pose);
    });
}

/// Feet plant and swing; a foot touching down triggers [`Footstep`].
#[allow(clippy::type_complexity)]
pub(super) fn plant_feet(
    assets: MotionAssets,
    mut mobs: Query<(Entity, &Transform, &MobBody, Option<&MobGait>, &Locomotion, &GaitClock, &ActiveStep, Option<&FootGround>, &mut Feet), With<MobSkeleton>>,
    commands: ParallelCommands,
) {
    mobs.par_iter_mut().for_each(|(entity, tf, body, gait, loco, clock, step, ground, mut feet)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        let hits = ground.map_or(&[][..], |g| &g.0[..]);
        let landed = stages::plant_feet(&f, loco, clock, step, hits, &mut feet);
        if !landed.is_empty() {
            commands.command_scope(|mut commands| {
                for leg in landed {
                    if let Some(foot) = feet.0.get(leg) {
                        commands.trigger(Footstep { entity, leg, position: foot.position });
                    }
                }
            });
        }
    });
}

#[allow(clippy::type_complexity)]
pub(super) fn bob_and_bend(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &Locomotion, &GaitClock, &ActiveStep, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, loco, clock, step, mut pose)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::bob_and_bend(&f, loco, clock, step, &mut pose);
    });
}

#[allow(clippy::type_complexity)]
pub(super) fn ride_torso(
    assets: MotionAssets,
    mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &Locomotion, &GaitClock, &ActiveStep, &Heft, &mut TorsoState, &mut MobPose), With<MobSkeleton>>,
) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, loco, clock, step, heft, mut torso, mut pose)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::ride_torso(&f, loco, clock, step, heft, &mut torso, &mut pose);
    });
}

#[allow(clippy::type_complexity)]
pub(super) fn swing_springs(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &Locomotion, &mut SpringState, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, loco, mut springs, mut pose)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::swing_springs(&f, loco, &mut springs, &mut pose);
    });
}

pub(super) fn apply_cycles(assets: MotionAssets, mut mobs: Query<(&MobBody, &CyclePose, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(body, cycles, mut pose)| {
        let (Some(b), Some(c)) = (assets.bodies.get(&body.0), &cycles.0) else { return };
        stages::apply_cycles(&b.anatomy, c, &mut pose);
    });
}

#[allow(clippy::type_complexity)]
pub(super) fn solve_legs(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &Feet, &CyclePose, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, feet, cycles, mut pose)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::solve_legs(&f, feet, cycles.0.as_ref(), &mut pose);
    });
}

#[allow(clippy::type_complexity)]
pub(super) fn look_at(assets: MotionAssets, mut mobs: Query<(&Transform, &MobBody, Option<&MobGait>, &MobIntent, &mut LookState, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(tf, body, gait, intent, mut look, mut pose)| {
        let Some(f) = assets.frame(body, gait, tf) else { return };
        stages::look_at(&f, intent.look_at, &mut look, &mut pose);
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

pub(super) fn shoulder_rhythm(assets: MotionAssets, mut mobs: Query<(&MobBody, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(body, mut pose)| {
        let Some(b) = assets.bodies.get(&body.0) else { return };
        stages::shoulder_rhythm(&b.anatomy, &mut pose);
    });
}

pub(super) fn pose_hands(assets: MotionAssets, mut mobs: Query<(&MobBody, &super::HandPose, &mut MobPose), With<MobSkeleton>>) {
    mobs.par_iter_mut().for_each(|(body, hands, mut pose)| {
        let Some(b) = assets.bodies.get(&body.0) else { return };
        super::hand::pose_hands(&b.anatomy, hands, &mut pose);
    });
}
