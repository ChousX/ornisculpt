use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Detail level, finest first. Mirrors the mob tiers (excluding `Pending`).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LodLevel {
    #[default]
    Hero = 0,
    Near = 1,
    Far = 2,
}

impl LodLevel {
    pub const ALL: [Self; 3] = [Self::Hero, Self::Near, Self::Far];

    /// This level's entry in a per-level array (in [`ALL`](Self::ALL) order).
    pub const fn pick<T>(self, per_level: &[T; 3]) -> &T {
        let [hero, near, far] = per_level;
        match self {
            Self::Hero => hero,
            Self::Near => near,
            Self::Far => far,
        }
    }
}

/// Coarsest level at which a part still exists (in mesh, skeleton and ECS).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PartDetail {
    /// Decided from size relative to the whole mob using [`PartLodRules`].
    #[default]
    Auto,
    /// Never culled (torso, main limbs, head).
    Always,
    /// Present at this level and every finer one.
    UpTo(LodLevel),
}

/// Thresholds for [`PartDetail::Auto`]: a part survives a level if
/// `part bounding radius / mob bounding radius >= ratio` for that level.
#[derive(Resource, Reflect, Clone, Copy, Debug)]
#[reflect(Resource)]
pub struct PartLodRules {
    pub near_min_ratio: f32,
    pub far_min_ratio: f32,
}

impl Default for PartLodRules {
    fn default() -> Self {
        Self { near_min_ratio: 0.08, far_min_ratio: 0.2 }
    }
}

impl PartLodRules {
    /// Coarsest level a part of the given relative size survives to.
    #[must_use]
    pub fn resolve(&self, detail: PartDetail, size_ratio: f32) -> LodLevel {
        match detail {
            PartDetail::Always => LodLevel::Far,
            PartDetail::UpTo(level) => level,
            PartDetail::Auto if size_ratio >= self.far_min_ratio => LodLevel::Far,
            PartDetail::Auto if size_ratio >= self.near_min_ratio => LodLevel::Near,
            PartDetail::Auto => LodLevel::Hero,
        }
    }
}
