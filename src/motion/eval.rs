//! Evaluating cycles on a body: targets resolve to bones once per anatomy ([`MotionRig`]), then
//! each frame a cycle at time `t` becomes a [`LayerPose`] of joint rotations, root and foot
//! offsets, and wing folds, which layers compose and the pose solver applies.
//!
//! Channel conventions are model-space, so they read the same on every body: positive pitch
//! raises the front / a chain's tip, positive yaw turns left (paired parts: outward), positive
//! flex swings the segment below a joint forward.

use bevy::prelude::*;

use super::cycle::{Channel, Cycle, Joint, LegSel, Sides, Target, Track};
use super::fit::BodyTraits;
use super::graph::BodyMask;
use crate::anatomy::{Anatomy, BoneId, JointLimit, LimbRole, PartPath, Side};
use crate::num::{Clamp, Idx, ToF32};

/// Bones each target resolves to on one body.
#[derive(Clone, Debug)]
pub struct MotionRig {
    pub traits: BodyTraits,
    spine: Vec<BoneId>,
    neck: Vec<BoneId>,
    head: Option<BoneId>,
    jaw: Option<BoneId>,
    tails: Vec<Vec<BoneId>>,
    ears: Vec<(Side, Vec<BoneId>)>,
    antennae: Vec<(Side, Vec<BoneId>)>,
    arms: Vec<(Side, Vec<BoneId>)>,
    digits: Vec<(Side, Vec<BoneId>)>,
    /// (side, index into `Anatomy::wings`, root bone)
    wings: Vec<(Side, usize, BoneId)>,
    roots: Vec<BoneId>,
    /// Each leg's bone chain, in `Anatomy::legs` order.
    legs: Vec<Vec<BoneId>>,
    /// Rest model-space rotation per bone (to express model-space axes locally).
    model_rot: Vec<Quat>,
    /// Rest model-space direction (+Y) per bone.
    model_dir: Vec<Vec3>,
    fore_pair: u8,
    hind_pair: u8,
}

fn side_of_path(path: &PartPath) -> Side {
    let last = path.0.rsplit('/').next().unwrap_or("");
    let suffix = last.rsplit_once('.').map(|(_, s)| s);
    if suffix == Some("l") {
        Side::Left
    } else if suffix == Some("r") {
        Side::Right
    } else {
        Side::Center
    }
}

/// The side a bone's name marks with an `l` or `r` component after its first (`ear.l.0`).
fn side_of_name(name: &str) -> Side {
    let has = |side: &str| name.split('.').skip(1).any(|part| part == side);
    if has("l") {
        Side::Left
    } else if has("r") {
        Side::Right
    } else {
        Side::Center
    }
}

impl MotionRig {
    #[must_use]
    pub fn new(a: &Anatomy) -> Self {
        let traits = BodyTraits::of(a);
        let bones = &a.skeleton.bones;
        let by_prefix = |prefix: &str| -> Vec<BoneId> {
            bones.iter().enumerate().filter(|(_, b)| b.name.starts_with(prefix)).map(|(i, _)| BoneId(i.clamp_u8())).collect()
        };
        // Paired chains grouped per side (`ear.l.0`, `ear.l.1`, …).
        let paired = |prefix: &str| -> Vec<(Side, Vec<BoneId>)> {
            let mut out: Vec<(Side, Vec<BoneId>)> = Vec::new();
            for id in by_prefix(prefix) {
                let side = bones.get(id.index()).map_or(Side::Center, |b| side_of_name(&b.name));
                match out.iter_mut().find(|(s, _)| *s == side) {
                    Some((_, v)) => v.push(id),
                    None => out.push((side, vec![id])),
                }
            }
            out
        };
        let mut tails: Vec<Vec<BoneId>> = Vec::new();
        for id in by_prefix("tail") {
            // A new tail starts wherever a tail bone's parent isn't a tail bone.
            let parent = bones.get(id.index()).and_then(|b| b.parent).and_then(|p| bones.get(p.index()));
            let parent_is_tail = parent.is_some_and(|p| p.name.starts_with("tail"));
            match tails.last_mut() {
                Some(t) if parent_is_tail => t.push(id),
                _ => tails.push(vec![id]),
            }
        }
        let part_bones = |p: &crate::anatomy::BuiltPart| -> Vec<BoneId> { (p.bones.start..p.bones.end).map(BoneId).collect() };
        let mut arms = Vec::new();
        let mut digits = Vec::new();
        for (i, p) in a.parts.iter().enumerate() {
            if matches!(p.role, Some(LimbRole::Manipulator)) {
                // The clavicle isn't part of the arm's chain: swings and raises start at the shoulder.
                let chain: Vec<BoneId> = part_bones(p).into_iter().filter(|b| !a.clavicles.iter().any(|c| c.bone == *b)).collect();
                arms.push((side_of_path(&p.path), chain));
                // Digits parts growing from this arm.
                for d in a.parts.iter().filter(|d| d.parent.is_some_and(|p| p.idx() == i)) {
                    let ids = part_bones(d);
                    if ids.iter().any(|b| bones.get(b.index()).is_some_and(|b| b.name.starts_with("digit"))) {
                        digits.push((side_of_path(&d.path), ids));
                    }
                }
            }
        }
        let wings = a
            .wings
            .iter()
            .enumerate()
            .filter_map(|(i, w)| Some((side_of_path(&a.parts.get(w.part.idx())?.path), i, w.bones.first()?.0)))
            .collect();
        let model = a.skeleton.model_rest();
        let model_rot: Vec<Quat> = model.iter().map(|m| Quat::from_mat3a(&m.matrix3).normalize()).collect();
        let model_dir = model_rot.iter().map(|q| *q * Vec3::Y).collect();
        let pairs = a.legs.iter().map(|l| l.pair).max().unwrap_or(0);
        Self {
            traits,
            spine: a.spine.clone(),
            neck: by_prefix("neck."),
            head: a.skeleton.find("skull"),
            jaw: a.skeleton.find("jaw"),
            tails,
            ears: paired("ear."),
            antennae: paired("antenna."),
            arms,
            digits,
            wings,
            roots: bones.iter().enumerate().filter(|(_, b)| b.parent.is_none()).map(|(i, _)| BoneId(i.clamp_u8())).collect(),
            legs: a.legs.iter().map(|l| l.chain.to_vec()).collect(),
            model_rot,
            model_dir,
            fore_pair: 0,
            hind_pair: pairs,
        }
    }

    #[must_use]
    pub const fn leg_length(&self) -> f32 {
        self.traits.leg_length
    }

    /// Bones a target moves (for masks and fit reporting).
    #[must_use]
    fn target_bones(&self, target: Target) -> Vec<BoneId> {
        match target {
            Target::Root => self.roots.clone(),
            Target::Spine => self.spine.clone(),
            Target::Neck => self.neck.clone(),
            Target::Head => self.head.into_iter().collect(),
            Target::Jaw => self.jaw.into_iter().collect(),
            Target::Tail => self.tails.iter().flatten().copied().collect(),
            Target::Ears(s) => sided(&self.ears, s),
            Target::Antennae(s) => sided(&self.antennae, s),
            Target::Arms(s) => sided(&self.arms, s),
            Target::Digits(s) => sided(&self.digits, s),
            Target::Wings(s) => self.wings.iter().filter(|w| s.includes(w.0)).map(|w| w.2).collect(),
            Target::Legs(_) => Vec::new(),
        }
    }

    /// Rest model-space rotation of a bone (identity for one this rig doesn't know).
    fn rest_rot(&self, bone: BoneId) -> Quat {
        self.model_rot.get(bone.index()).copied().unwrap_or(Quat::IDENTITY)
    }

    /// Rest model-space direction (+Y) of a bone.
    fn rest_dir(&self, bone: BoneId) -> Vec3 {
        self.model_dir.get(bone.index()).copied().unwrap_or(Vec3::Y)
    }

    /// A model-space axis expressed in a bone's rest frame.
    fn local(&self, bone: BoneId, axis: Vec3) -> Vec3 {
        (self.rest_rot(bone).inverse() * axis).normalize_or(Vec3::X)
    }

    /// Model-space axis for a chain rotation channel at `bone` (pitch / yaw / roll).
    fn chain_axis(&self, bone: BoneId, channel: Channel) -> Vec3 {
        let dir = self.rest_dir(bone);
        match channel {
            Channel::Pitch => {
                let a = dir.cross(Vec3::Y);
                if a.length_squared() < 1e-4 { Vec3::NEG_X } else { a.normalize() }
            }
            Channel::Yaw => Vec3::Y,
            _ => dir,
        }
    }

    /// Per-bone weights of a layer mask on this body.
    #[must_use]
    pub fn mask(&self, mask: &BodyMask) -> MaskWeights {
        let n = self.model_rot.len();
        let mut m = MaskWeights { bones: vec![0.0; n], root: 0.0, feet: 0.0, wings: 0.0 };
        let mut set = |bones: &[BoneId]| {
            for b in bones {
                if let Some(w) = m.bones.get_mut(b.index()) {
                    *w = 1.0;
                }
            }
        };
        let half = self.spine.len() / 2;
        let wing_bones: Vec<BoneId> = self.wings.iter().map(|w| w.2).collect();
        let head: Vec<BoneId> = self.head.into_iter().chain(self.jaw).collect();
        let pairs = |list: &[(Side, Vec<BoneId>)]| sided(list, Sides::Both);
        let (mut root, mut feet, mut wings) = (0.0, 0.0, 0.0);
        match mask {
            BodyMask::All => {
                m.bones.fill(1.0);
                (root, feet, wings) = (1.0, 1.0, 1.0);
            }
            BodyMask::Upper => {
                set(self.spine.get(half..).unwrap_or_default());
                for list in [&self.neck, &head, &wing_bones] {
                    set(list);
                }
                for list in [&self.ears, &self.antennae, &self.arms, &self.digits] {
                    set(&pairs(list));
                }
                wings = 1.0;
            }
            BodyMask::Lower => {
                set(self.spine.get(..half).unwrap_or_default());
                set(&self.tails.concat());
                set(&self.legs.concat());
                (root, feet) = (1.0, 1.0);
            }
            BodyMask::HeadNeck => {
                set(&self.neck);
                set(&head);
                set(&pairs(&self.ears));
                set(&pairs(&self.antennae));
            }
            BodyMask::Tail => set(&self.tails.concat()),
            BodyMask::Wings => {
                set(&wing_bones);
                wings = 1.0;
            }
            BodyMask::Arms(s) => {
                set(&sided(&self.arms, *s));
                set(&sided(&self.digits, *s));
            }
            BodyMask::Targets(targets) => {
                for t in targets {
                    match t {
                        Target::Root => root = 1.0,
                        Target::Legs(_) => {
                            feet = 1.0;
                            set(&self.legs.concat());
                        }
                        Target::Wings(_) => {
                            wings = 1.0;
                            set(&self.target_bones(*t));
                        }
                        _ => set(&self.target_bones(*t)),
                    }
                }
            }
        }
        (m.root, m.feet, m.wings) = (root, feet, wings);
        m
    }
}

/// A layer mask resolved on one body.
#[derive(Clone, Debug, Default)]
pub struct MaskWeights {
    pub bones: Vec<f32>,
    pub root: f32,
    pub feet: f32,
    pub wings: f32,
}

fn sided(list: &[(Side, Vec<BoneId>)], sides: Sides) -> Vec<BoneId> {
    list.iter().filter(|(s, _)| sides.includes(*s)).flat_map(|(_, b)| b.iter().copied()).collect()
}

/// Paired motion mirrors: positive sideways values always mean outward.
fn mirror(side: Side, channel: Channel) -> f32 {
    if side == Side::Right && matches!(channel, Channel::Yaw | Channel::Roll | Channel::Raise | Channel::Flap | Channel::Sweep | Channel::Spread) {
        -1.0
    } else {
        1.0
    }
}

/// One layer's contribution to the pose.
#[derive(Clone, Debug)]
pub struct LayerPose {
    /// Local rotation applied before IK (`local = local · rot`), per bone.
    pub rot: Vec<Quat>,
    /// Local rotation applied after leg IK (fetlocks, hocks), per bone.
    pub post: Vec<Quat>,
    /// How much this layer drives each bone, 0..1 (0 = untouched).
    pub touched: Vec<f32>,
    /// Whole-body offset (metres, mob space) and rotation.
    pub root_offset: Vec3,
    pub root_rot: Quat,
    /// Foot target offset per leg (metres, mob space).
    pub feet: Vec<Vec3>,
    /// Wing fold per wing rig as `(fold, weight)`: the body's rest fold moves toward `fold` by
    /// `weight` (0 = untouched).
    pub fold: Vec<(f32, f32)>,
}

impl LayerPose {
    #[must_use]
    pub fn identity(bones: usize, legs: usize, wings: usize) -> Self {
        Self {
            rot: vec![Quat::IDENTITY; bones],
            post: vec![Quat::IDENTITY; bones],
            touched: vec![0.0; bones],
            root_offset: Vec3::ZERO,
            root_rot: Quat::IDENTITY,
            feet: vec![Vec3::ZERO; legs],
            fold: vec![(0.0, 0.0); wings],
        }
    }

    /// Turns `bone` by `q` (after leg IK when `post`) and marks it driven.
    fn rotate(&mut self, bone: BoneId, q: Quat, post: bool) {
        let i = bone.index();
        let slot = if post { self.post.get_mut(i) } else { self.rot.get_mut(i) };
        if let (Some(slot), Some(touched)) = (slot, self.touched.get_mut(i)) {
            *slot *= q;
            *touched = 1.0;
        }
    }

    fn turn(&mut self, rig: &MotionRig, bone: BoneId, axis_model: Vec3, degrees: f32, post: bool) {
        self.rotate(bone, Quat::from_axis_angle(rig.local(bone, axis_model), degrees.to_radians()), post);
    }

    /// A `Flex` channel's turn: a declared hinge flexes its own way (positive closes it, an
    /// elbow forward and a knee back); any other joint turns about model -X.
    fn flex(&mut self, rig: &MotionRig, a: &Anatomy, bone: BoneId, degrees: f32, post: bool) {
        let Some(JointLimit::Hinge { flexes, .. }) = a.joint_limit(bone) else {
            return self.turn(rig, bone, Vec3::NEG_X, degrees, post);
        };
        self.rotate(bone, flexes.turn(degrees), post);
    }

    /// Adds `other` scaled by `weight`: how the cycles inside one layer mix (crossfades, blend
    /// spaces), where the weights sum to about one.
    pub fn accumulate(&mut self, other: &Self, weight: f32) {
        if weight <= 0.0 {
            return;
        }
        let ours = self.rot.iter_mut().zip(&mut self.post).zip(&mut self.touched);
        let theirs = other.rot.iter().zip(&other.post).zip(&other.touched);
        for (((rot, post), touched), ((their_rot, their_post), their_touched)) in ours.zip(theirs) {
            if *their_touched > 0.0 {
                *rot *= Quat::IDENTITY.slerp(*their_rot, weight);
                *post *= Quat::IDENTITY.slerp(*their_post, weight);
                *touched = their_touched.mul_add(weight, *touched).min(1.0);
            }
        }
        self.root_offset += other.root_offset * weight;
        self.root_rot *= Quat::IDENTITY.slerp(other.root_rot, weight);
        for (a, b) in self.feet.iter_mut().zip(&other.feet) {
            *a += *b * weight;
        }
        for (a, b) in self.fold.iter_mut().zip(&other.fold) {
            blend_fold(a, *b, weight);
        }
    }

    /// Stacks layer `other` on top by `weight` within `mask`: `additive` adds its rotations and
    /// offsets, otherwise it replaces wherever it drives a bone.
    pub fn compose(&mut self, other: &Self, weight: f32, mask: &MaskWeights, additive: bool) {
        let ours = self.rot.iter_mut().zip(&mut self.post).zip(&mut self.touched);
        let theirs = other.rot.iter().zip(&other.post).zip(&other.touched);
        for (i, (((rot, post), touched), ((their_rot, their_post), their_touched))) in ours.zip(theirs).enumerate() {
            let w = weight * mask.bones.get(i).copied().unwrap_or(1.0) * their_touched;
            if w <= 0.0 {
                continue;
            }
            if additive {
                *rot *= Quat::IDENTITY.slerp(*their_rot, w);
                *post *= Quat::IDENTITY.slerp(*their_post, w);
            } else {
                *rot = rot.slerp(*their_rot, w);
                *post = post.slerp(*their_post, w);
            }
            *touched = touched.max(w.min(1.0));
        }
        let (wr, wf) = (weight * mask.root, weight * mask.feet);
        if additive {
            self.root_offset += other.root_offset * wr;
            self.root_rot *= Quat::IDENTITY.slerp(other.root_rot, wr);
            for (a, b) in self.feet.iter_mut().zip(&other.feet) {
                *a += *b * wf;
            }
        } else {
            self.root_offset = self.root_offset.lerp(other.root_offset, wr);
            self.root_rot = self.root_rot.slerp(other.root_rot, wr);
            for (a, b) in self.feet.iter_mut().zip(&other.feet) {
                *a = a.lerp(*b, wf);
            }
        }
        for (a, b) in self.fold.iter_mut().zip(&other.fold) {
            blend_fold(a, *b, weight * mask.wings);
        }
    }
}

/// Moves a `(fold, weight)` toward another by `w`.
fn blend_fold(a: &mut (f32, f32), b: (f32, f32), w: f32) {
    let wb = b.1 * w;
    if wb <= 0.0 {
        return;
    }
    let total = a.1 + wb;
    a.0 = b.0.mul_add(wb, a.0 * a.1) / total;
    a.1 = total.min(1.0);
}

/// Inputs for evaluating a cycle at one moment.
pub struct EvalInput<'a> {
    pub anatomy: &'a Anatomy,
    pub rig: &'a MotionRig,
    /// Cycle time, 0..1.
    pub t: f32,
    /// Each leg's gait phase (for `per_leg` tracks); empty = use `t`.
    pub leg_phases: &'a [f32],
}

/// Evaluates every track of `cycle` at `input.t` into a fresh layer pose.
#[must_use]
pub fn evaluate(cycle: &Cycle, input: &EvalInput) -> LayerPose {
    let a = input.anatomy;
    let mut out = LayerPose::identity(a.skeleton.len(), a.legs.len(), a.wings.len());
    for track in &cycle.tracks {
        apply_track(track, input, &mut out);
    }
    out
}

fn apply_track(track: &Track, input: &EvalInput, out: &mut LayerPose) {
    let rig = input.rig;
    let value_at = |t: f32| track_value(track, t);
    let ch = track.channel;
    // A chain rotation spread over `bones`, the tip trailing by `lag`.
    let chain = |out: &mut LayerPose, bones: &[BoneId], side: Side| {
        let n = bones.len().max(1).to_f32();
        for (k, bone) in bones.iter().enumerate() {
            let along = if bones.len() > 1 { k.to_f32() / (n - 1.0) } else { 0.0 };
            let v = value_at(track.lag.mul_add(-along, input.t)) * mirror(side, ch) / n;
            out.turn(rig, *bone, rig.chain_axis(*bone, ch), v, false);
        }
    };
    match track.target {
        Target::Root => {
            let v = value_at(input.t);
            let l = rig.traits.leg_length;
            match ch {
                Channel::Bob => out.root_offset.y = f32::mul_add(v, l, out.root_offset.y),
                Channel::Sway => out.root_offset.x = f32::mul_add(v, l, out.root_offset.x),
                Channel::Surge => out.root_offset.z = f32::mul_add(v, l, out.root_offset.z),
                Channel::Pitch => out.root_rot *= Quat::from_axis_angle(Vec3::NEG_X, v.to_radians()),
                Channel::Roll => out.root_rot *= Quat::from_axis_angle(Vec3::Z, v.to_radians()),
                Channel::Yaw => out.root_rot *= Quat::from_axis_angle(Vec3::Y, v.to_radians()),
                _ => {}
            }
        }
        Target::Spine => chain(out, &rig.spine, Side::Center),
        Target::Neck => chain(out, &rig.neck, Side::Center),
        Target::Head => {
            if let Some(h) = rig.head {
                chain(out, &[h], Side::Center);
            }
        }
        Target::Jaw => {
            if let Some(j) = rig.jaw {
                let v = value_at(input.t);
                out.turn(rig, j, rig.chain_axis(j, Channel::Pitch), -35.0 * v, false);
            }
        }
        Target::Tail => {
            for tail in &rig.tails {
                chain(out, tail, Side::Center);
            }
        }
        Target::Ears(sides) | Target::Antennae(sides) => {
            let list = if matches!(track.target, Target::Ears(_)) { &rig.ears } else { &rig.antennae };
            for (side, bones) in list.iter().filter(|(s, _)| sides.includes(*s)) {
                if ch == Channel::Perk {
                    if let Some(root) = bones.first() {
                        out.turn(rig, *root, rig.chain_axis(*root, Channel::Pitch), 40.0 * value_at(input.t), false);
                    }
                } else {
                    chain(out, bones, *side);
                }
            }
        }
        Target::Legs(sel) => {
            apply_legs(track, input, out, sel);
        }
        Target::Arms(sides) => {
            apply_arms(track, input, out, sides);
        }
        Target::Wings(sides) => {
            for (side, index, root) in rig.wings.iter().filter(|w| sides.includes(w.0)) {
                let v = value_at(input.t);
                let m = mirror(*side, ch);
                match ch {
                    Channel::Fold => {
                        if let Some(fold) = out.fold.get_mut(*index) {
                            *fold = (v.clamp(0.0, 1.0), 1.0);
                        }
                    }
                    // Up/down about the body's long axis, back about the vertical.
                    Channel::Flap => out.turn(rig, *root, Vec3::Z * m, v, false),
                    Channel::Sweep => out.turn(rig, *root, Vec3::Y * m, v, false),
                    Channel::Roll => out.turn(rig, *root, rig.rest_dir(*root), v * m, false),
                    _ => {}
                }
            }
        }
        Target::Digits(sides) => {
            for (side, bones) in rig.digits.iter().filter(|(s, _)| sides.includes(*s)) {
                let v = value_at(input.t);
                for b in bones {
                    match ch {
                        // Close toward the palm: about each digit bone's own side axis.
                        Channel::Curl => {
                            let axis = rig.rest_rot(*b) * Vec3::X;
                            out.turn(rig, *b, axis, 70.0 * v, false);
                        }
                        Channel::Spread => {
                            let axis = rig.rest_rot(*b) * Vec3::Z;
                            out.turn(rig, *b, axis, v * mirror(*side, ch) * 0.5, false);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

const fn joint_index(j: Joint) -> usize {
    match j {
        Joint::Upper => 0,
        Joint::Middle => 1,
        Joint::Lower => 2,
        Joint::Tip => 3,
    }
}

/// A track's value at cycle time `t` (wrapped), scaled by its weight.
fn track_value(track: &Track, t: f32) -> f32 {
    track.curve.sample(t.rem_euclid(1.0).min(0.9999)) * track.weight
}

/// Leg `i`'s gait phase (the cycle time without per-leg phases).
fn leg_phase(input: &EvalInput, i: usize) -> f32 {
    input.leg_phases.get(i).copied().unwrap_or(input.t)
}

/// [`apply_track`] for `Target::Legs`.
fn apply_legs(track: &Track, input: &EvalInput, out: &mut LayerPose, sel: LegSel) {
    let (rig, a, ch) = (input.rig, input.anatomy, track.channel);
    let value_at = |t: f32| track_value(track, t);
    let leg_phase = |i: usize| leg_phase(input, i);
    for (i, leg) in a.legs.iter().enumerate() {
        let chosen = match sel {
            LegSel::All => true,
            LegSel::Fore => leg.pair == rig.fore_pair,
            LegSel::Hind => leg.pair == rig.hind_pair,
            LegSel::Side(s) => s.includes(leg.side),
        };
        if !chosen {
            continue;
        }
        let t = if track.per_leg { leg_phase(i) } else { input.t };
        let v = value_at(t);
        let l = rig.traits.leg_length;
        match ch {
            Channel::FootLift => {
                if let Some(foot) = out.feet.get_mut(i) {
                    foot.y = f32::mul_add(v, l, foot.y);
                }
            }
            Channel::FootReach => {
                if let Some(foot) = out.feet.get_mut(i) {
                    foot.z = f32::mul_add(v, l, foot.z);
                }
            }
            Channel::Flex(joint) => {
                let idx = joint_index(joint);
                if let Some(bone) = leg.chain.get(idx) {
                    out.flex(rig, a, *bone, v, true);
                }
            }
            _ => {}
        }
    }
}

/// [`apply_track`] for `Target::Arms`.
fn apply_arms(track: &Track, input: &EvalInput, out: &mut LayerPose, sides: Sides) {
    let (rig, a, ch) = (input.rig, input.anatomy, track.channel);
    let value_at = |t: f32| track_value(track, t);
    let leg_phase = |i: usize| leg_phase(input, i);
    for (side, bones) in rig.arms.iter().filter(|(s, _)| sides.includes(*s)) {
        // Arms swing against the opposite-side leg.
        let t = if track.per_leg {
            a.legs.iter().position(|l| l.side != *side && l.side != Side::Center).map_or(input.t, leg_phase)
        } else {
            input.t
        };
        let v = value_at(t) * mirror(*side, ch);
        if let Channel::Flex(joint) = ch {
            if let Some(b) = bones.get(joint_index(joint)) {
                out.flex(rig, a, *b, v, false);
            }
            continue;
        }
        let Some(&b) = bones.first() else { continue };
        let axis = match ch {
            Channel::Swing => Vec3::NEG_X,
            // About the long axis; `v` is already mirrored, so + is outward.
            Channel::Raise => Vec3::Z,
            Channel::Roll => rig.rest_dir(b),
            _ => continue,
        };
        out.turn(rig, b, axis, v, false);
    }
}
