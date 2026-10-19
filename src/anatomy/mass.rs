//! Mass from the surface primitives: each primitive's volume times its part's density, owned by
//! its bone. Overlapping primitives (smooth unions) count twice, so totals run a little heavy;
//! hard parts (horns, claws, eyes) and wing sheets are left out.

use std::f32::consts::PI;

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::{Anatomy, CrossSection, PrimLayer, PrimOp, PrimShape};

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
pub fn measure(a: &Anatomy) -> BodyMass {
    let model = a.skeleton.model_rest();
    let mut density = vec![1.0; a.primitives.len()];
    for p in &a.parts {
        for i in p.primitives.clone() {
            if let Some(d) = density.get_mut(i as usize) {
                *d = p.density;
            }
        }
    }
    let mut bones = vec![0.0; a.skeleton.len()];
    let mut moment = Vec3::ZERO;
    for (p, d) in a.primitives.iter().zip(density) {
        let m = model[p.bone.index()] * p.local.compute_affine();
        let (volume, centroid) = volume(&p.shape, &m);
        let layer = if p.layer == PrimLayer::Fat { 0.9 } else { 1.0 };
        // A stamp weighs as its whole footprint ellipsoid: the muscle or fat it stands for lies
        // mostly under the surface it raises.
        let sign = match p.op {
            PrimOp::Union => 1.0,
            PrimOp::Subtract => -1.0,
            PrimOp::Displace { height } => height.signum(),
        };
        let kg = sign * volume * d * layer * WATER;
        bones[p.bone.index()] += kg;
        moment += centroid * kg;
    }
    for b in &mut bones {
        *b = b.max(0.0);
    }
    let total: f32 = bones.iter().sum();
    let centre = if total > 0.0 { moment / total } else { Vec3::ZERO };
    BodyMass { total, centre, bones }
}

/// Volume (m³) and centroid (model space) of a primitive placed by `m`.
fn volume(shape: &PrimShape, m: &Affine3A) -> (f32, Vec3) {
    let scale = m.matrix3.determinant().abs();
    let at = |p: Vec3| m.transform_point3(p);
    // Tapered rod from `r0` at y = 0 to `r1` at y = `len`, with round ends.
    let rod = |r0: f32, r1: f32, len: f32| {
        let body = PI / 3.0 * len * (r0 * r0 + r0 * r1 + r1 * r1);
        let ends = 2.0 / 3.0 * PI * (r0.powi(3) + r1.powi(3));
        let y = if r0 + r1 > 0.0 { len * (r0 * r0 + 2.0 * r0 * r1 + 3.0 * r1 * r1) / (4.0 * (r0 * r0 + r0 * r1 + r1 * r1)) } else { 0.0 };
        ((body + ends) * scale, at(Vec3::Y * y))
    };
    match shape {
        PrimShape::Sphere { r } => (4.0 / 3.0 * PI * r.powi(3) * scale, at(Vec3::ZERO)),
        PrimShape::Ellipsoid { radii } => (4.0 / 3.0 * PI * radii.x * radii.y * radii.z * scale, at(Vec3::ZERO)),
        PrimShape::Capsule { r0, r1, len } | PrimShape::RoundCone { r0, r1, len } => rod(*r0, *r1, *len),
        PrimShape::RoundBox { half, round } => {
            let h = *half + Vec3::splat(*round);
            (8.0 * h.x * h.y * h.z * scale, at(Vec3::ZERO))
        }
        PrimShape::Sweep { points, radii, cross } => {
            let squash = match cross {
                CrossSection::Round => 1.0,
                CrossSection::Ellipse { aspect } | CrossSection::Flat { aspect } | CrossSection::Teardrop { aspect } => *aspect,
            };
            let (mut v, mut c) = (0.0, Vec3::ZERO);
            for i in 1..points.len() {
                let (r0, r1) = (radii.get(i - 1).copied().unwrap_or(0.0), radii.get(i).copied().unwrap_or(0.0));
                let len = points[i].distance(points[i - 1]);
                let seg = PI / 3.0 * len * (r0 * r0 + r0 * r1 + r1 * r1) * squash * scale;
                v += seg;
                c += at((points[i] + points[i - 1]) * 0.5) * seg;
            }
            (v, if v > 0.0 { c / v } else { at(points.first().copied().unwrap_or_default()) })
        }
        PrimShape::Grid { grid, mirror_x } => {
            let cell = grid.cell.x * grid.cell.y * grid.cell.z;
            let (mut v, mut c) = (0.0, Vec3::ZERO);
            let d = grid.dims;
            for (i, value) in grid.values.iter().enumerate() {
                if *value < 0.0 {
                    let i = i as u32;
                    let ix = UVec3::new(i % d.x, (i / d.x) % d.y, i / (d.x * d.y));
                    let mut p = grid.origin + grid.cell * ix.as_vec3();
                    if *mirror_x {
                        p.x = -p.x;
                    }
                    v += cell;
                    c += p;
                }
            }
            let n = (v / cell.max(1e-12)).max(1.0);
            (v * scale, at(c / n))
        }
    }
}
