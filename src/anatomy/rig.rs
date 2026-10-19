//! Motion-facing descriptions discovered while building parts. Consumed by the `motion` module.

use bevy::prelude::*;
use smallvec::SmallVec;

use super::{BoneId, Side, Stance};

/// A locomotor limb. Gaits assign phase from `(pair, side)`, never from bone names.
#[derive(Clone, Debug, PartialEq)]
pub struct LegInfo {
    /// Root (hip/shoulder) to foot, inclusive.
    pub chain: SmallVec<[BoneId; 4]>,
    pub foot: BoneId,
    /// 0 = front-most pair.
    pub pair: u8,
    pub side: Side,
    pub stance: Stance,
    /// Rest foot position in mob root space.
    pub rest_foot: Vec3,
    /// IK pole direction in mob root space (knee/elbow direction).
    pub pole: Vec3,
    /// Hip-to-foot rest length.
    pub length: f32,
}

/// Secondary-motion chain (tails, ears, antennae).
#[derive(Clone, Debug, PartialEq)]
pub struct SpringChainDef {
    pub bones: SmallVec<[BoneId; 12]>,
    pub stiffness: f32,
    pub damping: f32,
    pub gravity: f32,
}

/// A loose-cloth chain whose root turns with a leg's upper bone, so skirts and robes swing with
/// the thighs instead of being walked through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClothFollow {
    pub root: BoneId,
    /// Index into [`Anatomy::legs`](super::Anatomy::legs).
    pub leg: u8,
    /// Share of the leg's swing the chain takes, 0..1.
    pub weight: f32,
}

/// Neck/head bones that turn toward a look target, each taking a share of the rotation.
#[derive(Clone, Debug, PartialEq)]
pub struct LookDef {
    pub bones: SmallVec<[(BoneId, f32); 4]>,
    pub limit_deg: f32,
}

/// A shoulder-girdle bone between the torso and an arm. Arm raises past ~30° share part of their
/// rotation with it, so the shoulder lifts the way a scapula does (see `motion`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clavicle {
    pub bone: BoneId,
    /// The arm's first bone (its shoulder joint), a child of `bone`.
    pub arm: BoneId,
    pub side: Side,
}

/// The digits of one Digits part (fingers on a hand, toes on a foot), for posing them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandDef {
    pub side: Side,
    /// Built mirrored: a sideways turn (spread) goes the other way.
    pub mirrored: bool,
    /// Each digit's bones, knuckle to tip, in build order (the thumb first when `thumb`).
    pub digits: Vec<SmallVec<[BoneId; 4]>>,
    /// The first digit is an opposable thumb.
    pub thumb: bool,
}
