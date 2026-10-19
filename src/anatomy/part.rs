//! Part kinds and their parameters. All values are plain numbers; per-mob variation comes from
//! [`Form`](super::Form) lever patches supplied by the game.
//!
//! Enum variants here are part of the saved data format: adding variants later is backward
//! compatible, renaming or removing them is not. Reserved for later: `Wing`, `Fin`, `Shell`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Orient, RegionKind, WingParams};

/// Closed set of anatomical part kinds. Each variant's logic lives in its own
/// [`PartBuild`](super::PartBuild) impl; supported levers are listed by
/// [`PartKind::levers`].
#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
pub enum PartKind {
    /// Segmented trunk; also the whole body of legless mobs.
    /// Slots: `Start` (rear), `End` (front), `Along`, `Surface`.
    Torso(TorsoParams),
    /// Necks, tails, ears, antennae, tentacles, trunks, tongues, whiskers.
    /// Slots: `Start`, `End`, `Along`, `Surface`.
    Chain(ChainPart),
    /// Skull + optional jaw.
    /// Slots: `End`, `Surface`, `Jaw`, `Eye`, `Ear`, `Horn`, `Mouth`, `Nose`, `Crown`.
    Head(HeadParams),
    /// 2–4 segments + end. Slots: `Start`, `End`, `Along`, `Surface`.
    Limb(LimbParams),
    /// A group of fingers/toes fanned across the parent's end. Slots: `End` (middle digit tip).
    Digits(DigitParams),
    /// Free blob on the parent bone: humps, crests, bellies, cheeks, dewlaps. Slots: `End`.
    Mass(MassParams),
    /// Separate non-SDF mesh on the parent bone: claws, horns, teeth, eyes. No slots.
    Hard(HardParams),
    /// An authored mesh blended into the body as an SDF (feet, snouts, mouths, ears).
    /// Slots: `Start`, `End`, `Along`, `Surface` (along the part's local +Y).
    Sculpt(SculptParams),
    /// Bat, dragon and pterosaur membranes, bird wings, insect wings. Slots: `End` (wing tip).
    Wing(WingParams),
}

/// An authored soft part. The mesh must be watertight. See [`Orient`] for the authoring convention.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SculptParams {
    /// Asset path of a glTF mesh primitive, e.g. `"parts/feet.gltf#Mesh0/Primitive0"`.
    pub mesh: String,
    pub scale: f32,
    /// Smooth-min radius against the body it attaches to.
    pub blend: f32,
    /// SDF cells along the mesh's longest axis.
    pub resolution: u16,
    pub orient: Orient,
    pub region: RegionKind,
    /// Give the part its own bone (it can then be posed); otherwise it rides on the parent bone.
    pub own_bone: bool,
}

impl Default for SculptParams {
    fn default() -> Self {
        Self {
            mesh: String::new(),
            scale: 1.0,
            blend: 0.02,
            resolution: 40,
            orient: Orient::Model,
            region: RegionKind::Mass,
            own_bone: true,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared chain description
// ---------------------------------------------------------------------------------------------

/// A chain of segments shared by torsos, chains, limbs, digits and jaws.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ChainParams {
    /// One bone per segment. Changing it changes the skeleton.
    pub segments: u8,
    /// Total rest length in metres.
    pub length: f32,
    pub radius: Profile,
    /// Rest curvature in degrees per metre (positive bends toward local +Z).
    pub curl: f32,
    /// Smooth-min blend radius where the chain meets its parent.
    pub blend: f32,
    /// Relative segment lengths, root first (`[1.0, 1.0, 0.35]` = a short third segment). Empty
    /// or mismatched = equal segments. Scaled so the segments sum to `length`.
    pub ratios: Vec<f32>,
}

impl Default for ChainParams {
    fn default() -> Self {
        Self {
            segments: 3,
            length: 0.5,
            radius: Profile::default(),
            curl: 0.0,
            blend: 0.05,
            ratios: Vec::new(),
        }
    }
}

impl ChainParams {
    /// Length of each segment, root first; they sum to `length`.
    pub fn segment_lengths(&self) -> Vec<f32> {
        let n = self.segments.max(1) as usize;
        let total = self.length.max(0.005);
        let ratios: Vec<f32> = if self.ratios.len() == n && self.ratios.iter().all(|r| *r > 0.0) {
            self.ratios.clone()
        } else {
            vec![1.0; n]
        };
        let sum: f32 = ratios.iter().sum();
        ratios.iter().map(|r| total * r / sum).collect()
    }
}

/// Radius along a chain: `curve` maps `t ∈ 0..=1` to a relative radius, multiplied by `scale`.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Profile {
    pub curve: Vec<(f32, f32)>,
    /// Base radius in metres.
    pub scale: f32,
    pub cross: CrossSection,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            curve: vec![(0.0, 1.0), (1.0, 0.7)],
            scale: 0.08,
            cross: CrossSection::Round,
        }
    }
}

/// Shape of a chain's cross-section. `aspect` = width / height.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum CrossSection {
    #[default]
    Round,
    Ellipse {
        aspect: f32,
    },
    /// Strongly flattened: paddles, beaver tails, ear flaps.
    Flat {
        aspect: f32,
    },
    /// Rounded top, narrow keel below: bird chests, fish bodies, dorsal ridges.
    Teardrop {
        aspect: f32,
    },
}

// ---------------------------------------------------------------------------------------------
// Torso
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct TorsoParams {
    pub chain: ChainParams,
    /// Vertical bend along the torso, `t → offset` in metres.
    pub arch: Vec<(f32, f32)>,
    /// Belly sag/inflation, `0.0..=1.0`.
    pub belly: f32,
}

// ---------------------------------------------------------------------------------------------
// Chain
// ---------------------------------------------------------------------------------------------

/// A non-limb chain with an anatomical role.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ChainPart {
    pub role: ChainRole,
    pub chain: ChainParams,
}

/// Decides regions and motion: `Neck` drives look-at, `Tongue` is hidden in the mouth at rest,
/// everything else gets spring secondary motion.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChainRole {
    Neck,
    #[default]
    Tail,
    Ear,
    Antenna,
    Tentacle,
    /// Elephant trunk, tapir snout.
    Trunk,
    Tongue,
    /// Thin sensory hairs; hero detail only by default.
    Whisker,
}

// ---------------------------------------------------------------------------------------------
// Head
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct HeadParams {
    pub skull: MassParams,
    /// Snout protrusion in metres.
    pub snout: f32,
    pub jaw: Option<ChainParams>,
    /// Eye separation as a fraction of skull width.
    pub eye_spacing: f32,
    /// Eye placement: `0.0` = forward-facing (predator), `1.0` = side-facing (prey).
    pub eye_lateral: f32,
}

// ---------------------------------------------------------------------------------------------
// Limb
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct LimbParams {
    pub chain: ChainParams,
    pub role: LimbRole,
    pub stance: Stance,
    pub end: LimbEnd,
    /// Outward angle from the body in degrees (0 = straight down).
    pub splay_deg: f32,
    /// Rest bend at each joint, root first: `(forward, up)` degrees. Positive forward swings the
    /// next segment toward the front. Empty = the stance's default. Front and hind legs of real
    /// quadrupeds bend oppositely (elbow back, knee forward), so set these per limb.
    pub bends: Vec<(f32, f32)>,
    /// Manipulator arms: a clavicle from inside the chest to the shoulder, metres, so raised arms
    /// lift the shoulder instead of folding the skin there. `None` = automatic (0.45 × the upper
    /// segment for manipulators, none otherwise); `Some(0.0)` = none.
    pub clavicle: Option<f32>,
}

/// What a limb is used for. `Locomotor`/`Both` limbs become [`LegInfo`](super::LegInfo)s;
/// `Manipulator`/`Both` limbs with a grasping end get a `Grip` socket.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LimbRole {
    #[default]
    Locomotor,
    /// Arms.
    Manipulator,
    /// Knuckle-walkers, raccoon hands: walks and grips.
    Both,
    /// Flippers and paddles: locomotion in water, unused on land.
    Swimmer,
    /// Non-functional: vestigial or display limbs.
    Display,
}

/// How a locomotor limb meets the ground; shapes the joint chain and foot IK.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Stance {
    /// Whole foot down (bears, humans).
    #[default]
    Plantigrade,
    /// On the toes (dogs, cats, birds).
    Digitigrade,
    /// On the tips/hooves (horses, deer).
    Unguligrade,
    /// Out to the side, elbows/knees lateral (lizards, salamanders).
    Sprawl,
    /// Knee above the body, foot far out (spiders, crabs, insects).
    Arched,
}

/// The business end of a limb.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum LimbEnd {
    #[default]
    None,
    /// Grasping hand; add `Digits` children for fingers. `palm` = width in metres.
    Hand { palm: f32 },
    /// Flat foot; add `Digits` children for toes. `length` in metres.
    Foot { length: f32 },
    /// `split` = cloven (two-toed) hoof.
    Hoof { split: bool },
    /// Soft pad (cats, dogs); add `Hard(Claw)` children for claws.
    Pad,
    /// Crab/scorpion claw; grasps. `size` in metres.
    Pincer { size: f32 },
    /// Mantis-style blade. `length` in metres.
    Blade { length: f32 },
    /// Tapered tip (insect tarsus, spider leg).
    Point,
    /// Webbed paddle (ducks, frogs, seals). `width` in metres.
    Paddle { width: f32 },
}

impl LimbEnd {
    /// Ends that can hold tools.
    pub fn grasps(self) -> bool {
        matches!(self, LimbEnd::Hand { .. } | LimbEnd::Pincer { .. })
    }
}

// ---------------------------------------------------------------------------------------------
// Digits
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct DigitParams {
    /// Changing it changes the skeleton.
    pub count: u8,
    pub chain: ChainParams,
    /// Total fan angle across all digits.
    pub spread_deg: f32,
    /// First digit opposes the others (thumbs).
    pub opposable: bool,
    /// Webbing between digits, `0.0..=1.0`.
    pub webbing: f32,
    /// Hard part grown on every digit tip (claws, nails, talons).
    pub tip: Option<HardParams>,
}

impl Default for DigitParams {
    fn default() -> Self {
        Self {
            count: 4,
            chain: ChainParams {
                segments: 2,
                length: 0.06,
                ..default()
            },
            spread_deg: 40.0,
            opposable: false,
            webbing: 0.0,
            tip: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Mass
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct MassParams {
    pub shape: MassShape,
    /// Half-extents in metres (x = width, y = height, z = depth).
    pub size: Vec3,
    pub blend: f32,
}

impl Default for MassParams {
    fn default() -> Self {
        Self {
            shape: MassShape::Ellipsoid,
            size: Vec3::splat(0.1),
            blend: 0.05,
        }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MassShape {
    Sphere,
    #[default]
    Ellipsoid,
    /// Thin ridge along the parent's local Y (crests, sails).
    Crest,
    /// Broad rounded lump (camel humps, bison shoulders).
    Hump,
    /// Thin hanging sheet (dewlaps, wattles).
    Flap,
    /// Inflatable pouch (throat sacs, cheek pouches).
    Sac,
}

// ---------------------------------------------------------------------------------------------
// Hard parts
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct HardParams {
    pub kind: HardKind,
    /// Characteristic size in metres.
    pub size: f32,
    /// Bend along the length, `0.0` = straight, `1.0` = quarter circle.
    pub curve: f32,
    /// Spiral twist in turns (rams' horns, kudu horns).
    pub twist: f32,
}

impl Default for HardParams {
    fn default() -> Self {
        Self {
            kind: HardKind::Claw,
            size: 0.03,
            curve: 0.3,
            twist: 0.0,
        }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum HardKind {
    #[default]
    Claw,
    /// Flat nail (primates).
    Nail,
    Horn,
    /// Branching; `tines` per antler.
    Antler {
        tines: u8,
    },
    Tusk,
    Tooth,
    Fang,
    Beak,
    Spike,
    /// Flat armour plate / scute lying on the skin (`size` = length, `curve` = doming).
    Plate,
    Eye,
    /// An authored mesh attached rigidly (`size` is ignored; use `scale`).
    Mesh {
        /// Asset path of a glTF mesh primitive.
        path: String,
        #[serde(default)]
        orient: Orient,
        #[serde(default = "one")]
        scale: f32,
    },
}

fn one() -> f32 {
    1.0
}
