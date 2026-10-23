use bevy::math::{Affine3A, Mat3};
use bevy::prelude::*;
use smallvec::SmallVec;

use crate::num::{Clamp, Idx, min_axis_scale};

use super::{
    Anatomy, AnatomyError, AttachSlot, BoneDef, BoneId, BuiltPart, ClothFollow, HardParams,
    HardPart, JointLimit, LegInfo, LimbRole, LodLevel, LookDef, MAX_BONES, PartKind,
    PartLodRules, PartNode, PartPath, PrimLayer, PrimOp, PrimShape, Primitive, RegionDef,
    PartAssets, RegionId, RegionKind, Sheet, Side, SocketDef, SocketKind, SpringChainDef, Stance, Tone,
    Weigh, WingRig,
};

/// Implemented by every part kind's params. Emits bones, primitives, sockets and rig info into the
/// builder and returns the attach slots its children may use.
///
/// Parts work in the *logical* model frame: Y up, mob facing +Z, left = +X. Mirrored copies are
/// built exactly like their left twin; the builder reflects them on storage.
pub trait PartBuild {
    /// Builds the part attached at `at`.
    ///
    /// # Errors
    ///
    /// When the part exceeds a budget (bones, regions) or its parameters are invalid.
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError>;
}

/// Per-part build context.
#[derive(Clone, Debug)]
pub struct BuildCx {
    pub path: PartPath,
    /// Physical side (inherited from the nearest mirrored ancestor).
    pub side: Side,
    /// Body tone with this part's multipliers applied.
    pub tone: Tone,
    /// The node's authored offset. Most parts apply it as `at.model * offset`.
    pub offset: Transform,
}

/// Where a part starts, in the logical model frame. `model`'s +Y points away from the parent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AttachPoint {
    /// `None` only for the root part.
    pub bone: Option<BoneId>,
    pub model: Transform,
}

/// A sample on the central axis of a segmented part. `model` has +Y along the axis and
/// local -Z toward the dorsal side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisSample {
    pub t: f32,
    pub bone: BoneId,
    pub model: Transform,
    /// Cross-section radii: x = lateral (local X), y = dorsal/ventral (local Z).
    pub radii: Vec2,
    /// Ventral offset (local +Z, metres) of the cross-section's centre from the axis: a chest
    /// hanging below the spine (see [`Profile::drop`](super::Profile::drop)).
    pub drop: f32,
}

impl AxisSample {
    /// Round cross-section.
    #[must_use]
    pub const fn round(t: f32, bone: BoneId, model: Transform, radius: f32) -> Self {
        Self { t, bone, model, radii: Vec2::splat(radius), drop: 0.0 }
    }

    /// Centre of the cross-section (model space): the axis point moved by `drop`.
    #[must_use]
    pub fn centre(&self) -> Vec3 {
        self.model.translation + self.model.rotation * Vec3::Z * self.drop
    }

    /// Outward direction `around` the section (radians; 0 = dorsal, π/2 = local +X).
    #[must_use]
    pub fn normal_toward(&self, around: f32) -> Vec3 {
        let dorsal = self.model.rotation * Vec3::NEG_Z;
        let side = self.model.rotation * Vec3::X;
        (dorsal * around.cos() + side * around.sin()).normalize()
    }

    /// Surface point `around` the section (radians; 0 = dorsal, π/2 = local +X): from the
    /// section's centre out along [`normal_toward`](Self::normal_toward).
    #[must_use]
    pub fn surface_toward(&self, around: f32) -> Vec3 {
        self.centre() + self.normal_toward(around) * self.radius_toward(around)
    }

    /// Distance from the axis to the surface at `around` (radians; 0 = dorsal, π/2 = left).
    #[must_use]
    pub fn radius_toward(&self, around: f32) -> f32 {
        let (c, s) = (around.cos() / self.radii.y, around.sin() / self.radii.x);
        1.0 / c.hypot(s)
    }

    /// Smallest radius (for sizes that should not depend on direction).
    #[must_use]
    pub fn min_radius(&self) -> f32 {
        self.radii.min_element()
    }
}

/// Attach slots a built part exposes to its children.
#[derive(Clone, Debug, Default)]
pub struct PartSlots {
    /// Fixed slots: `Start`, `End`, `Jaw`, `Eye`, `Ear`, `Horn`, `Mouth`, `Nose`, `Crown`.
    pub fixed: SmallVec<[(AttachSlot, AttachPoint); 6]>,
    /// Axis samples (sorted by `t`) for `Along` and `Surface`; empty if unsupported.
    pub axis: SmallVec<[AxisSample; 9]>,
    /// Ellipsoidal skull frame for `StampAt::Skull` (heads only).
    pub skull: Option<SkullFrame>,
}

/// A head's skull ellipsoid: centre frame (x left, y forward, z down) and radii along those axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkullFrame {
    pub bone: BoneId,
    pub model: Transform,
    pub radii: Vec3,
}

impl PartSlots {
    #[must_use]
    pub fn resolve(&self, slot: AttachSlot) -> Option<AttachPoint> {
        match slot {
            AttachSlot::Along(t) => {
                let s = self.sample(t)?;
                Some(AttachPoint { bone: Some(s.bone), model: s.model })
            }
            AttachSlot::Surface { t, around_deg } => {
                let s = self.sample(t)?;
                let a = around_deg.to_radians();
                let normal = s.normal_toward(a);
                let along = s.model.rotation * Vec3::Y;
                Some(AttachPoint {
                    bone: Some(s.bone),
                    model: Transform::from_translation(s.surface_toward(a)).with_rotation(frame_y_z(normal, along)),
                })
            }
            fixed => self.fixed.iter().find(|(s, _)| *s == fixed).map(|(_, p)| *p),
        }
    }

    /// Only an `End` slot, at `model` on `bone`.
    #[must_use]
    pub fn end(bone: BoneId, model: Transform) -> Self {
        Self { fixed: std::iter::once((AttachSlot::End, AttachPoint { bone: Some(bone), model })).collect(), ..default() }
    }

    pub(crate) fn sample(&self, t: f32) -> Option<AxisSample> {
        sample_axis(&self.axis, t)
    }
}

/// Interpolates a part's axis samples (sorted by `t`) at `t`; clamps to the ends.
#[must_use]
pub fn sample_axis(axis: &[AxisSample], t: f32) -> Option<AxisSample> {
    let (first, last) = (axis.first()?, axis.last()?);
    if t <= first.t {
        return Some(*first);
    }
    if t >= last.t {
        return Some(*last);
    }
    let next = axis.iter().position(|s| s.t > t)?;
    let (a, b) = (axis.get(next.checked_sub(1)?)?, axis.get(next)?);
    let k = (t - a.t) / (b.t - a.t);
    Some(AxisSample {
        t,
        bone: if k < 0.5 { a.bone } else { b.bone },
        model: Transform::from_translation(a.model.translation.lerp(b.model.translation, k))
            .with_rotation(a.model.rotation.slerp(b.model.rotation, k)),
        radii: a.radii.lerp(b.radii, k),
        drop: a.drop.lerp(b.drop, k),
    })
}

/// Rotation whose +Y is `y` and whose +Z is `z_hint` made orthogonal to it.
#[must_use]
pub fn frame_y_z(y: Vec3, z_hint: Vec3) -> Quat {
    let y = y.normalize();
    let mut z = z_hint - y * z_hint.dot(y);
    if z.length_squared() < 1e-8 {
        z = y.any_orthonormal_vector();
    }
    let z = z.normalize();
    Quat::from_mat3(&Mat3::from_cols(y.cross(z), y, z))
}

/// Reflection across the model YZ plane, as a proper rotation (conjugation by `diag(-1, 1, 1)`).
#[must_use]
pub fn mirror_transform(t: Transform) -> Transform {
    let q = t.rotation;
    Transform {
        translation: Vec3::new(-t.translation.x, t.translation.y, t.translation.z),
        rotation: Quat::from_xyzw(q.x, -q.y, -q.z, q.w),
        scale: t.scale,
    }
}

#[must_use]
pub fn mirror_point(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, p.z)
}

#[must_use]
pub fn relative(parent: &Transform, child: &Transform) -> Transform {
    Transform::from_matrix((parent.compute_affine().inverse() * child.compute_affine()).into())
}

struct PartFrame {
    index: u16,
    mirrored: bool,
    side: Side,
}

/// Accumulates an [`Anatomy`] while the part tree is walked.
pub struct AnatomyBuilder {
    anatomy: Anatomy,
    assets: PartAssets,
    /// Stored (physical) model-space rest transform per bone.
    model: Vec<Transform>,
    stack: Vec<PartFrame>,
    body_tone: Tone,
    lod_rules: PartLodRules,
}

impl AnatomyBuilder {
    #[must_use]
    pub fn new(body_tone: Tone, lod_rules: PartLodRules, assets: PartAssets) -> Self {
        Self { anatomy: Anatomy::default(), assets, model: Vec::new(), stack: Vec::new(), body_tone, lod_rules }
    }

    /// Authored meshes available to this build.
    #[must_use]
    pub const fn assets(&self) -> &PartAssets {
        &self.assets
    }

    /// Whether the part being built is a reflected (right-side) copy.
    #[must_use]
    pub fn mirrored(&self) -> bool {
        self.stack.last().is_some_and(|f| f.mirrored)
    }

    /// Index of the part in progress (the root part if none is, which only a builder bug causes).
    fn current(&self) -> u16 {
        self.stack.last().map_or(0, |f| f.index)
    }

    fn current_part_mut(&mut self) -> Option<&mut BuiltPart> {
        let current = self.stack.last()?.index;
        self.anatomy.parts.get_mut(current.idx())
    }

    /// Stored model-space rest transform of a bone this builder made (identity for any other).
    fn bone_model(&self, bone: BoneId) -> Transform {
        self.model.get(bone.index()).copied().unwrap_or_default()
    }

    fn to_stored(&self, logical: Transform) -> Transform {
        if self.mirrored() { mirror_transform(logical) } else { logical }
    }

    /// `model` (logical) relative to `bone`'s stored rest transform.
    fn local_of(&self, bone: BoneId, model: Transform) -> Transform {
        relative(&self.bone_model(bone), &self.to_stored(model))
    }

    fn point_to_stored(&self, p: Vec3) -> Vec3 {
        if self.mirrored() { mirror_point(p) } else { p }
    }

    /// Starts a part; everything added until [`seal_part`](Self::seal_part) belongs to it.
    ///
    /// # Errors
    ///
    /// When a part with the same path already exists.
    pub fn begin_part(&mut self, node: &PartNode, path: PartPath) -> Result<BuildCx, AnatomyError> {
        if self.anatomy.parts.iter().any(|p| p.path == path) {
            return Err(AnatomyError::DuplicatePath(path));
        }
        let parent = self.stack.last();
        let mirrored = parent.is_some_and(|f| f.mirrored) ^ (node.side == Side::Right);
        let side = match node.side {
            Side::Center => parent.map_or(Side::Center, |f| f.side),
            s => s,
        };
        let tone = node.tone.apply(&self.body_tone);
        let index = self.anatomy.parts.len().clamp_u16();
        let (bones, prims, hard, regions) = self.counts();
        self.anatomy.parts.push(BuiltPart {
            path: path.clone(),
            parent: parent.map(|f| f.index),
            role: node.kind.role(),
            detail: node.detail,
            lod: LodLevel::Hero,
            tone,
            bones: bones..bones,
            primitives: prims..prims,
            hard_parts: hard..hard,
            regions: regions..regions,
            axis: SmallVec::new(),
            mirrored,
            density: node.density,
        });
        self.stack.push(PartFrame { index, mirrored, side });
        Ok(BuildCx { path, side, tone, offset: node.offset.to_transform() })
    }

    /// Closes the current part's own ranges (before its children are built).
    pub fn seal_part(&mut self) {
        let (bones, prims, hard, regions) = self.counts();
        let Some(p) = self.current_part_mut() else { return };
        p.bones.end = bones;
        p.primitives.end = prims;
        p.hard_parts.end = hard;
        p.regions.end = regions;
    }

    /// Records the current part's axis samples (logical model space) for later fitting.
    pub fn set_axis(&mut self, axis: &[AxisSample]) {
        if let Some(p) = self.current_part_mut() {
            p.axis = axis.iter().copied().collect();
        }
    }

    pub fn end_part(&mut self) {
        self.stack.pop();
    }

    /// Bones, primitives, hard parts and regions so far (each within its budget's type).
    fn counts(&self) -> (u8, u32, u16, u8) {
        (
            self.anatomy.skeleton.bones.len().clamp_u8(),
            self.anatomy.primitives.len().clamp_u32(),
            self.anatomy.hard_parts.len().clamp_u16(),
            self.anatomy.regions.regions.len().clamp_u8(),
        )
    }

    /// Adds a bone whose rest pose is `model` (logical model space; +Y along the bone).
    ///
    /// # Errors
    ///
    /// When the skeleton already has [`MAX_BONES`] bones.
    pub fn bone(
        &mut self,
        parent: Option<BoneId>,
        name: &str,
        model: Transform,
        length: f32,
        region: RegionId,
    ) -> Result<BoneId, AnatomyError> {
        let count = self.anatomy.skeleton.bones.len();
        let Some(id) = u8::try_from(count).ok().filter(|_| count < MAX_BONES) else {
            return Err(AnatomyError::BoneBudget { got: count + 1, max: MAX_BONES });
        };
        let stored = self.to_stored(Transform { scale: Vec3::ONE, ..model });
        let rest = parent.map_or(stored, |p| relative(&self.bone_model(p), &stored));
        self.anatomy.skeleton.bones.push(BoneDef {
            name: name.to_owned(),
            parent,
            rest,
            length,
            region,
            part: self.current(),
        });
        self.model.push(stored);
        Ok(BoneId(id))
    }

    #[must_use]
    pub fn bone_region(&self, bone: BoneId) -> RegionId {
        self.anatomy.skeleton.get(bone).map_or(RegionId(0), |b| b.region)
    }

    /// Adds (or reuses) a region of the current part.
    ///
    /// # Errors
    ///
    /// When the body already has [`RegionId::MAX`] regions, or no part is in progress.
    pub fn region(&mut self, kind: RegionKind, side: Side) -> Result<RegionId, AnatomyError> {
        let current = self.stack.last().map(|f| f.index).ok_or(AnatomyError::Internal("region outside a part"))?;
        let path = &self.anatomy.parts.get(current.idx()).ok_or(AnatomyError::Internal("part in progress missing"))?.path;
        let regions = &mut self.anatomy.regions.regions;
        if let Some(i) = regions.iter().position(|r| &r.part == path && r.kind == kind && r.side == side) {
            return Ok(RegionId(i.clamp_u8()));
        }
        if regions.len() >= RegionId::MAX {
            return Err(AnatomyError::RegionBudget { got: regions.len() + 1, max: RegionId::MAX });
        }
        regions.push(RegionDef { part: path.clone(), kind, side });
        Ok(RegionId(regions.len().saturating_sub(1).clamp_u8()))
    }

    /// Adds an SDF primitive placed at `model` (logical), owned by `bone`.
    #[allow(clippy::too_many_arguments)]
    pub fn primitive(
        &mut self,
        bone: BoneId,
        shape: PrimShape,
        model: Transform,
        blend: f32,
        op: PrimOp,
        layer: PrimLayer,
        region: RegionId,
    ) {
        self.primitive_at(bone, shape, model, blend, op, layer, region, LodLevel::Far);
    }

    /// A part's own base shape: a [`primitive`](Self::primitive) unioned on the base layer.
    pub fn base(&mut self, bone: BoneId, shape: PrimShape, model: Transform, blend: f32, region: RegionId) {
        self.primitive(bone, shape, model, blend, PrimOp::Union, PrimLayer::Base, region);
    }

    /// [`primitive`](Self::primitive) that only exists down to `lod` (used by tone stamps).
    #[allow(clippy::too_many_arguments)]
    pub fn primitive_at(
        &mut self,
        bone: BoneId,
        mut shape: PrimShape,
        model: Transform,
        blend: f32,
        op: PrimOp,
        layer: PrimLayer,
        region: RegionId,
        lod: LodLevel,
    ) {
        if self.mirrored()
            && let PrimShape::Grid { mirror_x, .. } = &mut shape
        {
            *mirror_x = !*mirror_x;
        }
        let local = self.local_of(bone, model);
        self.anatomy.primitives.push(Primitive { bone, shape, local, blend, blend_anchor: None, op, layer, region, lod, weigh: Weigh::Whole, soft: None });
    }

    /// Distance from `point` (model space, as the part in progress sees it) to the body built so
    /// far: every primitive's shape, unions blended, cuts and raised stamps applied, as the mesher
    /// evaluates them (without folds or anchored blends). For placing parts clear of padding.
    #[must_use]
    pub fn body_distance(&self, point: Vec3) -> f32 {
        use crate::surface::sdf;
        let at = bevy::math::Vec3A::from(self.point_to_stored(point));
        let mut dist: Option<f32> = None;
        let mut raised = 0.0;
        for prim in &self.anatomy.primitives {
            let model = self.bone_model(prim.bone).compute_affine() * prim.local.compute_affine();
            let scale = min_axis_scale(&model);
            let local = model.inverse().transform_point3a(at);
            match prim.op {
                PrimOp::Union => {
                    let own = sdf::shape(local, &prim.shape) * scale;
                    dist = Some(dist.map_or(own, |cur| sdf::smin(cur, own, prim.blend).0));
                }
                PrimOp::Subtract => {
                    if let Some(cur) = dist {
                        dist = Some(sdf::ssub(cur, sdf::shape(local, &prim.shape) * scale, prim.blend));
                    }
                }
                PrimOp::Displace { height } => {
                    if let PrimShape::Ellipsoid { radii } = &prim.shape {
                        let s = 1.0 - (local / bevy::math::Vec3A::from(*radii)).length_squared();
                        if s > 0.0 {
                            raised = (height * scale * s).mul_add(s, raised);
                        }
                    }
                }
            }
        }
        dist.map_or(f32::MAX, |cur| cur - raised)
    }

    /// Sets the most recent primitive's soft-tissue tuning (from its stamp).
    pub fn tune_last_soft(&mut self, tune: super::SoftTune) {
        if let Some(p) = self.anatomy.primitives.last_mut() {
            p.soft = Some(tune);
        }
    }

    /// Limits the most recent primitive's blend to within `reach` of `point` (primitive-local).
    pub fn anchor_last_blend(&mut self, point: Vec3, reach: f32) {
        if let Some(p) = self.anatomy.primitives.last_mut() {
            p.blend_anchor = Some((point, reach));
        }
    }

    /// Sets how the most recent primitive is weighed (see [`Weigh`]).
    pub fn weigh_last(&mut self, weigh: Weigh) {
        if let Some(p) = self.anatomy.primitives.last_mut() {
            p.weigh = weigh;
        }
    }

    pub fn hard_part(&mut self, bone: BoneId, model: Transform, mut params: HardParams, region: RegionId) {
        let mirror = self.mirrored();
        if mirror {
            params.twist = -params.twist;
        }
        let mesh = match &params.kind {
            super::HardKind::Mesh { path, .. } => self.assets.mesh(path),
            _ => None,
        };
        let local = self.local_of(bone, model);
        self.anatomy.hard_parts.push(HardPart { bone, local, params, region, mesh, mirror });
    }

    pub fn socket(&mut self, name: &str, kind: SocketKind, side: Side, bone: BoneId, model: Transform, radius: f32) {
        let local = self.local_of(bone, model);
        self.anatomy.sockets.push(SocketDef { name: name.to_owned(), kind, side, bone, local, size: radius });
    }

    /// A bone that turns with a leg's upper bone (see [`ClothFollow`]).
    pub fn follow(&mut self, follow: ClothFollow) {
        self.anatomy.cloth_follow.push(follow);
    }

    /// Legs registered so far: the index the next [`leg`](Self::leg) gets.
    #[must_use]
    pub const fn leg_count(&self) -> usize {
        self.anatomy.legs.len()
    }

    /// Registers a leg. Points/directions are logical model space; `pair` is assigned in `finish`.
    /// A chain without bones is ignored.
    pub fn leg(&mut self, chain: SmallVec<[BoneId; 4]>, side: Side, stance: Stance, rest_foot: Vec3, pole: Vec3) {
        let length = chain.iter().filter_map(|b| self.anatomy.skeleton.get(*b)).map(|b| b.length).sum();
        let Some(&foot) = chain.last() else { return };
        self.anatomy.legs.push(LegInfo {
            chain,
            foot,
            pair: 0,
            side,
            stance,
            rest_foot: self.point_to_stored(rest_foot),
            pole: self.point_to_stored(pole),
            length,
        });
    }

    /// Registers an arm's clavicle.
    pub fn clavicle(&mut self, bone: BoneId, arm: BoneId, side: Side) {
        self.anatomy.clavicles.push(super::Clavicle { bone, arm, side });
    }

    /// Adds a thin generated surface (logical model space; mirrored copies are reflected and
    /// rewound).
    pub fn sheet(&mut self, mut sheet: Sheet) {
        if self.mirrored() {
            for p in &mut sheet.positions {
                *p = mirror_point(*p);
            }
            for d in &mut sheet.dirs {
                *d = mirror_point(*d);
            }
            for t in sheet.indices.as_chunks_mut::<3>().0 {
                t.swap(1, 2);
            }
        }
        sheet.part = self.current();
        self.anatomy.sheets.push(sheet);
    }

    /// Records a wing's folded pose: `folded` gives each wing bone's folded logical model
    /// transform (parents before children); `half`, if given, the same bones half-way through the
    /// fold. Stored as local rotations, relative to the posed parent where the parent is a wing
    /// bone too.
    pub fn wing_rig(&mut self, fold: f32, folded: &[(BoneId, Transform)], half: Option<&[(BoneId, Transform)]>) {
        let bones = self.local_rotations(folded);
        let half = half.map_or_else(Vec::new, |h| self.local_rotations(h).into_iter().map(|(_, r)| r).collect());
        self.anatomy.wings.push(WingRig { part: self.current(), fold: fold.clamp(0.0, 1.0), bones, half });
    }

    /// Local rotations of posed bones given as logical model transforms (parents before children).
    fn local_rotations(&self, posed: &[(BoneId, Transform)]) -> Vec<(BoneId, Quat)> {
        let mut stored: Vec<(BoneId, Transform)> = Vec::with_capacity(posed.len());
        let mut bones = Vec::with_capacity(posed.len());
        for (bone, model) in posed {
            let model = self.to_stored(Transform { scale: Vec3::ONE, ..*model });
            let parent = self.anatomy.skeleton.get(*bone).and_then(|b| b.parent);
            let parent_model = parent.map(|p| stored.iter().find(|(b, _)| *b == p).map_or_else(|| self.bone_model(p), |(_, m)| *m));
            let local = parent_model.map_or(model, |pm| relative(&pm, &model));
            bones.push((*bone, local.rotation));
            stored.push((*bone, model));
        }
        bones
    }

    /// Axis samples of the part the current part grows from (logical model space).
    #[must_use]
    pub fn parent_axis(&self) -> SmallVec<[AxisSample; 9]> {
        let parts = &self.anatomy.parts;
        let parent = self.stack.last().and_then(|f| parts.get(f.index.idx())).and_then(|p| p.parent);
        parent.and_then(|p| parts.get(p.idx())).map_or_default(|p| p.axis.clone())
    }

    pub fn spine(&mut self, bones: &[BoneId]) {
        self.anatomy.spine.extend_from_slice(bones);
    }

    pub fn spring_chain(&mut self, chain: SpringChainDef) {
        self.anatomy.springs.push(chain);
    }

    pub fn hand(&mut self, hand: super::HandDef) {
        self.anatomy.hands.push(hand);
    }

    pub fn look(&mut self, look: LookDef) {
        self.anatomy.look = Some(look);
    }

    /// Limits `bone`'s joint; `limit` is in the logical (left) frame, like everything built.
    pub fn limit(&mut self, bone: BoneId, limit: JointLimit) {
        let limit = if self.mirrored() { limit.mirrored() } else { limit };
        self.anatomy.joint_limits.push((bone, limit));
    }

    /// Bones of the current part's ancestors whose region is of `kind`, nearest first.
    #[must_use]
    pub fn ancestor_bones(&self, from: BoneId, kind: RegionKind) -> SmallVec<[BoneId; 8]> {
        let bones = &self.anatomy.skeleton.bones;
        let regions = &self.anatomy.regions.regions;
        let mut out = SmallVec::new();
        let mut cur = Some(from);
        while let Some(b) = cur {
            let Some(def) = bones.get(b.index()) else { break };
            if regions.get(def.region.0.idx()).is_none_or(|r| r.kind != kind) {
                break;
            }
            out.push(b);
            cur = def.parent;
        }
        out
    }

    /// Grounds the body, resolves part LODs, builds inverse binds, the crowd skeleton and leg pairs.
    ///
    /// # Errors
    ///
    /// None at present; kept fallible for future validation.
    pub fn finish(mut self) -> Result<Anatomy, AnatomyError> {
        self.ground();
        self.resolve_lods();
        let a = &mut self.anatomy;

        a.finish_skeleton(&[]);

        // Leg pairs: rank front (+Z) to back per side.
        for side in [Side::Left, Side::Right, Side::Center] {
            let mut legs: Vec<&mut LegInfo> = a.legs.iter_mut().filter(|l| l.side == side).collect();
            legs.sort_by(|l, m| m.rest_foot.z.total_cmp(&l.rest_foot.z));
            for (rank, leg) in legs.into_iter().enumerate() {
                leg.pair = rank.clamp_u8();
            }
        }
        a.mass = super::mass::measure(a);
        Ok(self.anatomy)
    }

    /// Shifts root bones so the lowest point of the body rests on y = 0.
    fn ground(&mut self) {
        let min_y = lowest_union(&self.anatomy, &self.anatomy.skeleton.model_rest(), |_| true);
        if !min_y.is_finite() {
            return;
        }
        for bone in &mut self.anatomy.skeleton.bones {
            if bone.parent.is_none() {
                bone.rest.translation.y -= min_y;
            }
        }
        for part in &mut self.anatomy.parts {
            for s in &mut part.axis {
                s.model.translation.y -= min_y;
            }
        }
        for sheet in &mut self.anatomy.sheets {
            for p in &mut sheet.positions {
                p.y -= min_y;
            }
        }
        for leg in &mut self.anatomy.legs {
            leg.rest_foot.y -= min_y;
        }
        self.anatomy.ground = min_y;
        // The body now stands on its lowest sole; a rest pose need not put every sole that low (a
        // dog's fore paws were built 1.7 cm up, an insect's front feet 5.8 cm). Plant each foot
        // where its sole touches, so a planted foot stands on the ground rather than its rest height.
        let soles = super::sole_heights(&self.anatomy);
        for (leg, sole) in self.anatomy.legs.iter_mut().zip(soles) {
            if sole.is_finite() && sole > 0.0 {
                leg.rest_foot.y -= sole;
            }
        }
    }

    /// Resolves `BuiltPart::lod` from size, clamped so no part outlives its parent.
    fn resolve_lods(&mut self) {
        let model = self.anatomy.skeleton.model_rest();
        let a = &mut self.anatomy;
        let part_radius = |part: &BuiltPart| -> f32 {
            let prims = a.primitives.get(part.primitives.start.idx()..part.primitives.end.idx()).unwrap_or_default().iter();
            let from_prims = prims.map(|p| bounding_radius(&p.shape)).fold(0.0, f32::max);
            let from_hard = a.hard_parts.get(part.hard_parts.start.idx()..part.hard_parts.end.idx()).unwrap_or_default()
                .iter()
                .map(|h| h.params.size)
                .fold(0.0, f32::max);
            let from_bones = a.skeleton.bones.get(part.bones.start.idx()..part.bones.end.idx()).unwrap_or_default()
                .iter()
                .map(|b| b.length)
                .sum::<f32>();
            from_prims.max(from_hard).max(from_bones * 0.5)
        };
        let points = model.iter().map(|m| Vec3::from(m.translation));
        let (lo, hi) = points.fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| (lo.min(p), hi.max(p)));
        let mob_radius = ((hi - lo).length() * 0.5).max(1e-3);

        // Parents come before their children, so each parent's LOD is final when read.
        for i in 0..a.parts.len() {
            let Some(part) = a.parts.get(i) else { continue };
            let own = self.lod_rules.resolve(part.detail, part_radius(part) / mob_radius);
            let lod = match part.parent {
                Some(p) => own.min(a.parts.get(p.idx()).map_or(LodLevel::Far, |q| q.lod)),
                None => LodLevel::Far,
            };
            if let Some(part) = a.parts.get_mut(i) {
                part.lod = lod;
            }
        }
    }
}

/// Lowest model-space height of the union primitives on bones `keep` accepts, posed by `model`.
pub(super) fn lowest_union(a: &Anatomy, model: &[Affine3A], keep: impl Fn(BoneId) -> bool) -> f32 {
    a.primitives
        .iter()
        .filter(|p| p.op == PrimOp::Union && keep(p.bone))
        .filter_map(|p| Some(lowest_point(&p.shape, &(*model.get(p.bone.index())? * p.local.compute_affine()))))
        .fold(f32::INFINITY, f32::min)
}

/// Lowest model-space height of `shape` placed by `m`.
fn lowest_point(shape: &PrimShape, m: &Affine3A) -> f32 {
    let col = |i: usize| Vec3::from(m.matrix3.col(i));
    let s = (0..3).map(|i| col(i).length()).fold(f32::INFINITY, f32::min);
    let at = |y: f32| m.transform_point3(Vec3::Y * y).y;
    match shape {
        PrimShape::Sphere { r } => at(0.0) - r * s,
        PrimShape::Ellipsoid { radii } => {
            let ext = (0..3).map(|i| (col(i).y * radii[i]).powi(2)).sum::<f32>().sqrt();
            at(0.0) - ext
        }
        PrimShape::RoundCone { r0, r1, len } => {
            (at(0.0) - r0 * s).min(at(*len) - r1 * s)
        }
        PrimShape::RoundBox { half, .. } => at(0.0) - (0..3).map(|i| col(i).y.abs() * half[i]).sum::<f32>(),
        PrimShape::Grid { grid, mirror_x } => {
            let flip = if *mirror_x { Vec3::new(-1.0, 1.0, 1.0) } else { Vec3::ONE };
            if let Some(low) = grid.lowest(|p| m.transform_point3(p * flip).y, s) {
                return low;
            }
            let (lo, hi) = grid.bounds();
            (0..8)
                .map(|i| {
                    let c = Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    );
                    m.transform_point3(c * flip).y
                })
                .fold(f32::INFINITY, f32::min)
        }
    }
}

/// Conservative radius of a primitive shape around its local origin.
pub fn bounding_radius(shape: &PrimShape) -> f32 {
    match shape {
        PrimShape::Sphere { r } => *r,
        PrimShape::Ellipsoid { radii } => radii.max_element(),
        PrimShape::RoundCone { r0, r1, len } => len + r0.max(*r1),
        PrimShape::RoundBox { half, .. } => half.length(),
        PrimShape::Grid { grid, .. } => {
            let (lo, hi) = grid.bounds();
            lo.abs().max(hi.abs()).length()
        }
    }
}

impl PartBuild for PartKind {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        match self {
            Self::Torso(p) => p.build(b, at, cx),
            Self::Chain(p) => p.build(b, at, cx),
            Self::Head(p) => p.build(b, at, cx),
            Self::Limb(p) => p.build(b, at, cx),
            Self::Digits(p) => p.build(b, at, cx),
            Self::Mass(p) => p.build(b, at, cx),
            Self::Hard(p) => p.build(b, at, cx),
            Self::Sculpt(p) => p.build(b, at, cx),
            Self::Wing(p) => p.build(b, at, cx),
        }
    }
}

impl PartKind {
    /// Limb role, for [`RegionSelector::Role`](super::RegionSelector::Role).
    #[must_use]
    pub const fn role(&self) -> Option<LimbRole> {
        match self {
            Self::Limb(p) => Some(p.role),
            _ => None,
        }
    }
}
