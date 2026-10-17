//! Procedural motion: root locomotion, gait, foot planting, leg IK, spine, springs and look-at.
//!
//! Gameplay writes [`MobIntent`]; the plugin owns all bone motion. By default the plugin also
//! moves the mob's `Transform`; add [`ExternalRoot`] to move it yourself (physics, navmesh) and the
//! plugin infers velocity from it.
//!
//! Ground is the mob's root height unless the game writes [`FootGround`] (physics is the game's
//! choice): read [`FootProbes`] (one ray origin per leg, cast straight down) and write the hits.

mod bake;
mod crowd;
mod gait;
mod ik;
mod pose;

pub use bake::{BAKE_FRAMES, BakedClip, BakedMotion, SkinFrame, bake_motion};
pub use gait::{Gait, GaitLoader, GaitLoaderError, GaitMode, PhasePattern};
pub use ik::fabrik;

use bevy::prelude::*;
use smallvec::SmallVec;

/// What gameplay/AI wants the mob to do.
#[derive(Component, Reflect, Clone, Debug, Default)]
#[reflect(Component)]
pub struct MobIntent {
    /// Desired world velocity (horizontal), metres per second.
    pub velocity: Vec3,
    /// Desired facing; defaults to the direction of travel.
    pub facing: Option<Dir3>,
    /// World point for the head to look at.
    pub look_at: Option<Vec3>,
}

/// The game moves this mob's `Transform`; the plugin only animates (velocity is inferred).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct ExternalRoot;

/// Default gaits by leg count, built once (see [`Gait::for_legs`]).
#[derive(Resource)]
pub struct DefaultGaits(Vec<Gait>);

impl Default for DefaultGaits {
    fn default() -> Self {
        Self((0..=8).map(Gait::for_legs).collect())
    }
}

impl DefaultGaits {
    pub fn for_legs(&self, legs: usize) -> &Gait {
        &self.0[legs.min(self.0.len() - 1)]
    }
}

/// Optional per-mob gait; otherwise a default is chosen from the leg count.
#[derive(Component, Clone, Debug)]
pub struct MobGait(pub Handle<Gait>);

/// One ray origin per leg (in `Anatomy::legs` order), cast straight down, written every frame.
#[derive(Component, Clone, Debug, Default)]
pub struct FootProbes(pub SmallVec<[Vec3; 8]>);

/// Ground hit per leg, written by the game's physics. `None` (or no component) = root height.
#[derive(Component, Clone, Debug, Default)]
pub struct FootGround(pub SmallVec<[Option<Vec3>; 8]>);

/// Runtime motion state, created when a body is attached and reset when it changes.
#[derive(Component, Clone, Debug, Default)]
pub struct MotionState {
    /// Current (smoothed) world velocity.
    pub velocity: Vec3,
    /// Yaw rate, radians per second (positive = turning left).
    pub turn_rate: f32,
    /// Gait cycle phase, 0..1.
    pub phase: f32,
    pub mode: usize,
    /// Feet in world space, in `Anatomy::legs` order.
    pub feet: SmallVec<[Foot; 8]>,
    springs: Vec<SpringBone>,
    look: Quat,
    last_root: Option<(Vec3, Quat)>,
    prev_velocity: Vec3,
    body: Option<AssetId<crate::mob::GeneratedBody>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Foot {
    /// Where the foot is now (world).
    pub position: Vec3,
    pub planted: bool,
    swing_from: Vec3,
    swing_to: Vec3,
}

#[derive(Clone, Copy, Debug, Default)]
struct SpringBone {
    angle: Vec2,
    velocity: Vec2,
}

pub struct MotionPlugin;

impl Plugin for MotionPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<Gait>()
            .init_resource::<DefaultGaits>()
            .init_asset_loader::<GaitLoader>()
            .register_type::<MobIntent>()
            .add_systems(
                Update,
                (pose::move_roots, (pose::animate, crowd::crowd_animate)).chain().in_set(crate::mob::MobSet::Motion),
            );
    }
}
