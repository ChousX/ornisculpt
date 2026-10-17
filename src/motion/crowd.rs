//! Crowd-tier animation: advance the gait phase and pick which baked frames to blend. Only the
//! small per-slot blend instruction is uploaded, and only when it changed; the blend itself runs
//! in a compute pass (see `crate::mob::crowd_gpu`).

use bevy::prelude::*;
use bevy::utils::Parallel;

use super::pose::{gait_rate, leg_length};
use super::{DefaultGaits, MotionState};
use crate::mob::{CrowdInstance, CrowdSkin, CrowdSlot, CrowdUploaded, CrowdUploads, GeneratedBody, MobBody, MobSkeleton};

/// Non-hero mobs: advance the gait phase and queue changed blend instructions.
#[allow(clippy::type_complexity)]
pub(super) fn crowd_animate(
    time: Res<Time>,
    bodies: Res<Assets<GeneratedBody>>,
    gaits: Res<DefaultGaits>,
    skin: Res<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
    mut mobs: Query<(&MobBody, &CrowdSlot, &mut MotionState, &mut CrowdUploaded), Without<MobSkeleton>>,
    mut changed: Local<Parallel<Vec<(u32, CrowdInstance)>>>,
) {
    let dt = time.delta_secs().clamp(1e-4, 0.1);
    mobs.par_iter_mut().for_each(|(body, slot, mut state, mut uploaded)| {
        let Some(layout) = skin.layout(body.0.id()) else { return };
        let Some(body) = bodies.get(&body.0) else { return };
        let a = &body.anatomy;
        let leg = leg_length(a);
        let (mode, cycles) = gait_rate(&state, gaits.for_legs(a.legs.len()), leg, false);
        state.mode = mode;
        state.phase = (state.phase + cycles * dt).rem_euclid(1.0);
        let idle = 1.0 - (state.velocity.length() / leg / 0.3).clamp(0.0, 1.0);
        let instance = layout.instance(&body.motion, mode, state.phase, idle);
        if instance != uploaded.0 {
            uploaded.0 = instance;
            changed.borrow_local_mut().push((slot.0, instance));
        }
    });
    changed.drain_into(&mut uploads.instances);
}
