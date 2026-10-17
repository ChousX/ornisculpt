use bevy::math::{Affine3A, Mat3};
use bevy::prelude::*;
use smallvec::SmallVec;

use super::{
    Anatomy, AnatomyError, AttachSlot, BoneDef, BoneId, BuiltPart, HardParams,
    HardPart, JointLimit, LegInfo, LimbRole, LodLevel, LookDef, MAX_BONES, PartKind,
    PartLodRules, PartNode, PartPath, PrimLayer, PrimOp, PrimShape, Primitive, RegionDef,
    PartAssets, RegionId, RegionKind, Side, SocketDef, SocketKind, SpringChainDef, Stance, Tone,
};

/// Implemented by every part kind's params. Emits bones, primitives, sockets and rig info into the
/// builder and returns the attach slots its children may use.
///
/// Parts work in the *logical* model frame: Y up, mob facing +Z, left = +X. Mirrored copies are
/// built exactly like their left twin; the builder reflects them on storage.
pub trait PartBuild {
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
}

impl AxisSample {
    /// Round cross-section.
    pub fn round(t: f32, bone: BoneId, model: Transform, radius: f32) -> Self {
        Self { t, bone, model, radii: Vec2::splat(radius) }
    }

    /// Distance from the axis to the surface at `around` (radians; 0 = dorsal, π/2 = left).
    pub fn radius_toward(&self, around: f32) -> f32 {
        let (c, s) = (around.cos() / self.radii.y, around.sin() / self.radii.x);
        1.0 / (c * c + s * s).sqrt()
    }

    /// Smallest radius (for sizes that should not depend on direction).
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
    pub fn resolve(&self, slot: AttachSlot) -> Option<AttachPoint> {
        match slot {
            AttachSlot::Along(t) => {
                let s = self.sample(t)?;
                Some(AttachPoint { bone: Some(s.bone), model: s.model })
            }
            AttachSlot::Surface { t, around_deg } => {
                let s = self.sample(t)?;
                let a = around_deg.to_radians();
                let dorsal = s.model.rotation * Vec3::NEG_Z;
                let left = s.model.rotation * Vec3::X;
                let normal = (dorsal * a.cos() + left * a.sin()).normalize();
                let along = s.model.rotation * Vec3::Y;
                Some(AttachPoint {
                    bone: Some(s.bone),
                    model: Transform::from_translation(s.model.translation + normal * s.radius_toward(a))
                        .with_rotation(frame_y_z(normal, along)),
                })
            }
            fixed => self.fixed.iter().find(|(s, _)| *s == fixed).map(|(_, p)| *p),
        }
    }

    pub(crate) fn sample(&self, t: f32) -> Option<AxisSample> {
        sample_axis(&self.axis, t)
    }
}

/// Interpolates a part's axis samples (sorted by `t`) at `t`; clamps to the ends.
pub fn sample_axis(axis: &[AxisSample], t: f32) -> Option<AxisSample> {
    let (first, last) = (axis.first()?, axis.last()?);
    if t <= first.t {
        return Some(*first);
    }
    if t >= last.t {
        return Some(*last);
    }
    let i = axis.iter().position(|s| s.t > t)?;
    let (a, b) = (axis[i - 1], axis[i]);
    let k = (t - a.t) / (b.t - a.t);
    Some(AxisSample {
        t,
        bone: if k < 0.5 { a.bone } else { b.bone },
        model: Transform::from_translation(a.model.translation.lerp(b.model.translation, k))
            .with_rotation(a.model.rotation.slerp(b.model.rotation, k)),
        radii: a.radii.lerp(b.radii, k),
    })
}

/// Rotation whose +Y is `y` and whose +Z is `z_hint` made orthogonal to it.
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
pub fn mirror_transform(t: Transform) -> Transform {
    let q = t.rotation;
    Transform {
        translation: Vec3::new(-t.translation.x, t.translation.y, t.translation.z),
        rotation: Quat::from_xyzw(q.x, -q.y, -q.z, q.w),
        scale: t.scale,
    }
}

pub fn mirror_point(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, p.z)
}

fn affine(t: &Transform) -> Affine3A {
    t.compute_affine()
}

pub fn relative(parent: &Transform, child: &Transform) -> Transform {
    Transform::from_matrix((affine(parent).inverse() * affine(child)).into())
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
    pub fn new(body_tone: Tone, lod_rules: PartLodRules, assets: PartAssets) -> Self {
        Self { anatomy: Anatomy::default(), assets, model: Vec::new(), stack: Vec::new(), body_tone, lod_rules }
    }

    /// Authored meshes available to this build.
    pub fn assets(&self) -> &PartAssets {
        &self.assets
    }

    /// Whether the part being built is a reflected (right-side) copy.
    pub fn is_mirrored(&self) -> bool {
        self.mirrored()
    }

    fn mirrored(&self) -> bool {
        self.stack.last().is_some_and(|f| f.mirrored)
    }

    fn current(&self) -> u16 {
        self.stack.last().map(|f| f.index).expect("no part in progress")
    }

    fn to_stored(&self, logical: Transform) -> Transform {
        if self.mirrored() { mirror_transform(logical) } else { logical }
    }

    fn point_to_stored(&self, p: Vec3) -> Vec3 {
        if self.mirrored() { mirror_point(p) } else { p }
    }

    /// Starts a part; everything added until [`seal_part`](Self::seal_part) belongs to it.
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
        let index = self.anatomy.parts.len() as u16;
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
        });
        self.stack.push(PartFrame { index, mirrored, side });
        Ok(BuildCx { path, side, tone, offset: node.offset.to_transform() })
    }

    /// Closes the current part's own ranges (before its children are built).
    pub fn seal_part(&mut self) {
        let (bones, prims, hard, regions) = self.counts();
        let current = self.current() as usize;
        let p = &mut self.anatomy.parts[current];
        p.bones.end = bones;
        p.primitives.end = prims;
        p.hard_parts.end = hard;
        p.regions.end = regions;
    }

    /// Records the current part's axis samples (logical model space) for later fitting.
    pub fn set_axis(&mut self, axis: &[AxisSample]) {
        let current = self.current() as usize;
        self.anatomy.parts[current].axis = axis.iter().copied().collect();
    }

    pub fn end_part(&mut self) {
        self.stack.pop();
    }

    fn counts(&self) -> (u8, u32, u16, u8) {
        (
            self.anatomy.skeleton.bones.len() as u8,
            self.anatomy.primitives.len() as u32,
            self.anatomy.hard_parts.len() as u16,
            self.anatomy.regions.regions.len() as u8,
        )
    }

    /// Adds a bone whose rest pose is `model` (logical model space; +Y along the bone).
    pub fn bone(
        &mut self,
        parent: Option<BoneId>,
        name: &str,
        model: Transform,
        length: f32,
        region: RegionId,
    ) -> Result<BoneId, AnatomyError> {
        let id = self.anatomy.skeleton.bones.len();
        if id >= MAX_BONES {
            return Err(AnatomyError::BoneBudget { got: id + 1, max: MAX_BONES });
        }
        let stored = self.to_stored(Transform { scale: Vec3::ONE, ..model });
        let rest = match parent {
            Some(p) => relative(&self.model[p.index()], &stored),
            None => stored,
        };
        self.anatomy.skeleton.bones.push(BoneDef {
            name: name.to_owned(),
            parent,
            rest,
            length,
            region,
            part: self.current(),
        });
        self.model.push(stored);
        Ok(BoneId(id as u8))
    }

    pub fn bone_region(&self, bone: BoneId) -> RegionId {
        self.anatomy.skeleton.bones[bone.index()].region
    }

    /// Logical model-space rest transform of a bone (mirrored back if needed).
    pub fn model(&self, bone: BoneId) -> Transform {
        let stored = self.model[bone.index()];
        if self.mirrored() { mirror_transform(stored) } else { stored }
    }

    /// Adds (or reuses) a region of the current part.
    pub fn region(&mut self, kind: RegionKind, side: Side) -> Result<RegionId, AnatomyError> {
        let path = &self.anatomy.parts[self.current() as usize].path;
        let regions = &mut self.anatomy.regions.regions;
        if let Some(i) = regions.iter().position(|r| &r.part == path && r.kind == kind && r.side == side) {
            return Ok(RegionId(i as u8));
        }
        if regions.len() >= RegionId::MAX {
            return Err(AnatomyError::RegionBudget { got: regions.len() + 1, max: RegionId::MAX });
        }
        regions.push(RegionDef { part: path.clone(), kind, side });
        Ok(RegionId(regions.len() as u8 - 1))
    }

    /// Adds an SDF primitive placed at `model` (logical), owned by `bone`.
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
        if self.mirrored() {
            match &mut shape {
                PrimShape::Sweep { points, .. } => points.iter_mut().for_each(|p| *p = mirror_point(*p)),
                PrimShape::Grid { mirror_x, .. } => *mirror_x = !*mirror_x,
                _ => {}
            }
        }
        let local = relative(&self.model[bone.index()], &self.to_stored(model));
        self.anatomy.primitives.push(Primitive { bone, shape, local, blend, blend_anchor: None, op, layer, region, lod });
    }

    /// Limits the most recent primitive's blend to within `reach` of `point` (primitive-local).
    pub fn anchor_last_blend(&mut self, point: Vec3, reach: f32) {
        if let Some(p) = self.anatomy.primitives.last_mut() {
            p.blend_anchor = Some((point, reach));
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
        let local = relative(&self.model[bone.index()], &self.to_stored(model));
        self.anatomy.hard_parts.push(HardPart { bone, local, params, region, mesh, mirror });
    }

    pub fn socket(&mut self, name: &str, kind: SocketKind, side: Side, bone: BoneId, model: Transform, size: f32) {
        let local = relative(&self.model[bone.index()], &self.to_stored(model));
        self.anatomy.sockets.push(SocketDef { name: name.to_owned(), kind, side, bone, local, size });
    }

    /// Registers a leg. Points/directions are logical model space; `pair` is assigned in `finish`.
    pub fn leg(&mut self, chain: SmallVec<[BoneId; 4]>, side: Side, stance: Stance, rest_foot: Vec3, pole: Vec3) {
        let length = chain
            .iter()
            .map(|b| self.anatomy.skeleton.bones[b.index()].length)
            .sum();
        let foot = *chain.last().expect("leg with no bones");
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

    pub fn spine(&mut self, bones: &[BoneId]) {
        self.anatomy.spine.extend_from_slice(bones);
    }

    pub fn spring_chain(&mut self, chain: SpringChainDef) {
        self.anatomy.springs.push(chain);
    }

    pub fn look(&mut self, look: LookDef) {
        self.anatomy.look = Some(look);
    }

    pub fn limit(&mut self, bone: BoneId, limit: JointLimit) {
        self.anatomy.joint_limits.push((bone, limit));
    }

    /// Bones of the current part's ancestors whose region is of `kind`, nearest first.
    pub fn ancestor_bones(&self, from: BoneId, kind: RegionKind) -> SmallVec<[BoneId; 8]> {
        let bones = &self.anatomy.skeleton.bones;
        let regions = &self.anatomy.regions.regions;
        let mut out = SmallVec::new();
        let mut cur = Some(from);
        while let Some(b) = cur {
            let def = &bones[b.index()];
            if regions[def.region.0 as usize].kind != kind {
                break;
            }
            out.push(b);
            cur = def.parent;
        }
        out
    }

    /// Grounds the body, resolves part LODs, builds inverse binds, the crowd skeleton and leg pairs.
    pub fn finish(mut self) -> Result<Anatomy, AnatomyError> {
        self.ground();
        self.resolve_lods();
        let a = &mut self.anatomy;

        a.finish_skeleton(&[]);

        // Leg pairs: rank front (+Z) to back per side.
        for side in [Side::Left, Side::Right, Side::Center] {
            let mut idx: Vec<usize> = (0..a.legs.len()).filter(|&i| a.legs[i].side == side).collect();
            idx.sort_by(|&i, &j| a.legs[j].rest_foot.z.total_cmp(&a.legs[i].rest_foot.z));
            for (rank, i) in idx.into_iter().enumerate() {
                a.legs[i].pair = rank as u8;
            }
        }
        Ok(self.anatomy)
    }

    /// Shifts root bones so the lowest point of the body rests on y = 0.
    fn ground(&mut self) {
        let model = self.anatomy.skeleton.model_rest();
        let min_y = self
            .anatomy
            .primitives
            .iter()
            .filter(|p| p.op == PrimOp::Union)
            .map(|p| lowest_point(&p.shape, &(model[p.bone.index()] * affine(&p.local))))
            .fold(f32::INFINITY, f32::min);
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
        for leg in &mut self.anatomy.legs {
            leg.rest_foot.y -= min_y;
        }
    }

    /// Resolves `BuiltPart::lod` from size, clamped so no part outlives its parent.
    fn resolve_lods(&mut self) {
        let model = self.anatomy.skeleton.model_rest();
        let a = &mut self.anatomy;
        let part_radius = |part: &BuiltPart| -> f32 {
            let prims = a.primitives[part.primitives.start as usize..part.primitives.end as usize].iter();
            let from_prims = prims.map(|p| bounding_radius(&p.shape)).fold(0.0, f32::max);
            let from_hard = a.hard_parts[part.hard_parts.start as usize..part.hard_parts.end as usize]
                .iter()
                .map(|h| h.params.size)
                .fold(0.0, f32::max);
            let from_bones = a.skeleton.bones[part.bones.start as usize..part.bones.end as usize]
                .iter()
                .map(|b| b.length)
                .sum::<f32>();
            from_prims.max(from_hard).max(from_bones * 0.5)
        };
        let points = model.iter().map(|m| Vec3::from(m.translation));
        let (lo, hi) = points.fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| (lo.min(p), hi.max(p)));
        let mob_radius = ((hi - lo).length() * 0.5).max(1e-3);

        for i in 0..a.parts.len() {
            let own = self.lod_rules.resolve(a.parts[i].detail, part_radius(&a.parts[i]) / mob_radius);
            let lod = match a.parts[i].parent {
                Some(p) => own.min(a.parts[p as usize].lod),
                None => LodLevel::Far,
            };
            a.parts[i].lod = lod;
        }
    }
}

/// Lowest model-space Y of a primitive placed by `m`.
fn lowest_point(shape: &PrimShape, m: &Affine3A) -> f32 {
    let col = |i: usize| Vec3::from(m.matrix3.col(i));
    let s = (0..3).map(|i| col(i).length()).fold(f32::INFINITY, f32::min);
    let at = |y: f32| Vec3::from(m.transform_point3(Vec3::Y * y)).y;
    match shape {
        PrimShape::Sphere { r } => at(0.0) - r * s,
        PrimShape::Ellipsoid { radii } => {
            let ext = (0..3).map(|i| (col(i).y * radii[i]).powi(2)).sum::<f32>().sqrt();
            at(0.0) - ext
        }
        PrimShape::Capsule { r0, r1, len } | PrimShape::RoundCone { r0, r1, len } => {
            (at(0.0) - r0 * s).min(at(*len) - r1 * s)
        }
        PrimShape::RoundBox { half, .. } => at(0.0) - (0..3).map(|i| col(i).y.abs() * half[i]).sum::<f32>(),
        PrimShape::Sweep { points, radii, .. } => points
            .iter()
            .zip(radii)
            .map(|(p, r)| Vec3::from(m.transform_point3(*p)).y - r * s)
            .fold(f32::INFINITY, f32::min),
        PrimShape::Grid { grid, mirror_x } => {
            let (lo, hi) = grid.bounds();
            let flip = if *mirror_x { Vec3::new(-1.0, 1.0, 1.0) } else { Vec3::ONE };
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
pub(crate) fn bounding_radius(shape: &PrimShape) -> f32 {
    match shape {
        PrimShape::Sphere { r } => *r,
        PrimShape::Ellipsoid { radii } => radii.max_element(),
        PrimShape::Capsule { r0, r1, len } | PrimShape::RoundCone { r0, r1, len } => len + r0.max(*r1),
        PrimShape::RoundBox { half, .. } => half.length(),
        PrimShape::Sweep { points, radii, .. } => {
            points.iter().map(|p| p.length()).fold(0.0, f32::max) + radii.iter().copied().fold(0.0, f32::max)
        }
        PrimShape::Grid { grid, .. } => {
            let (lo, hi) = grid.bounds();
            lo.abs().max(hi.abs()).length()
        }
    }
}

impl PartBuild for PartKind {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        match self {
            PartKind::Torso(p) => p.build(b, at, cx),
            PartKind::Chain(p) => p.build(b, at, cx),
            PartKind::Head(p) => p.build(b, at, cx),
            PartKind::Limb(p) => p.build(b, at, cx),
            PartKind::Digits(p) => p.build(b, at, cx),
            PartKind::Mass(p) => p.build(b, at, cx),
            PartKind::Hard(p) => p.build(b, at, cx),
            PartKind::Sculpt(p) => p.build(b, at, cx),
        }
    }
}

impl PartKind {
    /// Limb role, for [`RegionSelector::Role`](super::RegionSelector::Role).
    pub fn role(&self) -> Option<LimbRole> {
        match self {
            PartKind::Limb(p) => Some(p.role),
            _ => None,
        }
    }
}
