//! Tone stamps: muscle bellies, fat pads, bone landmarks and hollows added on top of a part's
//! base shape. Fully deterministic: placement comes from fixed tables, size from [`Tone`].
//!
//! Stamps are placed in part-relative coordinates so the same table works on any body plan:
//! - `Surface { t, around_deg }` on segmented parts (`t` 0 = start, 1 = end; `around_deg`
//!   0 = dorsal/posterior, 90 = left/medial, 180 = ventral/anterior),
//! - `Skull { dir }` on heads (direction from the skull centre; x left, y forward, z down),
//! - `Slot { slot, offset }` at a part's attach slot (eye sockets, nostrils).
//!
//! Sizes and depths are relative to the local body radius, so stamps scale with the part.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::builder::{frame_y_z, mirror_transform};
use super::{
    AnatomyBuilder, AttachSlot, AxisSample, BuildCx, ChainRole, LimbRole, LodLevel, PartKind, PartSlots,
    PrimLayer, PrimOp, PrimShape, Tone,
};

/// Which stamps a part gets.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum StampSet {
    /// The built-in table for the part's kind and role.
    #[default]
    Default,
    None,
    Custom(Vec<Stamp>),
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Stamp {
    pub at: StampAt,
    pub kind: StampKind,
    /// Ellipsoid size relative to the local radius: (width, along the part, along the normal).
    pub size: Vec3,
    /// Protrusion (or, for hollows, indentation) relative to the local radius, before tone.
    pub depth: f32,
    /// Also place a mirrored copy on the other side of the part.
    #[serde(default)]
    pub mirror: bool,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum StampAt {
    Surface { t: f32, around_deg: f32 },
    Skull { dir: Vec3 },
    Slot { slot: AttachSlot, offset: Vec3 },
}

/// What a stamp is; decides which tone value drives it, its softness, layer and LOD.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StampKind {
    /// Driven by `Tone::muscle`. Exists at `Near` and finer.
    Muscle,
    /// Driven by `Tone::fat`. Exists at every level (fat changes silhouettes).
    Fat,
    /// Bony point or ridge; driven by `Tone::landmarks`, damped by fat. `Hero` only.
    Landmark,
    /// Sunk hollow (sockets, flank, temples); like `Landmark`. `Hero` only.
    Hollow,
}

impl StampKind {
    fn intensity(self, tone: &Tone) -> f32 {
        let bony = tone.landmarks * 0.6f32.mul_add(-tone.fat.min(1.0), 1.0);
        match self {
            Self::Muscle => tone.muscle * 1.6,
            Self::Fat => tone.fat * 1.5,
            Self::Landmark | Self::Hollow => bony,
        }
    }

    const fn layer(self) -> PrimLayer {
        match self {
            Self::Muscle => PrimLayer::Muscle,
            Self::Fat => PrimLayer::Fat,
            Self::Landmark | Self::Hollow => PrimLayer::Landmark,
        }
    }

    const fn lod(self) -> LodLevel {
        match self {
            Self::Muscle => LodLevel::Near,
            Self::Fat => LodLevel::Far,
            Self::Landmark | Self::Hollow => LodLevel::Hero,
        }
    }
}

/// A placed stamp: surface point, outward normal, along-part direction, local radius.
struct Placement {
    bone: super::BoneId,
    point: Vec3,
    normal: Vec3,
    along: Vec3,
    radius: f32,
}

fn place(at: StampAt, mirrored: bool, slots: &PartSlots) -> Option<Placement> {
    match at {
        StampAt::Surface { t, around_deg } => {
            let s = slots.sample(t)?;
            let a = (if mirrored { -around_deg } else { around_deg }).to_radians();
            let dorsal = s.model.rotation * Vec3::NEG_Z;
            let side = s.model.rotation * Vec3::X;
            let normal = (dorsal * a.cos() + side * a.sin()).normalize();
            Some(Placement {
                bone: s.bone,
                point: s.model.translation + normal * s.radius_toward(a),
                normal,
                along: s.model.rotation * Vec3::Y,
                radius: s.min_radius(),
            })
        }
        StampAt::Skull { dir } => {
            let sk = slots.skull?;
            let d = Vec3::new(if mirrored { -dir.x } else { dir.x }, dir.y, dir.z).normalize();
            let local = d / (d / sk.radii).length();
            let n_local = (local / (sk.radii * sk.radii)).normalize();
            Some(Placement {
                bone: sk.bone,
                point: sk.model.transform_point(local),
                normal: sk.model.rotation * n_local,
                along: sk.model.rotation * Vec3::Y,
                radius: sk.radii.min_element(),
            })
        }
        StampAt::Slot { slot, offset } => {
            let p = slots.resolve(slot)?;
            let radius = slots.skull.map_or_else(|| slots.axis.first().map_or(0.05, AxisSample::min_radius), |s| s.radii.min_element());
            let mut m = p.model * Transform::from_translation(offset * radius);
            if mirrored {
                m = mirror_transform(m);
            }
            Some(Placement {
                bone: p.bone?,
                point: m.translation,
                normal: m.rotation * Vec3::Y,
                along: m.rotation * Vec3::Z,
                radius,
            })
        }
    }
}

/// Emits a part's stamps. Called after the part's own shapes, before its children.
pub fn apply_stamps(
    b: &mut AnatomyBuilder,
    kind: &PartKind,
    set: &StampSet,
    slots: &PartSlots,
    cx: &BuildCx,
) {
    let defaults;
    let stamps: &[Stamp] = match set {
        StampSet::None => return,
        StampSet::Custom(s) => s,
        StampSet::Default => {
            // A torso standing up (bipeds) has a human back, not a quadruped's haunch and flank.
            let upright = matches!(kind, PartKind::Torso(_)) && slots.sample(0.5).is_some_and(|s| (s.model.rotation * Vec3::Y).y.abs() > 0.7);
            defaults = if upright { upright_torso_stamps() } else { default_stamps(kind) };
            &defaults
        }
    };
    for stamp in stamps {
        for mirrored in [false, true].into_iter().take(if stamp.mirror { 2 } else { 1 }) {
            let Some(p) = place(stamp.at, mirrored, slots) else { continue };
            let push = stamp.depth * p.radius * stamp.kind.intensity(&cx.tone);
            if push < p.radius * 0.005 {
                continue;
            }
            // Fat depots spread as well as deepen.
            let spread = if stamp.kind == StampKind::Fat { 0.5f32.mul_add(cx.tone.fat.min(2.0), 1.0) } else { 1.0 };
            let radii = stamp.size * p.radius * spread;
            let region = b.bone_region(p.bone);
            // A hollow on a slot (eye socket) is a real cut: what sits in the slot lies below the
            // surface and must show through, which a displacement of `push` can't reach.
            if stamp.kind == StampKind::Hollow && matches!(stamp.at, StampAt::Slot { .. }) {
                let centre = p.point + p.normal * (radii.z - push);
                let model = Transform::from_translation(centre).with_rotation(frame_y_z(p.along, p.normal));
                // Softened edges, but never wider than the cut itself: a wide blend round a small cut
                // (a nostril) carves the skin around it into islands.
                let blend = (0.2f32.mul_add(cx.tone.fat.min(1.5), 0.2) * p.radius).min(radii.min_element());
                b.primitive_at(p.bone, PrimShape::Ellipsoid { radii }, model, blend, PrimOp::Subtract, stamp.kind.layer(), region, stamp.kind.lod());
                continue;
            }
            // Muscles/fat/landmarks raise the surface by `push` at the centre; hollows sink it.
            let height = if stamp.kind == StampKind::Hollow { -push } else { push };
            let model = Transform::from_translation(p.point).with_rotation(frame_y_z(p.along, p.normal));
            b.primitive_at(
                p.bone,
                PrimShape::Ellipsoid { radii },
                model,
                0.0,
                PrimOp::Displace { height },
                stamp.kind.layer(),
                region,
                stamp.kind.lod(),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Default tables
// ---------------------------------------------------------------------------------------------

fn surf(t: f32, around_deg: f32, kind: StampKind, size: [f32; 3], depth: f32, mirror: bool) -> Stamp {
    Stamp { at: StampAt::Surface { t, around_deg }, kind, size: Vec3::from(size), depth, mirror }
}

fn skull(dir: [f32; 3], kind: StampKind, size: [f32; 3], depth: f32) -> Stamp {
    Stamp { at: StampAt::Skull { dir: Vec3::from(dir) }, kind, size: Vec3::from(size), depth, mirror: true }
}

/// The built-in stamp table for a part.
#[must_use]
pub fn default_stamps(kind: &PartKind) -> Vec<Stamp> {
    use StampKind as K;
    match kind {
        PartKind::Torso(_) => {
            let mut s = vec![
                surf(0.86, 150.0, K::Muscle, [0.45, 0.55, 0.35], 0.10, true), // chest
                surf(0.90, 110.0, K::Muscle, [0.40, 0.45, 0.35], 0.10, true), // shoulder mass
                surf(0.84, 55.0, K::Landmark, [0.25, 0.45, 0.15], 0.06, true), // shoulder blade
                surf(0.12, 75.0, K::Muscle, [0.50, 0.60, 0.40], 0.12, true),  // haunch
                surf(0.20, 45.0, K::Landmark, [0.15, 0.15, 0.12], 0.06, true), // hip point
                surf(0.60, 110.0, K::Landmark, [0.50, 0.70, 0.30], 0.04, true), // ribcage
                surf(0.35, 105.0, K::Hollow, [0.35, 0.60, 0.30], 0.025, true), // flank hollow
                surf(0.45, 180.0, K::Fat, [0.60, 0.90, 0.50], 0.12, false),   // belly
                surf(0.10, 60.0, K::Fat, [0.50, 0.50, 0.40], 0.06, true),     // rump
                surf(0.55, 100.0, K::Fat, [0.55, 0.90, 0.45], 0.08, true),    // flank
            ];
            // Spine ridge.
            s.extend((0u8..5).map(|i| surf(0.15f32.mul_add(f32::from(i), 0.15), 0.0, K::Landmark, [0.08, 0.18, 0.08], 0.04, false)));
            s
        }
        PartKind::Chain(c) => match c.role {
            ChainRole::Neck => vec![
                surf(0.50, 115.0, K::Muscle, [0.30, 1.60, 0.30], 0.10, true), // side neck muscle
                surf(0.35, 0.0, K::Muscle, [0.40, 1.40, 0.30], 0.08, false),  // nape
                surf(0.60, 180.0, K::Fat, [0.45, 1.20, 0.40], 0.10, false),   // throat / dewlap
            ],
            ChainRole::Tail => vec![surf(0.06, 0.0, K::Muscle, [0.60, 1.00, 0.40], 0.08, false)],
            ChainRole::Trunk => vec![surf(0.3, 180.0, K::Muscle, [0.4, 2.0, 0.3], 0.06, false)],
            _ => vec![],
        },
        PartKind::Limb(l) => {
            if matches!(l.role, LimbRole::Display | LimbRole::Swimmer) {
                return vec![];
            }
            let segments = l.chain.segments.max(1);
            let n = f32::from(segments);
            let seg = |i: f32, f: f32| ((i + f) / n).min(1.0);
            // Limb frame: around 0 = posterior, 180 = anterior, 90 = medial, 270 = lateral.
            let mut s = vec![
                // Flat, and short enough to stay within the limb's taper: a straight ellipsoid running on
                // toward a narrower joint stands proud of it and hangs like a pouch.
                surf(seg(0.0, 0.45), 180.0, K::Muscle, [0.60, 1.80, 0.30], 0.05, false), // front of upper
                surf(seg(0.0, 0.45), 0.0, K::Muscle, [0.60, 1.80, 0.30], 0.04, false),   // back of upper
                surf(seg(0.0, 0.40), 270.0, K::Muscle, [0.50, 1.70, 0.30], 0.035, false), // outer upper
                surf(seg(0.0, 0.25), 300.0, K::Fat, [0.55, 0.90, 0.45], 0.08, false),    // thigh / upper-arm fat
            ];
            if n >= 2.0 {
                s.extend([
                    surf(seg(1.0, 0.30), 0.0, K::Muscle, [0.50, 1.30, 0.30], 0.07, false),   // calf / forearm
                    surf(seg(1.0, 0.35), 180.0, K::Muscle, [0.35, 1.60, 0.30], 0.04, false), // shin
                ]);
            }
            for j in 1..segments.min(3) {
                let t = seg(f32::from(j), 0.0);
                s.push(surf(t, 90.0, K::Landmark, [0.30, 0.40, 0.20], 0.04, false)); // inner condyle
                s.push(surf(t, 270.0, K::Landmark, [0.30, 0.40, 0.20], 0.04, false)); // outer condyle
            }
            s
        }
        PartKind::Head(h) if h.face.is_some() => face_stamps(),
        PartKind::Head(_) => vec![
            skull([0.45, 0.75, -0.55], K::Landmark, [0.50, 0.35, 0.25], 0.10), // brow
            skull([0.85, 0.45, 0.10], K::Landmark, [0.30, 0.50, 0.25], 0.08),  // cheekbone
            skull([0.80, 0.10, 0.55], K::Muscle, [0.35, 0.50, 0.30], 0.10),    // jaw muscle
            skull([0.80, 0.20, -0.45], K::Hollow, [0.35, 0.40, 0.30], 0.06),   // temple
            Stamp {
                at: StampAt::Skull { dir: Vec3::new(0.0, -0.2, -1.0) },
                kind: K::Landmark,
                size: Vec3::new(0.15, 0.70, 0.15),
                depth: 0.05,
                mirror: false,
            }, // crown ridge
            Stamp {
                at: StampAt::Slot { slot: AttachSlot::Eye, offset: Vec3::ZERO },
                kind: K::Hollow,
                size: Vec3::splat(0.45),
                depth: 0.12,
                mirror: true,
            }, // eye socket
            Stamp {
                at: StampAt::Slot { slot: AttachSlot::Nose, offset: Vec3::new(0.25, -0.1, 0.0) },
                kind: K::Hollow,
                size: Vec3::new(0.12, 0.15, 0.12),
                depth: 0.06,
                mirror: true,
            }, // nostril
        ],
        PartKind::Digits(_) | PartKind::Mass(_) | PartKind::Hard(_) | PartKind::Sculpt(_) | PartKind::Wing(_) => vec![],
    }
}

/// Stamps for a head with a face: it carries its own brow; the rest is softer than on a muzzle,
/// the sockets just shade the eyes, and the nostrils open under the nose tip.
fn face_stamps() -> Vec<Stamp> {
    use StampKind as K;
    vec![
        skull([0.85, 0.45, 0.25], K::Landmark, [0.25, 0.40, 0.20], 0.05), // cheekbone
        skull([0.85, 0.05, 0.75], K::Muscle, [0.30, 0.35, 0.30], 0.06),   // jaw muscle
        skull([0.85, 0.30, -0.45], K::Hollow, [0.30, 0.35, 0.30], 0.03),  // temple
        Stamp {
            at: StampAt::Slot { slot: AttachSlot::Eye, offset: Vec3::ZERO },
            kind: K::Hollow,
            // Almond opening: (across, up-down, depth into the head) in skull radii.
            size: Vec3::new(0.16, 0.08, 0.2),
            depth: 0.05,
            mirror: true,
        }, // eye socket
        Stamp {
            at: StampAt::Slot { slot: AttachSlot::Nose, offset: Vec3::new(0.12, -0.1, 0.1) },
            kind: K::Hollow,
            size: Vec3::new(0.06, 0.08, 0.05),
            depth: 0.04,
            mirror: true,
        }, // nostril
    ]
}

/// Stamps for a torso that stands upright.
///
/// Pelvis at `t = 0`, shoulders at `t = 1`; around 0 = back, 90 = left, 180 = belly. A human back and chest instead of a quadruped's haunch, flank
/// and ribcage, which on an upright body read as a band round the waist and a shelf of rump.
#[must_use]
pub fn upright_torso_stamps() -> Vec<Stamp> {
    use StampKind as K;
    vec![
        surf(0.80, 150.0, K::Muscle, [0.70, 0.55, 0.40], 0.07, true), // chest (pectorals)
        surf(0.92, 100.0, K::Muscle, [0.30, 0.30, 0.30], 0.05, true), // top of the shoulder
        surf(0.80, 35.0, K::Landmark, [0.28, 0.40, 0.15], 0.03, true), // shoulder blade
        surf(0.50, 15.0, K::Muscle, [0.16, 1.60, 0.20], 0.05, true),  // the ridges either side of the spine
        surf(0.10, 30.0, K::Muscle, [0.65, 0.65, 0.50], 0.08, true),  // buttock
        surf(0.40, 180.0, K::Fat, [0.55, 0.70, 0.45], 0.08, false),   // belly
        surf(0.30, 95.0, K::Fat, [0.40, 0.60, 0.35], 0.04, true),     // hip / love handle
    ]
}
