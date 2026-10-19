//! Primitive signed distance functions and blending, in primitive-local space.
//! Formulas follow Inigo Quilez's reference SDFs.

use bevy::math::{Vec2, Vec3A};

use crate::anatomy::{CrossSection, PrimShape};

/// Polynomial smooth minimum. Returns `(distance, h)` where `h ∈ 0..=1` is how much `b`
/// contributes (0 = all `a`, 1 = all `b`). `h` drives skin-weight blending.
#[must_use]
pub fn smin(a: f32, b: f32, k: f32) -> (f32, f32) {
    if k <= 0.0 {
        return if a < b { (a, 0.0) } else { (b, 1.0) };
    }
    let h = (0.5 + 0.5 * (a - b) / k).clamp(0.0, 1.0);
    ((k * h).mul_add(-(1.0 - h), (a - b).mul_add(1.0 - h, b)), h)
}

/// Smooth subtraction of `b` from `a`.
#[must_use]
pub fn ssub(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, b, k).0
}

#[must_use]
pub fn sphere(p: Vec3A, r: f32) -> f32 {
    p.length() - r
}

/// Bound (not exact) ellipsoid distance; good enough near the surface.
#[must_use]
pub fn ellipsoid(p: Vec3A, radii: Vec3A) -> f32 {
    let k0 = (p / radii).length();
    let k1 = (p / (radii * radii)).length();
    if k1 == 0.0 { -radii.min_element() } else { k0 * (k0 - 1.0) / k1 }
}

/// Tapered capsule along +Y: radius `r0` at the origin, `r1` at `len`.
#[must_use]
pub fn capsule(p: Vec3A, r0: f32, r1: f32, len: f32) -> f32 {
    round_cone(p, r0, r1, len)
}

/// Round cone along +Y (exact).
#[must_use]
pub fn round_cone(p: Vec3A, r0: f32, r1: f32, len: f32) -> f32 {
    let q = Vec2::new(Vec2::new(p.x, p.z).length(), p.y);
    let sin = (r0 - r1) / len;
    let cos = sin.mul_add(-sin, 1.0).max(0.0).sqrt();
    let along = q.dot(Vec2::new(-sin, cos));
    if along < 0.0 {
        return q.length() - r0;
    }
    if along > cos * len {
        return (q - Vec2::new(0.0, len)).length() - r1;
    }
    q.dot(Vec2::new(cos, sin)) - r0
}

#[must_use]
pub fn round_box(p: Vec3A, half: Vec3A, round: f32) -> f32 {
    let q = p.abs() - half + Vec3A::splat(round);
    q.max(Vec3A::ZERO).length() + q.max_element().min(0.0) - round
}

/// Distance to a swept cross-section along a polyline.
///
/// A tube whose radius runs linearly between the points' radii. Non-round sections squash local
/// x by their aspect (the teardrop's keel isn't modelled); the result stays a lower bound but
/// is no longer exact.
#[must_use]
pub fn sweep(p: Vec3A, points: &[Vec3A], radii: &[f32], cross: CrossSection) -> f32 {
    let aspect = match cross {
        CrossSection::Round => 1.0,
        CrossSection::Ellipse { aspect } | CrossSection::Flat { aspect } | CrossSection::Teardrop { aspect } => aspect.max(1e-3),
    };
    let squash = |v: Vec3A| Vec3A::new(v.x / aspect, v.y, v.z);
    let radius = |i: usize| radii.get(i).copied().unwrap_or(0.0);
    let at = squash(p);
    let mut best = match points {
        [only] => (at - squash(*only)).length() - radius(0),
        _ => f32::INFINITY,
    };
    for (i, [start, end]) in points.array_windows().enumerate() {
        let (start, end) = (squash(*start), squash(*end));
        let seg = end - start;
        let along = ((at - start).dot(seg) / seg.length_squared().max(1e-12)).clamp(0.0, 1.0);
        let r = (radius(i + 1) - radius(i)).mul_add(along, radius(i));
        best = best.min((at - (start + seg * along)).length() - r);
    }
    best * aspect.min(1.0)
}

/// Dispatches on [`PrimShape`] (point already in primitive-local space).
#[must_use]
pub fn shape(p: Vec3A, s: &PrimShape) -> f32 {
    match s {
        PrimShape::Sphere { r } => sphere(p, *r),
        PrimShape::Ellipsoid { radii } => ellipsoid(p, (*radii).into()),
        PrimShape::Capsule { r0, r1, len } => capsule(p, *r0, *r1, *len),
        PrimShape::RoundCone { r0, r1, len } => round_cone(p, *r0, *r1, *len),
        PrimShape::RoundBox { half, round } => round_box(p, (*half).into(), *round),
        PrimShape::Grid { grid, mirror_x } => {
            let q = if *mirror_x { Vec3A::new(-p.x, p.y, p.z) } else { p };
            grid.sample(q.into())
        }
        PrimShape::Sweep { points, radii, cross } => {
            // `BodyField` converts the points once; this path converts them per call.
            let points: Vec<Vec3A> = points.iter().map(|&v| v.into()).collect();
            sweep(p, &points, radii, *cross)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_have_correct_surface() {
        assert!(sphere(Vec3A::new(1.0, 0.0, 0.0), 1.0).abs() < 1e-6);
        assert!(ellipsoid(Vec3A::new(2.0, 0.0, 0.0), Vec3A::new(2.0, 1.0, 1.0)).abs() < 1e-5);
        assert!(round_cone(Vec3A::new(0.5, 0.0, 0.0), 0.5, 0.25, 1.0).abs() < 1e-5);
        assert!(round_cone(Vec3A::new(0.0, 1.25, 0.0), 0.5, 0.25, 1.0).abs() < 1e-5);
        assert!(round_box(Vec3A::new(1.0, 0.0, 0.0), Vec3A::splat(1.0), 0.1).abs() < 1e-5);
    }

    #[test]
    fn smin_blends() {
        let (d, h) = smin(1.0, -1.0, 0.1);
        assert_eq!((d, h), (-1.0, 1.0));
        let (d, h) = smin(0.0, 0.0, 0.2);
        assert!(d < 0.0 && (h - 0.5).abs() < 1e-6);
    }
}
