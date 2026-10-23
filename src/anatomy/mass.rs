//! Mass from the surface primitives: each primitive's volume times its part's density, owned by
//! its bone. Overlapping primitives (smooth unions) count twice, so totals run a little heavy;
//! hard parts (horns, claws, eyes) and wing sheets are left out.

use std::f32::consts::PI;

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::{Anatomy, PrimLayer, PrimOp, PrimShape, Weigh};
use crate::num::Idx;

/// Water, kg/m³: a body of density `1.0` weighs what its volume of water would.
pub const WATER: f32 = 1000.0;

/// Where a body's weight is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BodyMass {
    /// Kilograms.
    pub total: f32,
    /// Centre of mass, model space (rest pose).
    pub centre: Vec3,
    /// Kilograms per bone, in skeleton order.
    pub bones: Vec<f32>,
}

impl BodyMass {
    /// Mass of a set of bones.
    pub fn of(&self, bones: impl IntoIterator<Item = super::BoneId>) -> f32 {
        bones.into_iter().map(|b| self.bones.get(b.index()).copied().unwrap_or(0.0)).sum()
    }
}

/// Weighs a built body. `Anatomy::parts[i].density` scales each part's primitives.
#[must_use]
pub fn measure(a: &Anatomy) -> BodyMass {
    let model = a.skeleton.model_rest();
    let mut density = vec![1.0; a.primitives.len()];
    for p in &a.parts {
        for i in p.primitives.clone() {
            if let Some(d) = density.get_mut(i.idx()) {
                *d = p.density;
            }
        }
    }
    let mut bones = vec![0.0; a.skeleton.len()];
    let mut moment = Vec3::ZERO;
    for (p, d) in a.primitives.iter().zip(density) {
        let Some(bone) = model.get(p.bone.index()) else { continue };
        let m = *bone * p.local.compute_affine();
        let (volume, centroid) = weighed(volume(&p.shape, &m), &p.shape, p.weigh, &m);
        let layer = if p.layer == PrimLayer::Fat { 0.9 } else { 1.0 };
        // A stamp weighs as its whole footprint ellipsoid: the muscle or fat it stands for lies
        // mostly under the surface it raises.
        let sign = match p.op {
            PrimOp::Union => 1.0,
            PrimOp::Subtract => -1.0,
            PrimOp::Displace { height } => height.signum(),
        };
        let kg = sign * volume * d * layer * WATER;
        if let Some(b) = bones.get_mut(p.bone.index()) {
            *b += kg;
        }
        moment += centroid * kg;
    }
    for b in &mut bones {
        *b = b.max(0.0);
    }
    let total: f32 = bones.iter().sum();
    let centre = if total > 0.0 { moment / total } else { Vec3::ZERO };
    BodyMass { total, centre, bones }
}

/// Applies a primitive's overlap correction to its `(volume, centroid)`.
fn weighed((volume, centroid): (f32, Vec3), shape: &PrimShape, weigh: Weigh, place: &Affine3A) -> (f32, Vec3) {
    match (weigh, shape) {
        (Weigh::Fraction(f), _) => (volume * f.clamp(0.0, 1.0), centroid),
        // The joint sphere (both caps) sits at the cone's start.
        (Weigh::Continues, PrimShape::RoundCone { r0, .. }) => {
            let joint = (4.0 / 3.0 * PI * r0.powi(3) * place.matrix3.determinant().abs()).min(volume * 0.9);
            let rest = volume - joint;
            let at = place.transform_point3(Vec3::ZERO);
            (rest, (centroid * volume - at * joint) / rest)
        }
        (Weigh::Whole | Weigh::Continues, _) => (volume, centroid),
    }
}

/// Volume (m³) and centroid (model space) of a primitive placed by `m`.
fn volume(shape: &PrimShape, place: &Affine3A) -> (f32, Vec3) {
    let scale = place.matrix3.determinant().abs();
    let at = |p: Vec3| place.transform_point3(p);
    // Tapered rod from `r0` at y = 0 to `r1` at y = `len`, with round ends.
    let rod = |r0: f32, r1: f32, len: f32| {
        let body = PI / 3.0 * len * r1.mul_add(r1, r0.mul_add(r1, r0 * r0));
        let ends = 2.0 / 3.0 * PI * (r0.powi(3) + r1.powi(3));
        let y = if r0 + r1 > 0.0 { len * (3.0 * r1).mul_add(r1, (2.0 * r0).mul_add(r1, r0 * r0)) / (4.0 * r1.mul_add(r1, r0.mul_add(r1, r0 * r0))) } else { 0.0 };
        ((body + ends) * scale, at(Vec3::Y * y))
    };
    match shape {
        PrimShape::Sphere { r } => (4.0 / 3.0 * PI * r.powi(3) * scale, at(Vec3::ZERO)),
        PrimShape::Ellipsoid { radii } => (4.0 / 3.0 * PI * radii.x * radii.y * radii.z * scale, at(Vec3::ZERO)),
        PrimShape::RoundCone { r0, r1, len } => rod(*r0, *r1, *len),
        PrimShape::RoundBox { half, round } => {
            let extent = *half + Vec3::splat(*round);
            (8.0 * extent.x * extent.y * extent.z * scale, at(Vec3::ZERO))
        }
        PrimShape::Grid { grid, mirror_x } => {
            let cell = grid.cell.x * grid.cell.y * grid.cell.z;
            let (mut total, mut moment) = (0.0, Vec3::ZERO);
            let dims = grid.dims;
            for (i, value) in (0u32..).zip(&grid.values) {
                if *value < 0.0 {
                    let ix = UVec3::new(i % dims.x, (i / dims.x) % dims.y, i / (dims.x * dims.y));
                    let mut p = grid.origin + grid.cell * ix.as_vec3();
                    if *mirror_x {
                        p.x = -p.x;
                    }
                    total += cell;
                    moment += p;
                }
            }
            let inside = (total / cell.max(1e-12)).max(1.0);
            (total * scale, at(moment / inside))
        }
    }
}
