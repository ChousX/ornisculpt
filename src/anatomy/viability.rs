use bevy::prelude::*;

use super::Anatomy;

/// A test a built body must pass to be considered viable. Used for authoring feedback now and as
/// the fitness gate for genetics later. Gait compatibility lives in the `motion` module, since it
/// needs a gait.
pub trait ViabilityCheck: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self, anatomy: &Anatomy) -> Viability;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Viability {
    Ok,
    Warn(String),
    Fail(String),
}

impl Viability {
    pub fn is_fail(&self) -> bool {
        matches!(self, Viability::Fail(_))
    }
}

/// Results of running every registered check.
#[derive(Clone, Debug, Default)]
pub struct ViabilityReport {
    pub results: Vec<(&'static str, Viability)>,
}

impl ViabilityReport {
    pub fn viable(&self) -> bool {
        !self.results.iter().any(|(_, v)| v.is_fail())
    }
}

/// Registered checks. Games may push their own.
#[derive(Resource, Default)]
pub struct ViabilityChecks(pub Vec<Box<dyn ViabilityCheck>>);

impl ViabilityChecks {
    pub fn with_builtins() -> Self {
        Self(vec![
            Box::new(BoneBudget),
            Box::new(FeetReachGround::default()),
            Box::new(StaticBalance),
            Box::new(RestSelfIntersection),
        ])
    }

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
        todo!("compare skeleton.len() / crowd.bones.len() to MAX_BONES / MAX_CROWD_BONES")
    }
}

/// Every locomotor foot can reach the ground plane under the rest pose.
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
        todo!("legs: rest_foot.y within tolerance of the lowest foot")
    }
}

/// Centre of mass (from primitive volumes) lies over the feet's support polygon.
pub struct StaticBalance;

impl ViabilityCheck for StaticBalance {
    fn name(&self) -> &'static str {
        "static_balance"
    }

    fn check(&self, anatomy: &Anatomy) -> Viability {
        todo!("approximate COM from primitive volumes; point-in-convex-hull of rest feet (xz)")
    }
}

/// Non-adjacent parts don't interpenetrate at rest (e.g. legs through the belly).
pub struct RestSelfIntersection;

impl ViabilityCheck for RestSelfIntersection {
    fn name(&self) -> &'static str {
        "rest_self_intersection"
    }

    fn check(&self, anatomy: &Anatomy) -> Viability {
        todo!("primitive AABB overlap between parts that are not parent/child")
    }
}
