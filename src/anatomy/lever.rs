//! Levers: the stable, named knobs a [`Form`](super::Form) can turn on a part.
//!
//! Levers decouple saved forms (and future genetics) from struct field names. Each lever has a
//! fixed [`LeverKind`] so a patch value always means the same thing, and each part kind declares
//! the levers it supports.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{AttachSlot, ChainParams, CrossSection, HardKind, LimbEnd, PartKind, PartNode};
use crate::num::Saturate;

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lever {
    // --- Placement (every part; acts on the node's offset/attach) ---------------------------
    /// Whole part, uniformly (length and girth together).
    Scale,
    /// Rotation about local X, degrees.
    Pitch,
    /// Rotation about local Y, degrees.
    Yaw,
    /// Rotation about local Z, degrees.
    Roll,
    /// Shift along the parent for `Along`/`Surface` attachments, in `t` units.
    Position,
    /// Angle around the parent for `Surface` attachments, degrees.
    Around,

    // --- Surface character (every SDF part; multiplies the body Tone locally) ----------------
    Muscle,
    Fat,
    Folds,
    /// Joint softness where the part meets its parent.
    Blend,

    // --- Chains (Torso, Chain, Limb, Digits) -------------------------------------------------
    Length,
    /// Radius.
    Girth,
    /// End radius relative to start radius.
    Taper,
    /// Rest bend.
    Curl,
    /// Cross-section aspect (width / height).
    Flatten,
    /// Bones in the chain. Topology-changing.
    Segments,

    // --- Torso ---------------------------------------------------------------------------------
    Arch,
    Belly,

    // --- Head ----------------------------------------------------------------------------------
    Skull,
    Snout,
    Jaw,
    EyeSpacing,
    EyeLateral,

    // --- Limb ----------------------------------------------------------------------------------
    /// Size of the hand/foot/hoof/pincer/blade/paddle.
    EndSize,
    Splay,

    // --- Digits ------------------------------------------------------------------------------
    /// Number of digits. Topology-changing.
    Count,
    Spread,
    Webbing,

    // --- Mass (and chains: `Width` and `Depth` scale the whole section) -----------------------
    /// Mass: size along X. Chains: section width, a multiplier on the profile's `width` curve.
    Width,
    Height,
    /// Mass: size along Z. Chains: section depth, a multiplier on the profile's `depth` curve.
    Depth,

    // --- Hard ----------------------------------------------------------------------------------
    /// Hard-part size (claw, horn, tooth...).
    Size,
    Curve,
    Twist,
    /// Antler tines. Topology-changing (hard-part mesh only, not the skeleton).
    Tines,
}

/// How a patch value is interpreted.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeverKind {
    /// Multiplies the authored value. `1.0` = unchanged.
    Factor,
    /// Added to the authored value (degrees for angles, unitless otherwise). `0.0` = unchanged.
    Delta,
    /// Replaces an integer; the value is rounded and clamped to the valid range.
    Count,
}

impl Lever {
    #[must_use]
    pub const fn kind(self) -> LeverKind {
        match self {
            Self::Pitch | Self::Yaw | Self::Roll | Self::Position | Self::Around | Self::Curl | Self::Belly | Self::EyeLateral | Self::Splay
            | Self::Webbing | Self::Curve | Self::Twist => LeverKind::Delta,
            Self::Segments | Self::Count | Self::Tines => LeverKind::Count,
            _ => LeverKind::Factor,
        }
    }

    /// The values this lever may take, in its [`kind`](Self::kind)'s terms: a factor's
    /// multiplier, a delta's change, a count's value. Every patch must lie within it, and so must
    /// all the patches on one part combined ([`apply_form`](super::apply_form) checks both).
    ///
    /// Factors stay positive, so a part can't be turned inside out or shrunk to nothing; angles
    /// stay under a quarter or so of a turn, so a leg can't be swung onto the back.
    #[must_use]
    pub const fn range(self) -> (f32, f32) {
        match self {
            // Sizes: a quarter to four times the authored.
            Self::Scale | Self::Length | Self::Girth | Self::Skull | Self::Jaw | Self::EndSize | Self::Width | Self::Height | Self::Depth
            | Self::Size => (0.25, 4.0),
            // A tip far finer than its base; a blend kept from vanishing.
            Self::Taper | Self::Blend => (0.1, 4.0),
            Self::Flatten => (0.2, 5.0),
            Self::EyeSpacing => (0.25, 3.0),
            // Amounts that may go to nothing: no fat, no snout.
            Self::Muscle | Self::Fat | Self::Folds | Self::Snout => (0.0, 4.0),
            // A flat back, digits closed up.
            Self::Arch | Self::Spread => (0.0, 3.0),
            // Degrees.
            Self::Pitch | Self::Yaw | Self::Roll => (-90.0, 90.0),
            Self::Around | Self::Splay => (-60.0, 60.0),
            // Degrees per metre.
            Self::Curl => (-360.0, 360.0),
            // `t` along the parent, and changes to `0..=1` amounts (each result is clamped to
            // `0..=1`).
            Self::Position | Self::Belly | Self::EyeLateral | Self::Webbing | Self::Curve => (-1.0, 1.0),
            // Turns.
            Self::Twist => (-2.0, 2.0),
            Self::Segments | Self::Tines => (1.0, 8.0),
            Self::Count => (1.0, 6.0),
        }
    }

    /// Two patches of this lever on one part, combined as [`apply_lever`](PartNode::apply_lever)
    /// combines them: factors multiply, deltas add, a count replaces.
    #[must_use]
    pub fn combine(self, before: f32, value: f32) -> f32 {
        match self.kind() {
            LeverKind::Factor => before * value,
            LeverKind::Delta => before + value,
            LeverKind::Count => value,
        }
    }

    /// Value meaning "no change" for this lever's kind (not meaningful for `Count`).
    #[must_use]
    pub const fn identity(self) -> f32 {
        match self.kind() {
            LeverKind::Factor => 1.0,
            LeverKind::Delta | LeverKind::Count => 0.0,
        }
    }
}

const PLACEMENT: &[Lever] = &[
    Lever::Scale,
    Lever::Pitch,
    Lever::Yaw,
    Lever::Roll,
    Lever::Position,
    Lever::Around,
];
const SURFACE: &[Lever] = &[Lever::Muscle, Lever::Fat, Lever::Folds, Lever::Blend];
const CHAIN: &[Lever] = &[
    Lever::Length,
    Lever::Girth,
    Lever::Taper,
    Lever::Curl,
    Lever::Flatten,
    Lever::Width,
    Lever::Depth,
    Lever::Segments,
];

impl PartKind {
    /// Levers specific to this kind (placement levers are always available; see
    /// [`PartNode::supports`]).
    #[must_use]
    pub fn levers(&self) -> Vec<Lever> {
        use Lever as L;
        let own: &[&[Lever]] = match self {
            Self::Torso(_) => &[SURFACE, CHAIN, &[L::Arch, L::Belly]],
            Self::Chain(_) => &[SURFACE, CHAIN],
            Self::Head(_) => &[SURFACE, &[L::Skull, L::Snout, L::Jaw, L::EyeSpacing, L::EyeLateral]],
            Self::Limb(_) => &[SURFACE, CHAIN, &[L::EndSize, L::Splay]],
            Self::Digits(_) => &[SURFACE, CHAIN, &[L::Count, L::Spread, L::Webbing]],
            Self::Mass(_) => &[SURFACE, &[L::Width, L::Height, L::Depth]],
            Self::Hard(p) => match p.kind {
                super::HardKind::Antler { .. } => &[&[L::Size, L::Curve, L::Twist, L::Tines]],
                _ => &[&[L::Size, L::Curve, L::Twist]],
            },
            Self::Sculpt(_) => &[&[L::Blend]],
            Self::Wing(_) => &[&[L::Size]],
        };
        own.iter().flat_map(|s| s.iter().copied()).collect()
    }
}

impl PartNode {
    #[must_use]
    pub fn supports(&self, lever: Lever) -> bool {
        PLACEMENT.contains(&lever) || self.kind.levers().contains(&lever)
    }

    /// Applies one patch value according to [`Lever::kind`].
    ///
    /// # Errors
    ///
    /// When this part doesn't support `lever`, or `value` isn't finite.
    pub fn apply_lever(&mut self, lever: Lever, value: f32) -> Result<(), LeverError> {
        use Lever as L;
        if !self.supports(lever) {
            return Err(LeverError::Unsupported(lever));
        }
        let (lo, hi) = lever.range();
        if !(lo..=hi).contains(&value) {
            return Err(LeverError::OutOfRange { lever, value, lo, hi });
        }
        let f = value; // Factor or Delta, depending on the lever
        let count = |min: u8, max: u8| value.round().clamp(f32::from(min), f32::from(max)).sat_u8();
        let unsupported = LeverError::Unsupported(lever);

        match lever {
            L::Scale => scale_kind(&mut self.kind, f),
            L::Pitch => self.offset.rotate_deg.x += f,
            L::Yaw => self.offset.rotate_deg.y += f,
            L::Roll => self.offset.rotate_deg.z += f,
            L::Position => match &mut self.attach {
                AttachSlot::Along(t) | AttachSlot::Surface { t, .. } => *t = (*t + f).clamp(0.0, 1.0),
                _ => return Err(unsupported),
            },
            L::Around => match &mut self.attach {
                AttachSlot::Surface { around_deg, .. } => *around_deg += f,
                _ => return Err(unsupported),
            },
            L::Muscle => self.tone.muscle *= f,
            L::Fat => self.tone.fat *= f,
            L::Folds => self.tone.folds *= f,
            L::Blend => match &mut self.kind {
                PartKind::Mass(m) => m.blend *= f,
                PartKind::Sculpt(s) => s.blend *= f,
                PartKind::Head(h) => h.skull.blend *= f,
                k => chain_mut(k).ok_or(unsupported)?.blend *= f,
            },
            L::Length => chain_mut(&mut self.kind).ok_or(unsupported)?.length *= f,
            L::Girth => chain_mut(&mut self.kind).ok_or(unsupported)?.radius.scale *= f,
            L::Taper => {
                let c = chain_mut(&mut self.kind).ok_or(unsupported)?;
                for (t, v) in &mut c.radius.curve {
                    *v *= (f - 1.0).mul_add(*t, 1.0);
                }
            }
            L::Curl => chain_mut(&mut self.kind).ok_or(unsupported)?.curl += f,
            L::Flatten => {
                let c = chain_mut(&mut self.kind).ok_or(unsupported)?;
                c.radius.cross = match c.radius.cross {
                    CrossSection::Round => CrossSection::Ellipse { aspect: f },
                    CrossSection::Ellipse { aspect } => CrossSection::Ellipse { aspect: aspect * f },
                    CrossSection::Flat { aspect } => CrossSection::Flat { aspect: aspect * f },
                    CrossSection::Teardrop { aspect } => CrossSection::Teardrop { aspect: aspect * f },
                };
            }
            L::Segments => chain_mut(&mut self.kind).ok_or(unsupported)?.segments = count(1, 8),
            _ => apply_kind_lever(&mut self.kind, lever, value)?,
        }
        Ok(())
    }
}

/// The levers specific to one part kind (see [`PartKind::levers`]).
fn apply_kind_lever(kind: &mut PartKind, lever: Lever, value: f32) -> Result<(), LeverError> {
    use Lever as L;
    let f = value;
    let count = |min: u8, max: u8| value.round().clamp(f32::from(min), f32::from(max)).sat_u8();
    let unsupported = LeverError::Unsupported(lever);
    match lever {
        L::Arch | L::Belly => {
            let PartKind::Torso(t) = kind else { return Err(unsupported) };
            if lever == L::Arch {
                t.arch.iter_mut().for_each(|(_, h)| *h *= f);
            } else {
                t.belly = (t.belly + f).clamp(0.0, 1.0);
            }
        }
        L::Skull | L::Snout | L::Jaw | L::EyeSpacing | L::EyeLateral => {
            let PartKind::Head(h) = kind else { return Err(unsupported) };
            match lever {
                L::Skull => h.skull.size *= f,
                L::Snout => h.snout *= f,
                L::Jaw => h.jaw.as_mut().ok_or(unsupported)?.length *= f,
                L::EyeSpacing => h.eye_spacing *= f,
                _ => h.eye_lateral = (h.eye_lateral + f).clamp(0.0, 1.0),
            }
        }
        L::EndSize | L::Splay => {
            let PartKind::Limb(l) = kind else { return Err(unsupported) };
            if lever == L::Splay {
                l.splay_deg += f;
            } else {
                scale_end(&mut l.end, f);
            }
        }
        L::Count | L::Spread | L::Webbing => {
            let PartKind::Digits(d) = kind else { return Err(unsupported) };
            match lever {
                L::Count => d.count = count(1, 6),
                L::Spread => d.spread_deg *= f,
                _ => d.webbing = (d.webbing + f).clamp(0.0, 1.0),
            }
        }
        L::Width | L::Depth if chain_mut(kind).is_some() => {
            let c = chain_mut(kind).ok_or(unsupported)?;
            let curve = if lever == L::Width { &mut c.radius.width } else { &mut c.radius.depth };
            if curve.is_empty() {
                curve.push((0.0, 1.0));
            }
            for (_, v) in curve.iter_mut() {
                *v *= f;
            }
        }
        L::Width | L::Height | L::Depth => {
            let PartKind::Mass(m) = kind else { return Err(unsupported) };
            match lever {
                L::Width => m.size.x *= f,
                L::Height => m.size.y *= f,
                _ => m.size.z *= f,
            }
        }
        L::Size | L::Curve | L::Twist | L::Tines => {
            let PartKind::Hard(h) = kind else { return Err(unsupported) };
            match lever {
                L::Size => h.size *= f,
                L::Curve => h.curve += f,
                L::Twist => h.twist += f,
                _ => match &mut h.kind {
                    HardKind::Antler { tines } => *tines = count(1, 8),
                    _ => return Err(unsupported),
                },
            }
        }
        _ => return Err(unsupported),
    }
    Ok(())
}

/// The chain inside chain-based part kinds.
const fn chain_mut(kind: &mut PartKind) -> Option<&mut ChainParams> {
    match kind {
        PartKind::Torso(t) => Some(&mut t.chain),
        PartKind::Chain(c) => Some(&mut c.chain),
        PartKind::Limb(l) => Some(&mut l.chain),
        PartKind::Digits(d) => Some(&mut d.chain),
        _ => None,
    }
}

/// Uniformly scales every size of a part (not its children, which keep their own sizes).
fn scale_kind(kind: &mut PartKind, f: f32) {
    if let Some(c) = chain_mut(kind) {
        c.length *= f;
        c.radius.scale *= f;
    }
    match kind {
        PartKind::Torso(t) => t.arch.iter_mut().for_each(|(_, h)| *h *= f),
        PartKind::Head(h) => {
            h.skull.size *= f;
            h.snout *= f;
            if let Some(j) = &mut h.jaw {
                j.length *= f;
                j.radius.scale *= f;
            }
        }
        PartKind::Limb(l) => scale_end(&mut l.end, f),
        PartKind::Mass(m) => m.size *= f,
        PartKind::Hard(h) => match &mut h.kind {
            HardKind::Mesh { scale, .. } => *scale *= f,
            _ => h.size *= f,
        },
        PartKind::Sculpt(s) => s.scale *= f,
        PartKind::Wing(w) => {
            w.span *= f;
            w.chord *= f;
            w.radius *= f;
        }
        PartKind::Chain(_) | PartKind::Digits(_) => {}
    }
}

fn scale_end(end: &mut LimbEnd, f: f32) {
    match end {
        LimbEnd::Hand { palm: s }
        | LimbEnd::Foot { length: s }
        | LimbEnd::Pincer { size: s }
        | LimbEnd::Blade { length: s }
        | LimbEnd::Paddle { width: s } => *s *= f,
        LimbEnd::None | LimbEnd::Hoof { .. } | LimbEnd::Pad | LimbEnd::Point => {}
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LeverError {
    #[error("lever {0:?} is not supported by this part kind")]
    Unsupported(Lever),
    #[error("lever {lever:?} value {value} is outside {lo}..={hi}")]
    OutOfRange { lever: Lever, value: f32, lo: f32, hi: f32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{HardParams, LimbParams};

    #[test]
    fn lever_kinds() {
        assert_eq!(Lever::Length.kind(), LeverKind::Factor);
        assert_eq!(Lever::Pitch.kind(), LeverKind::Delta);
        assert_eq!(Lever::Segments.kind(), LeverKind::Count);
    }

    #[test]
    fn part_lever_sets() {
        let limb = PartKind::Limb(LimbParams::default()).levers();
        assert!(limb.contains(&Lever::Length) && limb.contains(&Lever::Splay));
        assert!(!limb.contains(&Lever::Snout));
        let claw = PartKind::Hard(HardParams::default()).levers();
        assert!(!claw.contains(&Lever::Tines) && !claw.contains(&Lever::Muscle));
    }

    #[test]
    fn width_and_depth_scale_chain_sections() {
        let node = |kind: &str| ron::from_str::<PartNode>(&format!("(name: \"p\", kind: {kind}(()))")).unwrap();
        let mut torso = node("Torso");
        torso.apply_lever(Lever::Width, 1.5).unwrap();
        torso.apply_lever(Lever::Depth, 0.5).unwrap();
        torso.apply_lever(Lever::Depth, 2.0).unwrap();
        let PartKind::Torso(t) = &torso.kind else { panic!() };
        assert_eq!(t.chain.radius.width, vec![(0.0, 1.5)]);
        assert_eq!(t.chain.radius.depth, vec![(0.0, 1.0)]);
        assert!(torso.apply_lever(Lever::Height, 2.0).is_err());
        // Masses keep their own meaning.
        let mut mass = node("Mass");
        let before = match &mass.kind { PartKind::Mass(m) => m.size, _ => panic!() };
        mass.apply_lever(Lever::Width, 2.0).unwrap();
        let PartKind::Mass(m) = &mass.kind else { panic!() };
        assert_eq!(m.size.x, before.x * 2.0);
    }
}
