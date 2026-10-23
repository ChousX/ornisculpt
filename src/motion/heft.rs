//! How a body's weight and its shape over a step change the gait. Every response is a
//! [`Curve`], so it can be tuned in RON without touching code:
//!
//! - [`StepShape`] (on each gait mode and gait cycle): the foot's arc and the torso's signals
//!   over one leg's step.
//! - [`HeftResponse`] (on each [`Gait`](super::Gait)): what heaviness does to cadence, stance,
//!   foot lift, landing, waddle and agility.
//! - [`Heft`]: a body's heaviness and where its weight sits, from [`BodyMass`](crate::anatomy::BodyMass).

use serde::{Deserialize, Serialize};

use super::cycle::Curve;
use crate::anatomy::{Anatomy, WATER};
use crate::num::ToF32;

/// The shape of one step. Torso signals are sampled over the leg's own step time: `0..0.5` is
/// the stance (foot planted, moving back), `0.5..1` the swing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct StepShape {
    /// Foot height over the swing (`0..1`), times the mode's `lift`.
    pub arc: Curve,
    /// How far along its path the swinging foot is (`0..1` → `0..1`).
    pub travel: Curve,
    /// Foot forward (+1) or back (−1) of the hip: the girdles turn with it.
    pub reach: Curve,
    /// Foot in the air: its side of the girdle drops.
    pub raise: Curve,
    /// Weight on the foot: the hips shift over it, the girdle vaults over it.
    pub load: Curve,
    /// How lively the torso is by speed (leg lengths per second → `0..1`).
    pub lively: Curve,
}

impl Default for StepShape {
    fn default() -> Self {
        Self {
            arc: Curve::bump(1.0, 0.0, 1.0),
            travel: Curve::keys([(0.0, 0.0), (1.0, 1.0)]),
            reach: Curve::wave(1.0).phase(0.25),
            raise: Curve::bump(1.0, 0.5, 1.0),
            load: Curve::bump(1.0, 0.0, 0.5),
            lively: Curve::keys([(0.0, 0.0), (0.4, 1.0)]),
        }
    }
}

/// A body's weight as the gait feels it.
#[derive(bevy::prelude::Component, Clone, Copy, Debug, PartialEq)]
pub struct Heft {
    pub kg: f32,
    /// Weight over leg cross-section, against a dog or a human (`1.0`): big animals are heavier
    /// for their legs (the square–cube law), spindly legs under a barrel body too.
    pub heaviness: f32,
    /// Share of the weight on the fore legs (`0.5` for one pair).
    pub fore: f32,
    /// Shares of the total in the head and neck, and in the tails.
    pub head: f32,
    pub tail: f32,
}

impl Default for Heft {
    fn default() -> Self {
        Self { kg: 0.0, heaviness: 1.0, fore: 0.5, head: 0.1, tail: 0.0 }
    }
}

impl Heft {
    /// Kilograms per m² of leg section of a dog or a human.
    pub const REFERENCE: f32 = 2500.0;

    #[must_use]
    pub fn of(a: &Anatomy) -> Self {
        let m = &a.mass;
        if m.total <= 0.0 || a.legs.is_empty() {
            return Self::default();
        }
        // A leg's mean cross-section: its volume over its length.
        let section: f32 = a.legs.iter().map(|l| m.of(l.chain.iter().copied()) / (WATER * l.length.max(1e-3))).sum();
        let heaviness = (m.total / section.max(1e-6) / Self::REFERENCE).sqrt().clamp(0.25, 4.0);
        let (front, back) = (a.legs.iter().map(|l| l.pair).min().unwrap_or(0), a.legs.iter().map(|l| l.pair).max().unwrap_or(0));
        let z = |pair: u8| {
            let feet: Vec<f32> = a.legs.iter().filter(|l| l.pair == pair).map(|l| l.rest_foot.z).collect();
            feet.iter().sum::<f32>() / feet.len().max(1).to_f32()
        };
        let fore = if front == back { 0.5 } else { ((m.centre.z - z(back)) / (z(front) - z(back))).clamp(0.0, 1.0) };
        let share = |pick: &dyn Fn(&str) -> bool| {
            a.skeleton.bones.iter().zip(&m.bones).filter(|(b, _)| pick(&b.name)).map(|(_, kg)| kg).sum::<f32>() / m.total
        };
        Self {
            kg: m.total,
            heaviness,
            fore,
            head: share(&|n| n.starts_with("neck") || n == "skull" || n == "jaw"),
            tail: share(&|n| n.starts_with("tail")),
        }
    }
}

/// What heaviness does to a gait. Each curve is keyed on [`Heft::heaviness`] (`1.0` = a dog or
/// a human, `2.0` = a horse, `3.0` = an elephant).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct HeftResponse {
    /// Steps per second, ×.
    pub cadence: Curve,
    /// Added to each mode's duty: heavy bodies keep feet down longer. The cadence rises to
    /// match, so a planted foot travels no further back (legs don't overreach).
    pub duty: Curve,
    /// Foot lift, ×.
    pub lift: Curve,
    /// How far a girdle sinks as its feet take the weight, leg lengths.
    pub settle: Curve,
    /// The sink over the stance (`0..1`, touchdown to lift-off).
    pub landing: Curve,
    /// Girdle roll and hip sway, ×.
    pub waddle: Curve,
    /// Acceleration and turn rate, ×.
    pub agility: Curve,
}

impl Default for HeftResponse {
    fn default() -> Self {
        Self {
            cadence: Curve::linear([(0.5, 1.1), (1.0, 1.0)]),
            duty: Curve::linear([(0.5, -0.04), (1.0, 0.0), (2.0, 0.03), (3.0, 0.06)]),
            lift: Curve::linear([(0.5, 1.15), (1.0, 1.0), (2.0, 0.8), (3.0, 0.65)]),
            settle: Curve::linear([(0.5, 0.0), (1.0, 0.008), (2.0, 0.022), (3.0, 0.035)]),
            landing: Curve::bump(1.0, 0.0, 0.45),
            waddle: Curve::linear([(0.5, 0.85), (1.0, 1.0), (2.0, 1.3), (3.0, 1.6)]),
            agility: Curve::linear([(0.5, 1.3), (1.0, 1.0), (2.0, 0.6), (3.0, 0.4)]),
        }
    }
}

impl HeftResponse {
    /// No effect at any weight.
    #[must_use]
    pub fn none() -> Self {
        let one = || Curve::Constant(1.0);
        Self {
            cadence: one(),
            duty: Curve::Constant(0.0),
            lift: one(),
            settle: Curve::Constant(0.0),
            landing: Curve::Constant(0.0),
            waddle: one(),
            agility: one(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use super::*;
    use crate::motion::testing::{edited_body, sample_body};

    #[test]
    fn big_bodies_are_heavier_and_carry_weight_forward() {
        let dog = Heft::of(&sample_body(include_str!("../../assets/mobs/quadruped.body.ron")));
        let horse = Heft::of(&sample_body(include_str!("../../assets/mobs/horse.body.ron")));
        let human = Heft::of(&sample_body(include_str!("../../assets/mobs/biped.body.ron")));
        println!("dog {dog:?}\nhorse {horse:?}\nhuman {human:?}");
        assert!((0.8..1.25).contains(&dog.heaviness) && (0.8..1.25).contains(&human.heaviness));
        assert!(horse.heaviness > 1.6, "a horse should feel heavy: {}", horse.heaviness);
        // Dogs and horses carry about 60% on the fore legs.
        assert!((0.52..0.7).contains(&dog.fore) && (0.52..0.7).contains(&horse.fore));
    }

    #[test]
    fn density_scales_a_part() {
        let src = include_str!("../../assets/mobs/quadruped.body.ron");
        let plain = sample_body(src);
        let dense = edited_body(src, |p| p.root.density = 2.0);
        // Only the root part (the torso) doubles.
        assert!(dense.mass.total > plain.mass.total * 1.2 && dense.mass.total < plain.mass.total * 2.0);
    }

    #[test]
    fn gaits_round_trip_through_ron() {
        let gait = crate::motion::Gait::for_legs(4);
        let text = ron::ser::to_string_pretty(&gait, ron::ser::PrettyConfig::default()).unwrap();
        let back: crate::motion::Gait = ron::from_str(&text).unwrap();
        assert_eq!(back.heft, gait.heft);
        assert_eq!(back.modes[0].shape, gait.modes[0].shape);
        // Older files without the new fields still load.
        let old: crate::motion::Gait = ron::from_str("(modes: [(name: \"walk\", duty: 0.7)])").unwrap();
        assert_eq!(old.heft, HeftResponse::default());
        let at = text.find("heft").unwrap();
        println!("{}", text.get(at..).unwrap().chars().take(400).collect::<String>());
    }

    /// The default step shape is the hand-written motion it replaced.
    #[test]
    fn default_shape_matches_the_old_formulas() {
        let s = StepShape::default();
        for k in 0u8..=20 {
            let x = f32::from(k) / 20.0;
            let close = |a: f32, b: f32| assert!((a - b).abs() < 1e-4, "{a} vs {b} at {x}");
            close(s.arc.sample(x), (PI * x).sin());
            close(s.travel.sample(x), x * x * 2.0f32.mul_add(-x, 3.0));
            // Stance half: u = 2x; swing half: s = 2x − 1.
            let (stance, swing) = (x * 0.5, x.mul_add(0.5, 0.5));
            close(s.reach.sample(stance), (PI * x).cos());
            close(s.load.sample(stance), (PI * x).sin());
            close(s.reach.sample(swing), -(PI * x).cos());
            close(s.raise.sample(swing), (PI * x).sin());
            close(s.lively.sample(x * 0.4), x * x * 2.0f32.mul_add(-x, 3.0));
        }
    }
}
