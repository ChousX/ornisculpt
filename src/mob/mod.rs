//! Mobs in the ECS: spawning, async generation, tiers and runtime levers.
//!
//! ```ignore
//! commands.spawn((Mob::new(asset_server.load("mobs/quadruped.body.ron")), Form::default(), Transform::default()));
//! ```
//!
//! Flow: `Mob` added → [`GenRequest`] → body asset loaded → generation task on the async compute
//! pool (deduplicated through [`GenCache`]) → [`GeneratedBody`] asset → joints, per-LOD skinned
//! meshes and part entities spawned under the mob → [`BodyReady`] triggered.

mod crowd;
mod crowd_gpu;
mod generate;
mod levers;
mod spawn;
mod tier;

pub(crate) use crowd::crowd_slot_defs;
pub use crowd::{BONE_BYTES, ClipLayout, CrowdInstance, CrowdSkin, CrowdSlot, CrowdUploaded, CrowdUploads, SLOT_BYTES};
pub use generate::{
    BodyKey, CoatMeshHandles, GenBudget, GenCache, GenInput, GenOutput, GenRequest, GeneratedBody, LodMeshes, PartAssetCache, SlotMaterials,
    StageTimes, body_key, generate_body,
};
pub use levers::{BoneScale, BoneScales, GraftPart, SeverPart};
pub use spawn::{HeroVisual, LodMesh, MobJoint, MobParts, MobSkeleton, MobVisual, Part, PartLod, PartOf, Placeholder};
#[cfg(feature = "soft-tissue")]
pub use spawn::SoftJoint;
pub use tier::{ForceTier, MobTier, QualityProfile, TierCamera};

use bevy::prelude::*;

use crate::anatomy::{BodyPlan, Form};

/// A generated creature. Requires a [`BodyPlan`] handle; everything else is optional.
#[derive(Component, Clone, Debug)]
#[require(
    Transform,
    Visibility,
    Form,
    MobSeed,
    BoneScales,
    crate::motion::MobIntent,
    crate::motion::Locomotion,
    crate::motion::GaitClock,
    crate::motion::ActiveStep,
    crate::motion::GaitOverride,
    crate::motion::Feet,
    crate::motion::SpringState,
    crate::motion::LookState,
    crate::motion::TorsoState,
    crate::motion::Heft,
    crate::motion::FootProbes
)]
pub struct Mob {
    pub body: Handle<BodyPlan>,
}

impl Mob {
    #[must_use]
    pub const fn new(body: Handle<BodyPlan>) -> Self {
        Self { body }
    }
}

/// Seed for per-mob noise (skin creases, later coat variation). Anatomy never uses it.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[reflect(Component)]
pub struct MobSeed(pub u64);

/// The mob's own expanded, form-applied body plan: the source of truth after spawn.
/// Edit it (or its [`Part`] entities, or send [`SeverPart`]/[`GraftPart`]) to regenerate.
#[derive(Component, Clone, Debug)]
pub struct LiveBody(pub BodyPlan);

/// The generated body the mob currently displays.
#[derive(Component, Clone, Debug)]
pub struct MobBody(pub Handle<GeneratedBody>);

/// Triggered on a mob each time a (re)generated body has been attached.
#[derive(EntityEvent, Clone, Copy, Debug)]
pub struct BodyReady {
    pub entity: Entity,
}

/// Triggered on a mob when a requested (re)generation failed (the error is also logged). The mob
/// keeps the body it had, if any.
#[derive(EntityEvent, Clone, Debug)]
pub struct BodyFailed {
    pub entity: Entity,
    pub error: String,
}

/// System sets, in order, all in `Update`.
#[derive(SystemSet, Hash, Eq, PartialEq, Clone, Debug)]
pub enum MobSet {
    /// Start/poll generation tasks, attach finished bodies.
    Generation,
    /// Choose each mob's tier and update visibility / part entities.
    Tiering,
    /// Root locomotion and procedural pose (see `crate::motion`).
    Motion,
    /// Apply runtime levers (bone scales) to joints.
    Levers,
}

pub struct MobPlugin;

impl Plugin for MobPlugin {
    fn finish(&self, app: &mut App) {
        use bevy::render::renderer::{RenderGraph, RenderGraphSystems};
        use bevy::render::{ExtractSchedule, GpuResourceAppExt, Render, RenderApp, RenderSystems};
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<crowd_gpu::CrowdBlendPipeline>()
            .init_resource::<crowd_gpu::CrowdGpu>()
            .add_systems(ExtractSchedule, crowd_gpu::extract_crowd)
            .add_systems(
                Render,
                (
                    crowd_gpu::prepare_crowd_buffers.in_set(RenderSystems::PrepareResources),
                    crowd_gpu::prepare_crowd_bind_group.in_set(RenderSystems::PrepareBindGroups),
                ),
            )
            .add_systems(RenderGraph, crowd_gpu::dispatch_crowd_blend.in_set(RenderGraphSystems::Begin));
    }

    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "crowd_blend.wesl");
        app.init_asset::<GeneratedBody>()
            .init_resource::<CrowdSkin>()
            .init_resource::<CrowdUploads>()
            .add_systems(First, crowd::clear_uploads)
            .init_resource::<GenCache>()
            .init_resource::<GenBudget>()
            .init_resource::<PartAssetCache>()
            .init_resource::<QualityProfile>()
            .register_type::<MobSeed>()
            .register_type::<QualityProfile>()
            .configure_sets(Update, (MobSet::Generation, MobSet::Tiering, MobSet::Motion, MobSet::Levers).chain())
            .add_observer(generate::request_on_add)
            .add_observer(generate::forget_waiting_key)
            .add_observer(generate::request_on_outfit)
            .add_observer(generate::request_on_undress)
            .add_observer(levers::sever_part)
            .add_observer(levers::graft_part)
            .add_observer(crowd::release_slot)
            .add_systems(
                Update,
                (
                    (
                        generate::reload_changed_assets,
                        spawn::sync_part_edits,
                        generate::start_generation,
                        generate::apply_worker_budget,
                        generate::finish_generation,
                        spawn::attach_body,
                        crowd::register_clips,
                        crowd::sync_bone_capacity,
                    )
                        .chain()
                        .in_set(MobSet::Generation),
                    (tier::choose_tiers, spawn::sync_hero_visuals, tier::apply_tier_visibility, spawn::sync_part_entities)
                        .chain()
                        .in_set(MobSet::Tiering),
                    levers::apply_bone_scales.in_set(MobSet::Levers),
                ),
            );
    }
}
