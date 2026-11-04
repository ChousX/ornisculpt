use bevy::camera::Projection;
use bevy::prelude::*;

use bevy::light::NotShadowCaster;

use super::{CrowdCastable, GeneratedBody, LodMesh, MobBody};
use crate::anatomy::LodLevel;


/// The detail tier a mob is shown at. Missing until the mob's first body is attached.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MobTier(pub LodLevel);

/// Pins a mob to a tier (story mobs, first-person targets).
#[derive(Component, Clone, Copy, Debug)]
pub struct ForceTier(pub LodLevel);

/// A crowd-tier mob allowed to cast a shadow ([`QualityProfile::max_casters`]). Hero mobs cast
/// through their proxy instead (`spawn::SHADOW_PROXY_LAYER`) and never take this.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CrowdCaster;

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
    /// Max crowd-tier mobs casting a shadow at once, biggest on screen first. A hero casts
    /// through its proxy whatever this says.
    ///
    /// **Off on every profile but `HIGH`**, because what a caster costs depends on how big it
    /// is on screen, not on its triangles: the same 64 casters cost 0.1 ms in a wide crowd shot
    /// and 1.3 ms with the camera down among them (toth, 4000 mobs, `bench/LOG.md`). Pick a
    /// number against your own framing. A mob pops its shadow as it crosses the budget; the
    /// order is by on-screen size and stable between frames, so that happens when two mobs
    /// really cross, not every frame at the boundary.
    pub max_casters: usize,
}

impl Default for QualityProfile {
    fn default() -> Self {
        Self::MEDIUM
    }
}

impl QualityProfile {
    pub const LOW: Self =
        Self { hero_px: 500.0, near_px: 120.0, hysteresis: 0.15, promotions_per_frame: 4, max_hero: 8, max_casters: 0 };
    pub const MEDIUM: Self =
        Self { hero_px: 300.0, near_px: 80.0, hysteresis: 0.15, promotions_per_frame: 8, max_hero: 32, max_casters: 0 };
    pub const HIGH: Self =
        Self { hero_px: 200.0, near_px: 50.0, hysteresis: 0.15, promotions_per_frame: 16, max_hero: 64, max_casters: 64 };
}

/// Approximate on-screen height (pixels) of a sphere of `radius` at `world_pos`.
fn screen_px(cam: &GlobalTransform, proj: &Projection, viewport_h: f32, world_pos: Vec3, radius: f32) -> f32 {
    match proj {
        Projection::Perspective(p) => {
            let depth = (world_pos - cam.translation()).dot(cam.forward().as_vec3());
            // Nothing behind the camera is on screen. Without this a mob behind it takes the
            // clamped near depth and reads as the biggest thing in the scene, so a camera
            // standing in a crowd spends its hero and caster budgets on mobs at its back.
            if depth <= -radius {
                return 0.0;
            }
            radius * 2.0 / (depth.max(radius) * (p.fov * 0.5).tan() * 2.0) * viewport_h
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
    mut mobs: Query<(Entity, &MobBody, &GlobalTransform, Option<&mut MobTier>, Option<&ForceTier>, Has<CrowdCaster>)>,
) {
    let cams: Vec<_> = cameras
        .iter()
        .filter_map(|(t, p, c)| c.logical_viewport_size().map(|s| (*t, p.clone(), s.y)))
        .collect();
    let mut wanted: Vec<(Entity, LodLevel, f32)> = Vec::new();
    for (entity, body, transform, tier, force, _) in &mobs {
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
    // Crowd mobs allowed to cast so far, biggest on screen first.
    let mut casters = 0;
    for (entity, mut level, _) in wanted {
        let Ok((_, _, _, tier, force, casting)) = mobs.get_mut(entity) else { continue };
        if level == LodLevel::Hero && force.is_none() {
            if heroes >= profile.max_hero {
                level = LodLevel::Near;
            } else {
                heroes += 1;
            }
        }
        // A hero casts through its proxy, so only crowd tiers take from the budget.
        let cast = level != LodLevel::Hero && casters < profile.max_casters;
        if cast {
            casters += 1;
        }
        if cast != casting {
            let mut mob = commands.entity(entity);
            if cast {
                mob.insert(CrowdCaster);
            } else {
                mob.remove::<CrowdCaster>();
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

/// Lets a mob's crowd meshes cast when it becomes a [`CrowdCaster`], and stops them when it
/// stops being one. Only the body meshes are eligible ([`CrowdCastable`]): a coat's fur shells
/// and feather cards are alpha-tested, and a shadow pass over them costs far more than the
/// silhouette is worth.
///
/// Observers, not a sweep over every crowd mesh each frame: only a few mobs cross the budget in
/// a frame. A body that regenerates under a casting mob is handled where it is spawned
/// (`spawn::attach_body`). The writes are `try_`: a mob that is despawned loses `CrowdCaster`
/// with its children already going.
pub(super) fn cast_shadows(ev: On<Add<CrowdCaster>>, mut commands: Commands, children: Query<&Children>, meshes: Query<(), With<CrowdCastable>>) {
    for child in children.get(ev.entity).into_iter().flatten() {
        if meshes.contains(*child) {
            commands.entity(*child).try_remove::<NotShadowCaster>();
        }
    }
}

pub(super) fn stop_casting(
    ev: On<Remove<CrowdCaster>>,
    mut commands: Commands,
    children: Query<&Children>,
    meshes: Query<(), With<CrowdCastable>>,
) {
    for child in children.get(ev.entity).into_iter().flatten() {
        if meshes.contains(*child) {
            commands.entity(*child).try_insert(NotShadowCaster);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera standing in a crowd used to spend its hero budget on the mobs at its back: a
    /// mob behind it took the clamped near depth and read as the biggest thing in the scene.
    #[test]
    fn nothing_behind_the_camera_is_on_screen() {
        let cam = GlobalTransform::from(Transform::from_xyz(0.0, 0.0, 0.0).looking_to(Vec3::Z, Vec3::Y));
        let proj = Projection::Perspective(PerspectiveProjection::default());
        let px = |z: f32| screen_px(&cam, &proj, 1080.0, Vec3::new(0.0, 0.0, z), 1.0);
        assert_eq!(px(-5.0), 0.0, "a mob well behind the camera");
        assert_eq!(px(-1.0), 0.0, "a mob just behind it");
        assert!(px(1.0) > 0.0 && px(20.0) > 0.0, "mobs in front are on screen");
        assert!(px(5.0) > px(20.0), "nearer is bigger");
        // One straddling the camera plane is huge but bounded, not divided by nearly zero.
        assert!(px(0.0) > px(5.0) && px(0.0).is_finite());
        assert!(px(0.0) < 1e6, "a mob at the camera must not read as infinitely large: {}", px(0.0));
    }

    /// Each profile's shadow budget is what it says, and the costed ones are in order.
    #[test]
    fn the_profiles_budget_their_casters() {
        assert_eq!(QualityProfile::LOW.max_casters, 0, "the low profile casts no crowd shadows");
        assert_eq!(QualityProfile::MEDIUM.max_casters, 0, "nor the default: the cost depends on the framing");
        // A const block, because the condition is a constant: this is a claim about the
        // profile table itself, so it should fail the build rather than a test run.
        const { assert!(QualityProfile::HIGH.max_casters > 0, "the high profile casts crowd shadows") };
        assert_eq!(QualityProfile::default().max_casters, QualityProfile::MEDIUM.max_casters);
    }
}
