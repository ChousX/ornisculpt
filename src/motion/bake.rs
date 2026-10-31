//! Baked motion for crowd tiers: the procedural solver run headlessly, recorded as clips of final
//! skin matrices (model space × inverse bind), so playback is just a blend of two frames.
//! [`bake_motion`] bakes a body's gait modes at generation; [`bake_cycle`] bakes one idle or
//! action of a motion graph on a body (standing, with the layer's mask applied), for the crowd
//! layers.

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::Gait;
use super::cycle::{Cycle, CycleKind};
use super::eval::{EvalInput, LayerPose, MaskWeights, MotionRig, evaluate};
use super::stages::{Frame, Sim, leg_length};
use crate::anatomy::{Anatomy, MAX_CROWD_BONES};
use crate::num::{Idx, Saturate, ToF32};

/// The longest step the pose stages take while baking (their eases and springs are tuned for
/// frame rates; a slow idle's frames are further apart than that).
const MAX_SIM_DT: f32 = 1.0 / 30.0;
/// Seconds of standing the pose stages get before a cycle (or the standing frame) is recorded:
/// the torso's eases and the spring chains have settled by then.
const SETTLE_S: f32 = 3.0;

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
    /// Gait phase of frame 0 (0 for idles and actions).
    pub start_phase: f32,
    /// `BAKE_FRAMES` frames: a loop (gaits, idles), or an action from its start to its end.
    pub frames: Vec<SkinFrame>,
}

impl BakedClip {
    /// Which entries of the frames (crowd bones, then soft regions) move at all over the clip,
    /// against `rest` (the standing frame it was baked from): a crowd layer playing this clip
    /// drives only those.
    #[must_use]
    pub fn driven(&self, rest: &[[f32; 16]]) -> Vec<bool> {
        rest.iter()
            .enumerate()
            .map(|(i, rest)| {
                self.frames.iter().filter_map(|f| f.get(i)).any(|m| m.iter().zip(rest).any(|(a, b)| (a - b).abs() > 1e-3))
            })
            .collect()
    }
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

/// The crowd skeleton's idle frame (and, with soft tissue, its regions at rest): the pose crowd
/// layers start from.
#[must_use]
pub fn idle_frame(anatomy: &Anatomy) -> SkinFrame {
    let idle = skin_frame(anatomy, &anatomy.idle_pose());
    #[cfg(feature = "soft-tissue")]
    let idle = {
        let soft = crate::soft::SoftTissue::of(anatomy).crowd;
        with_regions(anatomy, &soft, Affine3A::IDENTITY, &crate::soft::SoftState::new(soft.len()), idle)
    };
    idle
}

/// The crowd skeleton's frame for the hero stages' standing pose, with no cycle playing.
///
/// Feet solved onto their rest spots, torso settled: what [`bake_cycle`]'s clips differ from
/// where a cycle moves them. Differs a little from [`idle_frame`], the raw rest pose crowd mobs
/// stand in.
#[must_use]
pub fn standing_frame(anatomy: &Anatomy) -> SkinFrame {
    let gait = Gait::for_legs(anatomy.legs.len());
    let leg = leg_length(anatomy);
    let root = Affine3A::IDENTITY;
    let mut sim = Sim::new(anatomy, root);
    sim.torso.upright_biped = Some(false);
    #[cfg(feature = "soft-tissue")]
    let soft = crate::soft::SoftTissue::of(anatomy).crowd;
    let mut frame = SkinFrame::new();
    for _ in 0..(SETTLE_S / MAX_SIM_DT).ceil().sat_usize() {
        // Time stays at 0: the spring chains' slow idle sway would otherwise differ between
        // this frame and every clip baked against it.
        let f = Frame { a: anatomy, gait: &gait, root, dt: MAX_SIM_DT, time: 0.0, leg };
        let local = sim.frame_with(&f, None, None).to_vec();
        frame = skin_frame(anatomy, &local);
    }
    #[cfg(feature = "soft-tissue")]
    let frame = with_regions(anatomy, &soft, root, &crate::soft::SoftState::new(soft.len()), frame);
    frame
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

/// Bakes `cycle` on `anatomy` standing still, as the layer whose mask is `mask` would play it.
///
/// `BAKE_FRAMES` frames over one period (idles loop) or from start to end (actions), each the
/// crowd skeleton's skin matrices (and, with soft tissue, its regions') for the hero stages' pose
/// with the cycle on top. Gait cycles aren't baked this way (the body's gait clips serve them):
/// `None`.
#[must_use]
pub fn bake_cycle(anatomy: &Anatomy, rig: &MotionRig, cycle: &Cycle, mask: &MaskWeights) -> Option<BakedClip> {
    if matches!(cycle.kind, CycleKind::Gait { .. }) {
        return None;
    }
    let loops = cycle.kind.loops();
    let n = BAKE_FRAMES;
    let dt = cycle.period(0.0) / n.to_f32();
    let sim_dt = dt.min(MAX_SIM_DT);
    let gait = Gait::for_legs(anatomy.legs.len());
    let leg = leg_length(anatomy);
    let root = Affine3A::IDENTITY;
    let mut sim = Sim::new(anatomy, root);
    // A standing biped shifts its weight from leg to leg over seconds: not in a loop.
    sim.torso.upright_biped = Some(false);
    #[cfg(feature = "soft-tissue")]
    let soft = crate::soft::SoftTissue::of(anatomy).crowd;
    #[cfg(feature = "soft-tissue")]
    let mut tissue = crate::soft::SoftState::new(soft.len());
    let pose_at = |t: f32| {
        let input = EvalInput { anatomy, rig, t, leg_phases: &[] };
        let mut masked = LayerPose::identity(anatomy.skeleton.len(), anatomy.legs.len(), anatomy.wings.len());
        masked.compose(&evaluate(cycle, &input), 1.0, mask, false);
        masked
    };
    #[allow(unused_mut)]
    let mut step = |sim: &mut Sim, t: f32| -> SkinFrame {
        let pose = pose_at(t);
        // Time stays at 0 (as in `standing_frame`), so only the cycle moves the spring chains.
        let f = Frame { a: anatomy, gait: &gait, root, dt: sim_dt, time: 0.0, leg };
        let local = sim.frame_with(&f, Some(&pose), None).to_vec();
        let frame = skin_frame(anatomy, &local);
        #[cfg(feature = "soft-tissue")]
        let frame = {
            crate::soft::step(anatomy, &soft, root, &local, &sim.feet.planted(), sim_dt, &mut tissue);
            with_regions(anatomy, &soft, root, &tissue, frame)
        };
        frame
    };
    // Warm-up: a loop runs through whole times until the stages have settled, so the seam
    // closes; an action holds its first pose that long.
    let time = |i: usize| if loops { i.to_f32() / n.to_f32() } else { i.to_f32() / (n - 1).to_f32() };
    let warm = (SETTLE_S / (sim_dt * n.to_f32())).ceil().sat_usize().max(1) * n;
    for i in 0..warm {
        step(&mut sim, if loops { time(i % n) } else { 0.0 });
    }
    let frames = (0..n).map(|i| step(&mut sim, time(i))).collect();
    Some(BakedClip { start_phase: 0.0, frames })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::build_src;
    use crate::motion::graph::BodyMask;
    use crate::motion::library;

    fn quadruped() -> Anatomy {
        build_src(include_str!("../../assets/mobs/quadruped.body.ron"))
    }

    /// Which crowd slot entries a clip moves, named by their bones.
    fn driven_names(a: &Anatomy, clip: &BakedClip) -> Vec<String> {
        let standing = standing_frame(a);
        clip.driven(&standing)
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| a.skeleton.crowd.bones.get(i).and_then(|b| a.skeleton.get(*b)).map_or_else(|| format!("region.{i}"), |b| b.name.clone()))
            .collect()
    }

    #[test]
    fn a_masked_idle_moves_only_its_mask() {
        let a = quadruped();
        let rig = MotionRig::new(&a);
        let look = library::idles().into_iter().find(|c| c.name == "look_around").unwrap();
        let clip = bake_cycle(&a, &rig, &look, &rig.mask(&BodyMask::HeadNeck)).unwrap();
        assert_eq!(clip.frames.len(), BAKE_FRAMES);
        let driven = driven_names(&a, &clip);
        assert!(!driven.is_empty(), "the head turns");
        assert!(driven.iter().all(|n| n.starts_with("neck.") || n.starts_with("skull") || n.starts_with("jaw") || n.starts_with("ear.") || n.starts_with("region.")), "{driven:?}");
        // Loops close: the last frame is a step away from the first, not a jump.
        let (first, last) = (clip.frames.first().unwrap(), clip.frames.last().unwrap());
        let seam = first.iter().zip(last).flat_map(|(x, y)| x.iter().zip(y).map(|(a, b)| (a - b).abs())).fold(0.0, f32::max);
        assert!(seam < 0.05, "seam {seam}");
    }

    #[test]
    fn an_action_starts_at_rest_and_gaits_are_not_baked() {
        let a = quadruped();
        let rig = MotionRig::new(&a);
        let rear = library::actions().into_iter().find(|c| c.name == "rear").unwrap();
        let clip = bake_cycle(&a, &rig, &rear, &rig.mask(&BodyMask::All)).unwrap();
        let standing = standing_frame(&a);
        let first = clip.frames.first().unwrap();
        let start = first.iter().zip(&standing).flat_map(|(x, y)| x.iter().zip(y).map(|(a, b)| (a - b).abs())).fold(0.0, f32::max);
        assert!(start < 0.02, "the rear starts from standing: {start}");
        let mid = clip.frames.get(BAKE_FRAMES / 2).unwrap();
        let lift = mid.iter().zip(&standing).map(|(x, y)| x[13] - y[13]).fold(0.0, f32::max);
        assert!(lift > 0.05, "the body comes up mid-rear: {lift}");
        let walk = library::gaits().into_iter().find(|c| c.name == "quadruped.walk").unwrap();
        assert!(bake_cycle(&a, &rig, &walk, &rig.mask(&BodyMask::All)).is_none());
    }
}

#[cfg(test)]
mod timing {
    use super::*;
    use crate::anatomy::build_src;

    #[test]
    #[ignore = "timing probe"]
    fn standing_frame_timing() {
        for (name, src) in [("quadruped", include_str!("../../assets/mobs/quadruped.body.ron")), ("man", include_str!("../../assets/mobs/man.body.ron"))] {
            let a = build_src(src);
            let t = std::time::Instant::now();
            let _ = standing_frame(&a);
            let per = t.elapsed().as_secs_f32() / (SETTLE_S / MAX_SIM_DT);
            println!("{name}: {:.2} ms per pose frame ({} bones)", per * 1000.0, a.skeleton.len());
        }
    }
}
