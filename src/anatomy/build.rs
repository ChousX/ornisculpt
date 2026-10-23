use std::ops::Range;

use bevy::prelude::*;

use smallvec::SmallVec;

use super::{
    AxisSample, ClothFollow, CrowdSkeleton, Sheet, WingRig, MAX_CROWD_BONES, AnatomyError, BodyPlan, BoneId, HardPart, JointLimit, LegInfo, LimbRole, LodLevel,
    LookDef, PartDetail, PartLodRules, PartPath, Primitive, RegionTable, Skeleton, SocketDef,
    SpringChainDef, Tone,
};
use super::{AnatomyBuilder, AttachPoint, HardParams, PartAssets, PartBuild, PartNode};
use crate::num::{Clamp, Idx};

/// A fully resolved mob body: everything later stages (surface, motion, equip) consume.
/// Immutable once built; shared behind an `Arc`.
#[derive(Clone, Debug, Default)]
pub struct Anatomy {
    pub skeleton: Skeleton,
    pub primitives: Vec<Primitive>,
    pub hard_parts: Vec<HardPart>,
    pub sockets: Vec<SocketDef>,
    pub regions: RegionTable,
    pub legs: Vec<LegInfo>,
    /// Shoulder girdles of manipulator arms.
    pub clavicles: Vec<super::Clavicle>,
    /// Torso bones, rear to front.
    pub spine: Vec<BoneId>,
    pub springs: Vec<SpringChainDef>,
    /// Loose-cloth chains that swing with the legs (added by clothing).
    pub cloth_follow: Vec<ClothFollow>,
    /// Thin generated surfaces: wing membranes, flight feathers, insect wings.
    pub sheets: Vec<Sheet>,
    /// Wing fold rigs: folded local rotations blended in by the pose.
    pub wings: Vec<WingRig>,
    pub look: Option<LookDef>,
    /// Digits parts, for posing fingers and toes.
    pub hands: Vec<super::HandDef>,
    pub joint_limits: Vec<(BoneId, JointLimit)>,
    /// Weight per bone and centre of mass (rest pose).
    pub mass: super::BodyMass,
    /// Depth-first order; a parent precedes its children.
    pub parts: Vec<BuiltPart>,
    /// How far the body was lowered so its lowest point rests on y = 0 (metres; negative when
    /// raised). Everything above is stored after it.
    pub ground: f32,
}

/// What one part produced, as index ranges into [`Anatomy`]'s arrays.
#[derive(Clone, Debug)]
pub struct BuiltPart {
    pub path: PartPath,
    pub parent: Option<u16>,
    pub role: Option<LimbRole>,
    /// Meshed as a surface of its own that joins its parent only at its root (a limb, wing or
    /// tail; see [`PartKind::apart`](super::PartKind::apart)).
    pub apart: bool,
    pub detail: PartDetail,
    /// Resolved: the coarsest level this part appears in.
    pub lod: LodLevel,
    /// Body tone with this part's Muscle/Fat/Folds levers applied.
    pub tone: Tone,
    pub bones: Range<u8>,
    pub primitives: Range<u32>,
    pub hard_parts: Range<u16>,
    pub regions: Range<u8>,
    /// Axis samples along the part, logical model space (empty for parts without an axis).
    pub axis: SmallVec<[AxisSample; 9]>,
    /// Built mirrored: stored transforms are the logical ones reflected across x = 0.
    pub mirrored: bool,
    /// Relative to water; see [`PartNode::density`](super::PartNode::density).
    pub density: f32,
}

impl Anatomy {
    /// The limit a body declares for `bone`'s joint, if any.
    #[must_use]
    pub fn joint_limit(&self, bone: BoneId) -> Option<JointLimit> {
        self.joint_limits.iter().find(|(b, _)| *b == bone).map(|(_, l)| *l)
    }

    /// The idle pose: every bone at rest, wings folded by their rest fold (local transforms, one
    /// per bone).
    #[must_use]
    pub fn idle_pose(&self) -> Vec<Transform> {
        let mut pose: Vec<Transform> = self.skeleton.bones.iter().map(|b| b.rest).collect();
        for w in &self.wings {
            w.apply(w.fold, &mut pose);
        }
        pose
    }

    #[must_use]
    pub fn part(&self, path: &PartPath) -> Option<&BuiltPart> {
        self.parts.iter().find(|p| &p.path == path)
    }

    /// Recomputes what derives from the bone list: inverse bind poses and the crowd skeleton
    /// (bones of parts present at `Near` or coarser, plus `extra_crowd` bones while the crowd
    /// budget allows; the rest fold into their nearest kept ancestor).
    pub fn finish_skeleton(&mut self, extra_crowd: &[BoneId]) {
        let model = self.skeleton.model_rest();
        self.skeleton.inverse_bind = model.iter().map(|m| Mat4::from(*m).inverse()).collect();
        self.skeleton.rest_model = model;
        let mut crowd = CrowdSkeleton { map: Vec::with_capacity(self.skeleton.len()), bones: Vec::new() };
        for (id, bone) in (0u8..).map(BoneId).zip(&self.skeleton.bones) {
            // The crowd bone buffer holds MAX_CROWD_BONES per mob: past that, bones fold too.
            let room = crowd.bones.len() < MAX_CROWD_BONES;
            let kept = bone.parent.is_none()
                || (room && (extra_crowd.contains(&id) || self.parts.get(bone.part.idx()).is_some_and(|p| p.lod >= LodLevel::Near)));
            let mapped = if kept {
                crowd.bones.push(id);
                (crowd.bones.len() - 1).clamp_u8()
            } else {
                // Not kept, so not the root: fold into the parent's crowd bone.
                bone.parent.and_then(|p| crowd.map.get(p.index())).copied().unwrap_or_default()
            };
            crowd.map.push(mapped);
        }
        self.skeleton.crowd = crowd;
    }
}

impl Anatomy {
    /// `plan` in its default form, built: [`apply_form`](super::apply_form) with [`Form::default`],
    /// then [`build_anatomy`].
    ///
    /// # Errors
    ///
    /// When the default form doesn't apply ([`AnatomyError::Form`]) or the plan can't be built.
    pub fn from_plan(plan: &BodyPlan, lod_rules: &PartLodRules, assets: &PartAssets) -> Result<Self, AnatomyError> {
        build_anatomy(&super::apply_form(plan, &super::Form::default())?, lod_rules, assets)
    }
}

/// Expanded, form-applied plan (see [`apply_form`](super::apply_form)) → `Anatomy`.
///
/// Pure function of its inputs: no RNG, no seed. Randomness (skin noise, coat variation) belongs
/// to later stages, so identical bodies always share one anatomy and one base mesh.
/// `assets` must contain every mesh the plan references (see [`referenced_assets`]).
///
/// # Errors
///
/// When the plan can't be built: a missing asset, a bone budget overrun, or an invalid part.
pub fn build_anatomy(live: &BodyPlan, lod_rules: &PartLodRules, assets: &PartAssets) -> Result<Anatomy, AnatomyError> {
    let mut b = AnatomyBuilder::new(live.tone, *lod_rules, assets.clone());
    let root = AttachPoint { bone: None, model: Transform::IDENTITY };
    walk(&mut b, &live.root, PartPath::root(&live.root.name), root)?;
    b.finish()
}

/// Asset paths of every authored mesh a plan references (sculpts and mesh hard parts).
#[must_use]
pub fn referenced_assets(plan: &BodyPlan) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mesh_path = |h: &HardParams| match &h.kind {
        super::HardKind::Mesh { path, .. } => Some(path.clone()),
        _ => None,
    };
    plan.visit(|_, node| {
        let path = match &node.kind {
            super::PartKind::Sculpt(s) => Some(s.mesh.clone()),
            super::PartKind::Hard(h) => mesh_path(h),
            super::PartKind::Digits(d) => d.tip.as_ref().and_then(mesh_path),
            _ => None,
        };
        if let Some(p) = path
            && !out.contains(&p) {
                out.push(p);
            }
    });
    out
}

fn walk(b: &mut AnatomyBuilder, node: &PartNode, path: PartPath, at: AttachPoint) -> Result<(), AnatomyError> {
    let cx = b.begin_part(node, path.clone())?;
    let slots = node.kind.build(b, at, &cx)?;
    b.set_axis(&slots.axis);
    super::tone::apply_stamps(b, &node.kind, &node.stamps, &slots, &cx);
    b.seal_part();
    for child in &node.children {
        let child_path = path.child(&child.name);
        let at = slots
            .resolve(child.attach)
            .ok_or_else(|| AnatomyError::MissingAttachSlot { path: child_path.clone(), slot: child.attach })?;
        walk(b, child, child_path, at)?;
    }
    b.end_part();
    Ok(())
}
