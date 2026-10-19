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
    #[must_use]
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Bones in topological order: a parent's index is always lower than its children's.
#[derive(Clone, Debug, Default)]
pub struct Skeleton {
    pub bones: Vec<BoneDef>,
    /// Model-space rest pose → bone space, one per bone.
    pub inverse_bind: Vec<Mat4>,
    /// Model-space rest transform of every bone, cached by [`Anatomy::finish_skeleton`](super::Anatomy::finish_skeleton).
    pub rest_model: Vec<Affine3A>,
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
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bones.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: BoneId) -> Option<&BoneDef> {
        self.bones.get(id.index())
    }

    #[must_use]
    pub fn find(&self, name: &str) -> Option<BoneId> {
        (0u8..).zip(&self.bones).find(|(_, b)| b.name == name).map(|(i, _)| BoneId(i))
    }

    pub fn children(&self, id: BoneId) -> impl Iterator<Item = BoneId> + '_ {
        (0u8..).zip(&self.bones).filter(move |(_, b)| b.parent == Some(id)).map(|(i, _)| BoneId(i))
    }

    /// The cached rest pose, or a fresh one if the bones changed since it was cached.
    #[must_use]
    pub fn rest_model(&self) -> std::borrow::Cow<'_, [Affine3A]> {
        if self.rest_model.len() == self.bones.len() { self.rest_model.as_slice().into() } else { self.model_rest().into() }
    }

    /// Model-space rest transform of every bone (forward kinematics over `rest`).
    #[must_use]
    pub fn model_rest(&self) -> Vec<Affine3A> {
        self.model_from(&[])
    }

    /// Model-space transform of every bone for the local pose `local` (forward kinematics); bones
    /// past the end of `local` keep their rest pose.
    #[must_use]
    pub fn model_from(&self, local: &[Transform]) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let l = local.get(i).unwrap_or(&b.rest).compute_affine();
            let parent = b.parent.and_then(|p| out.get(p.index()));
            out.push(parent.map_or(l, |m| *m * l));
        }
        out
    }

    /// Bones from `root` down to `tip`, inclusive. `None` if `tip` is not under `root`.
    #[must_use]
    pub fn chain(&self, root: BoneId, tip: BoneId) -> Option<Vec<BoneId>> {
        let mut chain = vec![tip];
        let mut cur = tip;
        while cur != root {
            cur = self.get(cur)?.parent?;
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
