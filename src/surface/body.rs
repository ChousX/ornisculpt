use bevy::math::Affine3A;
use bevy::prelude::*;
use bevy::shape::{Aabb3d, BoundingVolume, IntersectsVolume};

use super::{Field, Influences, SurfaceInfo, sdf};
use crate::num::{Idx, Saturate, ToF32, min_axis_scale};
use crate::anatomy::{Anatomy, BoneId, BuiltPart, LodLevel, PrimLayer, PrimOp, PrimShape, RegionId};
use crate::random::{Noise, NoiseSettings};
use smallvec::SmallVec;

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
    /// Per part, where it attaches to the body if it is a limb.
    attach: Vec<Option<Attachment>>,
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

/// The least margin around where a limb attaches within which it blends into the body, metres.
const ATTACH_MARGIN: f32 = 0.01;

/// Creases only displace the field this near the surface, metres.
const CREASE_NEAR: f32 = 0.03;

/// One limb's share of [`BodyField::eval`] at a point.
#[derive(Clone, Copy)]
struct LimbField {
    id: u16,
    /// The smooth union of its own primitives (`f32::MAX` until its first).
    own: f32,
    /// Its primitives blended into the body as they come, each by its own blend (`f32::MAX`
    /// until its first): how it joins the body where it attaches.
    with_body: f32,
    /// Its own raised stamps.
    raised: f32,
}

impl LimbField {
    const EMPTY: Self = Self { id: u16::MAX, own: f32::MAX, with_body: f32::MAX, raised: 0.0 };

    /// Its own surface, raised by its own stamps.
    fn own(&self) -> f32 {
        self.own - self.raised
    }
}

/// The body and each limb at one point, kept apart: what [`BodyField::joined`] and the sheets
/// are made of.
struct Acc<'a> {
    /// Primitives on no limb (`None` until the first).
    body: Option<f32>,
    /// The body's raised stamps, which raise everything.
    raised: f32,
    limbs: [LimbField; LIMB_SLOTS],
    used: usize,
    nearest: Option<&'a PreparedPrim>,
    /// The nearest union primitive on no limb.
    body_nearest: (f32, Option<&'a PreparedPrim>),
    /// Per limb slot, its own nearest union primitive.
    limb_nearest: [(f32, Option<&'a PreparedPrim>); LIMB_SLOTS],
}

impl<'a> Acc<'a> {
    /// Accumulates the primitives on `sheet` (all of them for [`Sheet::Whole`]).
    #[inline]
    fn gather(prims: impl Iterator<Item = &'a PreparedPrim>, sheet: Sheet, p: Vec3A) -> Self {
        let mut acc = Self {
            body: None,
            raised: 0.0,
            limbs: [LimbField::EMPTY; LIMB_SLOTS],
            used: 0,
            nearest: None,
            body_nearest: (f32::INFINITY, None),
            limb_nearest: [(f32::INFINITY, None); LIMB_SLOTS],
        };
        let mut best = f32::INFINITY;
        for prim in prims.filter(|prim| sheet.has(prim)) {
            let (dl, blend) = prim.eval(p);
            match prim.op {
                PrimOp::Union => {
                    let body = acc.body;
                    if let Some((l, near)) = prim.limb.and_then(|limb| acc.slot(limb)) {
                        l.own = if l.own < f32::MAX { sdf::smin(l.own, dl, blend).0 } else { dl };
                        // A new limb starts from the body so far.
                        let from = if l.with_body < f32::MAX { Some(l.with_body) } else { body };
                        l.with_body = from.map_or(dl, |d| sdf::smin(d, dl, blend).0);
                        if dl < near.0 {
                            *near = (dl, Some(prim));
                        }
                    } else {
                        acc.body = Some(acc.body.map_or(dl, |d| sdf::smin(d, dl, blend).0));
                        for l in acc.limbs.iter_mut().take(acc.used).filter(|l| l.with_body < f32::MAX) {
                            l.with_body = sdf::smin(l.with_body, dl, blend).0;
                        }
                        if dl < acc.body_nearest.0 {
                            acc.body_nearest = (dl, Some(prim));
                        }
                    }
                    if dl < best {
                        best = dl;
                        acc.nearest = Some(prim);
                    }
                }
                PrimOp::Subtract => {
                    acc.body = acc.body.map(|d| sdf::ssub(d, dl, blend));
                    for l in acc.limbs.iter_mut().take(acc.used).filter(|l| l.own < f32::MAX) {
                        l.own = sdf::ssub(l.own, dl, blend);
                        l.with_body = sdf::ssub(l.with_body, dl, blend);
                    }
                }
                PrimOp::Displace { height } => {
                    if let PrimShape::Ellipsoid { radii } = &prim.shape {
                        let s = 1.0 - (prim.to_local.transform_point3a(p) / Vec3A::from(*radii)).length_squared();
                        if s > 0.0 {
                            // A limb's stamps raise only that limb.
                            let rise = height * prim.min_scale * s * s;
                            match prim.limb.and_then(|limb| acc.slot(limb)) {
                                Some((l, _)) => l.raised += rise,
                                None => acc.raised += rise,
                            }
                        }
                    }
                }
            }
        }
        acc
    }

    /// The slot of `limb`, opened if new (limbs past the last slot share it).
    fn slot(&mut self, limb: u16) -> Option<(&mut LimbField, &mut (f32, Option<&'a PreparedPrim>))> {
        let i = self.limbs.iter().take(self.used).position(|l| l.id == limb).unwrap_or_else(|| {
            self.used = (self.used + 1).min(LIMB_SLOTS);
            self.used - 1
        });
        let (l, near) = (self.limbs.get_mut(i)?, self.limb_nearest.get_mut(i)?);
        if l.id == u16::MAX {
            l.id = limb;
        }
        Some((l, near))
    }

    fn limbs(&self) -> impl Iterator<Item = &LimbField> {
        self.limbs.iter().take(self.used)
    }
}

/// One sheet of a body's surface.
///
/// Where a limb touches the body away from where it attaches, or touches another limb, the mesher
/// builds each one's own surface through the other instead of one surface around both, so they
/// part cleanly when they move (see [`RawMesh::sheets`](super::RawMesh::sheets)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    /// The whole body: every primitive, limbs joined.
    Whole,
    /// The body with the limbs attached at each point (every limb but those clear of where they
    /// attach).
    Body,
    /// One limb (a part index): its own primitives where it is clear of where it attaches, joined
    /// to the body where it is not.
    Limb(u16),
}

impl Sheet {
    /// [`RawMesh::sheets`](super::RawMesh::sheets) id of [`Sheet::Whole`].
    pub const WHOLE_ID: u16 = u16::MAX;
    /// [`RawMesh::sheets`](super::RawMesh::sheets) id of [`Sheet::Body`].
    pub const BODY_ID: u16 = u16::MAX - 1;

    #[must_use]
    pub const fn id(self) -> u16 {
        match self {
            Self::Whole => Self::WHOLE_ID,
            Self::Body => Self::BODY_ID,
            Self::Limb(l) => l,
        }
    }

    #[must_use]
    pub const fn from_id(id: u16) -> Self {
        match id {
            Self::WHOLE_ID => Self::Whole,
            Self::BODY_ID => Self::Body,
            l => Self::Limb(l),
        }
    }

    /// Whether a primitive is on this sheet.
    #[inline]
    fn has(self, prim: &PreparedPrim) -> bool {
        match self {
            Self::Whole | Self::Body => true,
            Self::Limb(l) => prim.limb.is_none_or(|own| own == l),
        }
    }
}

/// The field and its sheets at one point (all with raised stamps and creases).
#[derive(Clone)]
pub(super) struct Sample {
    /// The whole field.
    pub(super) whole: f32,
    /// The body alone (`f32::MAX` where no body primitive reaches).
    pub(super) body: f32,
    pub(super) limbs: SmallVec<[SheetSample; 4]>,
}

/// One limb in a [`Sample`].
#[derive(Clone, Copy)]
pub(super) struct SheetSample {
    pub(super) limb: u16,
    /// Its own surface.
    pub(super) own: f32,
    /// Joined to the body (as the whole field joins it).
    pub(super) joined: f32,
    /// Clear of where it attaches (see [`BodyField::detached`]).
    pub(super) detached: bool,
}

/// Where a limb attaches to the body: the stretch of its skeleton inside the body, as balls as
/// thick as the limb there. The limb blends into the body within `margin` of them, fading out
/// over the next `margin`; beyond, the two only touch.
struct Attachment {
    balls: SmallVec<[(Vec3A, f32); 8]>,
    margin: f32,
}

impl Attachment {
    /// 1 where the limb attaches, fading to 0 clear of it.
    #[inline]
    fn weight(&self, p: Vec3A) -> f32 {
        let e = self.balls.iter().fold(f32::INFINITY, |e, (c, r)| e.min(p.distance(*c) - r));
        if e <= self.margin {
            return 1.0;
        }
        let t = ((e - self.margin) / self.margin).min(1.0);
        (t * t).mul_add(-2.0f32.mul_add(-t, 3.0), 1.0)
    }
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
            let min_scale = min_axis_scale(&m);
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
        let mut field = Self { prims, bone_parent, unbind, bone_model: model, bounds, creases, attach: Vec::new() };
        field.attach = field.attachments(anatomy);
        field
    }

    /// Where each limb attaches: at each of its root joints (a bone whose parent is off the limb,
    /// or past it to the first joint inside the limb's own surface, the shoulder past the
    /// clavicle), a ball as thick as the limb there, reaching to the body if the joint stands
    /// clear of it. However far the limb's bones run on inside the body (a horse's upper foreleg),
    /// only there does it join it: elsewhere it is pressed against it.
    fn attachments(&self, anatomy: &Anatomy) -> Vec<Option<Attachment>> {
        let body: Vec<usize> = (0..self.prims.len()).filter(|&i| self.prims.get(i).is_some_and(|p| p.limb.is_none())).collect();
        if body.is_empty() {
            return Vec::new();
        }
        (0u16..)
            .zip(&anatomy.parts)
            .map(|(pi, part)| {
                part.role?;
                let own: Vec<usize> = (0..self.prims.len()).filter(|&i| self.prims.get(i).is_some_and(|p| p.limb == Some(pi))).collect();
                let first = own.iter().filter_map(|&i| self.prims.get(i)).find(|p| p.op == PrimOp::Union)?;
                let on_part = |b: BoneId| part.bones.contains(&b.0);
                let joint = |b: BoneId| self.bone_model.get(b.index()).map(|m| m.translation);
                let mut balls = SmallVec::new();
                for root in part.bones.clone().map(BoneId).filter(|&b| self.bone_parent.get(b.index()).copied().flatten().is_none_or(|q| !on_part(q))) {
                    // Down the chain from the root to the first joint inside the limb's surface.
                    let mut at = Some(root);
                    while let Some(b) = at {
                        let Some(q) = joint(b) else { break };
                        let depth = -self.distance_of(&own, q);
                        if depth > 0.0 {
                            balls.push((q, depth + self.distance_of(&body, q).max(0.0)));
                            break;
                        }
                        at = part.bones.clone().map(BoneId).find(|&c| self.bone_parent.get(c.index()).copied().flatten() == Some(b));
                    }
                }
                (!balls.is_empty()).then(|| Attachment { balls, margin: first.blend.max(ATTACH_MARGIN) })
            })
            .collect()
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
        let prims: Vec<u32> = (0u32..)
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
        let may_part = may_part(prims.iter().filter_map(|&i| self.prims.get(i.idx())));
        ChunkView { field: self, prims, joints, bounds: region, may_part }
    }

    #[must_use]
    pub const fn primitive_count(&self) -> usize {
        self.prims.len()
    }

    /// The field over `prims` (as [`Sheet`] picks them), plus creases from `joints`; returns the
    /// distance and the nearest union primitive.
    #[inline]
    fn eval<'a, 'j>(
        &self,
        prims: impl Iterator<Item = &'a PreparedPrim>,
        joints: impl Iterator<Item = &'j CreaseJoint>,
        p: Vec3A,
    ) -> (f32, Option<&'a PreparedPrim>) {
        let acc = Acc::gather(prims, Sheet::Whole, p);
        let d = acc.body.map_or(f32::MAX, |b| b - acc.raised);
        let d = acc.limbs().fold(d, |d, l| d.min(self.joined(l, acc.body, p) - acc.raised));
        (self.creased(joints, p, d), acc.nearest)
    }

    /// One sheet of the field at `p` (see [`Sheet`]), with the nearest union primitive on it.
    fn eval_sheet<'a, 'j>(
        &self,
        prims: impl Iterator<Item = &'a PreparedPrim>,
        joints: impl Iterator<Item = &'j CreaseJoint>,
        sheet: Sheet,
        p: Vec3A,
    ) -> (f32, Option<&'a PreparedPrim>) {
        if sheet == Sheet::Whole {
            return self.eval(prims, joints, p);
        }
        let acc = Acc::gather(prims, sheet, p);
        let closer = |a: (f32, Option<&'a PreparedPrim>), b: (f32, Option<&'a PreparedPrim>)| if b.0 < a.0 { b } else { a };
        let mut d = match sheet {
            Sheet::Limb(_) => f32::MAX,
            _ => acc.body.unwrap_or(f32::MAX),
        };
        let mut near = acc.body_nearest;
        for (l, &own_near) in acc.limbs().zip(&acc.limb_nearest) {
            let detached = self.detached(l.id, p);
            match sheet {
                // The body with the limbs attached here.
                Sheet::Body if !detached => {
                    d = d.min(self.joined(l, acc.body, p));
                    near = closer(near, own_near);
                }
                // Clear of where it attaches, the limb alone: its surface runs on through the body.
                Sheet::Limb(id) if id == l.id && detached => (d, near) = (l.own(), own_near),
                Sheet::Limb(id) if id == l.id => (d, near) = (self.joined(l, acc.body, p), closer(near, own_near)),
                _ => {}
            }
        }
        (self.creased(joints, p, d - acc.raised), near.1)
    }

    /// The whole field at `p` and each of its sheets there, for meshing limbs apart.
    fn eval_sample<'a, 'j>(&self, prims: impl Iterator<Item = &'a PreparedPrim>, joints: impl Iterator<Item = &'j CreaseJoint>, p: Vec3A) -> Sample {
        let acc = Acc::gather(prims, Sheet::Whole, p);
        let crease = self.creases.as_ref().map_or(0.0, |c| c.displacement(joints, p));
        // As `creased`, which only displaces near the surface.
        let c = |d: f32| if d.abs() < CREASE_NEAR { d + crease } else { d };
        let body = acc.body.map_or(f32::MAX, |b| c(b - acc.raised));
        let mut whole = acc.body.map_or(f32::MAX, |b| b - acc.raised);
        let mut limbs = SmallVec::new();
        for l in acc.limbs() {
            let joined = self.joined(l, acc.body, p) - acc.raised;
            whole = whole.min(joined);
            limbs.push(SheetSample { limb: l.id, own: c(l.own() - acc.raised), joined: c(joined), detached: self.detached(l.id, p) });
        }
        Sample { whole: c(whole), body, limbs }
    }

    /// A limb joined to the body (`body`, before the body's raised stamps): blended in as its
    /// primitives come where it attaches, fading to a hard union clear of that.
    #[inline]
    fn joined(&self, l: &LimbField, body: Option<f32>, p: Vec3A) -> f32 {
        // A limb with only raised stamps here has no union yet: it raises the body.
        if l.own >= f32::MAX {
            return body.map_or(f32::MAX, |b| b - l.raised);
        }
        let own = l.own();
        let Some(b) = body else { return own };
        let hard = b.min(own);
        let blended = l.with_body - l.raised;
        // Where the blends change nothing, skip the zone test.
        if (blended - hard).abs() < 1e-6 {
            return hard;
        }
        let w = self.attach.get(usize::from(l.id)).and_then(Option::as_ref).map_or(0.0, |a| a.weight(p));
        (blended - hard).mul_add(w, hard)
    }

    /// The field and its sheets at `p`.
    pub(super) fn sample_at(&self, p: Vec3A) -> Sample {
        let joints = self.creases.as_ref().map_or(&[][..], |c| &c.joints);
        self.eval_sample(self.prims.iter(), joints.iter(), p)
    }

    /// The limb whose own surface `p` lies on rather than the body's, if any.
    pub(super) fn surface_limb(&self, p: Vec3A) -> Option<u16> {
        let s = self.sample_at(p);
        s.limbs.iter().filter(|l| l.own.abs() <= s.body.abs()).min_by(|a, b| a.own.abs().total_cmp(&b.own.abs())).map(|l| l.limb)
    }

    /// How much `limb` attaches to the body at `p`: 1 where it does, fading to 0 clear of it.
    pub(super) fn attach_weight(&self, limb: u16, p: Vec3A) -> f32 {
        self.attach.get(usize::from(limb)).and_then(Option::as_ref).map_or(0.0, |a| a.weight(p))
    }

    /// How thick `limb` is where it attaches (0 if it doesn't).
    pub(super) fn attach_radius(&self, limb: u16) -> f32 {
        self.attach.get(usize::from(limb)).and_then(Option::as_ref).map_or(0.0, |a| a.balls.iter().map(|b| b.1).fold(0.0, f32::max))
    }

    /// Whether `limb` is clear of where it attaches at `p`: there it and the body are separate
    /// surfaces, however close.
    pub(super) fn detached(&self, limb: u16, p: Vec3A) -> bool {
        self.attach.get(usize::from(limb)).and_then(Option::as_ref).is_none_or(|a| a.weight(p) <= 0.0)
    }

    /// `d` displaced by the skin creases near the surface.
    #[inline]
    fn creased<'j>(&self, joints: impl Iterator<Item = &'j CreaseJoint>, p: Vec3A, d: f32) -> f32 {
        match &self.creases {
            Some(c) if d.abs() < CREASE_NEAR => d + c.displacement(joints, p),
            _ => d,
        }
    }

    fn info<'a>(&self, prims: impl Iterator<Item = &'a PreparedPrim> + Clone, p: Vec3A) -> SurfaceInfo {
        self.info_on(prims, Sheet::Whole, p)
    }

    /// [`Field::surface_info`] of one sheet: weights only from the primitives on it (a limb's own,
    /// where it is clear of the body).
    fn info_on<'a>(&self, prims: impl Iterator<Item = &'a PreparedPrim> + Clone, sheet: Sheet, p: Vec3A) -> SurfaceInfo {
        let (d, near) = self.eval_sheet(prims.clone(), std::iter::empty(), sheet, p);
        let on = |prim: &PreparedPrim| match (sheet, prim.limb) {
            (Sheet::Whole, _) | (Sheet::Body | Sheet::Limb(_), None) => !matches!(sheet, Sheet::Limb(l) if self.detached(l, p)),
            (Sheet::Body, Some(l)) => !self.detached(l, p),
            (Sheet::Limb(l), Some(own)) => own == l,
        };
        let prims = prims.filter(|prim| on(prim));
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
    may_part: bool,
}

impl ChunkView<'_> {
    /// No primitive reaches this chunk; the mesher can skip it.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.prims.is_empty()
    }

    /// Whether the chunk reaches a limb and the body, or two limbs: where the surface may part
    /// into sheets.
    pub(super) const fn may_part(&self) -> bool {
        self.may_part
    }

    /// The field and its sheets at `p`.
    pub(super) fn sample(&self, p: Vec3A) -> Sample {
        self.field.eval_sample(self.prims(), self.joints(), p)
    }

    /// One sheet of the field at `p`.
    pub(super) fn sheet_distance(&self, sheet: Sheet, p: Vec3A) -> f32 {
        self.field.eval_sheet(self.prims(), self.joints(), sheet, p).0
    }

    /// One sheet's gradient at `p` (as [`Field::gradient`]: a raw central difference over ±1 mm).
    pub(super) fn sheet_gradient(&self, sheet: Sheet, p: Vec3A) -> Vec3A {
        let d = |o: Vec3A| self.sheet_distance(sheet, p + o) - self.sheet_distance(sheet, p - o);
        Vec3A::new(d(Vec3A::X * 1e-3), d(Vec3A::Y * 1e-3), d(Vec3A::Z * 1e-3))
    }
}

/// Whether primitives reach a limb and the body, or two limbs (see [`ChunkView::may_part`]).
fn may_part<'a>(prims: impl Iterator<Item = &'a PreparedPrim>) -> bool {
    let mut body = false;
    let mut limb: Option<u16> = None;
    for prim in prims.filter(|p| p.op == PrimOp::Union) {
        match (prim.limb, limb) {
            (None, Some(_)) => return true,
            (None, None) => body = true,
            (Some(l), Some(m)) if l != m => return true,
            (Some(l), _) => {
                if body {
                    return true;
                }
                limb = Some(l);
            }
        }
    }
    false
}

/// One sheet of a [`BodyField`] as a field of its own, for the vertices the mesher put on it.
pub struct SheetField<'a> {
    field: &'a BodyField,
    sheet: Sheet,
}

impl Field for SheetField<'_> {
    fn bounds(&self) -> Aabb3d {
        self.field.bounds
    }

    fn distance(&self, p: Vec3A) -> f32 {
        let joints = self.field.creases.as_ref().map_or(&[][..], |c| &c.joints);
        self.field.eval_sheet(self.field.prims.iter(), joints.iter(), self.sheet, p).0
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.field.info_on(self.field.prims.iter(), self.sheet, p)
    }
}

impl BodyField {
    /// One sheet of the field, as a field of its own.
    #[must_use]
    pub const fn sheet(&self, sheet: Sheet) -> SheetField<'_> {
        SheetField { field: self, sheet }
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
        let prims: Vec<u32> = self
            .prims
            .iter()
            .copied()
            .filter(|&i| field.prims.get(i.idx()).is_some_and(|prim| prim.aabb.intersects(&region)))
            .collect();
        let joints = self.joints.clone();
        let may_part = self.may_part && may_part(prims.iter().filter_map(|&i| field.prims.get(i.idx())));
        ChunkView { field, prims, joints, bounds: region, may_part }
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

#[cfg(test)]
#[allow(clippy::as_conversions, clippy::cast_precision_loss, clippy::many_single_char_names, reason = "throwaway diagnostic probe")]
mod attach_probe {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};

    /// Per limb: each chain joint's body distance and the limb's own prims there.
    #[test]
    #[ignore = "probe"]
    fn horse_zones() {
        for name in ["horse", "biped"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{name}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let f = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: true });
            let body: Vec<usize> = (0..f.prims.len()).filter(|&i| f.prims[i].limb.is_none()).collect();
            for (pi, p) in a.parts.iter().enumerate() {
                let Some(att) = f.attach.get(pi).and_then(Option::as_ref) else { continue };
                let own: Vec<usize> = (0..f.prims.len()).filter(|&i| f.prims[i].limb == u16::try_from(pi).ok()).collect();
                println!("{name} {}: {} balls, max r {:.3}, margin {:.3}", p.path.0, att.balls.len(), att.balls.iter().map(|b| b.1).fold(0.0, f32::max), att.margin);
                // Along the limb's surface on the body side: from each joint, step towards the body centre.
                for b in p.bones.clone() {
                    let m = f.bone_model[usize::from(b)];
                    let len = a.skeleton.bones[usize::from(b)].length;
                    for k in 0..=4 {
                        let q = m.translation + m.matrix3.y_axis.normalize() * len * (k as f32 / 4.0);
                        let (db, dl) = (f.distance_of(&body, q), f.distance_of(&own, q));
                        print!(" [{}:{k} body {db:+.3} own {dl:+.3} w {:.2}]", a.skeleton.bones[usize::from(b)].name, att.weight(q));
                    }
                    println!();
                }
            }
        }
    }

    #[test]
    #[ignore = "probe"]
    fn limb_joints_vs_body() {
        for name in ["horse", "biped", "thick", "quadruped", "beast", "bat", "dragon", "hexapod"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{name}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let f = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: true });
            let body: Vec<usize> = (0..f.prims.len()).filter(|&i| f.prims[i].limb.is_none()).collect();
            println!("== {name}");
            for (pi, p) in a.parts.iter().enumerate() {
                if p.role.is_none() { continue; }
                let own: Vec<usize> = (0..f.prims.len()).filter(|&i| f.prims[i].limb == u16::try_from(pi).ok()).collect();
                let s: Vec<String> = p.bones.clone().map(|b| {
                    let at = f.bone_model[usize::from(b)].translation;
                    format!("{}:{:+.3}/{:+.3}", a.skeleton.bones[usize::from(b)].name, f.distance_of(&body, at), f.distance_of(&own, at))
                }).collect();
                println!("{} {:?} parent {:?}: {}", p.path.0, p.role, p.parent, s.join("  "));
            }
        }
    }
}
