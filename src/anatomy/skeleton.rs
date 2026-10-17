use bevy::math::Affine3A;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{RegionId, Side};

/// Hard cap on bones per mob at hero detail (fingers included).
pub const MAX_BONES: usize = 64;
/// Cap on bones in the crowd skeleton (finer parts folded into ancestors).
pub const MAX_CROWD_BONES: usize = 32;

/// Index into [`Skeleton::bones`].
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoneId(pub u8);

impl BoneId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Bones in topological order: a parent's index is always lower than its children's.
#[derive(Clone, Debug, Default)]
pub struct Skeleton {
    pub bones: Vec<BoneDef>,
    /// Model-space rest pose → bone space, one per bone.
    pub inverse_bind: Vec<Mat4>,
    pub crowd: CrowdSkeleton,
}

#[derive(Clone, Debug)]
pub struct BoneDef {
    /// e.g. `"torso.2"`, `"limb.front.l.1"`.
    pub name: String,
    pub parent: Option<BoneId>,
    /// Local rest transform, relative to the parent bone.
    pub rest: Transform,
    /// Extends along local +Y.
    pub length: f32,
    pub region: RegionId,
    /// Index into [`Anatomy::parts`](super::Anatomy::parts).
    pub part: u16,
}

/// Mapping from the full skeleton to the reduced crowd skeleton. The crowd skeleton keeps the bones
/// of parts whose LOD is `Near` or coarser; all other bones fold into the nearest kept ancestor.
#[derive(Clone, Debug, Default)]
pub struct CrowdSkeleton {
    /// Full bone index → crowd bone index.
    pub map: Vec<u8>,
    /// Crowd bone index → full bone.
    pub bones: Vec<BoneId>,
}

impl Skeleton {
    pub fn len(&self) -> usize {
        self.bones.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    pub fn get(&self, id: BoneId) -> &BoneDef {
        &self.bones[id.index()]
    }

    pub fn find(&self, name: &str) -> Option<BoneId> {
        self.bones.iter().position(|b| b.name == name).map(|i| BoneId(i as u8))
    }

    pub fn children(&self, id: BoneId) -> impl Iterator<Item = BoneId> + '_ {
        self.bones
            .iter()
            .enumerate()
            .filter(move |(_, b)| b.parent == Some(id))
            .map(|(i, _)| BoneId(i as u8))
    }

    /// Model-space rest transform of every bone (forward kinematics over `rest`).
    pub fn model_rest(&self) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.bones.len());
        for b in &self.bones {
            let local = b.rest.compute_affine();
            out.push(match b.parent {
                Some(p) => out[p.index()] * local,
                None => local,
            });
        }
        out
    }

    /// Bones from `root` down to `tip`, inclusive. `None` if `tip` is not under `root`.
    pub fn chain(&self, root: BoneId, tip: BoneId) -> Option<Vec<BoneId>> {
        let mut chain = vec![tip];
        let mut cur = tip;
        while cur != root {
            cur = self.get(cur).parent?;
            chain.push(cur);
        }
        chain.reverse();
        Some(chain)
    }
}

/// Swing/twist limits for IK and ragdolls, in degrees.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct JointLimit {
    /// Min/max swing around local X and Z.
    pub swing_deg: Vec2,
    /// Min/max twist around local Y.
    pub twist_deg: Vec2,
}

/// Attachment point for tools (and later clothing anchors).
#[derive(Clone, Debug, PartialEq)]
pub struct SocketDef {
    /// e.g. `"grip.r"`, `"back"`.
    pub name: String,
    pub kind: SocketKind,
    pub side: Side,
    pub bone: BoneId,
    pub local: Transform,
    /// Characteristic size (palm width, back width) for tool scaling.
    pub size: f32,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SocketKind {
    /// Hands and pincers.
    Grip,
    Mouth,
    Head,
    Neck,
    Shoulder,
    Chest,
    Back,
    /// Riding position on the back.
    Saddle,
    Hip,
    Wrist,
    TailTip,
}
