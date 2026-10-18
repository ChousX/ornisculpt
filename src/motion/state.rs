//! Motion state, one concern per component. Every [`Mob`](crate::mob::Mob) gets the shared ones;
//! the pose buffers ([`MobPose`], [`CyclePose`]) come with the hero tier's skeleton.

use bevy::math::Affine3A;
use bevy::prelude::*;
use smallvec::SmallVec;

use super::eval::LayerPose;
use super::gait::{BodyMotion, PhasePattern};
use super::heft::StepShape;
use super::FootPlan;
use crate::anatomy::LegInfo;

/// How the mob's root is moving.
#[derive(Component, Clone, Debug, Default)]
pub struct Locomotion {
    /// Current (smoothed) world velocity.
    pub velocity: Vec3,
    /// Yaw rate, radians per second (positive = turning left).
    pub turn_rate: f32,
    pub(crate) prev_velocity: Vec3,
    /// External roots: last frame's transform, to infer velocity from.
    pub(crate) last_root: Option<(Vec3, Quat)>,
}

/// Where the mob is in its gait.
#[derive(Component, Clone, Debug, Default)]
pub struct GaitClock {
    /// Gait cycle phase, 0..1.
    pub phase: f32,
    /// Index into the gait's modes.
    pub mode: usize,
    /// Gait cycles per second this frame.
    pub rate: f32,
    /// Whether the feet step (moving, or catching up after turning on the spot).
    pub stepping: bool,
}

/// The step being taken: the gait mode's (or a playing gait cycle's) settings with the body's
/// weight applied. Curves are copied only when they change.
#[derive(Component, Clone, Debug, Default)]
pub struct ActiveStep {
    pub pattern: PhasePattern,
    /// Fraction of the cycle each foot is down.
    pub duty: f32,
    /// Foot lift, leg lengths.
    pub lift: f32,
    /// Body bob, leg lengths.
    pub bob: f32,
    /// Distance per cycle, leg lengths.
    pub stride: f32,
    pub body: BodyMotion,
    pub shape: StepShape,
    /// The duty before weight lengthened it: the cadence rises to match, so feet don't overreach.
    pub(crate) base_duty: f32,
}

impl ActiveStep {
    /// Leg `i`'s phase in its own step.
    pub fn leg_phase(&self, phase: f32, legs: &[LegInfo], i: usize) -> f32 {
        (phase + self.pattern.offset(&legs[i], i, legs)).rem_euclid(1.0)
    }

    /// The step time [`StepShape`]'s torso curves take: `0..0.5` stance, `0.5..1` swing.
    pub fn shape_time(&self, p: f32) -> f32 {
        if p < self.duty { 0.5 * p / self.duty } else { 0.5 + 0.5 * (p - self.duty) / (1.0 - self.duty) }
    }
}

/// A playing gait cycle's foot plan and stride, replacing the gait's mode (written by the player).
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct GaitOverride(pub Option<(FootPlan, f32)>);

/// One foot.
#[derive(Clone, Copy, Debug, Default)]
pub struct Foot {
    /// Where the foot is now (world).
    pub position: Vec3,
    pub planted: bool,
    pub(crate) swing_from: Vec3,
    pub(crate) swing_to: Vec3,
}

/// Feet in world space, in `Anatomy::legs` order.
#[derive(Component, Clone, Debug, Default)]
pub struct Feet(pub SmallVec<[Foot; 8]>);

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SpringBone {
    pub angle: Vec2,
    pub velocity: Vec2,
}

/// Tails, ears and other spring chains, one entry per bone.
#[derive(Component, Clone, Debug, Default)]
pub struct SpringState(pub(crate) Vec<SpringBone>);

/// The head's current turn toward `MobIntent::look_at`.
#[derive(Component, Clone, Copy, Debug)]
pub struct LookState(pub Quat);

impl Default for LookState {
    fn default() -> Self {
        Self(Quat::IDENTITY)
    }
}

/// The torso motion being eased toward the step's, and how lively it is (0 standing).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TorsoState {
    pub(crate) motion: Option<BodyMotion>,
    pub(crate) level: f32,
}

/// This frame's pose, local transform per bone (hero tier). Written by the pose stages, copied
/// to the joint entities last.
#[derive(Component, Clone, Debug, Default)]
pub struct MobPose {
    pub local: Vec<Transform>,
    /// World transforms after the body stages, before leg IK (look-at and cloth read them).
    pub(crate) world: Vec<Affine3A>,
}

/// What the playing cycles add to this frame's pose (hero tier).
#[derive(Component, Clone, Debug, Default)]
pub struct CyclePose(pub(crate) Option<LayerPose>);

/// Frame time for motion, evened out. Frames are shown at an even rate but their measured deltas
/// alternate around it (about 15 and 19 ms at 60 Hz), so stepping poses by the raw delta makes
/// everything judder at half the frame rate. This follows the average instead, and passes real
/// hitches (well over the average) straight through so time isn't lost.
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct MotionTime {
    /// Seconds to advance motion by this frame.
    pub dt: f32,
    /// Motion seconds since start.
    pub elapsed: f32,
    average: f32,
}

impl MotionTime {
    pub(crate) fn tick(&mut self, raw: f32) {
        let raw = raw.clamp(1e-4, 0.1);
        self.average = if self.average <= 0.0 { raw } else { self.average + (raw.clamp(self.average * 0.5, self.average * 2.0) - self.average) * 0.05 };
        self.dt = if raw > self.average * 1.8 { raw } else { self.average };
        self.elapsed += self.dt;
    }
}

pub(super) fn tick_motion_time(time: Res<Time>, mut motion: ResMut<MotionTime>) {
    motion.tick(time.delta_secs());
}

#[cfg(test)]
mod tests {
    use super::MotionTime;

    #[test]
    fn alternating_deltas_even_out_and_hitches_pass() {
        let mut t = MotionTime::default();
        for i in 0..240 {
            t.tick(if i % 2 == 0 { 0.015 } else { 0.0185 });
        }
        assert!((t.dt - 0.01675).abs() < 0.0005, "steady dt {}", t.dt);
        t.tick(0.05);
        assert!((t.dt - 0.05).abs() < 1e-6, "a hitch passes through");
    }
}
