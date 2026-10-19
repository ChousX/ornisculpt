use bevy::camera::Projection;
use bevy::prelude::*;

use super::{GeneratedBody, LodMesh, MobBody};
use crate::anatomy::LodLevel;

/// The detail tier a mob is shown at. Missing until the mob's first body is attached.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MobTier(pub LodLevel);

/// Pins a mob to a tier (story mobs, first-person targets).
#[derive(Component, Clone, Copy, Debug)]
pub struct ForceTier(pub LodLevel);

/// Cameras considered when choosing tiers (largest on-screen size wins).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct TierCamera;

/// Quality knobs for low-end to high-end hardware.
#[derive(Resource, Reflect, Clone, Debug)]
#[reflect(Resource)]
pub struct QualityProfile {
    /// On-screen height in pixels at or above which a mob is `Hero` / `Near`; below = `Far`.
    pub hero_px: f32,
    pub near_px: f32,
    /// Fractional band around thresholds that must be crossed to change tier.
    pub hysteresis: f32,
    /// Max mobs promoted to a finer tier per frame (spreads spawn/visibility work).
    pub promotions_per_frame: usize,
    /// Max mobs at `Hero` at once; extras stay `Near`.
    pub max_hero: usize,
}

impl Default for QualityProfile {
    fn default() -> Self {
        Self::MEDIUM
    }
}

impl QualityProfile {
    pub const LOW: Self = Self { hero_px: 500.0, near_px: 120.0, hysteresis: 0.15, promotions_per_frame: 4, max_hero: 8 };
    pub const MEDIUM: Self = Self { hero_px: 300.0, near_px: 80.0, hysteresis: 0.15, promotions_per_frame: 8, max_hero: 32 };
    pub const HIGH: Self = Self { hero_px: 200.0, near_px: 50.0, hysteresis: 0.15, promotions_per_frame: 16, max_hero: 64 };
}

/// Approximate on-screen height (pixels) of a sphere of `radius` at `world_pos`.
fn screen_px(cam: &GlobalTransform, proj: &Projection, viewport_h: f32, world_pos: Vec3, radius: f32) -> f32 {
    match proj {
        Projection::Perspective(p) => {
            let depth = (world_pos - cam.translation()).dot(cam.forward().as_vec3()).max(0.01);
            radius * 2.0 / (depth * (p.fov * 0.5).tan() * 2.0) * viewport_h
        }
        Projection::Orthographic(o) => radius * 2.0 / o.area.height().max(1e-3) * viewport_h,
        Projection::Custom(_) => viewport_h,
    }
}

#[allow(clippy::type_complexity)]
pub(super) fn choose_tiers(
    mut commands: Commands,
    profile: Res<QualityProfile>,
    bodies: Res<Assets<GeneratedBody>>,
    cameras: Query<(&GlobalTransform, &Projection, &Camera), With<TierCamera>>,
    mut mobs: Query<(Entity, &MobBody, &GlobalTransform, Option<&mut MobTier>, Option<&ForceTier>)>,
) {
    let cams: Vec<_> = cameras
        .iter()
        .filter_map(|(t, p, c)| c.logical_viewport_size().map(|s| (*t, p.clone(), s.y)))
        .collect();
    let mut wanted: Vec<(Entity, LodLevel, f32)> = Vec::new();
    for (entity, body, transform, tier, force) in &mobs {
        let Some(body) = bodies.get(&body.0) else { continue };
        let radius = body.radius * transform.scale().max_element();
        let px = cams
            .iter()
            .map(|(t, p, h)| screen_px(t, p, *h, transform.translation(), radius))
            .fold(0.0, f32::max);
        let current = tier.map(|t| t.0);
        let level = force.map_or_else(
            || {
            // Hysteresis: thresholds shift against the current tier.
            let h = profile.hysteresis;
            let (hero, near) = match current {
                Some(LodLevel::Hero) => (profile.hero_px * (1.0 - h), profile.near_px * (1.0 - h)),
                Some(LodLevel::Near) => (profile.hero_px * (1.0 + h), profile.near_px * (1.0 - h)),
                _ => (profile.hero_px * (1.0 + h), profile.near_px * (1.0 + h)),
            };
            if cams.is_empty() || px >= hero {
                LodLevel::Hero
            } else if px >= near {
                LodLevel::Near
            } else {
                LodLevel::Far
            }
            },
            |f| f.0,
        );
        wanted.push((entity, level, px));
    }

    // Biggest on screen first, so caps and promotion budgets favour what the player sees.
    wanted.sort_by(|a, b| b.2.total_cmp(&a.2));
    let mut heroes = 0;
    let mut promotions = 0;
    for (entity, mut level, _) in wanted {
        let Ok((_, _, _, tier, force)) = mobs.get_mut(entity) else { continue };
        if level == LodLevel::Hero && force.is_none() {
            if heroes >= profile.max_hero {
                level = LodLevel::Near;
            } else {
                heroes += 1;
            }
        }
        match tier {
            None => {
                commands.entity(entity).insert(MobTier(level));
            }
            Some(mut t) if t.0 != level => {
                let promoting = level < t.0;
                if promoting {
                    if promotions >= profile.promotions_per_frame {
                        continue;
                    }
                    promotions += 1;
                }
                t.0 = level;
            }
            Some(_) => {}
        }
    }
}

/// Shows the mesh entities of each mob's current tier.
pub(super) fn apply_tier_visibility(
    mobs: Query<(&MobTier, &Children), Changed<MobTier>>,
    mut meshes: Query<(&LodMesh, &mut Visibility)>,
) {
    for (tier, children) in &mobs {
        for child in children {
            if let Ok((lod, mut vis)) = meshes.get_mut(*child) {
                *vis = if lod.0 == tier.0 { Visibility::Inherited } else { Visibility::Hidden };
            }
        }
    }
}
