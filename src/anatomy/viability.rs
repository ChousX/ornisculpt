use bevy::prelude::*;

use super::builder::{affine, lowest_point};
use super::{Anatomy, BoneId, MAX_BONES, MAX_CROWD_BONES, PrimOp, measure_mass};

/// A test a built body must pass to be considered viable. Used for authoring feedback now and as
/// the fitness gate for genetics later. Gait compatibility lives in the `motion` module, since it
/// needs a gait.
pub trait ViabilityCheck: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self, anatomy: &Anatomy) -> Viability;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Viability {
    Ok,
    Warn(String),
    Fail(String),
}

impl Viability {
    #[must_use]
    pub const fn is_fail(&self) -> bool {
        matches!(self, Self::Fail(_))
    }
}

/// Results of running every registered check.
#[derive(Clone, Debug, Default)]
pub struct ViabilityReport {
    pub results: Vec<(&'static str, Viability)>,
}

impl ViabilityReport {
    #[must_use]
    pub fn viable(&self) -> bool {
        !self.results.iter().any(|(_, v)| v.is_fail())
    }
}

/// Registered checks. Games may push their own.
#[derive(Resource, Default)]
pub struct ViabilityChecks(pub Vec<Box<dyn ViabilityCheck>>);

impl ViabilityChecks {
    #[must_use]
    pub fn with_builtins() -> Self {
        Self(vec![
            Box::new(BoneBudget),
            Box::new(FeetReachGround::default()),
            Box::new(StaticBalance),
            Box::new(RestSelfIntersection),
        ])
    }

    #[must_use]
    pub fn run(&self, anatomy: &Anatomy) -> ViabilityReport {
        ViabilityReport {
            results: self.0.iter().map(|c| (c.name(), c.check(anatomy))).collect(),
        }
    }
}

/// Bone and crowd-bone counts within budget.
pub struct BoneBudget;

impl ViabilityCheck for BoneBudget {
    fn name(&self) -> &'static str {
        "bone_budget"
    }

    fn check(&self, anatomy: &Anatomy) -> Viability {
        let (bones, crowd) = (anatomy.skeleton.len(), anatomy.skeleton.crowd.bones.len());
        if bones > MAX_BONES {
            Viability::Fail(format!("{bones} bones, budget {MAX_BONES}"))
        } else if crowd > MAX_CROWD_BONES {
            Viability::Fail(format!("{crowd} crowd bones, budget {MAX_CROWD_BONES}"))
        } else {
            Viability::Ok
        }
    }
}

/// Every locomotor foot can reach its planted place on the ground with its leg straightened.
///
/// The planted place is the rest foot, which the builder lowers until the sole touches. A leg that
/// can't reach it from the rest-pose hip stands on nothing (the beast's hind paws were built 3.5 cm
/// up, on legs too straight to reach down).
pub struct FeetReachGround {
    /// Allowed gap in metres.
    pub tolerance: f32,
}

impl Default for FeetReachGround {
    fn default() -> Self {
        Self { tolerance: 0.02 }
    }
}

impl ViabilityCheck for FeetReachGround {
    fn name(&self) -> &'static str {
        "feet_reach_ground"
    }

    fn check(&self, anatomy: &Anatomy) -> Viability {
        let model = anatomy.skeleton.rest_model();
        let at = |b: &BoneId| model.get(b.index()).map(|m| Vec3::from(m.translation));
        let short: Vec<String> = anatomy
            .legs
            .iter()
            .filter_map(|l| {
                let hip = l.chain.first().and_then(at)?;
                // The straightened leg (its bones end to end) against the hip's distance to where
                // the foot plants.
                let gap = hip.distance(l.rest_foot) - l.length;
                (gap > self.tolerance).then(|| format!("{:?} leg {} is {gap:.3} m short", l.side, l.pair))
            })
            .collect();
        if short.is_empty() { Viability::Ok } else { Viability::Fail(short.join("; ")) }
    }
}

/// Height of each leg's sole above y = 0 in the rest pose, in `anatomy.legs` order: the lowest
/// point of the leg below its first joint (its foot and anything hanging off it).
#[must_use]
pub fn sole_heights(anatomy: &Anatomy) -> Vec<f32> {
    let model = anatomy.skeleton.rest_model();
    let bones = anatomy.skeleton.bones.len();
    anatomy
        .legs
        .iter()
        .map(|l| {
            // The leg below its root, and everything hanging off its last bone.
            let mut below = vec![false; bones];
            for b in l.chain.iter().skip(1) {
                if let Some(m) = below.get_mut(b.index()) {
                    *m = true;
                }
            }
            for (i, def) in anatomy.skeleton.bones.iter().enumerate() {
                if def.parent.is_some_and(|p| below.get(p.index()).copied().unwrap_or(false))
                    && let Some(m) = below.get_mut(i)
                {
                    *m = true;
                }
            }
            anatomy
                .primitives
                .iter()
                .filter(|p| p.op == PrimOp::Union && below.get(p.bone.index()).copied().unwrap_or(false))
                .filter_map(|p| Some(lowest_point(&p.shape, &(*model.get(p.bone.index())? * affine(&p.local)))))
                .fold(f32::INFINITY, f32::min)
        })
        .collect()
}

/// Centre of mass (from primitive volumes) lies over the feet's support polygon.
pub struct StaticBalance;

impl ViabilityCheck for StaticBalance {
    fn name(&self) -> &'static str {
        "static_balance"
    }

    /// Needs three or more feet: one or two only balance in motion, so they always pass.
    fn check(&self, anatomy: &Anatomy) -> Viability {
        let feet: Vec<Vec2> = anatomy.legs.iter().map(|l| l.rest_foot.xz()).collect();
        if feet.len() < 3 {
            return Viability::Ok;
        }
        let centre = measure_mass(anatomy).centre.xz();
        if convex_hull(feet).array_windows().all(|[a, b]| (*b - *a).perp_dot(centre - *a) >= 0.0) {
            Viability::Ok
        } else {
            Viability::Fail(format!("centre of mass ({:.2}, {:.2}) is outside the feet", centre.x, centre.y))
        }
    }
}

/// Convex hull, counter-clockwise and closed (the first point repeats at the end): Andrew's
/// monotone chain.
fn convex_hull(mut points: Vec<Vec2>) -> Vec<Vec2> {
    /// Adds `p`, dropping the points it makes concave, but keeping at least `floor`.
    fn extend(hull: &mut Vec<Vec2>, floor: usize, p: Vec2) {
        while hull.len() >= floor + 2
            && let [.., a, b] = hull.as_slice()
            && (*b - *a).perp_dot(p - *a) <= 0.0
        {
            hull.pop();
        }
        hull.push(p);
    }
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let mut hull: Vec<Vec2> = Vec::with_capacity(points.len() * 2);
    // Lower hull left to right, then the upper hull back right to left.
    for &p in &points {
        extend(&mut hull, 0, p);
    }
    let floor = hull.len().saturating_sub(1);
    for &p in points.iter().rev().skip(1) {
        extend(&mut hull, floor, p);
    }
    hull
}

/// Non-adjacent parts don't interpenetrate at rest (e.g. legs through the belly).
pub struct RestSelfIntersection;

impl ViabilityCheck for RestSelfIntersection {
    fn name(&self) -> &'static str {
        "rest_self_intersection"
    }

    fn check(&self, _anatomy: &Anatomy) -> Viability {
        // Planned: primitive bounds overlapping between parts that are not parent and child.
        Viability::Warn("rest_self_intersection is not implemented yet".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_contains_inner_points() {
        let square = [Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y, Vec2::splat(0.5)];
        let hull = convex_hull(square.to_vec());
        assert_eq!(hull.len(), 5, "four corners, closed: {hull:?}");
        let inside = |c: Vec2| hull.array_windows().all(|[a, b]| (*b - *a).perp_dot(c - *a) >= 0.0);
        assert!(inside(Vec2::splat(0.5)));
        assert!(!inside(Vec2::new(1.5, 0.5)));
    }
}
