//! Levers: the stable, named knobs a [`Form`](super::Form) can turn on a part.
//!
//! Levers decouple saved forms (and future genetics) from struct field names. Each lever has a
//! fixed [`LeverKind`] so a patch value always means the same thing, and each part kind declares
//! the levers it supports.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{AttachSlot, ChainParams, CrossSection, HardKind, LimbEnd, PartKind, PartNode};

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

    // --- Mass ----------------------------------------------------------------------------------
    Width,
    Height,
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
    pub fn kind(self) -> LeverKind {
        use Lever::*;
        match self {
            Pitch | Yaw | Roll | Position | Around | Curl | Belly | EyeLateral | Splay
            | Webbing | Curve | Twist => LeverKind::Delta,
            Segments | Count | Tines => LeverKind::Count,
            _ => LeverKind::Factor,
        }
    }

    /// Changes bone count or hard-part topology. Such mobs can't share a skeleton (and
    /// therefore baked motion) with unpatched mobs of the same body plan.
    pub fn changes_topology(self) -> bool {
        self.kind() == LeverKind::Count
    }

    /// Value meaning "no change" for this lever's kind (not meaningful for `Count`).
    pub fn identity(self) -> f32 {
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
    Lever::Segments,
];

impl PartKind {
    /// Levers specific to this kind (placement levers are always available; see
    /// [`PartNode::supports`]).
    pub fn levers(&self) -> Vec<Lever> {
        use Lever::*;
        let own: &[&[Lever]] = match self {
            PartKind::Torso(_) => &[SURFACE, CHAIN, &[Arch, Belly]],
            PartKind::Chain(_) => &[SURFACE, CHAIN],
            PartKind::Head(_) => &[SURFACE, &[Skull, Snout, Jaw, EyeSpacing, EyeLateral]],
            PartKind::Limb(_) => &[SURFACE, CHAIN, &[EndSize, Splay]],
            PartKind::Digits(_) => &[SURFACE, CHAIN, &[Count, Spread, Webbing]],
            PartKind::Mass(_) => &[SURFACE, &[Width, Height, Depth]],
            PartKind::Hard(p) => match p.kind {
                super::HardKind::Antler { .. } => &[&[Size, Curve, Twist, Tines]],
                _ => &[&[Size, Curve, Twist]],
            },
            PartKind::Sculpt(_) => &[&[Blend]],
            PartKind::Wing(_) => &[&[Size]],
        };
        own.iter().flat_map(|s| s.iter().copied()).collect()
    }
}

impl PartNode {
    pub fn supports(&self, lever: Lever) -> bool {
        PLACEMENT.contains(&lever) || self.kind.levers().contains(&lever)
    }

    /// Applies one patch value according to [`Lever::kind`].
    pub fn apply_lever(&mut self, lever: Lever, value: f32) -> Result<(), LeverError> {
        use Lever::*;
        if !self.supports(lever) {
            return Err(LeverError::Unsupported(lever));
        }
        if !value.is_finite() {
            return Err(LeverError::OutOfRange { lever, value });
        }
        let f = value; // Factor or Delta, depending on the lever
        let count = |min: u8, max: u8| (value.round() as i32).clamp(min as i32, max as i32) as u8;
        let unsupported = LeverError::Unsupported(lever);

        match lever {
            Scale => scale_kind(&mut self.kind, f),
            Pitch => self.offset.rotate_deg.x += f,
            Yaw => self.offset.rotate_deg.y += f,
            Roll => self.offset.rotate_deg.z += f,
            Position => match &mut self.attach {
                AttachSlot::Along(t) | AttachSlot::Surface { t, .. } => *t = (*t + f).clamp(0.0, 1.0),
                _ => return Err(unsupported),
            },
            Around => match &mut self.attach {
                AttachSlot::Surface { around_deg, .. } => *around_deg += f,
                _ => return Err(unsupported),
            },
            Muscle => self.tone.muscle *= f,
            Fat => self.tone.fat *= f,
            Folds => self.tone.folds *= f,
            Blend => match &mut self.kind {
                PartKind::Mass(m) => m.blend *= f,
                PartKind::Sculpt(s) => s.blend *= f,
                PartKind::Head(h) => h.skull.blend *= f,
                k => chain_mut(k).ok_or(unsupported)?.blend *= f,
            },
            Length => chain_mut(&mut self.kind).ok_or(unsupported)?.length *= f,
            Girth => chain_mut(&mut self.kind).ok_or(unsupported)?.radius.scale *= f,
            Taper => {
                let c = chain_mut(&mut self.kind).ok_or(unsupported)?;
                for (t, v) in &mut c.radius.curve {
                    *v *= 1.0 + (f - 1.0) * *t;
                }
            }
            Curl => chain_mut(&mut self.kind).ok_or(unsupported)?.curl += f,
            Flatten => {
                let c = chain_mut(&mut self.kind).ok_or(unsupported)?;
                c.radius.cross = match c.radius.cross {
                    CrossSection::Round => CrossSection::Ellipse { aspect: f },
                    CrossSection::Ellipse { aspect } => CrossSection::Ellipse { aspect: aspect * f },
                    CrossSection::Flat { aspect } => CrossSection::Flat { aspect: aspect * f },
                    CrossSection::Teardrop { aspect } => CrossSection::Teardrop { aspect: aspect * f },
                };
            }
            Segments => chain_mut(&mut self.kind).ok_or(unsupported)?.segments = count(1, 8),
            Arch | Belly => {
                let PartKind::Torso(t) = &mut self.kind else { return Err(unsupported) };
                if lever == Arch {
                    t.arch.iter_mut().for_each(|(_, h)| *h *= f);
                } else {
                    t.belly = (t.belly + f).clamp(0.0, 1.0);
                }
            }
            Skull | Snout | Jaw | EyeSpacing | EyeLateral => {
                let PartKind::Head(h) = &mut self.kind else { return Err(unsupported) };
                match lever {
                    Skull => h.skull.size *= f,
                    Snout => h.snout *= f,
                    Jaw => {
                        if let Some(j) = &mut h.jaw {
                            j.length *= f;
                        }
                    }
                    EyeSpacing => h.eye_spacing *= f,
                    _ => h.eye_lateral = (h.eye_lateral + f).clamp(0.0, 1.0),
                }
            }
            EndSize | Splay => {
                let PartKind::Limb(l) = &mut self.kind else { return Err(unsupported) };
                if lever == Splay {
                    l.splay_deg += f;
                } else {
                    scale_end(&mut l.end, f);
                }
            }
            Count | Spread | Webbing => {
                let PartKind::Digits(d) = &mut self.kind else { return Err(unsupported) };
                match lever {
                    Count => d.count = count(1, 6),
                    Spread => d.spread_deg *= f,
                    _ => d.webbing = (d.webbing + f).clamp(0.0, 1.0),
                }
            }
            Width | Height | Depth => {
                let PartKind::Mass(m) = &mut self.kind else { return Err(unsupported) };
                match lever {
                    Width => m.size.x *= f,
                    Height => m.size.y *= f,
                    _ => m.size.z *= f,
                }
            }
            Size | Curve | Twist | Tines => {
                let PartKind::Hard(h) = &mut self.kind else { return Err(unsupported) };
                match lever {
                    Size => h.size *= f,
                    Curve => h.curve += f,
                    Twist => h.twist += f,
                    _ => match &mut h.kind {
                        HardKind::Antler { tines } => *tines = count(1, 8),
                        _ => return Err(unsupported),
                    },
                }
            }
        }
        Ok(())
    }
}

/// The chain inside chain-based part kinds.
fn chain_mut(kind: &mut PartKind) -> Option<&mut ChainParams> {
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
    #[error("lever {lever:?} value {value} is out of range")]
    OutOfRange { lever: Lever, value: f32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{HardParams, LimbParams};

    #[test]
    fn lever_kinds() {
        assert_eq!(Lever::Length.kind(), LeverKind::Factor);
        assert_eq!(Lever::Pitch.kind(), LeverKind::Delta);
        assert!(Lever::Segments.changes_topology());
        assert!(!Lever::Girth.changes_topology());
    }

    #[test]
    fn part_lever_sets() {
        let limb = PartKind::Limb(LimbParams::default()).levers();
        assert!(limb.contains(&Lever::Length) && limb.contains(&Lever::Splay));
        assert!(!limb.contains(&Lever::Snout));
        let claw = PartKind::Hard(HardParams::default()).levers();
        assert!(!claw.contains(&Lever::Tines) && !claw.contains(&Lever::Muscle));
    }
}
