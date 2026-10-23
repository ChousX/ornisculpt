use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::heft::{HeftResponse, StepShape};
use crate::anatomy::{LegInfo, Side};

/// How a mob moves its legs. Independent of any body plan: legs get their phase from their
/// `(pair, side)`, so one gait works on any number of legs. Loaded from `*.gait.ron`.
#[derive(Asset, Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Gait {
    /// Sorted by `min_speed`; the fastest mode whose `min_speed` is reached is used.
    pub modes: Vec<GaitMode>,
    /// Max turn rate, degrees per second.
    pub turn_rate_deg: f32,
    /// Acceleration in leg lengths per second².
    pub accel: f32,
    /// What the body's weight does to every mode.
    #[reflect(ignore)]
    pub heft: HeftResponse,
}

impl Default for Gait {
    fn default() -> Self {
        Self::for_legs(4)
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct GaitMode {
    pub name: String,
    /// Speed (leg lengths per second) from which this mode is used.
    pub min_speed: f32,
    pub pattern: PhasePattern,
    /// Fraction of the cycle each foot is on the ground.
    pub duty: f32,
    /// Distance covered per cycle, in leg lengths.
    pub stride: f32,
    /// Foot lift at mid-swing, in leg lengths.
    pub lift: f32,
    /// Body bob, in leg lengths.
    pub bob: f32,
    /// How the torso moves over the feet.
    pub body: BodyMotion,
    /// Foot arc and the torso's step signals.
    #[reflect(ignore)]
    pub shape: StepShape,
}

impl Default for GaitMode {
    fn default() -> Self {
        Self {
            name: "walk".into(),
            min_speed: 0.0,
            pattern: PhasePattern::LateralSequence,
            duty: 0.65,
            stride: 1.0,
            lift: 0.15,
            bob: 0.03,
            body: BodyMotion::default(),
            shape: StepShape::default(),
        }
    }
}

/// Phase offset assignment for legs.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[derive(Default)]
pub enum PhasePattern {
    /// Left/right alternate (bipeds; a "pace" on quadrupeds).
    Alternate,
    /// Walk: hind left, fore left, hind right, fore right.
    #[default]
    LateralSequence,
    /// Trot: diagonal pairs together.
    Diagonal,
    /// Front pair together, hind pair together.
    Bound,
    /// Transverse gallop led by `lead`.
    Gallop { lead: Side },
    /// Hexapod: alternating tripods.
    Tripod,
    /// Many legs: a wave travelling back to front, `lag` cycle per pair.
    Wave { lag: f32 },
    /// Explicit offsets, in `Anatomy::legs` order.
    Custom(Vec<f32>),
}

impl Gait {
    /// A reasonable default for a leg count: bipeds walk/run, quadrupeds walk/trot,
    /// hexapods tripod, more legs wave.
    #[must_use]
    pub fn for_legs(legs: usize) -> Self {
        let mode = |name: &str, min_speed, pattern, duty, stride, lift, bob, body| GaitMode {
            name: name.into(),
            min_speed,
            pattern,
            duty,
            stride,
            lift,
            bob,
            body,
            shape: StepShape::default(),
        };
        let modes = match legs {
            0..=2 => vec![
                mode("walk", 0.0, PhasePattern::Alternate, 0.6, 1.0, 0.12, 0.03, BodyMotion::walk()),
                mode("run", 2.5, PhasePattern::Alternate, 0.4, 1.8, 0.2, 0.05, BodyMotion::run()),
            ],
            3..=4 => vec![
                mode("walk", 0.0, PhasePattern::LateralSequence, 0.65, 1.0, 0.15, 0.025, BodyMotion::walk()),
                mode("trot", 2.0, PhasePattern::Diagonal, 0.5, 1.5, 0.2, 0.04, BodyMotion::trot()),
            ],
            5..=6 => vec![mode("tripod", 0.0, PhasePattern::Tripod, 0.55, 0.9, 0.2, 0.01, BodyMotion::rigid())],
            _ => vec![mode("wave", 0.0, PhasePattern::Wave { lag: 0.15 }, 0.7, 0.8, 0.15, 0.005, BodyMotion::rigid())],
        };
        Self { modes, turn_rate_deg: 180.0, accel: 4.0, heft: HeftResponse::default() }
    }

    /// Index of the mode for a speed in leg lengths per second.
    #[must_use]
    pub fn mode_for(&self, speed: f32) -> usize {
        self.modes.iter().rposition(|m| speed >= m.min_speed).unwrap_or(0)
    }
}

impl PhasePattern {
    /// Phase offset (0..1) of `leg` among `legs`.
    #[must_use]
    pub fn offset(&self, leg: &LegInfo, index: usize, legs: &[LegInfo]) -> f32 {
        let pairs = f32::from(legs.iter().map(|l| l.pair.saturating_add(1)).max().unwrap_or(1));
        let right = leg.side == Side::Right;
        let side = f32::from(u8::from(right));
        let pair = f32::from(leg.pair);
        let odd = |n: u8| f32::from(n % 2);
        let o = match self {
            Self::Alternate => side * 0.5,
            // Hind pair leads; each pair forward lags by 1/(2·pairs).
            Self::LateralSequence => f32::mul_add(side, 0.5, (pairs - 1.0 - pair) * 0.5 / pairs),
            Self::Diagonal | Self::Tripod => odd(leg.pair.wrapping_add(u8::from(right))) * 0.5,
            Self::Bound => odd(leg.pair) * 0.5,
            Self::Gallop { lead } => {
                let trailing = if right == (*lead == Side::Right) { 0.1 } else { 0.0 };
                (if leg.pair == 0 { 0.5 } else { 0.0 }) + trailing
            }
            Self::Wave { lag } => f32::mul_add(side, 0.5, (pairs - 1.0 - pair) * lag),
            Self::Custom(v) => v.get(index).copied().unwrap_or(0.0),
        };
        o.rem_euclid(1.0)
    }
}

impl crate::ron_loader::RonAsset for Gait {
    const EXTENSIONS: &'static [&'static str] = &["gait.ron"];
}

/// Loads `*.gait.ron`.
pub type GaitLoader = crate::ron_loader::RonLoader<Gait>;
pub type GaitLoaderError = crate::ron_loader::RonLoaderError;

/// How the torso rides the feet.
///
/// Each girdle (hips over the hind legs, shoulders over the fore
/// legs) turns toward its forward leg, drops on its swinging side and shifts over its planted
/// leg; the spine bends between the two; the head steadies against it. Degrees and leg lengths,
/// at full speed (scaled down toward a standstill).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct BodyMotion {
    /// Girdle turn toward the forward leg, degrees.
    pub yaw: f32,
    /// Girdle drop on the swinging side, degrees.
    pub roll: f32,
    /// Hips shift over the planted foot, leg lengths.
    pub sway: f32,
    /// Hips and shoulders rise and fall apart (the body rocks), leg lengths.
    pub rock: f32,
    /// Upright bodies: how far the chest turns against the hips, 0..1.
    pub counter: f32,
    /// How much the neck cancels the body's turning, 0..1.
    pub steady: f32,
    /// Head nod with the fore feet, degrees.
    pub nod: f32,
    /// Tail swing against the hips, 0..2.
    pub tail: f32,
}

impl Default for BodyMotion {
    fn default() -> Self {
        Self::walk()
    }
}

impl BodyMotion {
    #[must_use]
    pub const fn walk() -> Self {
        Self { yaw: 7.0, roll: 5.0, sway: 0.03, rock: 0.04, counter: 0.7, steady: 0.6, nod: 6.0, tail: 1.0 }
    }

    #[must_use]
    pub const fn run() -> Self {
        Self { yaw: 10.0, roll: 3.0, sway: 0.012, rock: 0.03, counter: 0.9, steady: 0.7, nod: 3.0, tail: 0.6 }
    }

    /// Diagonal pairs move together, so the back stays stiff and the hips and shoulders roll.
    #[must_use]
    pub const fn trot() -> Self {
        Self { yaw: 3.0, roll: 5.0, sway: 0.01, rock: 0.02, counter: 0.5, steady: 0.7, nod: 3.0, tail: 0.8 }
    }

    /// The back flexes and extends: big rocking, little twisting.
    #[must_use]
    pub const fn gallop() -> Self {
        Self { yaw: 2.0, roll: 2.5, sway: 0.0, rock: 0.12, counter: 0.3, steady: 0.5, nod: 10.0, tail: 0.5 }
    }

    /// Strutting birds and theropods: a level body, a very steady head.
    #[must_use]
    pub const fn strut() -> Self {
        Self { yaw: 5.0, roll: 6.0, sway: 0.04, rock: 0.0, counter: 0.0, steady: 0.9, nod: 0.0, tail: 1.2 }
    }

    /// Insects and other armoured bodies: barely any.
    #[must_use]
    pub const fn rigid() -> Self {
        Self { yaw: 2.5, roll: 2.0, sway: 0.0, rock: 0.01, counter: 0.0, steady: 0.3, nod: 0.0, tail: 0.5 }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self { yaw: 0.0, roll: 0.0, sway: 0.0, rock: 0.0, counter: 0.0, steady: 0.0, nod: 0.0, tail: 0.0 }
    }
}

impl BodyMotion {
    /// Moves every amount `t` of the way toward `to`.
    pub fn approach(&mut self, to: &Self, t: f32) {
        for (a, b) in [
            (&mut self.yaw, to.yaw),
            (&mut self.roll, to.roll),
            (&mut self.sway, to.sway),
            (&mut self.rock, to.rock),
            (&mut self.counter, to.counter),
            (&mut self.steady, to.steady),
            (&mut self.nod, to.nod),
            (&mut self.tail, to.tail),
        ] {
            *a = f32::mul_add(b - *a, t, *a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::Stance;

    fn leg(pair: u8, side: Side) -> LegInfo {
        LegInfo {
            chain: default(),
            foot: crate::anatomy::BoneId(0),
            pair,
            side,
            stance: Stance::Digitigrade,
            rest_foot: Vec3::ZERO,
            pole: Vec3::Z,
            length: 1.0,
        }
    }

    #[test]
    fn quadruped_patterns() {
        let legs = [leg(0, Side::Left), leg(0, Side::Right), leg(1, Side::Left), leg(1, Side::Right)];
        let off = |p: &PhasePattern| legs.iter().enumerate().map(|(i, l)| p.offset(l, i, &legs)).collect::<Vec<_>>();
        // Walk: LH 0, LF .25, RH .5, RF .75.
        assert_eq!(off(&PhasePattern::LateralSequence), [0.25, 0.75, 0.0, 0.5]);
        // Trot: diagonals together.
        assert_eq!(off(&PhasePattern::Diagonal), [0.0, 0.5, 0.5, 0.0]);
    }

    #[test]
    fn hexapod_tripod_alternates() {
        let legs: Vec<_> = (0..3).flat_map(|p| [leg(p, Side::Left), leg(p, Side::Right)]).collect();
        let o: Vec<_> = legs.iter().enumerate().map(|(i, l)| PhasePattern::Tripod.offset(l, i, &legs)).collect();
        assert_eq!(o, [0.0, 0.5, 0.5, 0.0, 0.0, 0.5]);
    }
}

