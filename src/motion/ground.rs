//! Crowd tiers on uneven ground. Baked clips assume flat ground; a crowd mob whose game writes
//! [`FootGround`] hits gets a [`CrowdGround`]: its root raised and tilted onto the plane its
//! feet's hits make, and each foot moved up or down the rest, which the blend applies down the
//! leg (`crowd_blend.wesl`). Cheap next to the hero tier's leg IK, and enough for feet to sit on
//! slopes and steps from a distance. The game keeps the mob's `Transform` where its terrain says.

use bevy::math::Affine3A;
use bevy::prelude::*;
use smallvec::SmallVec;

use super::stages::leg_length;
use super::{FootGround, MotionTime};
use crate::anatomy::LegInfo;
use crate::mob::{CrowdSlot, GeneratedBody, MobBody, MobSkeleton};
use crate::num::ToF32;

/// Steepest tilt the fit takes, as a slope.
const MAX_SLOPE: f32 = 0.6;
/// Furthest a foot moves from the fitted plane, in leg lengths.
const MAX_FOOT: f32 = 0.4;
/// Furthest the root moves, in leg lengths.
const MAX_LIFT: f32 = 0.6;
/// How fast the fit follows its target, per second.
const EASE: f32 = 8.0;

/// How a crowd mob sits on its ground hits: added to [`FootGround`], eased each frame by the
/// crowd systems, read into the mob's `CrowdInstance`.
#[derive(Component, Clone, Debug)]
pub struct CrowdGround {
    /// The root raised, metres.
    pub lift: f32,
    /// The root's tilt, mob space.
    pub tilt: Quat,
    /// Each foot's offset from the tilted rest, metres, in `Anatomy::legs` order.
    pub feet: SmallVec<[f32; 8]>,
}

impl Default for CrowdGround {
    fn default() -> Self {
        Self { lift: 0.0, tilt: Quat::IDENTITY, feet: SmallVec::new() }
    }
}

impl CrowdGround {
    /// Whether this is flat ground at the root's height.
    #[must_use]
    pub fn is_flat(&self) -> bool {
        self.lift == 0.0 && self.tilt == Quat::IDENTITY && self.feet.iter().all(|f| *f == 0.0)
    }

    /// Eases toward `target`, snapping onto it once close so a settled mob stops changing.
    fn ease_toward(&mut self, target: &Self, dt: f32) {
        let k = 1.0 - (-EASE * dt).exp();
        let snap = |v: &mut f32, t: f32| {
            *v = if (t - *v).abs() < 1e-3 { t } else { (t - *v).mul_add(k, *v) };
        };
        snap(&mut self.lift, target.lift);
        self.tilt = if self.tilt.angle_between(target.tilt) < 1e-3 { target.tilt } else { self.tilt.slerp(target.tilt, k) };
        if self.feet.len() == target.feet.len() {
            for (f, t) in self.feet.iter_mut().zip(&target.feet) {
                snap(f, *t);
            }
        } else {
            self.feet.clone_from(&target.feet);
        }
    }
}

/// Fits a body with `legs` at `root` onto `hits` (world, one per leg, `None` where the game
/// found no ground).
///
/// The plane through the hits (least squares, stiffened so a lone foot on a step moves that
/// foot more than it tilts the body, and flat where the feet don't spread) as a lift and tilt of
/// the root, and each foot's distance from that plane. `None` without a hit.
#[must_use]
pub fn ground_fit(legs: &[LegInfo], root: &Affine3A, hits: &[Option<Vec3>], leg: f32) -> Option<CrowdGround> {
    // Each hit foot: its rest spot in mob space, and how far the ground is from where the body
    // expects it (world up).
    let found: SmallVec<[(Vec3, f32); 8]> = legs
        .iter()
        .zip(hits)
        .filter_map(|(l, hit)| hit.map(|h| (l.rest_foot, h.y - root.transform_point3(l.rest_foot).y)))
        .collect();
    if found.is_empty() {
        return None;
    }
    let n = found.len().to_f32();
    let lift = found.iter().map(|(_, d)| d).sum::<f32>() / n;
    let centre = found.iter().map(|(r, _)| *r).sum::<Vec3>() / n;
    // Least-squares slopes about the centre, stiffened: the plane keeps level across a direction
    // the feet don't span (a biped's front-back), and leans only partway toward one foot that
    // disagrees with the others (the foot's own offset takes the rest).
    let stiff = (0.35 * leg).powi(2);
    let (mut sxx, mut szz, mut sxz, mut sxd, mut szd) = (stiff, stiff, 0.0, 0.0, 0.0);
    for (r, d) in &found {
        let (x, z, d) = (r.x - centre.x, r.z - centre.z, d - lift);
        sxx = x.mul_add(x, sxx);
        szz = z.mul_add(z, szz);
        sxz = x.mul_add(z, sxz);
        sxd = x.mul_add(d, sxd);
        szd = z.mul_add(d, szd);
    }
    let det = sxz.mul_add(-sxz, sxx * szz).max(1e-9);
    let sx = (sxd.mul_add(szz, -(szd * sxz)) / det).clamp(-MAX_SLOPE, MAX_SLOPE);
    let sz = (szd.mul_add(sxx, -(sxd * sxz)) / det).clamp(-MAX_SLOPE, MAX_SLOPE);
    let tilt = Quat::from_rotation_arc(Vec3::Y, Vec3::new(-sx, 1.0, -sz).normalize());
    let lift = lift.clamp(-MAX_LIFT * leg, MAX_LIFT * leg);
    let feet = legs
        .iter()
        .zip(hits)
        .map(|(l, hit)| {
            hit.map_or(0.0, |h| {
                let plane = sz.mul_add(l.rest_foot.z - centre.z, sx.mul_add(l.rest_foot.x - centre.x, root.transform_point3(l.rest_foot).y + lift));
                (h.y - plane).clamp(-MAX_FOOT * leg, MAX_FOOT * leg)
            })
        })
        .collect();
    Some(CrowdGround { lift, tilt, feet })
}

/// Crowd mobs with ground hits: ease their fit toward this frame's.
#[allow(clippy::type_complexity)]
pub(super) fn crowd_ground(
    time: Res<MotionTime>,
    bodies: Res<Assets<GeneratedBody>>,
    mut mobs: Query<(&Transform, &MobBody, &FootGround, &mut CrowdGround), (With<CrowdSlot>, Without<MobSkeleton>)>,
) {
    let dt = time.dt;
    mobs.par_iter_mut().for_each(|(tf, body, hits, mut ground)| {
        let Some(b) = bodies.get(&body.0) else { return };
        let target = ground_fit(&b.anatomy.legs, &tf.compute_affine(), &hits.0, leg_length(&b.anatomy)).unwrap_or_default();
        ground.ease_toward(&target, dt);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::build_src;

    fn quadruped() -> (Vec<LegInfo>, f32) {
        let a = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let leg = leg_length(&a);
        (a.legs, leg)
    }

    #[test]
    fn flat_ground_is_no_fit() {
        let (legs, leg) = quadruped();
        let root = Affine3A::from_translation(Vec3::new(3.0, 0.5, -2.0));
        let hits: Vec<Option<Vec3>> = legs.iter().map(|l| Some(root.transform_point3(l.rest_foot))).collect();
        let fit = ground_fit(&legs, &root, &hits, leg).unwrap();
        assert!(fit.lift.abs() < 1e-4 && fit.tilt.angle_between(Quat::IDENTITY) < 1e-4, "{fit:?}");
        assert!(fit.feet.iter().all(|f| f.abs() < 1e-4), "{fit:?}");
        assert!(ground_fit(&legs, &root, &[None, None, None, None], leg).is_none());
    }

    #[test]
    fn a_slope_tilts_the_root_and_a_step_lifts_one_foot() {
        let (legs, leg) = quadruped();
        let root = Affine3A::IDENTITY;
        // Ground rising ahead (+Z): the body pitches nose up, feet stay on the plane.
        let hits: Vec<Option<Vec3>> = legs.iter().map(|l| Some(l.rest_foot + Vec3::Y * (0.3 * l.rest_foot.z))).collect();
        let fit = ground_fit(&legs, &root, &hits, leg).unwrap();
        let forward = fit.tilt * Vec3::Z;
        let slope = forward.y / forward.z;
        assert!(slope > 0.15 && slope <= 0.3, "nose up by most of the slope: {forward}");
        assert!((fit.tilt * Vec3::X).y.abs() < 1e-3, "no roll");
        // The stiffness leaves the rest to the feet: front ones up, hind ones down, on the ground.
        let on_plane = |l: &LegInfo, f: f32| 0.3f32.mul_add(-l.rest_foot.z, (fit.tilt * l.rest_foot).y + fit.lift + f).abs();
        assert!(legs.iter().zip(&fit.feet).all(|(l, f)| on_plane(l, *f) < 0.02), "{fit:?}");
        // One foot on a 0.2 m step: that foot leaves the plane most (the body leans partway
        // toward it, which is what the others' smaller offsets pay for), the root rises a little.
        let mut hits: Vec<Option<Vec3>> = legs.iter().map(|l| Some(l.rest_foot)).collect();
        if let Some(Some(h)) = hits.first_mut() {
            h.y += 0.2;
        }
        let fit = ground_fit(&legs, &root, &hits, leg).unwrap();
        assert!(fit.lift > 0.03 && fit.lift < 0.1, "{fit:?}");
        let first = fit.feet.first().copied().unwrap_or(0.0);
        assert!(first > 0.06 && fit.feet.iter().skip(1).all(|f| f.abs() < first), "{fit:?}");
    }

    #[test]
    fn the_fit_eases_and_settles() {
        let mut g = CrowdGround::default();
        let target = CrowdGround { lift: 0.2, tilt: Quat::from_rotation_x(0.2), feet: SmallVec::from_slice(&[0.1, -0.1]) };
        for _ in 0..240 {
            g.ease_toward(&target, 1.0 / 60.0);
        }
        assert_eq!(g.lift, target.lift);
        assert_eq!(g.tilt, target.tilt);
        assert_eq!(g.feet, target.feet);
        g.ease_toward(&CrowdGround::default(), 1.0 / 60.0);
        assert!(g.lift < 0.2 && g.lift > 0.1, "on its way back: {}", g.lift);
    }
}
