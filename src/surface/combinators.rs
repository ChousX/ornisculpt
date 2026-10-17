//! Field combinators. Garments (later) are built from these over the body field:
//! a shirt is `RegionClip(Offset(body, thickness), torso_mask)`.

use bevy::math::Vec3A;
use bevy::shape::{Aabb3d, BoundingVolume};

use super::{Field, SurfaceInfo};
use crate::anatomy::RegionMask;
use crate::random::Noise;

/// Inflates (`amount > 0`) or deflates the surface.
pub struct Offset<F> {
    pub inner: F,
    pub amount: f32,
}

impl<F: Field> Field for Offset<F> {
    fn bounds(&self) -> Aabb3d {
        self.inner.bounds().grow(bevy::math::Vec3A::splat(self.amount.max(0.0)))
    }

    fn distance(&self, p: Vec3A) -> f32 {
        self.inner.distance(p) - self.amount
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        // Project back onto the inner surface so weights/regions match the body underneath.
        let n = self.inner.gradient(p).normalize_or_zero();
        self.inner.surface_info(p - n * self.amount)
    }
}

pub struct Intersect<A, B> {
    pub a: A,
    pub b: B,
}

impl<A: Field, B: Field> Field for Intersect<A, B> {
    fn bounds(&self) -> Aabb3d {
        todo!("intersection of the two bounds")
    }

    fn distance(&self, p: Vec3A) -> f32 {
        self.a.distance(p).max(self.b.distance(p))
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.a.surface_info(p)
    }
}

pub struct Subtract<A, B> {
    pub a: A,
    pub b: B,
}

impl<A: Field, B: Field> Field for Subtract<A, B> {
    fn bounds(&self) -> Aabb3d {
        self.a.bounds()
    }

    fn distance(&self, p: Vec3A) -> f32 {
        self.a.distance(p).max(-self.b.distance(p))
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.a.surface_info(p)
    }
}

/// Keeps the surface only where the inner field's region is in `mask`; elsewhere the field is
/// pushed outward so no surface forms. Open edges are closed by the mesher's boundary.
pub struct RegionClip<F> {
    pub inner: F,
    pub mask: RegionMask,
}

impl<F: Field> Field for RegionClip<F> {
    fn bounds(&self) -> Aabb3d {
        self.inner.bounds()
    }

    fn distance(&self, p: Vec3A) -> f32 {
        todo!("needs region per sample: cheap region lookup (dominant prim) rather than full surface_info")
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.inner.surface_info(p)
    }
}

/// Adds noise displacement (folds, bumps, garment wrinkles).
pub struct Displace<F> {
    pub inner: F,
    pub noise: Noise,
}

impl<F: Field> Field for Displace<F> {
    fn bounds(&self) -> Aabb3d {
        todo!("inner bounds grown by noise amplitude")
    }

    fn distance(&self, p: Vec3A) -> f32 {
        self.inner.distance(p) + self.noise.sample(p.into())
    }

    fn surface_info(&self, p: Vec3A) -> SurfaceInfo {
        self.inner.surface_info(p)
    }
}
