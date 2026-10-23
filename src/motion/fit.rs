//! Which bodies a cycle may play on. A body's [`BodyFamily`] and [`Feature`]s are derived from
//! its anatomy, never authored; a cycle's [`Fit`] lists what it needs, and a misfit never plays
//! (the reason is reported instead of the motion looking wrong).

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::anatomy::{Anatomy, BuiltPart, HardKind, LimbRole, Side, SocketKind, Stance};
use crate::num::Clamp;

/// How a body is laid out for moving. Proportions don't change the family: humans, goblins and
/// elves are all `UprightBiped`; a dragon is a `Quadruped` with `Wings`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BodyFamily {
    /// Two legs under an upright torso (humans, goblins, elves).
    UprightBiped,
    /// Two legs under a horizontal body (birds, theropods, wyverns).
    Biped,
    Quadruped,
    Hexapod,
    Octopod,
    /// Ten or more legs (millipedes, centipedes).
    Myriapod,
    /// No legs (snakes, worms, eels).
    Serpentine,
    Other,
}

/// Add-ons a cycle can require or exclude, independent of family.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    Neck,
    /// A neck at least as long as a leg.
    LongNeck,
    Tail,
    /// A tail at least twice a leg's length.
    LongTail,
    Jaw,
    Ears,
    Antennae,
    Horns,
    /// Manipulator limbs besides the legs.
    Arms,
    /// A grasping hand or pincer on that side.
    Grip(Side),
    Wings,
}

/// Bodies a cycle may play on. Empty lists mean "any".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Fit {
    pub family: Vec<BodyFamily>,
    /// Locomotor legs.
    pub legs: RangeInclusive<u8>,
    /// Every leg's stance must be one of these.
    pub stance: Vec<Stance>,
    pub needs: Vec<Feature>,
    pub excludes: Vec<Feature>,
    /// Leg length in metres (a gallop for horses, not mice).
    pub size: Option<RangeInclusive<f32>>,
}

impl Default for Fit {
    fn default() -> Self {
        Self { family: Vec::new(), legs: 0..=255, stance: Vec::new(), needs: Vec::new(), excludes: Vec::new(), size: None }
    }
}

impl Fit {
    /// Fits any body.
    #[must_use]
    pub fn any() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn family(family: BodyFamily) -> Self {
        Self { family: vec![family], ..Self::default() }
    }

    #[must_use]
    pub fn or_family(mut self, family: BodyFamily) -> Self {
        self.family.push(family);
        self
    }

    #[must_use]
    pub const fn legs(mut self, legs: RangeInclusive<u8>) -> Self {
        self.legs = legs;
        self
    }

    #[must_use]
    pub fn stance(mut self, stances: impl IntoIterator<Item = Stance>) -> Self {
        self.stance = stances.into_iter().collect();
        self
    }

    #[must_use]
    pub fn needs(mut self, feature: Feature) -> Self {
        self.needs.push(feature);
        self
    }

    #[must_use]
    pub fn excludes(mut self, feature: Feature) -> Self {
        self.excludes.push(feature);
        self
    }

    #[must_use]
    pub const fn size(mut self, leg_length: RangeInclusive<f32>) -> Self {
        self.size = Some(leg_length);
        self
    }

    /// Every reason this body doesn't fit (empty = fits).
    #[must_use]
    pub fn check(&self, body: &BodyTraits) -> Vec<Misfit> {
        let mut out = Vec::new();
        if !self.family.is_empty() && !self.family.contains(&body.family) {
            out.push(Misfit::Family(body.family));
        }
        if !self.legs.contains(&body.legs) {
            out.push(Misfit::Legs(body.legs));
        }
        if !self.stance.is_empty()
            && let Some(s) = body.stances.iter().find(|s| !self.stance.contains(s)) {
                out.push(Misfit::Stance(*s));
            }
        out.extend(self.needs.iter().filter(|f| !body.features.contains(f)).map(|f| Misfit::Missing(*f)));
        out.extend(self.excludes.iter().filter(|f| body.features.contains(f)).map(|f| Misfit::Excluded(*f)));
        if let Some(size) = &self.size
            && !size.contains(&body.leg_length) {
                out.push(Misfit::Size(body.leg_length));
            }
        out
    }
}

/// Why a cycle doesn't fit a body.
#[derive(Clone, Debug, PartialEq)]
pub enum Misfit {
    Family(BodyFamily),
    Legs(u8),
    Stance(Stance),
    Missing(Feature),
    Excluded(Feature),
    Size(f32),
}

/// What a body is, for fitting cycles: derived once per anatomy.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyTraits {
    pub family: BodyFamily,
    pub legs: u8,
    pub stances: Vec<Stance>,
    pub features: Vec<Feature>,
    /// Mean leg length, metres.
    pub leg_length: f32,
}

impl BodyTraits {
    #[must_use]
    pub fn of(a: &Anatomy) -> Self {
        let legs = a.legs.len().clamp_u8();
        let leg_length = super::stages::leg_length(a);
        let model = a.skeleton.model_rest();
        let upright = match (a.spine.first(), a.spine.last()) {
            (Some(first), Some(last)) => model.get(last.index()).zip(model.get(first.index())).is_some_and(|(top, bottom)| {
                let d = top.translation - bottom.translation;
                d.y > 0.6 * d.length()
            }),
            _ => false,
        };
        let family = match legs {
            0 => BodyFamily::Serpentine,
            2 if upright => BodyFamily::UprightBiped,
            2 => BodyFamily::Biped,
            4 => BodyFamily::Quadruped,
            6 => BodyFamily::Hexapod,
            8 => BodyFamily::Octopod,
            n if n >= 10 => BodyFamily::Myriapod,
            _ => BodyFamily::Other,
        };
        let mut stances: Vec<Stance> = a.legs.iter().map(|l| l.stance).collect();
        stances.dedup();
        let chain_len = |prefix: &str| -> f32 {
            a.skeleton.bones.iter().filter(|b| b.name.starts_with(prefix)).map(|b| b.length).sum::<f32>()
        };
        let has_bone = |prefix: &str| a.skeleton.bones.iter().any(|b| b.name.starts_with(prefix));
        let mut features = Vec::new();
        let neck = chain_len("neck.");
        if neck > 0.0 {
            features.push(Feature::Neck);
            if neck >= leg_length {
                features.push(Feature::LongNeck);
            }
        }
        let tail = chain_len("tail.");
        if tail > 0.0 {
            features.push(Feature::Tail);
            if tail >= 2.0 * leg_length {
                features.push(Feature::LongTail);
            }
        }
        for (prefix, feature) in [("jaw", Feature::Jaw), ("ear.", Feature::Ears), ("antenna.", Feature::Antennae)] {
            if has_bone(prefix) {
                features.push(feature);
            }
        }
        if a.hard_parts.iter().any(|h| matches!(h.params.kind, HardKind::Horn | HardKind::Antler { .. })) {
            features.push(Feature::Horns);
        }
        if a.parts.iter().any(|p: &BuiltPart| matches!(p.role, Some(LimbRole::Manipulator))) {
            features.push(Feature::Arms);
        }
        for s in a.sockets.iter().filter(|s| s.kind == SocketKind::Grip) {
            features.push(Feature::Grip(s.side));
        }
        if !a.wings.is_empty() {
            features.push(Feature::Wings);
        }
        Self { family, legs, stances, features, leg_length }
    }

    #[must_use]
    pub fn has(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }
}
