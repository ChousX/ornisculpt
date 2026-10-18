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
mod cycle;
mod eval;
mod fit;
mod gait;
mod graph;
mod heft;
pub mod library;
mod ik;
mod player;
mod pose;
mod stages;
mod state;

pub use bake::{BAKE_FRAMES, BakedClip, BakedMotion, SkinFrame, bake_motion};
pub use cycle::{
    Channel, Curve, Cycle, CycleError, CycleEvent, CycleKind, CycleLoader, CycleLoaderError, FootPlan, Joint, LegSel,
    RootMotion, Sides, Target, Track,
};
pub use eval::{EvalInput, LayerPose, MotionRig, evaluate};
pub use fit::{BodyFamily, BodyTraits, Feature, Fit, Misfit};
pub use graph::{
    BodyMask, Cmp, ComposedGraph, CycleSource, Condition, GraphError, GraphLoader, GraphLoaderError, Layer, LayerMode, MotionGraph,
    Param, ParamValue, Play, State, Switch, Transition, compose,
};
pub use gait::{BodyMotion, Gait, GaitLoader, GaitLoaderError, GaitMode, PhasePattern};
pub use heft::{Heft, HeftResponse, StepShape};
pub use state::{ActiveStep, CyclePose, Feet, Foot, GaitClock, GaitOverride, Locomotion, LookState, MobPose, SpringState, TorsoState};
pub use ik::{fabrik, orient_to_pole, two_bone};
pub use player::{Footstep, MobMotion, MotionCache, MotionParams, MotionRuntime, PlayCycle, RootMotionDelta, TRIGGER_LIFETIME};

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

/// The motion systems, in order, inside [`MobSet::Motion`](crate::mob::MobSet::Motion).
#[derive(SystemSet, Hash, Eq, PartialEq, Clone, Debug)]
pub enum MotionSystems {
    /// Roots move toward their intent.
    Root,
    /// Graphs and direct cycles advance; transitions, events, root motion.
    Player,
    /// Every tier: gait mode, step and phase.
    Clock,
    /// Hero tier: the pose, stage by stage, then the joints. Crowd tier: baked frame choice.
    Pose,
}

pub struct MotionPlugin;

impl Plugin for MotionPlugin {
    fn build(&self, app: &mut App) {
        use MotionSystems::*;
        app.init_asset::<Gait>()
            .init_resource::<DefaultGaits>()
            .init_asset_loader::<GaitLoader>()
            .init_asset::<Cycle>()
            .init_asset_loader::<CycleLoader>()
            .init_asset::<MotionGraph>()
            .init_asset_loader::<GraphLoader>()
            .init_resource::<player::MotionCache>()
            .init_resource::<player::MotionWarnings>()
            .register_type::<MobIntent>()
            .add_observer(pose::bind_body)
            .configure_sets(Update, (Root, Player, Clock, Pose).chain().in_set(crate::mob::MobSet::Motion))
            .add_systems(
                Update,
                (
                    pose::move_roots.in_set(Root),
                    (player::prepare_motion, player::update_motion).chain().in_set(Player),
                    (pose::choose_steps, pose::advance_clocks).chain().in_set(Clock),
                    (
                        (
                            pose::write_probes,
                            player::evaluate_cycles,
                            pose::rest_poses,
                            pose::fold_wings,
                            pose::plant_feet,
                            pose::bob_and_bend,
                            pose::ride_torso,
                            pose::swing_springs,
                            pose::apply_cycles,
                            pose::solve_legs,
                            pose::look_at,
                            pose::write_joints,
                        )
                            .chain(),
                        crowd::crowd_animate,
                    )
                        .in_set(Pose),
                ),
            );
    }
}

/// A cycle's named event fired on this mob (observe with `On<MotionEvent>`).
#[derive(EntityEvent, Clone, Debug)]
pub struct MotionEvent {
    pub entity: Entity,
    /// The event's name from the cycle.
    pub name: String,
    /// The cycle that fired it.
    pub cycle: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the schedule once: Bevy rejects systems whose queries conflict (B0001) there, which
    /// no unit test of the stages would catch.
    #[test]
    fn motion_systems_schedule_without_conflicts() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<crate::mob::GeneratedBody>()
            .init_asset::<bevy::render::storage::ShaderBuffer>()
            .init_resource::<crate::mob::CrowdSkin>()
            .init_resource::<crate::mob::CrowdUploads>()
            .add_plugins(MotionPlugin);
        app.update();
        app.update();
    }
}
