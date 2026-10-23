//! Baked motion for crowd tiers: the procedural solver run headlessly, recorded as looping clips of
//! final skin matrices (model space × inverse bind), so playback is just a blend of two frames.

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::Gait;
use super::stages::{Frame, Sim, leg_length};
use crate::anatomy::{Anatomy, MAX_CROWD_BONES};
use crate::num::{Idx, Saturate, ToF32};

/// Frames recorded per gait cycle.
pub const BAKE_FRAMES: usize = 24;

/// One frame of skin matrices for the crowd skeleton (column-major).
pub type SkinFrame = Vec<[f32; 16]>;

/// Looping clips for every gait mode plus an idle pose, over the crowd skeleton.
#[derive(Clone, Debug, Default)]
pub struct BakedMotion {
    /// One looping clip per gait mode (same order as `Gait::modes`).
    pub clips: Vec<BakedClip>,
    /// Rest pose, used when standing still.
    pub idle: SkinFrame,
}

#[derive(Clone, Debug, Default)]
pub struct BakedClip {
    /// Gait phase of frame 0.
    pub start_phase: f32,
    /// `BAKE_FRAMES` frames.
    pub frames: Vec<SkinFrame>,
}

impl BakedMotion {
    /// Matrix `index` of this body's clips as laid out in the crowd atlas: the idle pose, then
    /// every clip's frames in order, `bones` matrices each.
    #[must_use]
    pub fn matrix(&self, index: usize) -> Option<&[f32; 16]> {
        let bones = self.idle.len();
        if index < bones {
            return self.idle.get(index);
        }
        let k = index - bones;
        let per_clip = bones * BAKE_FRAMES;
        let clip = self.clips.get(k / per_clip)?;
        let rem = k % per_clip;
        clip.frames.get(rem / bones)?.get(rem % bones)
    }

    /// Writes the skin matrices for a mode at a gait phase into `out` (one per crowd bone),
    /// blended toward idle by `idle_weight`. Matrices are blended linearly: frames are close
    /// together, so the error is negligible.
    pub fn sample_into(&self, mode: usize, phase: f32, idle_weight: f32, out: &mut [[f32; 16]]) {
        let Some(clip) = self.clips.get(mode).filter(|c| !c.frames.is_empty()) else {
            for (dst, idle) in out.iter_mut().zip(&self.idle) {
                *dst = *idle;
            }
            return;
        };
        let count = clip.frames.len();
        let at = (phase - clip.start_phase).rem_euclid(1.0) * count.to_f32();
        let (i0, t) = (at.floor().sat_usize() % count, at.fract());
        let (Some(from), Some(to)) = (clip.frames.get(i0), clip.frames.get((i0 + 1) % count)) else { return };
        let (wa, wb, wi) = ((1.0 - t) * (1.0 - idle_weight), t * (1.0 - idle_weight), idle_weight);
        for (((dst, idle), ma), mb) in out.iter_mut().zip(&self.idle).zip(from).zip(to) {
            for (((d, i), x), y) in dst.iter_mut().zip(idle).zip(ma).zip(mb) {
                *d = i.mul_add(wi, f32::mul_add(*y, wb, x * wa));
            }
        }
    }
}

/// Skin matrices of the crowd skeleton for a full local pose.
fn skin_frame(anatomy: &Anatomy, local: &[Transform]) -> SkinFrame {
    let skeleton = &anatomy.skeleton;
    let crowd = &skeleton.crowd;
    let mut model: Vec<Affine3A> = Vec::with_capacity(crowd.bones.len());
    crowd
        .bones
        .iter()
        .map(|b| {
            let l = local.get(b.index()).copied().unwrap_or_default();
            let local = Affine3A::from_rotation_translation(l.rotation, l.translation);
            let parent = skeleton
                .get(*b)
                .and_then(|def| def.parent)
                .and_then(|p| crowd.map.get(p.index()))
                .and_then(|c| model.get(c.idx()));
            let m = parent.map_or(local, |parent| *parent * local);
            model.push(m);
            let inverse_bind = skeleton.inverse_bind.get(b.index()).copied().unwrap_or(Mat4::IDENTITY);
            (Mat4::from(m) * inverse_bind).to_cols_array()
        })
        .collect()
}

/// `frame` followed by one matrix per crowd soft region (`soft::crowd_matrices`).
#[cfg(feature = "soft-tissue")]
fn with_regions(anatomy: &Anatomy, regions: &[crate::soft::SoftRegion], root: Affine3A, state: &crate::soft::SoftState, mut frame: SkinFrame) -> SkinFrame {
    let bones = frame.clone();
    crate::soft::crowd_matrices(anatomy, regions, root, state, &bones, &mut frame);
    frame
}

/// Bakes `gait` on `anatomy`: each mode is walked in a straight line at a representative speed;
/// after a warm-up, one cycle is recorded.
///
/// With soft tissue, every frame also carries one matrix per crowd region after the crowd bones:
/// the springs run through the warm-up and the recorded cycle (`soft::crowd_matrices`).
#[must_use]
pub fn bake_motion(anatomy: &Anatomy, gait: &Gait) -> BakedMotion {
    debug_assert!(anatomy.skeleton.crowd.bones.len() <= MAX_CROWD_BONES);
    #[cfg(feature = "soft-tissue")]
    let soft = crate::soft::SoftTissue::of(anatomy).crowd;
    let idle = skin_frame(anatomy, &anatomy.idle_pose());
    // Standing still, the tissue rests: the soft joints sit on their bones.
    #[cfg(feature = "soft-tissue")]
    let idle = with_regions(anatomy, &soft, Affine3A::IDENTITY, &crate::soft::SoftState::new(soft.len()), idle);
    let leg = leg_length(anatomy);

    let clips = gait
        .modes
        .iter()
        .enumerate()
        .map(|(m, mode)| {
            let lo = mode.min_speed;
            let speed = gait.modes.get(m + 1).map_or(lo + 1.0, |next| f32::midpoint(lo, next.min_speed)).max(0.3);
            let mut root = Affine3A::IDENTITY;
            let mut sim = Sim::new(anatomy, root);
            #[cfg(feature = "soft-tissue")]
            let mut tissue = crate::soft::SoftState::new(soft.len());
            sim.loco.velocity = Vec3::Z * speed * leg;
            sim.loco.prev_velocity = sim.loco.velocity;
            // One frame to settle the step and its rate, then a cycle per BAKE_FRAMES frames.
            let settle = Frame { a: anatomy, gait, root, dt: 1e-4, time: 0.0, leg };
            sim.frame(&settle, None);
            let dt = 1.0 / (sim.clock.rate.max(1e-3) * BAKE_FRAMES.to_f32());
            #[allow(unused_mut)]
            let mut step = |sim: &mut Sim, root: &mut Affine3A, i: usize| {
                root.translation += bevy::math::Vec3A::from(sim.loco.velocity * dt);
                let f = Frame { a: anatomy, gait, root: *root, dt, time: i.to_f32() * dt, leg };
                let local = sim.frame(&f, None).to_vec();
                let frame = skin_frame(anatomy, &local);
                #[cfg(feature = "soft-tissue")]
                let frame = {
                    crate::soft::step(anatomy, &soft, *root, &local, &sim.feet.planted(), dt, &mut tissue);
                    with_regions(anatomy, &soft, *root, &tissue, frame)
                };
                frame
            };
            // Springs settle over the warm-up too, so the recorded cycle loops without a jump.
            for i in 0..BAKE_FRAMES * 3 {
                step(&mut sim, &mut root, i);
            }
            let mut start_phase = 0.0;
            let frames = (0..BAKE_FRAMES)
                .map(|i| {
                    let frame = step(&mut sim, &mut root, i);
                    if i == 0 {
                        start_phase = sim.clock.phase;
                    }
                    frame
                })
                .collect();
            BakedClip { start_phase, frames }
        })
        .collect();
    BakedMotion { clips, idle }
}
