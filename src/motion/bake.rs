//! Baked motion for crowd tiers: the procedural solver run headlessly, recorded as looping clips of
//! final skin matrices (model space × inverse bind), so playback is just a blend of two frames.

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::pose::{PoseInput, gait_rate, leg_length, reset_state, solve_pose};
use super::{Gait, MotionState};
use crate::anatomy::{Anatomy, MAX_CROWD_BONES};

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
            out[..self.idle.len()].copy_from_slice(&self.idle);
            return;
        };
        let n = clip.frames.len();
        let f = (phase - clip.start_phase).rem_euclid(1.0) * n as f32;
        let (i0, t) = (f.floor() as usize % n, f.fract());
        let (a, b) = (&clip.frames[i0], &clip.frames[(i0 + 1) % n]);
        let (wa, wb, wi) = ((1.0 - t) * (1.0 - idle_weight), t * (1.0 - idle_weight), idle_weight);
        for (k, dst) in out.iter_mut().take(a.len()).enumerate() {
            for e in 0..16 {
                dst[e] = a[k][e] * wa + b[k][e] * wb + self.idle[k][e] * wi;
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
            let l = &local[b.index()];
            let local = Affine3A::from_rotation_translation(l.rotation, l.translation);
            let m = match skeleton.bones[b.index()].parent {
                Some(p) => model[crowd.map[p.index()] as usize] * local,
                None => local,
            };
            model.push(m);
            (Mat4::from(m) * skeleton.inverse_bind[b.index()]).to_cols_array()
        })
        .collect()
}

/// Bakes `gait` on `anatomy`: each mode is walked in a straight line at a representative speed;
/// after a warm-up, one cycle is recorded.
pub fn bake_motion(anatomy: &Anatomy, gait: &Gait) -> BakedMotion {
    debug_assert!(anatomy.skeleton.crowd.bones.len() <= MAX_CROWD_BONES);
    let rest: Vec<Transform> = anatomy.skeleton.bones.iter().map(|b| b.rest).collect();
    let idle = skin_frame(anatomy, &rest);
    let leg = leg_length(anatomy);
    let scales = vec![Vec3::ONE; anatomy.skeleton.len()];

    let clips = (0..gait.modes.len())
        .map(|m| {
            let lo = gait.modes[m].min_speed;
            let speed = gait.modes.get(m + 1).map_or(lo + 1.0, |next| (lo + next.min_speed) * 0.5).max(0.3);
            let mut state = MotionState::default();
            let mut root = Affine3A::IDENTITY;
            reset_state(&mut state, anatomy, root);
            state.velocity = Vec3::Z * speed * leg;
            state.prev_velocity = state.velocity;
            let (_, cycles) = gait_rate(&state, gait, leg, true);
            let dt = 1.0 / (cycles * BAKE_FRAMES as f32);
            let step = |state: &mut MotionState, root: &mut Affine3A, i: usize| {
                root.translation += bevy::math::Vec3A::from(state.velocity * dt);
                let input = PoseInput {
                    anatomy,
                    gait,
                    root: *root,
                    ground_y: 0.0,
                    hits: &[],
                    look_at: None,
                    scales: &scales,
                    dt,
                    time: i as f32 * dt,
                };
                solve_pose(&input, state)
            };
            for i in 0..BAKE_FRAMES * 3 {
                step(&mut state, &mut root, i);
            }
            let mut start_phase = 0.0;
            let frames = (0..BAKE_FRAMES)
                .map(|i| {
                    let local = step(&mut state, &mut root, i);
                    if i == 0 {
                        start_phase = state.phase;
                    }
                    skin_frame(anatomy, &local)
                })
                .collect();
            BakedClip { start_phase, frames }
        })
        .collect();
    BakedMotion { clips, idle }
}
