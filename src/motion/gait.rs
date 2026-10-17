use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
        }
    }
}

/// Phase offset assignment for legs.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum PhasePattern {
    /// Left/right alternate (bipeds; a "pace" on quadrupeds).
    Alternate,
    /// Walk: hind left, fore left, hind right, fore right.
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
    pub fn for_legs(legs: usize) -> Self {
        let mode = |name: &str, min_speed, pattern, duty, stride, lift, bob| GaitMode {
            name: name.into(),
            min_speed,
            pattern,
            duty,
            stride,
            lift,
            bob,
        };
        let modes = match legs {
            0..=2 => vec![
                mode("walk", 0.0, PhasePattern::Alternate, 0.6, 1.0, 0.12, 0.03),
                mode("run", 2.5, PhasePattern::Alternate, 0.4, 1.8, 0.2, 0.05),
            ],
            3..=4 => vec![
                mode("walk", 0.0, PhasePattern::LateralSequence, 0.65, 1.0, 0.15, 0.025),
                mode("trot", 2.0, PhasePattern::Diagonal, 0.5, 1.5, 0.2, 0.04),
            ],
            5..=6 => vec![mode("tripod", 0.0, PhasePattern::Tripod, 0.55, 0.9, 0.2, 0.01)],
            _ => vec![mode("wave", 0.0, PhasePattern::Wave { lag: 0.15 }, 0.7, 0.8, 0.15, 0.005)],
        };
        Self { modes, turn_rate_deg: 180.0, accel: 4.0 }
    }

    /// Index of the mode for a speed in leg lengths per second.
    pub fn mode_for(&self, speed: f32) -> usize {
        self.modes.iter().rposition(|m| speed >= m.min_speed).unwrap_or(0)
    }
}

impl PhasePattern {
    /// Phase offset (0..1) of `leg` among `legs`.
    pub fn offset(&self, leg: &LegInfo, index: usize, legs: &[LegInfo]) -> f32 {
        let pairs = legs.iter().map(|l| l.pair + 1).max().unwrap_or(1) as f32;
        let side = match leg.side {
            Side::Right => 1.0,
            _ => 0.0,
        };
        let pair = leg.pair as f32;
        let odd = |x: f32| (x as u32 % 2) as f32;
        let o = match self {
            PhasePattern::Alternate => side * 0.5,
            // Hind pair leads; each pair forward lags by 1/(2·pairs).
            PhasePattern::LateralSequence => side * 0.5 + (pairs - 1.0 - pair) * 0.5 / pairs,
            PhasePattern::Diagonal | PhasePattern::Tripod => odd(pair + side) * 0.5,
            PhasePattern::Bound => odd(pair) * 0.5,
            PhasePattern::Gallop { lead } => {
                let lead_side = if *lead == Side::Right { 1.0 } else { 0.0 };
                let trailing = if side == lead_side { 0.1 } else { 0.0 };
                (if pair == 0.0 { 0.5 } else { 0.0 }) + trailing
            }
            PhasePattern::Wave { lag } => side * 0.5 + (pairs - 1.0 - pair) * lag,
            PhasePattern::Custom(v) => v.get(index).copied().unwrap_or(0.0),
        };
        o.rem_euclid(1.0)
    }
}

/// Loads `*.gait.ron`.
#[derive(Default, TypePath)]
pub struct GaitLoader;

#[derive(Debug, Error)]
pub enum GaitLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for GaitLoader {
    type Asset = Gait;
    type Settings = ();
    type Error = GaitLoaderError;

    async fn load(&self, reader: &mut dyn Reader, _: &(), _: &mut LoadContext<'_>) -> Result<Gait, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        &["gait.ron"]
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
