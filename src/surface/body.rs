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
/// are made of. A limb blends into the body, and into another limb only where both attach (see
/// [`BodyField::paired`]); elsewhere two only ever meet.
struct Acc<'a> {
    /// The smooth union of the primitives on no limb (`None` until the first).
    body: Option<f32>,
    /// The body's raised stamps, which raise everything.
    raised: f32,
    /// Per limb met so far, its share (limbs past the last slot share it).
    limbs: [LimbField; LIMB_SLOTS],
    /// Slots of `limbs` in use.
    used: usize,
    /// The nearest union primitive of all.
    nearest: Option<&'a PreparedPrim>,
    /// The nearest union primitive on no limb.
    body_nearest: (f32, Option<&'a PreparedPrim>),
    /// Per limb slot, its own nearest union primitive.
    limb_nearest: [(f32, Option<&'a PreparedPrim>); LIMB_SLOTS],
}

impl<'a> Acc<'a> {
    /// Accumulates the primitives on `sheet` (all of them for [`Sheet::Whole`] and [`Sheet::Body`]).
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

    /// The slots in use.
    fn attached_slots(&self) -> &[LimbField] {
        self.limbs.get(..self.used).unwrap_or_default()
    }

    fn limbs(&self) -> impl Iterator<Item = &LimbField> {
        self.limbs.iter().take(self.used)
    }
}

/// One sheet of a body's surface.
///
/// A limb here is any part meshed apart ([`BuiltPart::apart`](crate::anatomy::BuiltPart::apart)):
/// a leg, arm, wing or tail. Where a limb touches the body away from where it attaches, or
/// touches another limb, the mesher builds each one's own surface through the other instead of
/// one surface around both, so they part cleanly when they move (see [`RawMesh::sheets`](super::RawMesh::sheets)).
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
    /// Limbs blended into each other where both attach (see [`BodyField::paired`]; `f32::MAX`
    /// where no two do here): part of the body's sheet with them attached.
    pub(super) paired: f32,
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

/// Where a limb attaches to the body: a ball at each of its root joints, as thick as the limb
/// there (see [`BodyField::attachments`]). The limb blends into the body within `margin` of them,
/// fading out over the next `margin`; beyond, the two only touch.
struct Attachment {
    balls: SmallVec<[(Vec3A, f32); 8]>,
    margin: f32,
    /// A wing or tail (not a leg or arm): it blends into another limb where both attach (see
    /// [`BodyField::paired`]). Legs and arms never blend into each other: thighs touch, they don't
    /// web, even where both hips' attachments overlap at the crotch.
    pairs: bool,
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

/// Where a limb attaches, for the GPU preview: its balls (centre, radius), margin and whether it
/// blends into other limbs where both attach (see [`BodyField::attachment`]).
pub(super) type AttachmentView<'a> = (&'a [(Vec3A, f32)], f32, bool);

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
    /// clear of it, with the limb's first blend as its margin. However far the limb's bones run on
    /// inside the body (a horse's upper foreleg), only there does it join it: elsewhere it is
    /// pressed against it.
    fn attachments(&self, anatomy: &Anatomy) -> Vec<Option<Attachment>> {
        let body: Vec<usize> = (0..self.prims.len()).filter(|&i| self.prims.get(i).is_some_and(|p| p.limb.is_none())).collect();
        if body.is_empty() {
            return Vec::new();
        }
        (0u16..)
            .zip(&anatomy.parts)
            .map(|(pi, part)| {
                part.apart.then_some(())?;
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
                            // A wing or tail joins the body all round it, however flat (a bird's
                            // tail), and on out to where it comes out of the body if sunk into it.
                            let wide = part.role.is_none();
                            let axis = self.bone_model.get(b.index()).map_or(Vec3A::Y, |m| m.matrix3.y_axis);
                            let girth = if wide { self.girth(&own, q, axis, depth) } else { depth };
                            balls.push((q, girth + self.distance_of(&body, q).max(0.0)));
                            if wide {
                                self.sunk_root(&own, &body, part, b, &mut balls);
                            }
                            break;
                        }
                        at = part.bones.clone().map(BoneId).find(|&c| self.bone_parent.get(c.index()).copied().flatten() == Some(b));
                    }
                }
                (!balls.is_empty()).then(|| Attachment { balls, margin: first.blend.max(ATTACH_MARGIN), pairs: part.role.is_none() })
            })
            .collect()
    }

    /// Balls on down the chain of `part` from its root joint `root`, sunk into the body (`body`),
    /// to where it comes out of it: each as thick as the part (`own`) there, the last where its
    /// axis leaves the body. A limb's bones may run on inside the body and part from it (a horse's
    /// upper foreleg); a tail's sunk into a rump, or a wing's into a shoulder, only roots it.
    fn sunk_root(&self, own: &[usize], body: &[usize], part: &crate::anatomy::BuiltPart, root: BoneId, balls: &mut SmallVec<[(Vec3A, f32); 8]>) {
        /// The most balls a sunk root adds.
        const MAX_SUNK: usize = 12;
        let joint = |b: BoneId| self.bone_model.get(b.index()).map(|m| m.translation);
        let child = |b: BoneId| part.bones.clone().map(BoneId).find(|&c| self.bone_parent.get(c.index()).copied().flatten() == Some(b));
        let mut chain: SmallVec<[Vec3A; 8]> = joint(root).into_iter().collect();
        let mut at = child(root);
        while let Some(b) = at {
            chain.extend(joint(b));
            at = child(b);
        }
        let mut added = 0;
        let mut last = balls.last().map_or(0.0, |b| b.1);
        for [a, b] in chain.array_windows::<2>() {
            let len = a.distance(*b).max(1e-4);
            let mut t = 0.0;
            loop {
                // Balls half their radius apart, on to the next joint.
                t += (last * 0.5).max(ATTACH_MARGIN * 0.5);
                let along = (t / len).min(1.0);
                let p = a.lerp(*b, along);
                let depth = -self.distance_of(own, p);
                if depth <= 0.0 || added >= MAX_SUNK {
                    return;
                }
                let girth = self.girth(own, p, *b - *a, depth);
                balls.push((p, girth));
                added += 1;
                last = girth;
                if self.distance_of(body, p) >= 0.0 {
                    return;
                }
                if along >= 1.0 {
                    break;
                }
            }
        }
    }

    /// How far the surface of `own` reaches from `p` (inside it, `depth` from its surface) across
    /// `axis`: the most of eight directions round it.
    fn girth(&self, own: &[usize], p: Vec3A, axis: Vec3A, depth: f32) -> f32 {
        let axis = axis.normalize_or(Vec3A::Y);
        let side = axis.any_orthonormal_vector();
        let up = axis.cross(side);
        (0u8..8)
            .map(|k| {
                let (sin, cos) = (f32::from(k) * std::f32::consts::FRAC_PI_4).sin_cos();
                let dir = side * cos + up * sin;
                // March out by the distance left (a smooth union's distance is conservative).
                let mut t = depth;
                for _ in 0..16 {
                    let d = -self.distance_of(own, p + dir * t);
                    if d <= 1e-4 {
                        break;
                    }
                    t += d;
                }
                t
            })
            .fold(depth, f32::max)
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
        let mut joined = [f32::MAX; LIMB_SLOTS];
        let mut d = acc.body.unwrap_or(f32::MAX);
        for (l, j) in acc.limbs().zip(&mut joined) {
            *j = self.joined(l, acc.body, p);
            d = d.min(*j);
        }
        let d = d.min(self.paired(acc.attached_slots(), &joined, p));
        let d = if d < f32::MAX { d - acc.raised } else { d };
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
        let mut joined = [f32::MAX; LIMB_SLOTS];
        for ((l, &own_near), j) in acc.limbs().zip(&acc.limb_nearest).zip(&mut joined) {
            let detached = self.detached(l.id, p);
            match sheet {
                // The body with the limbs attached here (blended into each other where two do).
                Sheet::Body if !detached => {
                    *j = self.joined(l, acc.body, p);
                    d = d.min(*j);
                    near = closer(near, own_near);
                }
                // Clear of where it attaches, the limb alone: its surface runs on through the body.
                Sheet::Limb(id) if id == l.id && detached => (d, near) = (l.own(), own_near),
                Sheet::Limb(id) if id == l.id => (d, near) = (self.joined(l, acc.body, p), closer(near, own_near)),
                _ => {}
            }
        }
        if sheet == Sheet::Body {
            d = d.min(self.paired(acc.attached_slots(), &joined, p));
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
        let mut all = [f32::MAX; LIMB_SLOTS];
        for (l, j) in acc.limbs().zip(&mut all) {
            *j = self.joined(l, acc.body, p);
            let joined = *j - acc.raised;
            whole = whole.min(joined);
            limbs.push(SheetSample { limb: l.id, own: c(l.own() - acc.raised), joined: c(joined), detached: self.detached(l.id, p) });
        }
        let paired = self.paired(acc.attached_slots(), &all, p);
        let paired = if paired < f32::MAX { c(paired - acc.raised) } else { paired };
        Sample { whole: c(whole.min(paired)), body, limbs, paired }
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

    /// Limbs `limbs` (joined to the body as `joined`, in order) blended into each other where both
    /// attach, if one of the two is a wing or tail: each two's own surfaces blended into each other
    /// by the lesser of their margins (each joined to the body as it is), fading to the hard union
    /// between them (`min` of the two) as the lesser of their [`Attachment::weight`]s does. `f32::MAX` where no two blend here. Elsewhere limbs only meet:
    /// a hind leg and an insect's abdomen web together at the thorax, not down the leg.
    #[inline]
    fn paired(&self, limbs: &[LimbField], joined: &[f32], p: Vec3A) -> f32 {
        let mut out = f32::MAX;
        if limbs.len() < 2 {
            return out;
        }
        // Each limb's attachment and its weight here, worked out when first wanted.
        let mut weight: [Option<f32>; LIMB_SLOTS] = [None; LIMB_SLOTS];
        let attach = |l: &LimbField| self.attach.get(usize::from(l.id)).and_then(Option::as_ref);
        for (i, (a, &ja)) in limbs.iter().zip(joined).enumerate() {
            let Some(at_a) = attach(a).filter(|_| a.own < f32::MAX) else { continue };
            for (j, (b, &jb)) in limbs.iter().zip(joined).enumerate().skip(i + 1) {
                let Some(at_b) = attach(b).filter(|at_b| b.own < f32::MAX && (at_a.pairs || at_b.pairs)) else { continue };
                let (oa, ob) = (a.own(), b.own());
                let k = at_a.margin.min(at_b.margin);
                // Out of each other's blend the two only meet: skip the zone test.
                if (oa - ob).abs() >= k {
                    continue;
                }
                let mut w = |n: usize, at: &Attachment| weight.get_mut(n).map_or_else(|| at.weight(p), |w| *w.get_or_insert_with(|| at.weight(p)));
                let w = w(i, at_a).min(w(j, at_b));
                if w <= 0.0 {
                    continue;
                }
                // Their own surfaces blended into each other (not the body again, which each is
                // already joined to as it is).
                let hard = ja.min(jb);
                let pair = sdf::smin(oa, ob, k).0.min(hard);
                out = out.min((pair - hard).mul_add(w, hard));
            }
        }
        out
    }

    /// The other limbs (up to two; `u16::MAX` past them) that attach at `p` as `limb` does, their
    /// zones overlapping, and blend into it there (one of the two a wing or tail).
    pub(super) fn shares_attachment(&self, limb: u16, p: Vec3A) -> [u16; 2] {
        let mut out = [u16::MAX; 2];
        if self.attach_weight(limb, p) <= 0.0 {
            return out;
        }
        let pairs = self.attach.get(usize::from(limb)).and_then(Option::as_ref).is_some_and(|a| a.pairs);
        let others = (0u16..).zip(&self.attach).filter(|(m, a)| *m != limb && a.as_ref().is_some_and(|a| (pairs || a.pairs) && a.weight(p) > 0.0));
        for (slot, (m, _)) in out.iter_mut().zip(others) {
            *slot = m;
        }
        out
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

    /// Where `limb` attaches: its balls (centre, radius), margin and whether it blends into other
    /// limbs where both attach (`None` if it doesn't attach), for the GPU preview.
    pub(super) fn attachment(&self, limb: u16) -> Option<AttachmentView<'_>> {
        self.attach.get(usize::from(limb)).and_then(Option::as_ref).map(|a| (&a.balls[..], a.margin, a.pairs))
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

    /// How far `p` is past where `limb` attaches, out to where it is [`detached`](Self::detached)
    /// (0 inside that; infinite for a limb that doesn't attach).
    #[cfg(test)]
    pub(super) fn past_attachment(&self, limb: u16, p: Vec3A) -> f32 {
        self.attach.get(usize::from(limb)).and_then(Option::as_ref).map_or(f32::INFINITY, |a| {
            let e = a.balls.iter().fold(f32::INFINITY, |e, (c, r)| e.min(p.distance(*c) - r));
            2.0f32.mul_add(-a.margin, e).max(0.0)
        })
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

/// The limb `part` belongs to: itself if it is meshed apart (a leg, arm, wing or tail, see
/// [`BuiltPart::apart`](crate::anatomy::BuiltPart::apart)), else its parent if that is one (hands,
/// toes, hooves, a tail's club).
pub(super) fn limb_of(anatomy: &Anatomy, part: u16) -> Option<u16> {
    let p = anatomy.parts.get(part.idx())?;
    if p.apart {
        return Some(part);
    }
    p.parent.filter(|q| anatomy.parts.get(q.idx()).is_some_and(|q| q.apart))
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
mod apart_tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};

    fn field_of(body: &str) -> (Anatomy, BodyField) {
        let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let plan: BodyPlan = ron::from_str(&text).unwrap();
        let anatomy = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
        let field = BodyField::new(&anatomy, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false });
        (anatomy, field)
    }

    /// Every mob's limbs (legs, arms, wings, tails) fit the slots `BodyField::eval` keeps them
    /// apart in at one point (and `sdf_field.wesl` in the preview): a dragon has seven.
    #[test]
    fn every_mob_fits_its_limb_slots() {
        let dir = format!("{}/assets/mobs", env!("CARGO_MANIFEST_DIR"));
        let mut bodies: Vec<String> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix(".body.ron").map(str::to_owned)).collect();
        bodies.sort();
        assert!(bodies.iter().any(|b| b == "dragon"), "{bodies:?}");
        for body in bodies {
            let text = std::fs::read_to_string(format!("{dir}/{body}.body.ron")).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            // Bodies with authored parts need their meshes loaded (`beast`): left out.
            let Ok(anatomy) = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()) else {
                println!("{body}: needs authored meshes, skipped");
                continue;
            };
            let field = BodyField::new(&anatomy, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false });
            let mut limbs: Vec<u16> = field.prims.iter().filter_map(|p| p.limb).collect();
            limbs.sort_unstable();
            limbs.dedup();
            assert!(limbs.len() <= LIMB_SLOTS, "{body}: {} limbs, {LIMB_SLOTS} slots", limbs.len());
        }
    }

    /// A hexapod's hind leg and its abdomen (a tail) both attach at the thorax: there they blend
    /// into each other as each does into the body; wherever either is clear of where it attaches
    /// they only meet, a hard union of the two.
    #[test]
    fn limbs_blend_only_where_both_attach() {
        let (a, field) = field_of("hexapod");
        let part = |name: &str| (0u16..).zip(&a.parts).find(|(_, p)| p.path.0.ends_with(name)).map(|(i, _)| i).unwrap();
        let (leg, abdomen) = (part("leg.hind.l"), part("abdomen"));
        let on = |limb: Option<u16>| -> Vec<usize> { (0..field.prims.len()).filter(|&i| field.prims[i].limb.is_none() || field.prims[i].limb == limb).collect() };
        let (with_leg, with_abdomen) = (on(Some(leg)), on(Some(abdomen)));
        let both: Vec<usize> = (0..field.prims.len()).filter(|&i| field.prims[i].limb.is_none_or(|l| l == leg || l == abdomen)).collect();
        let hip = field.bone_model[usize::from(a.parts[leg.idx()].bones.start)].translation;
        let (mut blended, mut worst_apart) = (0, 0.0f32);
        let step = 0.012;
        for z in -12i8..=12 {
            for y in -12i8..=12 {
                for x in -12i8..=12 {
                    let p = hip + Vec3A::new(f32::from(x), f32::from(y), f32::from(z)) * step;
                    let hard = field.distance_of(&with_leg, p).min(field.distance_of(&with_abdomen, p));
                    let d = field.distance_of(&both, p);
                    if field.attach_weight(leg, p).min(field.attach_weight(abdomen, p)) <= 0.0 {
                        worst_apart = worst_apart.max((d - hard).abs());
                    } else if d < hard - 1e-3 {
                        blended += 1;
                    }
                }
            }
        }
        println!("{blended} points blended, at most {worst_apart} m off the hard union clear of where both attach");
        assert!(blended > 0, "the hind leg and abdomen never blend where both attach");
        assert!(worst_apart < 1e-5, "they blend {worst_apart} m clear of where both attach");
    }

    /// Where each wing and tail attaches: its root joints' depth in the body, its balls and margin.
    #[test]
    #[ignore = "probe"]
    fn wing_tail_roots() {
        for body in ["bat", "dragon", "wyvern", "bird", "horse", "quadruped", "beetle", "hexapod", "dragonfly"] {
            let (a, field) = field_of(body);
            let body_prims: Vec<usize> = (0..field.prims.len()).filter(|&i| field.prims[i].limb.is_none()).collect();
            for (pi, part) in (0u16..).zip(&a.parts).filter(|(_, p)| p.apart && p.role.is_none()) {
                let root = field.bone_model[usize::from(part.bones.start)].translation;
                let prims = field.prims.iter().filter(|p| p.limb == Some(pi)).count();
                println!(
                    "{body} {}: {prims} prims, root {:+.3} in body, attach {:?}",
                    part.path.0,
                    field.distance_of(&body_prims, root),
                    field.attachment(pi).map(|(b, m, _)| (b.iter().map(|(_, r)| (r * 1000.0).round() / 1000.0).collect::<Vec<_>>(), m)),
                );
            }
        }
    }
}
