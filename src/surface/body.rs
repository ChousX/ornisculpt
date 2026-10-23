use bevy::math::Affine3A;
use bevy::prelude::*;
use bevy::shape::{Aabb3d, BoundingVolume, IntersectsVolume};

use super::{Field, Influences, SurfaceInfo, sdf};
use crate::num::{Idx, Saturate, ToF32};
use crate::anatomy::{Anatomy, BoneId, BuiltPart, LodLevel, PrimLayer, PrimOp, PrimShape, RegionId};
use crate::random::{Noise, NoiseSettings};

/// Inputs to building a [`BodyField`] beyond the anatomy itself.
#[derive(Clone, Debug)]
pub struct BodyFieldOptions {
    pub level: LodLevel,
    /// Seed for fold/wrinkle noise (the mob seed). Anatomy itself is seed-free.
    pub seed: u64,
    /// Displace geometry with fold noise. Off for coarse levels, where folds only live in shading.
    pub geometric_folds: bool,
    /// Build the surface in the spread meshing pose ([`spread_pose`](crate::anatomy::spread_pose))
    /// instead of the rest pose; [`BodyField::unbind`] then brings the mesh back to rest.
    pub spread: bool,
}

/// The whole-body SDF: smooth union of the anatomy's primitives present at `level`, plus
/// fold displacement. Built once per (anatomy, level); meshers evaluate it through [`ChunkView`]s.
pub struct BodyField {
    pub(super) prims: Vec<PreparedPrim>,
    /// Parent of every bone, for keeping skin weights on one limb.
    bone_parent: Vec<Option<BoneId>>,
    /// Per bone, the spread pose → rest pose transform (empty when built at rest).
    unbind: Vec<Affine3A>,
    /// Model transform of every bone in the pose the field was built in.
    bone_model: Vec<Affine3A>,
    pub(super) bounds: Aabb3d,
    creases: Option<Creases>,
}

/// Skin creases around joints: ridges running across the bone axis, wobbled by noise and faded
/// out with distance from the joint. Only evaluated near the surface.
struct Creases {
    joints: Vec<CreaseJoint>,
    wobble: Noise,
    /// Depth in metres.
    amplitude: f32,
    /// Ridges per metre along the bone axis.
    frequency: f32,
}

struct CreaseJoint {
    position: Vec3A,
    axis: Vec3A,
    radius: f32,
}

impl Creases {
    #[inline]
    fn displacement<'a>(&self, joints: impl Iterator<Item = &'a CreaseJoint>, p: Vec3A) -> f32 {
        let mut mask = 0.0f32;
        let mut along = 0.0;
        for joint in joints {
            let offset = p - joint.position;
            let m = (1.0 - offset.length() / joint.radius).max(0.0);
            if m > mask {
                mask = m;
                along = offset.dot(joint.axis);
            }
        }
        if mask <= 0.0 {
            return 0.0;
        }
        let phase = self.wobble.sample(p.into()).mul_add(2.0, along * self.frequency * std::f32::consts::TAU);
        let ridge = (1.0 - phase.sin().abs()).powi(4);
        self.amplitude * mask * mask * ridge
    }
}

/// How far apart legs must stand to mesh as separate surfaces, metres: two hero voxels.
const LEG_GAP: f32 = 0.016;
/// How far under the body legs may still overlap in the meshing pose, metres: the crotch.
const LEG_CROTCH: f32 = 0.04;
/// The most the meshing pose swings legs apart, degrees, and the step it searches in.
const LEG_SPREAD_MAX: f32 = 24.0;
const LEG_SPREAD_STEP: f32 = 2.0;

/// Limbs [`BodyField::eval`] keeps apart at one point (as `LIMB_SLOTS` in `sdf_field.wesl`).
const LIMB_SLOTS: usize = 8;

/// One limb's share of [`BodyField::eval`] at a point.
#[derive(Clone, Copy)]
struct LimbField {
    id: u16,
    /// Its smooth union with the body (`f32::MAX` until its first primitive).
    with_body: f32,
    /// Its own raised stamps.
    raised: f32,
}

impl LimbField {
    const EMPTY: Self = Self { id: u16::MAX, with_body: f32::MAX, raised: 0.0 };
}

/// The slot of `limb` in [`BodyField::eval`]'s per-limb fields, opened if new (limbs past the last
/// slot share it).
fn slot<'a>(limbs: &'a mut [LimbField; LIMB_SLOTS], used: &mut usize, limb: u16) -> Option<&'a mut LimbField> {
    let i = limbs.iter().take(*used).position(|l| l.id == limb).unwrap_or_else(|| {
        *used = (*used + 1).min(LIMB_SLOTS);
        *used - 1
    });
    let l = limbs.get_mut(i)?;
    if l.id == u16::MAX {
        l.id = limb;
    }
    Some(l)
}

/// A primitive moved into model space at rest pose, ready for evaluation.
pub(super) struct PreparedPrim {
    /// Model space → primitive local space.
    pub(super) to_local: Affine3A,
    /// Smallest axis scale of the local → model transform (keeps distances conservative).
    pub(super) min_scale: f32,
    pub(super) shape: PrimShape,
    pub(super) blend: f32,
    /// Local anchor point and reach for blend fading (see `Primitive::blend_anchor`).
    pub(super) anchor: Option<(Vec3A, f32)>,
    pub(super) op: PrimOp,
    layer: PrimLayer,
    region: RegionId,
    bone: BoneId,
    /// The part this primitive belongs to (index into [`Anatomy::parts`]).
    pub(super) part: u16,
    /// The limb (part index) this primitive belongs to, if it is on one: different limbs meet
    /// with a hard union, so an arm hanging by a thigh never webs onto it.
    pub(super) limb: Option<u16>,
    /// Model-space bounds padded by `blend` (and fold amplitude) for culling.
    pub(super) aabb: Aabb3d,
}

impl PreparedPrim {
    /// Distance and effective blend radius at `p`.
    #[inline]
    fn eval(&self, p: Vec3A) -> (f32, f32) {
        let q = self.to_local.transform_point3a(p);
        let d = sdf::shape(q, &self.shape);
        let blend = match self.anchor {
            Some((a, reach)) => self.blend * (1.0 - (q - a).length() * self.min_scale / reach).clamp(0.0, 1.0),
            None => self.blend,
        };
        (d * self.min_scale, blend)
    }
}

/// Local-space bounds of a shape.
pub(super) fn local_bounds(shape: &PrimShape) -> (Vec3, Vec3) {
    match shape {
        PrimShape::Sphere { r } => (Vec3::splat(-r), Vec3::splat(*r)),
        PrimShape::Ellipsoid { radii } => (-*radii, *radii),
        PrimShape::RoundCone { r0, r1, len } => {
            let r = r0.max(*r1);
            (Vec3::new(-r, -r0, -r), Vec3::new(r, len + r1, r))
        }
        PrimShape::RoundBox { half, .. } => (-*half, *half),
        PrimShape::Grid { grid, mirror_x } => {
            let (lo, hi) = grid.bounds();
            if *mirror_x { (Vec3::new(-hi.x, lo.y, lo.z), Vec3::new(-lo.x, hi.y, hi.z)) } else { (lo, hi) }
        }
    }
}

pub(super) fn transform_aabb(m: &Affine3A, (lo, hi): (Vec3, Vec3)) -> Aabb3d {
    let mut min = Vec3A::INFINITY;
    let mut max = Vec3A::NEG_INFINITY;
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let p = m.transform_point3a(c.into());
        min = min.min(p);
        max = max.max(p);
    }
    Aabb3d { min, max }
}

impl BodyField {
    #[must_use]
    pub fn new(anatomy: &Anatomy, opts: &BodyFieldOptions) -> Self {
        if !opts.spread {
            return Self::build(anatomy, opts, None);
        }
        // Legs that overlap at rest (thick thighs) swing apart, the least that parts them, so
        // each meshes as its own surface and binds to its own leg: skinned back to rest they pass
        // through each other, and part again as the legs move, instead of fusing into one.
        let mut spread = crate::anatomy::Spread::default();
        loop {
            let field = Self::build(anatomy, opts, Some(&crate::anatomy::spread_pose(anatomy, &spread)));
            if spread.legs >= LEG_SPREAD_MAX || field.legs_apart(anatomy, LEG_GAP) {
                return field;
            }
            spread.legs += LEG_SPREAD_STEP;
        }
    }

    /// Whether every two legs stand at least `gap` apart along their length below the body
    /// (compared at equal shares of each, as mirrored legs are), by their own primitives. Inside
    /// the body and the crotch just under it, where they join it anyway, they may overlap: no
    /// swing at the hip parts thighs there.
    fn legs_apart(&self, anatomy: &Anatomy, gap: f32) -> bool {
        const SAMPLES: u8 = 16;
        let legs: Vec<(Vec<usize>, Vec<Vec3A>)> = anatomy
            .legs
            .iter()
            .filter_map(|leg| {
                let part = anatomy.skeleton.bones.get(leg.chain.first()?.index())?.part;
                let own = (0..self.prims.len()).filter(|&i| self.prims.get(i).is_some_and(|p| p.limb == Some(part))).collect();
                let joints = leg.chain.iter().chain([&leg.foot]).filter_map(|b| self.bone_model.get(b.index())).map(|m| m.translation).collect();
                Some((own, joints))
            })
            .collect();
        let body: Vec<usize> = (0..self.prims.len()).filter(|&i| self.prims.get(i).is_some_and(|p| p.limb.is_none())).collect();
        // The point a share `t` of the way down a chain of joints.
        let along = |joints: &[Vec3A], t: f32| {
            let segments = joints.len().saturating_sub(1).max(1);
            let at = t * segments.to_f32();
            let seg = at.sat_usize().min(segments - 1);
            let start = joints.get(seg).copied().unwrap_or_default();
            let end = joints.get(seg + 1).copied().unwrap_or(start);
            start.lerp(end, at - seg.to_f32())
        };
        legs.iter().enumerate().all(|(i, (own_a, a))| {
            legs.iter().skip(i + 1).all(|(own_b, b)| {
                (0..=SAMPLES).all(|k| {
                    let t = f32::from(k) / f32::from(SAMPLES);
                    let mid = (along(a, t) + along(b, t)) * 0.5;
                    self.distance_of(&body, mid) <= LEG_CROTCH || self.distance_of(own_a, mid) + self.distance_of(own_b, mid) >= gap
                })
            })
        })
    }

    /// The field with the bones in the local pose `pose` (one transform per bone) rather than at
    /// rest; `opts.spread` is ignored. [`unbind`](Self::unbind) brings a mesh of it back to rest.
    #[must_use]
    pub fn with_pose(anatomy: &Anatomy, opts: &BodyFieldOptions, pose: &[Transform]) -> Self {
        Self::build(anatomy, opts, Some(pose))
    }

    fn build(anatomy: &Anatomy, opts: &BodyFieldOptions, pose: Option<&[Transform]>) -> Self {
        let rest = anatomy.skeleton.model_rest();
        let (model, unbind) = if let Some(pose) = pose {
            let model = anatomy.skeleton.model_from(pose);
            let unbind = rest.iter().zip(&model).map(|(r, s)| *r * s.inverse()).collect();
            (model, unbind)
        } else {
            (rest, Vec::new())
        };
        let prim_part = primitive_parts(anatomy);
        let body_tone = anatomy.parts.first().map_or_default(|p| p.tone);
        let fold_amp = body_tone.fold_noise.amplitude * body_tone.folds;
        let creases = (opts.geometric_folds && fold_amp > 0.0).then(|| {
            let wobble = NoiseSettings { amplitude: 1.0, frequency: body_tone.fold_noise.frequency * 0.5, ..body_tone.fold_noise };
            let joints = anatomy
                .skeleton
                .bones
                .iter()
                .enumerate()
                .filter(|(_, b)| anatomy.parts.get(b.part.idx()).is_some_and(|part| part.lod >= opts.level))
                .filter_map(|(i, b)| {
                    let parent = model.get(b.parent?.index())?;
                    Some(CreaseJoint {
                        position: model.get(i)?.translation,
                        axis: parent.matrix3.y_axis.normalize_or(Vec3A::Y),
                        radius: (b.length * 0.6).clamp(0.02, 0.15),
                    })
                })
                .collect();
            Creases {
                joints,
                wobble: Noise::new(&wobble, opts.seed, "creases"),
                amplitude: fold_amp,
                frequency: body_tone.fold_noise.frequency,
            }
        });
        let fold_pad = if creases.is_some() { fold_amp } else { 0.0 };

        let mut prims = Vec::new();
        for (p, &part) in anatomy.primitives.iter().zip(&prim_part) {
            if anatomy.parts.get(part.idx()).is_none_or(|part| part.lod < opts.level) || p.lod < opts.level {
                continue;
            }
            let Some(bone) = model.get(p.bone.index()) else { continue };
            let m = *bone * p.local.compute_affine();
            let min_scale = (0..3).map(|c| m.matrix3.col(c).length()).fold(f32::INFINITY, f32::min);
            let aabb = transform_aabb(&m, local_bounds(&p.shape)).grow(Vec3A::splat(p.blend + fold_pad));
            let shape = p.shape.clone();
            prims.push(PreparedPrim {
                to_local: m.inverse(),
                min_scale,
                shape,
                blend: p.blend,
                anchor: p.blend_anchor.map(|(a, r)| (a.into(), r)),
                op: p.op,
                layer: p.layer,
                region: p.region,
                bone: p.bone,
                part,
                limb: limb_of(anatomy, part),
                aabb,
            });
        }
        let bounds = prims
            .iter()
            .map(|p| p.aabb)
            .reduce(|a, b| a.merge(&b))
            .unwrap_or(Aabb3d { min: Vec3A::ZERO, max: Vec3A::ZERO });
        let bone_parent = anatomy.skeleton.bones.iter().map(|b| b.parent).collect();
        Self { prims, bone_parent, unbind, bone_model: model, bounds, creases }
    }

    /// Distance to the smooth union of a subset of primitives (no creases). Indices are
    /// [`Anatomy::primitives`] indices, which match this field's own only at `Hero` level, where
    /// every primitive is present.
    #[must_use]
    pub fn distance_among(&self, prims: std::ops::Range<usize>, p: Vec3A) -> f32 {
        self.eval(self.subset(prims), std::iter::empty(), p).0
    }

    /// Distance to the smooth union of the listed primitives (this field's indices, in order).
    pub(super) fn distance_of(&self, prims: &[usize], p: Vec3A) -> f32 {
        self.eval(prims.iter().filter_map(|&i| self.prims.get(i)), std::iter::empty(), p).0
    }

    /// [`Field::surface_info`] over a subset of primitives (see [`distance_among`](Self::distance_among)).
    #[must_use]
    pub fn surface_info_among(&self, prims: std::ops::Range<usize>, p: Vec3A) -> SurfaceInfo {
        self.info(self.subset(prims), p)
    }

    /// The primitives in `range`, clipped to those present.
    fn subset(&self, range: std::ops::Range<usize>) -> std::slice::Iter<'_, PreparedPrim> {
        let len = self.prims.len();
        self.prims.get(range.start.min(len)..range.end.min(len)).unwrap_or_default().iter()
    }

    /// The part owning the union primitive nearest to `p` (an index into [`Anatomy::parts`]).
    #[must_use]
    pub fn part_at(&self, p: Vec3A) -> Option<u16> {
        self.eval(self.prims.iter(), std::iter::empty(), p).1.map(|n| n.part)
    }

    /// Restricts evaluation to primitives that can affect `region` (a meshing chunk).
    pub fn view(&self, region: Aabb3d) -> ChunkView<'_> {
        let prims = (0u32..)
            .zip(&self.prims)
            .filter(|(_, prim)| prim.aabb.intersects(&region))
            .map(|(i, _)| i)
            .collect();
        let joints = self.creases.as_ref().map_or_else(Vec::new, |c| {
            (0u32..)
                .zip(&c.joints)
                .filter(|(_, j)| {
                    let closest = j.position.clamp(region.min, region.max);
                    closest.distance(j.position) < j.radius
                })
                .map(|(i, _)| i)
                .collect()
        });
        ChunkView { field: self, prims, joints, bounds: region }
    }

    #[must_use]
    pub const fn primitive_count(&self) -> usize {
        self.prims.len()
    }

    /// Smooth union over `indices`, plus creases from `joints`; returns the distance and the
    /// nearest union primitive.
    #[inline]
    fn eval<'a, 'j>(
        &self,
        prims: impl Iterator<Item = &'a PreparedPrim>,
        joints: impl Iterator<Item = &'j CreaseJoint>,
        p: Vec3A,
    ) -> (f32, Option<&'a PreparedPrim>) {
        // The body (primitives on no limb) and, per limb, that limb's smooth union with the body:
        // a limb blends into the body, never into another limb, so two only ever meet. Limbs past
        // the last slot share it.
        let mut body: Option<f32> = None;
        let mut limbs = [LimbField::EMPTY; LIMB_SLOTS];
        let mut used = 0;
        let mut raised = 0.0;
        let mut nearest: Option<&PreparedPrim> = None;
        let mut best = f32::INFINITY;
        for prim in prims {
            let (dl, blend) = prim.eval(p);
            match prim.op {
                PrimOp::Union => {
                    if let Some(l) = prim.limb.and_then(|limb| slot(&mut limbs, &mut used, limb)) {
                        // A new limb starts from the body so far.
                        let from = if l.with_body < f32::MAX { Some(l.with_body) } else { body };
                        l.with_body = from.map_or(dl, |d| sdf::smin(d, dl, blend).0);
                    } else {
                        body = Some(body.map_or(dl, |d| sdf::smin(d, dl, blend).0));
                        // (A limb with only raised stamps so far has no union yet: it takes the body.)
                        for l in limbs.iter_mut().take(used).filter(|l| l.with_body < f32::MAX) {
                            l.with_body = sdf::smin(l.with_body, dl, blend).0;
                        }
                    }
                    if dl < best {
                        best = dl;
                        nearest = Some(prim);
                    }
                }
                PrimOp::Subtract => {
                    body = body.map(|d| sdf::ssub(d, dl, blend));
                    for l in limbs.iter_mut().take(used).filter(|l| l.with_body < f32::MAX) {
                        l.with_body = sdf::ssub(l.with_body, dl, blend);
                    }
                }
                PrimOp::Displace { height } => {
                    if let PrimShape::Ellipsoid { radii } = &prim.shape {
                        let s = 1.0 - (prim.to_local.transform_point3a(p) / Vec3A::from(*radii)).length_squared();
                        if s > 0.0 {
                            // A limb's stamps raise only that limb.
                            let rise = height * prim.min_scale * s * s;
                            match prim.limb.and_then(|limb| slot(&mut limbs, &mut used, limb)) {
                                Some(l) => l.raised += rise,
                                None => raised += rise,
                            }
                        }
                    }
                }
            }
        }
        // Each limb with its own stamps raised; a limb with no union (only raised stamps here)
        // raises the body.
        let limbs = limbs.get(..used).unwrap_or_default();
        let d = limbs
            .iter()
            .filter_map(|l| Some(if l.with_body < f32::MAX { l.with_body } else { body? } - l.raised))
            .fold(body, |d, l| Some(d.map_or(l, |d| d.min(l))));
        let mut d = d.map_or(f32::MAX, |d| d - raised);
        if let Some(c) = &self.creases
            && d.abs() < 0.03 {
                d += c.displacement(joints, p);
            }
        (d, nearest)
    }

    fn info<'a>(&self, prims: impl Iterator<Item = &'a PreparedPrim> + Clone, p: Vec3A) -> SurfaceInfo {
        let (d, near) = self.eval(prims.clone(), std::iter::empty(), p);
        let near_bone = near.map(|n| n.bone);
        let mut influences = Influences::default();
        for prim in prims {
            // Weights only from bones in line with the nearest one (its parents and children):
            // never a neighbouring finger, never the other leg.
            if prim.op != PrimOp::Union || near_bone.is_some_and(|b| !self.in_line(b, prim.bone)) {
                continue;
            }
            let (dl, blend) = prim.eval(p);
            let gap = (dl - d).max(0.0);
            let reach = f32::mul_add(blend, 2.0, 1e-4);
            let w = (1.0 - gap / reach).max(0.0);
            influences.add(prim.bone, w * w);
        }
        influences.normalize(near.map_or(BoneId(0), |n| n.bone));
        SurfaceInfo {
            influences,
            region: near.map_or(RegionId(0), |n| n.region),
            layer: near.map_or(PrimLayer::Base, |n| n.layer),
        }
    }
}

/// Owning part index of every primitive (from the parts' primitive ranges).
pub fn primitive_parts(anatomy: &Anatomy) -> Vec<u16> {
    range_owners(anatomy, anatomy.primitives.len(), |p| p.primitives.start.idx()..p.primitives.end.idx())
}

/// Owning part index of each of `len` items, from the item range `range` gives every part (0 for
/// items in none).
pub(super) fn range_owners(anatomy: &Anatomy, len: usize, range: impl Fn(&BuiltPart) -> std::ops::Range<usize>) -> Vec<u16> {
    let mut out = vec![0u16; len];
    for (pi, part) in (0u16..).zip(&anatomy.parts) {
        for i in range(part) {
            if let Some(slot) = out.get_mut(i) {
                *slot = pi;
            }
        }
    }
    out
}

/// [`BodyField`] restricted to one chunk's relevant primitives.
pub struct ChunkView<'a> {
    field: &'a BodyField,
    prims: Vec<u32>,
    joints: Vec<u32>,
    bounds: Aabb3d,
}

impl ChunkView<'_> {
    /// No primitive reaches this chunk; the mesher can skip it.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.prims.is_empty()
    }
}

impl Field for BodyField {
    fn bounds(&self) -> Aabb3d {
        self.bounds
    }

    fn distance(&self, p: Vec3A) -> f32 {
        let joints = self.creases.as_ref().map_or(&[][..], |c| &c.joints);
        self.eval(self.prims.iter(), joints.iter(), p).0
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.info(self.prims.iter(), p)
    }
}

impl Field for ChunkView<'_> {
    fn bounds(&self) -> Aabb3d {
        self.bounds
    }

    fn distance(&self, p: Vec3A) -> f32 {
        self.field.eval(self.prims(), self.joints(), p).0
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.field.info(self.prims(), p)
    }
}

impl<'a> ChunkView<'a> {
    fn prims(&self) -> impl Iterator<Item = &'a PreparedPrim> + Clone + '_ {
        self.prims.iter().filter_map(|&i| self.field.prims.get(i.idx()))
    }

    fn joints(&self) -> impl Iterator<Item = &'a CreaseJoint> + '_ {
        let joints = self.field.creases.as_ref().map_or(&[][..], |c| &c.joints);
        self.joints.iter().filter_map(move |&i| joints.get(i.idx()))
    }
}

impl BodyField {
    /// Whether `a` and `b` are in line (see [`in_line`]).
    pub(crate) fn in_line(&self, a: BoneId, b: BoneId) -> bool {
        in_line(&self.bone_parent, a, b)
    }
}

/// Whether `a` and `b` are the same bone or one is a near ancestor of the other by `parent`
/// (within three steps: chest bone → clavicle → arm still counts).
pub(super) fn in_line(parent: &[Option<BoneId>], a: BoneId, b: BoneId) -> bool {
    let up = |from: BoneId, to: BoneId| {
        let mut cur = Some(from);
        for _ in 0..=3 {
            match cur {
                Some(c) if c == to => return true,
                Some(c) => cur = parent.get(c.index()).copied().flatten(),
                None => return false,
            }
        }
        false
    };
    up(a, b) || up(b, a)
}

/// The limb `part` belongs to: itself if it is a limb, else its parent if that is one (hands,
/// toes, hooves).
pub(super) fn limb_of(anatomy: &Anatomy, part: u16) -> Option<u16> {
    let p = anatomy.parts.get(part.idx())?;
    if p.role.is_some() {
        return Some(part);
    }
    p.parent.filter(|q| anatomy.parts.get(q.idx()).is_some_and(|q| q.role.is_some()))
}

impl BodyField {
    /// Skins a mesh built in the spread pose back into the rest pose, by its own weights (the
    /// same blend the GPU applies when animating). No-op for a field built at rest.
    pub fn unbind(&self, data: &mut super::BodyMeshData) {
        if self.unbind.is_empty() {
            return;
        }
        let vertices = data.positions.iter_mut().zip(&data.joints).zip(&data.weights).enumerate();
        for (i, ((position, joints), weights)) in vertices {
            let mut m = bevy::math::Mat3A::ZERO;
            let mut t = Vec3A::ZERO;
            for (&j, &w) in joints.iter().zip(weights) {
                let Some(u) = self.unbind.get(j.idx()) else { continue };
                m += u.matrix3 * w;
                t += u.translation * w;
            }
            let blended = Affine3A { matrix3: m, translation: t };
            *position = blended.transform_point3(*position);
            if let Some(n) = data.normals.get_mut(i) {
                *n = (m * Vec3A::from(*n)).normalize_or(Vec3A::from(*n)).into();
            }
        }
    }
}

impl BodyField {
    /// Distance at `p` over a chunk view's primitives, and the nearest primitive's bone and limb.
    pub(crate) fn probe(&self, view: &ChunkView, p: Vec3A) -> (f32, Option<(BoneId, Option<u16>)>) {
        let (d, nearest) = self.eval(view.prims.iter().filter_map(|&i| self.prims.get(i.idx())), std::iter::empty(), p);
        (d, nearest.map(|n| (n.bone, n.limb)))
    }

    /// Model transform of every bone in the pose the field was built in.
    pub(crate) fn bone_model(&self) -> &[Affine3A] {
        &self.bone_model
    }
}


impl ChunkView<'_> {
    /// This view restricted to a sub-region.
    pub(crate) fn narrow(&self, region: Aabb3d) -> Self {
        let field = self.field;
        let prims = self
            .prims
            .iter()
            .copied()
            .filter(|&i| field.prims.get(i.idx()).is_some_and(|prim| prim.aabb.intersects(&region)))
            .collect();
        let joints = self.joints.clone();
        ChunkView { field, prims, joints, bounds: region }
    }
}

#[cfg(test)]
mod spread_tests {
    use super::*;
    use crate::anatomy::build_src;

    /// A vertex fully weighted to the hand, placed where the hand is in the spread pose, lands
    /// where the hand is at rest.
    #[test]
    fn unbind_returns_vertices_to_rest() {
        let a = build_src(include_str!("../../assets/mobs/biped.body.ron"));
        let field = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: true });
        let arm = a.parts.iter().find(|p| p.role == Some(crate::anatomy::LimbRole::Manipulator)).unwrap();
        let hand = usize::from(arm.bones.end - 1);
        let rest = a.skeleton.model_rest()[hand].transform_point3(Vec3::Y * 0.02);
        let spread = field.unbind[hand].inverse().transform_point3(rest);
        assert!(spread.distance(rest) > 0.05, "the hand moved for meshing");
        let mut data = crate::surface::BodyMeshData { positions: vec![spread], normals: vec![Vec3::X], joints: vec![[u16::try_from(hand).unwrap(), 0, 0, 0]], weights: vec![[1.0, 0.0, 0.0, 0.0]], ..Default::default() };
        field.unbind(&mut data);
        assert!(data.positions[0].distance(rest) < 1e-4, "{} vs {}", data.positions[0], rest);
    }
}

#[cfg(test)]
mod limb_tests {
    use super::*;
    use crate::anatomy::{LimbRole, build_src};

    /// Between the thighs, from the crotch to the knees, the field is the nearer thigh's (with the
    /// body it blends into), never a blend of the two: thighs touch, they don't web together.
    #[test]
    fn thighs_never_web() {
        let mut webbed = Vec::new();
        for (name, ron) in [
            ("biped", include_str!("../../assets/mobs/biped.body.ron")),
            ("curvy", include_str!("../../assets/mobs/curvy.body.ron")),
            ("thick", include_str!("../../assets/mobs/thick.body.ron")),
        ] {
            let anatomy = build_src(ron);
            let field = BodyField::new(&anatomy, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false });
            let rest = anatomy.skeleton.model_rest();
            let legs: Vec<usize> = (0..anatomy.parts.len()).filter(|&i| anatomy.parts[i].role == Some(LimbRole::Locomotor)).collect();
            assert_eq!(legs.len(), 2, "{name}");
            let limbs: Vec<Option<u16>> = field.prims.iter().map(|p| p.limb).collect();
            // Each leg with the body: its primitives and every primitive on no limb.
            let with_body = |leg: usize| -> Vec<usize> {
                (0..limbs.len()).filter(|&i| limbs[i].is_none() || limbs[i] == u16::try_from(leg).ok()).collect()
            };
            let (left, right) = (with_body(legs[0]), with_body(legs[1]));
            let joint = |leg: usize, k: usize| rest[usize::from(anatomy.parts[leg].bones.start) + k].translation;
            let (hip, knee) = ((joint(legs[0], 0) + joint(legs[1], 0)) * 0.5, (joint(legs[0], 1) + joint(legs[1], 1)) * 0.5);
            let mut worst = 0.0f32;
            for i in 0..=20u8 {
                let p = hip.lerp(knee, f32::from(i) / 20.0);
                let want = field.distance_of(&left, p).min(field.distance_of(&right, p));
                let got = field.distance(p);
                worst = worst.max(want - got);
                println!("{name} {i:2}: field {got:+.4}, apart {want:+.4}");
            }
            if worst >= 1e-4 {
                webbed.push(format!("{name} by {worst} m"));
            }
        }
        assert!(webbed.is_empty(), "the thighs web together: {}", webbed.join(", "));
    }

    /// Legs that overlap at rest (thick's thighs) are swung apart for meshing until they clear,
    /// and come back to rest with the mesh; legs already apart (the biped's) stay put.
    #[test]
    fn overlapping_legs_mesh_apart() {
        let spread_of = |ron: &str| {
            let anatomy = build_src(ron);
            let opts = |spread| BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread };
            let (rest, meshing) = (BodyField::new(&anatomy, &opts(false)), BodyField::new(&anatomy, &opts(true)));
            let hip = anatomy.legs[0].chain[0].index();
            let foot = anatomy.legs[0].foot.index();
            let moved = meshing.bone_model[foot].translation.distance(rest.bone_model[foot].translation);
            (rest.legs_apart(&anatomy, LEG_GAP), meshing.legs_apart(&anatomy, LEG_GAP), moved, meshing.unbind[hip])
        };
        let (rest, meshing, moved, _) = spread_of(include_str!("../../assets/mobs/biped.body.ron"));
        assert!(rest && meshing && moved < 1e-5, "biped: apart {rest}/{meshing}, foot moved {moved}");
        let (rest, meshing, moved, unbind) = spread_of(include_str!("../../assets/mobs/thick.body.ron"));
        assert!(!rest && meshing && moved > 0.02, "thick: apart {rest}/{meshing}, foot moved {moved}");
        assert!(!unbind.abs_diff_eq(Affine3A::IDENTITY, 1e-4), "the mesh is skinned back to rest");
    }
}
