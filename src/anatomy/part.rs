//! Part kinds and their parameters. All values are plain numbers; per-mob variation comes from
//! [`Form`](super::Form) lever patches supplied by the game.
//!
//! Enum variants here are part of the saved data format: adding variants later is backward
//! compatible, renaming or removing them is not. Reserved for later: `Wing`, `Fin`, `Shell`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Flexes, JointLimit, Orient, RegionKind, WingParams};

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
    /// Jointed like an exoskeleton, `0.0..=1.0`: each segment swells a little between narrow
    /// joints that barely blend, so the segments read apart (arthropod legs, a lobster's tail,
    /// antennae). `0.0` = one smooth surface. Plain profiles only (no section curves).
    pub segmented: f32,
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
            segmented: 0.0,
        }
    }
}

impl ChainParams {
    /// Length of each segment, root first; they sum to `length`.
    #[must_use]
    pub fn segment_lengths(&self) -> Vec<f32> {
        let n = usize::from(self.segments.max(1));
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
///
/// The section curves (`width`, `depth`, `drop`) shape the cross-section along the chain on top
/// of `cross`, in the same `(t, value)` keys as `curve`; empty = no change. Where they vary
/// inside a bone, the bone's surface is built from two or three pieces (the skeleton is
/// unchanged).
#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Profile {
    pub curve: Vec<(f32, f32)>,
    /// Base radius in metres.
    pub scale: f32,
    pub cross: CrossSection,
    /// Lateral (side to side) multiplier along `t`: a barrel that swells, a waist that narrows.
    pub width: Vec<(f32, f32)>,
    /// Dorsal-ventral (top to bottom) multiplier along `t`.
    pub depth: Vec<(f32, f32)>,
    /// Ventral shift of the section's centre along `t`, in units of the radius at `t` (`curve ×
    /// scale`): the section hangs below the axis while the axis (and the bones) stay put. With
    /// `depth`, it deepens a chest downward and keeps the topline flat: a section `depth` `d`
    /// times as deep keeps its top in place with `drop = (d − 1) × h`, where `h` is the cross's
    /// height factor (`1` round, `1/√aspect` for the others). Negative values raise the section.
    pub drop: Vec<(f32, f32)>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            curve: vec![(0.0, 1.0), (1.0, 0.7)],
            scale: 0.08,
            cross: CrossSection::Round,
            width: Vec::new(),
            depth: Vec::new(),
            drop: Vec::new(),
        }
    }
}

impl Profile {
    /// No section curves: every bone is one plain piece of the `cross` section.
    #[must_use]
    pub const fn is_plain(&self) -> bool {
        self.width.is_empty() && self.depth.is_empty() && self.drop.is_empty()
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
    /// Strongly flattened: paddles, beaver tails, ear flaps. Built as an ellipse; the name
    /// documents intent (an `aspect` far from `1`).
    Flat {
        aspect: f32,
    },
    /// Rounded top, narrow keel below: bird chests, fish bodies, dorsal ridges. An `aspect`
    /// ellipse with a narrower keel blended along its underside, reaching
    /// [`TEARDROP_KEEL`](Self::TEARDROP_KEEL) further down.
    Teardrop {
        aspect: f32,
    },
}

impl CrossSection {
    /// How much deeper (ventrally, as a fraction of the ellipse's half height) a teardrop's
    /// keel reaches than its ellipse.
    pub const TEARDROP_KEEL: f32 = 0.2;

    /// A teardrop's keel, relative to its main ellipse: half width (x) and half height (y), and
    /// the ventral offset of its centre (z, of the half height). Its bottom is
    /// [`TEARDROP_KEEL`](Self::TEARDROP_KEEL) below the ellipse's.
    pub const KEEL: Vec3 = Vec3::new(0.5, 0.7, 1.0 + Self::TEARDROP_KEEL - 0.7);

    /// Width / height of the section's main ellipse.
    #[must_use]
    pub const fn aspect(self) -> f32 {
        match self {
            Self::Round => 1.0,
            Self::Ellipse { aspect } | Self::Flat { aspect } | Self::Teardrop { aspect } => aspect,
        }
    }
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
    /// How far the head is carried nose-down, in degrees from level (`0.0` = the skull's long
    /// axis level, as in dogs; horses and other grazers carry the face steeply).
    pub pitch: f32,
    /// A flat, primate face (nose, brow, cheekbones, lips, mandible and chin) in place of a
    /// muzzle, with the ears on the sides of the head. `None` = an animal head.
    pub face: Option<FaceParams>,
    /// Shape of an animal's muzzle and jaw (ignored under a `face`). The default is the plain
    /// round, tapering snout.
    pub muzzle: MuzzleParams,
    /// An authored head mesh in place of the cranium, face or muzzle: blended into the body as
    /// an SDF, scaled to stand `skull.size.y` high (half the head's height), its slots at the
    /// mesh's own landmarks. Eyes, hair and anything else still attach by slot.
    pub sculpt: Option<SculptHead>,
}

/// An authored head for [`HeadParams::sculpt`].
///
/// A watertight glTF head in metres, origin at the base of the neck, Y up, +Z forward (as
/// `tools/gen_head.py` writes one), and where its landmarks lie in those coordinates. The
/// defaults are the `MakeHuman` head's (`parts/head_mh.gltf`).
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct SculptHead {
    /// Asset path of the glTF mesh primitive.
    pub mesh: String,
    /// SDF cells along the mesh's longest axis.
    pub resolution: u16,
    /// Smooth-min radius against the neck.
    pub blend: f32,
    /// The left eyeball's centre, and the eyeball's radius the lids are built round.
    pub eye: Vec3,
    pub eye_radius: f32,
    /// The left nostril: the mesh's nostril cavities are filled with a small sphere here, since
    /// a cavity meshes as a hole.
    pub nostril: Vec3,
    /// The left ear's root.
    pub ear: Vec3,
    pub nose: Vec3,
    pub mouth: Vec3,
    pub crown: Vec3,
}

impl Default for SculptHead {
    fn default() -> Self {
        Self {
            mesh: "parts/head_mh.gltf#Mesh0/Primitive0".into(),
            resolution: 128,
            blend: 0.045,
            eye: Vec3::new(0.0308, 0.0945, 0.0488),
            eye_radius: 0.0138,
            nostril: Vec3::new(0.011, 0.05, 0.078),
            ear: Vec3::new(0.085, 0.09, -0.038),
            nose: Vec3::new(0.0, 0.057, 0.092),
            mouth: Vec3::new(0.0, 0.037, 0.082),
            crown: Vec3::new(0.0, 0.2153, -0.006),
        }
    }
}

/// The muzzle of an animal [`HeadParams`]: its snout and lower jaw.
///
/// The snout runs `snout` metres forward of the skull. Sizes are fractions of the skull's half
/// height (`skull.size.y`) unless noted; the defaults give the plain round snout.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct MuzzleParams {
    /// Half (width, height) of the snout where it leaves the skull.
    pub base: Vec2,
    /// Half (width, height) of the snout at its tip.
    pub tip: Vec2,
    /// Height of the snout's axis where it leaves the skull, up from the skull's centre line
    /// (`-0.15` = a little low, leaving a stop under the brow; raise it until the snout's top
    /// runs on from the forehead for a straight horse profile).
    pub rise: f32,
    /// Downward tilt of the snout in degrees (the nose line dropping toward the nose).
    pub drop: f32,
    /// Dish: how far the bridge of the nose sinks between the eyes and the nostrils (Arab
    /// horses, pugs). `0.0` = straight.
    pub dish: f32,
    /// Broad, flat cheeks over the back of the lower jaw (horses, cattle, deer). `0.0` = none,
    /// `1.0` = the jowl reaches half the skull's height below it.
    pub cheek: f32,
    /// Flared nostrils at the end of the snout, `0.0` = none, `1.0` = a horse's.
    pub nostrils: f32,
    /// Fleshy lips round the end of the snout and the jaw, `0.0` = none.
    pub lips: f32,
    /// Depth (top to bottom) of the lower jaw, relative to the plain jaw.
    pub jaw_depth: f32,
    /// Width of the lower jaw, relative to the plain jaw.
    pub jaw_width: f32,
}

impl Default for MuzzleParams {
    fn default() -> Self {
        Self {
            base: Vec2::splat(0.55),
            tip: Vec2::splat(0.35),
            rise: -0.15,
            drop: 0.0,
            dish: 0.0,
            cheek: 0.0,
            nostrils: 0.0,
            lips: 0.0,
            jaw_depth: 1.0,
            jaw_width: 1.0,
        }
    }
}

/// Facial features of a [`HeadParams::face`], each a fraction of the skull's size (`1.0` = a
/// typical adult human).
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct FaceParams {
    /// How far the nose stands out from the face.
    pub nose: f32,
    /// Bony ridge over the eyes.
    pub brow: f32,
    pub cheekbones: f32,
    pub lips: f32,
    /// How far the chin juts forward and down.
    pub chin: f32,
    /// Width of the jaw at its angle (below the ears).
    pub jaw_width: f32,
    /// Fullness of the cheeks (the buccal fat and masseter between cheekbone and jaw): `0.0` =
    /// none, leaving a hollow under each cheekbone; `1.0` = a typical adult's.
    pub cheeks: f32,
    /// How far open the eyes are: the lids wrap each eyeball, split by an almond opening this tall
    /// (`1.0` = a relaxed adult's, 0.45 of the eyeball's radius; `0.0` = no lids, the bare
    /// eyeball on the face). The lids are built for an eyeball 0.115 of the skull's height in radius.
    pub eyelids: f32,
}

impl Default for FaceParams {
    fn default() -> Self {
        Self { nose: 1.0, brow: 1.0, cheekbones: 1.0, lips: 1.0, chin: 1.0, jaw_width: 1.0, cheeks: 0.0, eyelids: 1.0 }
    }
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
    /// The limb's anatomical joints and their rest flexion, in place of `bends` (set one or the
    /// other). The joints then fix which way each one bends, so a knee can't be authored (or
    /// posed, with the `joint-limits` feature) bending like an elbow.
    pub joints: Option<LimbJoints>,
}

/// A limb's rest pose by anatomical joint: the root swings, every later joint is a hinge that
/// flexes one way (see [`JointPlan`]).
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LimbJoints {
    pub plan: JointPlan,
    /// The root (hip/shoulder) swung forward from straight down, degrees (negative = back).
    pub swing: f32,
    /// The root raised toward up, degrees: an insect's coxa lifting its femur.
    #[serde(default)]
    pub lift: f32,
    /// Flexion at each hinge below the root, degrees, root first: `0` = straight, positive closes
    /// the joint the way it bends. One per segment after the first.
    pub flex: Vec<f32>,
}

/// Which limb's joints, root down. Each names how its hinges close and how far they turn.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JointPlan {
    /// A quadruped's front leg: shoulder, elbow (points back), carpus, fetlock.
    Foreleg,
    /// A hind leg (quadrupeds, birds, wyverns, bats): hip, stifle (points forward), hock (the
    /// bird's "backward knee"), fetlock.
    Hindleg,
    /// An upright biped's leg: hip, knee (points forward), ankle.
    Leg,
    /// An arm: shoulder, elbow (points back), wrist.
    Arm,
    /// An insect's or spider's leg out to the side: coxa, femur-tibia joint (the knee, up),
    /// tibia-tarsus joint.
    Arched,
}

/// One hinge of a [`JointPlan`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hinge {
    pub name: &'static str,
    pub flexes: Flexes,
    /// Allowed flexion, degrees: (most extended, most flexed).
    pub flex: (f32, f32),
}

impl Hinge {
    const fn new(name: &'static str, flexes: Flexes, flex: (f32, f32)) -> Self {
        Self { name, flexes, flex }
    }

    #[must_use]
    pub const fn limit(self) -> JointLimit {
        JointLimit::Hinge { flexes: self.flexes, flex: self.flex }
    }
}

impl JointPlan {
    /// The root joint: how far it may swing from its rest direction.
    #[must_use]
    pub const fn root(self) -> JointLimit {
        let cone = match self {
            Self::Foreleg | Self::Hindleg | Self::Arched => 75.0,
            Self::Leg => 110.0,
            // Arms raise overhead and swing far back.
            Self::Arm => 160.0,
        };
        JointLimit::Ball { cone }
    }

    /// The hinges below the root, root first.
    #[must_use]
    pub const fn hinges(self) -> &'static [Hinge] {
        use Flexes::{Back, Down, Forward};
        const FORELEG: [Hinge; 3] = [
            Hinge::new("elbow", Forward, (-10.0, 140.0)),
            Hinge::new("carpus", Back, (-30.0, 150.0)),
            Hinge::new("fetlock", Back, (-60.0, 120.0)),
        ];
        const HINDLEG: [Hinge; 3] = [
            Hinge::new("stifle", Back, (-10.0, 140.0)),
            Hinge::new("hock", Forward, (-15.0, 140.0)),
            Hinge::new("fetlock", Back, (-60.0, 120.0)),
        ];
        const LEG: [Hinge; 2] = [Hinge::new("knee", Back, (-5.0, 150.0)), Hinge::new("ankle", Forward, (-50.0, 30.0))];
        const ARM: [Hinge; 2] = [Hinge::new("elbow", Forward, (-5.0, 150.0)), Hinge::new("wrist", Forward, (-70.0, 80.0))];
        const ARCHED: [Hinge; 2] = [Hinge::new("knee", Down, (-10.0, 170.0)), Hinge::new("tarsus", Down, (-45.0, 120.0))];
        match self {
            Self::Foreleg => &FORELEG,
            Self::Hindleg => &HINDLEG,
            Self::Leg => &LEG,
            Self::Arm => &ARM,
            Self::Arched => &ARCHED,
        }
    }
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
    /// A foot: heel, arch, the broad ball and the toes, sole on the ground; add `Digits` children
    /// for separate toes. `length` in metres.
    Foot { length: f32 },
    /// A hoof wider at the sole, its toe forward; `split` = cloven (two claws with a cleft
    /// between them: cattle, deer, pigs).
    Hoof { split: bool },
    /// Soft pad (cats, dogs); add `Hard(Claw)` children for claws.
    Pad,
    /// Crab, lobster or scorpion claw (chela): a swollen palm and two fingers, the fixed one and
    /// the hinged one, slightly open; grasps. `size` in metres (the claw is about twice as long).
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
    #[must_use]
    pub const fn grasps(self) -> bool {
        matches!(self, Self::Hand { .. } | Self::Pincer { .. })
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
    /// Eyes: how much white shows round the iris, `0.0` = none (the iris fills the eye, as in
    /// most animals) .. `1.0` = a human eye.
    pub sclera: f32,
}

impl Default for HardParams {
    fn default() -> Self {
        Self {
            kind: HardKind::Claw,
            size: 0.03,
            curve: 0.3,
            twist: 0.0,
            sclera: 0.0,
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

const fn one() -> f32 {
    1.0
}
