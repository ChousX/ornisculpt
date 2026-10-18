use bevy::math::Affine3A;
use bevy::prelude::*;
use bevy::shape::{Aabb3d, BoundingVolume, IntersectsVolume};

use super::{Field, Influences, SurfaceInfo, sdf};
use crate::anatomy::{Anatomy, BoneId, CrossSection, LodLevel, PrimLayer, PrimOp, PrimShape, RegionId};
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
    prims: Vec<PreparedPrim>,
    /// Parent of every bone, for keeping skin weights on one limb.
    bone_parent: Vec<Option<BoneId>>,
    /// Per bone, the spread pose → rest pose transform (empty when built at rest).
    unbind: Vec<Affine3A>,
    /// Model transform of every bone in the pose the field was built in.
    bone_model: Vec<Affine3A>,
    bounds: Aabb3d,
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
    fn displacement(&self, joints: impl Iterator<Item = usize>, p: Vec3A) -> f32 {
        let mut mask = 0.0f32;
        let mut along = 0.0;
        for j in joints {
            let joint = &self.joints[j];
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
        let phase = along * self.frequency * std::f32::consts::TAU + self.wobble.sample(p.into()) * 2.0;
        let ridge = (1.0 - phase.sin().abs()).powi(4);
        self.amplitude * mask * mask * ridge
    }
}

/// A primitive moved into model space at rest pose, ready for evaluation.
struct PreparedPrim {
    /// Model space → primitive local space.
    to_local: Affine3A,
    /// Smallest axis scale of the local → model transform (keeps distances conservative).
    min_scale: f32,
    shape: PreparedShape,
    blend: f32,
    /// Local anchor point and reach for blend fading (see `Primitive::blend_anchor`).
    anchor: Option<(Vec3A, f32)>,
    op: PrimOp,
    layer: PrimLayer,
    region: RegionId,
    bone: BoneId,
    /// The limb (part index) this primitive belongs to, if it is on one: different limbs meet
    /// with a hard union, so an arm hanging by a thigh never webs onto it.
    limb: Option<u16>,
    /// Model-space bounds padded by `blend` (and fold amplitude) for culling.
    aabb: Aabb3d,
}

/// [`PrimShape`] with sweeps pre-converted for fast evaluation.
enum PreparedShape {
    Simple(PrimShape),
    Sweep { points: Vec<Vec3A>, radii: Vec<f32>, cross: CrossSection },
}

impl PreparedPrim {
    /// Distance and effective blend radius at `p`.
    #[inline]
    fn eval(&self, p: Vec3A) -> (f32, f32) {
        let q = self.to_local.transform_point3a(p);
        let d = match &self.shape {
            PreparedShape::Simple(s) => sdf::shape(q, s),
            PreparedShape::Sweep { points, radii, cross } => sdf::sweep(q, points, radii, *cross),
        };
        let blend = match self.anchor {
            Some((a, reach)) => self.blend * (1.0 - (q - a).length() * self.min_scale / reach).clamp(0.0, 1.0),
            None => self.blend,
        };
        (d * self.min_scale, blend)
    }
}

/// Local-space bounds of a shape.
fn local_bounds(shape: &PrimShape) -> (Vec3, Vec3) {
    match shape {
        PrimShape::Sphere { r } => (Vec3::splat(-r), Vec3::splat(*r)),
        PrimShape::Ellipsoid { radii } => (-*radii, *radii),
        PrimShape::Capsule { r0, r1, len } | PrimShape::RoundCone { r0, r1, len } => {
            let r = r0.max(*r1);
            (Vec3::new(-r, -r0, -r), Vec3::new(r, len + r1, r))
        }
        PrimShape::RoundBox { half, .. } => (-*half, *half),
        PrimShape::Grid { grid, mirror_x } => {
            let (lo, hi) = grid.bounds();
            if *mirror_x { (Vec3::new(-hi.x, lo.y, lo.z), Vec3::new(-lo.x, hi.y, hi.z)) } else { (lo, hi) }
        }
        PrimShape::Sweep { points, radii, .. } => {
            let r = radii.iter().copied().fold(0.0, f32::max);
            let lo = points.iter().copied().fold(Vec3::INFINITY, Vec3::min);
            let hi = points.iter().copied().fold(Vec3::NEG_INFINITY, Vec3::max);
            (lo - r, hi + r)
        }
    }
}

fn transform_aabb(m: &Affine3A, (lo, hi): (Vec3, Vec3)) -> Aabb3d {
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
    pub fn new(anatomy: &Anatomy, opts: &BodyFieldOptions) -> Self {
        let rest = anatomy.skeleton.model_rest();
        let (model, unbind) = if opts.spread {
            let pose = crate::anatomy::spread_pose(anatomy, &crate::anatomy::Spread::default());
            let mut model: Vec<Affine3A> = Vec::with_capacity(pose.len());
            for (i, b) in anatomy.skeleton.bones.iter().enumerate() {
                let l = pose[i].compute_affine();
                model.push(b.parent.map_or(l, |p| model[p.index()] * l));
            }
            let unbind = rest.iter().zip(&model).map(|(r, s)| *r * s.inverse()).collect();
            (model, unbind)
        } else {
            (rest, Vec::new())
        };
        let prim_part = primitive_parts(anatomy);
        let body_tone = anatomy.parts.first().map(|p| p.tone).unwrap_or_default();
        let fold_amp = body_tone.fold_noise.amplitude * body_tone.folds;
        let creases = (opts.geometric_folds && fold_amp > 0.0).then(|| {
            let wobble = NoiseSettings { amplitude: 1.0, frequency: body_tone.fold_noise.frequency * 0.5, ..body_tone.fold_noise };
            let joints = anatomy
                .skeleton
                .bones
                .iter()
                .enumerate()
                .filter(|(_, b)| b.parent.is_some() && anatomy.parts[b.part as usize].lod >= opts.level)
                .map(|(i, b)| {
                    let parent = &model[b.parent.expect("filtered").index()];
                    CreaseJoint {
                        position: model[i].translation,
                        axis: parent.matrix3.y_axis.normalize_or(Vec3A::Y),
                        radius: (b.length * 0.6).clamp(0.02, 0.15),
                    }
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

        // A limb part, or a part growing straight from one (hands, toes, hooves), belongs to that limb.
        let limb_of = |part: usize| -> Option<u16> {
            let p = &anatomy.parts[part];
            if p.role.is_some() {
                return Some(part as u16);
            }
            p.parent.filter(|q| anatomy.parts[*q as usize].role.is_some())
        };
        let mut prims = Vec::new();
        for (i, p) in anatomy.primitives.iter().enumerate() {
            if anatomy.parts[prim_part[i] as usize].lod < opts.level || p.lod < opts.level {
                continue;
            }
            let m = model[p.bone.index()] * p.local.compute_affine();
            let min_scale = (0..3).map(|c| m.matrix3.col(c).length()).fold(f32::INFINITY, f32::min);
            let aabb = transform_aabb(&m, local_bounds(&p.shape)).grow(Vec3A::splat(p.blend + fold_pad));
            let shape = match &p.shape {
                PrimShape::Sweep { points, radii, cross } => PreparedShape::Sweep {
                    points: points.iter().map(|&v| v.into()).collect(),
                    radii: radii.clone(),
                    cross: *cross,
                },
                s => PreparedShape::Simple(s.clone()),
            };
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
                limb: limb_of(prim_part[i] as usize),
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
    pub fn distance_among(&self, prims: std::ops::Range<usize>, p: Vec3A) -> f32 {
        self.eval(prims.start.min(self.prims.len())..prims.end.min(self.prims.len()), 0..0, p).0
    }

    /// [`Field::surface_info`] over a subset of primitives (see [`distance_among`](Self::distance_among)).
    pub fn surface_info_among(&self, prims: std::ops::Range<usize>, p: Vec3A) -> SurfaceInfo {
        self.info(prims.start.min(self.prims.len())..prims.end.min(self.prims.len()), p)
    }

    /// Restricts evaluation to primitives that can affect `region` (a meshing chunk).
    pub fn view(&self, region: Aabb3d) -> ChunkView<'_> {
        let prims = (0..self.prims.len() as u32)
            .filter(|&i| self.prims[i as usize].aabb.intersects(&region))
            .collect();
        let joints = self.creases.as_ref().map_or_else(Vec::new, |c| {
            (0..c.joints.len() as u32)
                .filter(|&i| {
                    let j = &c.joints[i as usize];
                    let closest = j.position.clamp(region.min, region.max);
                    closest.distance(j.position) < j.radius
                })
                .collect()
        });
        ChunkView { field: self, prims, joints, bounds: region }
    }

    pub fn primitive_count(&self) -> usize {
        self.prims.len()
    }

    /// Smooth union over `indices`, plus creases from `joints`; returns the distance and the
    /// nearest union primitive.
    #[inline]
    fn eval(
        &self,
        indices: impl Iterator<Item = usize>,
        joints: impl Iterator<Item = usize>,
        p: Vec3A,
    ) -> (f32, Option<usize>) {
        let mut d: Option<f32> = None;
        let mut nearest = None;
        let mut best = f32::INFINITY;
        for i in indices {
            let prim = &self.prims[i];
            let (dl, blend) = prim.eval(p);
            match prim.op {
                PrimOp::Union => {
                    // Two different limbs only meet, never blend.
                    let apart = nearest.is_some_and(|n: usize| {
                        let other = self.prims[n].limb;
                        prim.limb.is_some() && other.is_some() && prim.limb != other
                    });
                    d = Some(match d {
                        None => dl,
                        Some(d) if apart => d.min(dl),
                        Some(d) => sdf::smin(d, dl, blend).0,
                    });
                    if dl < best {
                        best = dl;
                        nearest = Some(i);
                    }
                }
                PrimOp::Subtract => {
                    if let Some(cur) = d {
                        d = Some(sdf::ssub(cur, dl, blend));
                    }
                }
            }
        }
        let mut d = d.unwrap_or(f32::MAX);
        if let Some(c) = &self.creases {
            if d.abs() < 0.03 {
                d += c.displacement(joints, p);
            }
        }
        (d, nearest)
    }

    fn info(&self, indices: impl Iterator<Item = usize> + Clone, p: Vec3A) -> SurfaceInfo {
        let (d, nearest) = self.eval(indices.clone(), std::iter::empty(), p);
        let near_bone = nearest.map(|n| self.prims[n].bone);
        let mut influences = Influences::default();
        for i in indices {
            let prim = &self.prims[i];
            // Weights only from bones in line with the nearest one (its parents and children):
            // never a neighbouring finger, never the other leg.
            if prim.op != PrimOp::Union || near_bone.is_some_and(|b| !self.in_line(b, prim.bone)) {
                continue;
            }
            let (dl, blend) = prim.eval(p);
            let gap = (dl - d).max(0.0);
            let reach = blend * 2.0 + 1e-4;
            let w = (1.0 - gap / reach).max(0.0);
            influences.add(prim.bone, w * w);
        }
        let near = nearest.map(|i| &self.prims[i]);
        influences.normalize(near.map_or(BoneId(0), |n| n.bone));
        SurfaceInfo {
            influences,
            region: near.map_or(RegionId(0), |n| n.region),
            layer: near.map_or(PrimLayer::Base, |n| n.layer),
        }
    }
}

/// Owning part index of every primitive (from the parts' primitive ranges).
pub(crate) fn primitive_parts(anatomy: &Anatomy) -> Vec<u16> {
    let mut out = vec![0u16; anatomy.primitives.len()];
    for (pi, part) in anatomy.parts.iter().enumerate() {
        for i in part.primitives.clone() {
            out[i as usize] = pi as u16;
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
    pub fn is_empty(&self) -> bool {
        self.prims.is_empty()
    }
}

impl Field for BodyField {
    fn bounds(&self) -> Aabb3d {
        self.bounds
    }

    fn distance(&self, p: Vec3A) -> f32 {
        let joints = self.creases.as_ref().map_or(0, |c| c.joints.len());
        self.eval(0..self.prims.len(), 0..joints, p).0
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.info(0..self.prims.len(), p)
    }
}

impl Field for ChunkView<'_> {
    fn bounds(&self) -> Aabb3d {
        self.bounds
    }

    fn distance(&self, p: Vec3A) -> f32 {
        self.field.eval(self.prims.iter().map(|&i| i as usize), self.joints.iter().map(|&i| i as usize), p).0
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.field.info(self.prims.iter().map(|&i| i as usize), p)
    }
}

impl BodyField {
    /// Whether `a` and `b` are the same bone or one is a near ancestor of the other.
    pub(crate) fn in_line(&self, a: BoneId, b: BoneId) -> bool {
        let up = |from: BoneId, to: BoneId| {
            let mut cur = Some(from);
            for _ in 0..=2 {
                match cur {
                    Some(c) if c == to => return true,
                    Some(c) => cur = self.bone_parent.get(c.index()).copied().flatten(),
                    None => return false,
                }
            }
            false
        };
        up(a, b) || up(b, a)
    }
}

impl BodyField {
    /// Skins a mesh built in the spread pose back into the rest pose, by its own weights (the
    /// same blend the GPU applies when animating). No-op for a field built at rest.
    pub fn unbind(&self, data: &mut super::BodyMeshData) {
        if self.unbind.is_empty() {
            return;
        }
        for i in 0..data.positions.len() {
            let (joints, weights) = (data.joints[i], data.weights[i]);
            let mut m = bevy::math::Mat3A::ZERO;
            let mut t = Vec3A::ZERO;
            for k in 0..4 {
                let Some(u) = self.unbind.get(joints[k] as usize) else { continue };
                m += u.matrix3 * weights[k];
                t += u.translation * weights[k];
            }
            let blended = Affine3A { matrix3: m, translation: t };
            data.positions[i] = blended.transform_point3(data.positions[i]);
            if let Some(n) = data.normals.get_mut(i) {
                *n = (m * Vec3A::from(*n)).normalize_or(Vec3A::from(*n)).into();
            }
        }
    }
}

#[cfg(test)]
mod spread_tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};

    /// A vertex fully weighted to the hand, placed where the hand is in the spread pose, lands
    /// where the hand is at rest.
    #[test]
    fn unbind_returns_vertices_to_rest() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let field = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: true });
        let arm = a.parts.iter().find(|p| p.role == Some(crate::anatomy::LimbRole::Manipulator)).unwrap();
        let hand = (arm.bones.end - 1) as usize;
        let rest = a.skeleton.model_rest()[hand].transform_point3(Vec3::Y * 0.02);
        let spread = field.unbind[hand].inverse().transform_point3(rest);
        assert!(spread.distance(rest) > 0.05, "the hand moved for meshing");
        let mut data = crate::surface::BodyMeshData { positions: vec![spread], normals: vec![Vec3::X], joints: vec![[hand as u16, 0, 0, 0]], weights: vec![[1.0, 0.0, 0.0, 0.0]], ..Default::default() };
        field.unbind(&mut data);
        assert!(data.positions[0].distance(rest) < 1e-4, "{} vs {}", data.positions[0], rest);
    }
}

impl BodyField {
    /// Distance at `p` over a chunk view's primitives, and the nearest primitive's bone and limb.
    pub(crate) fn probe(&self, view: &ChunkView, p: Vec3A) -> (f32, Option<(BoneId, Option<u16>)>) {
        let (d, nearest) = self.eval(view.prims.iter().map(|&i| i as usize), std::iter::empty(), p);
        (d, nearest.map(|i| (self.prims[i].bone, self.prims[i].limb)))
    }

    /// Model transform of every bone in the pose the field was built in.
    pub(crate) fn bone_model(&self) -> &[Affine3A] {
        &self.bone_model
    }
}


impl<'a> ChunkView<'a> {
    /// This view restricted to a sub-region.
    pub(crate) fn narrow(&self, region: Aabb3d) -> ChunkView<'a> {
        let field = self.field;
        let prims = self.prims.iter().copied().filter(|&i| field.prims[i as usize].aabb.intersects(&region)).collect();
        let joints = self.joints.clone();
        ChunkView { field, prims, joints, bounds: region }
    }
}
