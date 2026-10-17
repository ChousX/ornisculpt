use bevy::math::Vec3A;
use bevy::shape::Aabb3d;

use crate::anatomy::{BoneId, PrimLayer, RegionId};

/// A signed distance field (negative inside).
///
/// Meshing calls [`distance`](Self::distance) millions of times; [`surface_info`](Self::surface_info)
/// is called once per *output vertex*, so it may be much more expensive.
pub trait Field: Send + Sync {
    fn bounds(&self) -> Aabb3d;

    fn distance(&self, p: Vec3A) -> f32;

    /// Batch evaluation; override for SIMD / cache-friendly traversal.
    fn distance_batch(&self, ps: &[Vec3A], out: &mut [f32]) {
        for (p, o) in ps.iter().zip(out) {
            *o = self.distance(*p);
        }
    }

    /// Outward normal direction (not normalized).
    fn gradient(&self, p: Vec3A) -> Vec3A {
        const H: f32 = 1e-3;
        let dx = Vec3A::new(H, 0.0, 0.0);
        let dy = Vec3A::new(0.0, H, 0.0);
        let dz = Vec3A::new(0.0, 0.0, H);
        Vec3A::new(
            self.distance(p + dx) - self.distance(p - dx),
            self.distance(p + dy) - self.distance(p - dy),
            self.distance(p + dz) - self.distance(p - dz),
        )
    }

    /// Skin weights, region and tone layer at a surface point.
    fn surface_info(&self, p: Vec3A) -> SurfaceInfo;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceInfo {
    pub influences: Influences,
    pub region: RegionId,
    pub layer: PrimLayer,
}

/// Up to four bone influences, kept sorted by weight (heaviest first).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Influences {
    pub bones: [BoneId; 4],
    pub weights: [f32; 4],
}

impl Default for Influences {
    fn default() -> Self {
        Self { bones: [BoneId(0); 4], weights: [0.0; 4] }
    }
}

impl Influences {
    /// Adds weight to `bone`, merging with an existing entry or replacing the lightest one.
    pub fn add(&mut self, bone: BoneId, weight: f32) {
        if weight <= 0.0 {
            return;
        }
        if let Some(i) = (0..4).find(|&i| self.weights[i] > 0.0 && self.bones[i] == bone) {
            self.weights[i] += weight;
        } else if weight > self.weights[3] {
            self.bones[3] = bone;
            self.weights[3] = weight;
        } else {
            return;
        }
        // Restore descending order (at most 4 elements).
        for i in (1..4).rev() {
            if self.weights[i] > self.weights[i - 1] {
                self.weights.swap(i, i - 1);
                self.bones.swap(i, i - 1);
            }
        }
    }

    /// Scales weights to sum to 1. A vertex with no influence keeps weight 1 on `fallback`.
    pub fn normalize(&mut self, fallback: BoneId) {
        let sum: f32 = self.weights.iter().sum();
        if sum <= 0.0 {
            *self = Self { bones: [fallback; 4], weights: [1.0, 0.0, 0.0, 0.0] };
        } else {
            self.weights.iter_mut().for_each(|w| *w /= sum);
        }
    }

    pub fn primary(&self) -> BoneId {
        self.bones[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn influences_keep_top_four_sorted() {
        let mut inf = Influences::default();
        for (b, w) in [(1, 0.1), (2, 0.5), (3, 0.2), (4, 0.05), (5, 0.3), (2, 0.1)] {
            inf.add(BoneId(b), w);
        }
        assert_eq!(inf.bones, [BoneId(2), BoneId(5), BoneId(3), BoneId(1)]);
        inf.normalize(BoneId(0));
        assert!((inf.weights.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(inf.weights.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn empty_influences_fall_back() {
        let mut inf = Influences::default();
        inf.normalize(BoneId(7));
        assert_eq!(inf.primary(), BoneId(7));
        assert_eq!(inf.weights[0], 1.0);
    }
}
