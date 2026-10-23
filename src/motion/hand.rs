//! Hand poses: per-digit curl and spread that gameplay sets on a mob, laid over whatever its
//! cycles play. Digits exist at the hero tier only, so this is a hero-tier effect.

use bevy::prelude::*;

use super::MobPose;
use crate::anatomy::{Anatomy, HandDef, LimbRole, Side};
use crate::num::ToF32;

/// One hand's pose. Digits count from the thumb (or from the first digit of a hand without one);
/// a hand with more digits than `curl` holds repeats the last value.
#[derive(Reflect, Clone, Copy, Debug, PartialEq)]
pub struct Hand {
    /// Per digit, thumb first: `0.0` = straight .. `1.0` = closed into the palm (negative bends
    /// back).
    pub curl: [f32; 5],
    /// Extra fan between the fingers in degrees (negative draws them together).
    pub spread: f32,
    /// How fully this replaces the animated hand, `0.0..=1.0`.
    pub weight: f32,
}

impl Hand {
    /// Loosely curled, as a hand hangs at rest.
    pub const RELAXED: Self = Self::new([0.2, 0.2, 0.25, 0.3, 0.35], 0.0);
    /// Flat, fingers fanned.
    pub const OPEN: Self = Self::new([0.0; 5], 8.0);
    pub const FIST: Self = Self::new([0.9, 1.0, 1.0, 1.0, 1.0], -4.0);
    /// Closed round a handle (a tool held in a grip socket).
    pub const GRIP: Self = Self::new([0.6, 0.75, 0.8, 0.8, 0.8], -4.0);
    /// Index finger out, the rest closed.
    pub const POINT: Self = Self::new([0.85, 0.0, 1.0, 1.0, 1.0], 0.0);
    /// Thumb and index tips together.
    pub const PINCH: Self = Self::new([0.55, 0.5, 0.15, 0.2, 0.25], 0.0);

    #[must_use]
    pub const fn new(curl: [f32; 5], spread: f32) -> Self {
        Self { curl, spread, weight: 1.0 }
    }

    /// This pose with one digit's curl replaced (`0` = the thumb).
    #[must_use]
    pub fn with_digit(mut self, digit: usize, curl: f32) -> Self {
        if let Some(c) = self.curl.get_mut(digit) {
            *c = curl;
        }
        self
    }

    fn digit(&self, i: usize) -> f32 {
        self.curl.get(i).or_else(|| self.curl.last()).copied().unwrap_or(0.0)
    }
}

impl Default for Hand {
    fn default() -> Self {
        Self::RELAXED
    }
}

/// Hand poses for a mob's grasping hands, over its animation. `None` leaves that hand to the
/// cycles. A hand on a centre arm (one arm only) takes `left`.
#[derive(Component, Reflect, Clone, Copy, Debug, Default, PartialEq)]
#[reflect(Component)]
pub struct HandPose {
    pub left: Option<Hand>,
    pub right: Option<Hand>,
}

impl HandPose {
    #[must_use]
    pub const fn both(hand: Hand) -> Self {
        Self { left: Some(hand), right: Some(hand) }
    }

    #[must_use]
    pub const fn get(&self, side: Side) -> Option<Hand> {
        match side {
            Side::Right => self.right,
            Side::Left | Side::Center => self.left,
        }
    }

    pub const fn set(&mut self, side: Side, hand: Option<Hand>) {
        match side {
            Side::Right => self.right = hand,
            Side::Left | Side::Center => self.left = hand,
        }
    }
}

/// Bend of each finger joint at full curl, knuckle first, degrees (a fist); later joints of a
/// longer digit repeat the last.
const FINGER_BEND: [f32; 3] = [85.0, 100.0, 70.0];
/// The thumb folds less at its base and more toward its tip.
const THUMB_BEND: [f32; 3] = [30.0, 45.0, 65.0];

/// Whether a digits part is a hand: it grows from a manipulator arm (toes and claws on feet
/// aren't posed).
fn is_hand(a: &Anatomy, hand: &HandDef) -> bool {
    let bones = &a.skeleton.bones;
    hand.digits
        .first()
        .and_then(|d| d.first())
        .and_then(|b| bones.get(b.index())?.parent)
        .and_then(|p| a.parts.get(usize::from(bones.get(p.index())?.part)))
        .is_some_and(|p| matches!(p.role, Some(LimbRole::Manipulator | LimbRole::Both)))
}

/// Sets each posed hand's digit joints from its [`Hand`]: curl about each bone's own side axis
/// (the axis the rest curl uses), spread about the knuckle's outward axis.
pub fn pose_hands(a: &Anatomy, hands: &HandPose, pose: &mut MobPose) {
    for hand in a.hands.iter().filter(|h| is_hand(a, h)) {
        let Some(want) = hands.get(hand.side) else { continue };
        let weight = want.weight.clamp(0.0, 1.0);
        if weight <= 0.0 {
            continue;
        }
        let fingers = hand.digits.len() - usize::from(hand.thumb);
        let mirror = if hand.mirrored { -1.0 } else { 1.0 };
        for (i, bones) in hand.digits.iter().enumerate() {
            let is_thumb = hand.thumb && i == 0;
            let joints = if is_thumb { &THUMB_BEND } else { &FINGER_BEND };
            let curl = want.digit(i);
            // Fingers fan from the middle of the row, the outer ones most.
            let place = i.saturating_sub(usize::from(hand.thumb));
            let k = if is_thumb || fingers < 2 {
                0.0
            } else {
                place.to_f32() / (fingers - 1).to_f32() - 0.5
            };
            let fan = Quat::from_rotation_z((want.spread * k * mirror).to_radians());
            for (j, b) in bones.iter().enumerate() {
                let (Some(def), Some(local)) = (a.skeleton.bones.get(b.index()), pose.local.get_mut(b.index())) else { continue };
                let angle = joints.get(j).or_else(|| joints.last()).copied().unwrap_or(0.0) * curl;
                let bent = Quat::from_rotation_x(angle.to_radians());
                // A digit's later joints rest with only their curl; the knuckle keeps its rest
                // placement on the hand.
                let target = if j == 0 { def.rest.rotation * fan * bent } else { bent };
                local.rotation = local.rotation.slerp(target, weight);
            }
        }
    }
}
