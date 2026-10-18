//! Crowd-tier animation: advance the gait phase and pick which baked frames to blend. Only the
//! small per-slot blend instruction is uploaded, and only when it changed; the blend itself runs
//! in a compute pass (see `crate::mob::crowd_gpu`).

use bevy::prelude::*;
use bevy::utils::Parallel;

use super::stages::leg_length;
use super::{GaitClock, Locomotion};
use crate::mob::{CrowdInstance, CrowdSkin, CrowdSlot, CrowdUploaded, CrowdUploads, GeneratedBody, MobBody, MobSkeleton};

/// Non-hero mobs: queue changed blend instructions for the phase the clock systems advanced.
#[allow(clippy::type_complexity)]
pub(super) fn crowd_animate(
    bodies: Res<Assets<GeneratedBody>>,
    skin: Res<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
    mut mobs: Query<(&MobBody, &CrowdSlot, &Locomotion, &GaitClock, &mut CrowdUploaded), Without<MobSkeleton>>,
    mut changed: Local<Parallel<Vec<(u32, CrowdInstance)>>>,
) {
    mobs.par_iter_mut().for_each(|(body, slot, loco, clock, mut uploaded)| {
        let Some(layout) = skin.layout(body.0.id()) else { return };
        let Some(body) = bodies.get(&body.0) else { return };
        let leg = leg_length(&body.anatomy);
        let idle = 1.0 - (loco.velocity.length() / leg / 0.3).clamp(0.0, 1.0);
        let instance = layout.instance(&body.motion, clock.mode.min(body.motion.clips.len().saturating_sub(1)), clock.phase, idle);
        if instance != uploaded.0 {
            uploaded.0 = instance;
            changed.borrow_local_mut().push((slot.0, instance));
        }
    });
    changed.drain_into(&mut uploads.instances);
}
