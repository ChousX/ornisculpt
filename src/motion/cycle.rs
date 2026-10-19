//! Cycles: body-relative descriptions of motion (gaits, idles, actions) that play on any body
//! they fit. See `docs/design/motion-api.md`.
//!
//! A cycle never names bones. It drives **targets** (anatomical roles: spine, head, tail, legs,
//! arms, wings, …) through typed **channels** (pitch, open, flex, fold, …) with **curves** over
//! its own time, 0..1. Units are body-relative: distances in leg lengths, angles in degrees, so
//! one cycle scales from a goblin to an ogre. Every (target, channel) pair and every value range
//! is checked when the cycle loads; a cycle also declares which bodies it [`Fit`]s, and never
//! plays on any other. This is what keeps motion from looking wrong on bodies it wasn't written
//! for (the approach Spore took: semantic motion turned into pose goals per creature).
//!
//! Cycles are data (`*.cycle.ron`) or built in Rust:
//!
//! ```ignore
//! use ornisculpt::motion::*;
//! let shake = Cycle::idle("shake", 0.6)
//!     .fits(Fit::family(BodyFamily::Quadruped))
//!     .track(Target::Spine, Channel::Roll, Curve::wave(25.0).harmonic(3, 0.4))
//!     .with(Track::new(Target::Tail, Channel::Yaw, Curve::wave(30.0)).lag(0.3))
//!     .event(0.5, "spray");
//! assert!(shake.validate().is_ok());
//! ```

use std::f32::consts::{PI, TAU};

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::PhasePattern;
use super::gait::BodyMotion;
use super::heft::StepShape;
use crate::anatomy::Side;

use super::fit::Fit;

/// One loop or one-shot of body-relative motion.
#[derive(Asset, TypePath, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Cycle {
    pub name: String,
    pub kind: CycleKind,
    /// Which bodies this cycle may play on.
    pub fits: Fit,
    /// Gaits: when each foot is planted and how it swings.
    pub feet: Option<FootPlan>,
    /// Everything else that moves.
    pub tracks: Vec<Track>,
    /// Named moments for gameplay (footsteps are emitted automatically by gaits).
    pub events: Vec<CycleEvent>,
    /// Movement of the whole mob over the cycle (lunges, hops, steps back).
    pub root: Option<RootMotion>,
}

impl Default for Cycle {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: CycleKind::Idle { period_s: 2.0 },
            fits: Fit::default(),
            feet: None,
            tracks: Vec::new(),
            events: Vec::new(),
            root: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum CycleKind {
    /// Speed-driven loop; one cycle = one stride. Speeds in leg lengths per second.
    Gait { min_speed: f32, max_speed: f32, stride: f32 },
    /// A loop of `period_s` seconds; plays standing or on top of moving.
    Idle { period_s: f32 },
    /// Plays once over `duration_s`. `hold` keeps the last pose until the state is left.
    Action { duration_s: f32, hold: bool },
}

impl CycleKind {
    #[must_use]
    pub const fn loops(&self) -> bool {
        !matches!(self, Self::Action { .. })
    }
}

/// When feet are planted and how they swing (gaits only).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct FootPlan {
    pub pattern: PhasePattern,
    /// Fraction of the cycle each foot is on the ground, 0.2..0.9.
    pub duty: f32,
    /// Foot lift at mid-swing, leg lengths, 0..0.6.
    pub lift: f32,
    /// Body bob per stride, leg lengths, 0..0.2.
    pub bob: f32,
    /// How the torso moves over the feet.
    pub body: BodyMotion,
    /// Foot arc and the torso's step signals.
    pub shape: StepShape,
}

impl Default for FootPlan {
    fn default() -> Self {
        Self { pattern: PhasePattern::LateralSequence, duty: 0.65, lift: 0.15, bob: 0.025, body: BodyMotion::default(), shape: StepShape::default() }
    }
}

/// One channel of one target, driven by a curve over the cycle.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Track {
    pub target: Target,
    pub channel: Channel,
    pub curve: Curve,
    /// Scales the curve, 0..1.
    pub weight: f32,
    /// Chains: how far behind the root the tip moves, as a fraction of the cycle (a travelling
    /// wave down a tail or neck).
    pub lag: f32,
    /// Limbs: time the curve by each leg's own gait phase instead of the cycle (a fetlock that
    /// flexes during each foot's swing; arms swinging against the opposite leg).
    pub per_leg: bool,
}

impl Track {
    #[must_use]
    pub fn new(target: Target, channel: Channel, curve: Curve) -> Self {
        Self { target, channel, curve, ..default() }
    }

    /// Chains: the tip trails the root by `lag` of a cycle.
    #[must_use]
    pub const fn lag(mut self, lag: f32) -> Self {
        self.lag = lag;
        self
    }

    /// Limbs: time by each leg's own gait phase.
    #[must_use]
    pub const fn per_leg(mut self) -> Self {
        self.per_leg = true;
        self
    }

    #[must_use]
    pub const fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }
}

impl Default for Track {
    fn default() -> Self {
        Self { target: Target::Root, channel: Channel::Bob, curve: Curve::Constant(0.0), weight: 1.0, lag: 0.0, per_leg: false }
    }
}

/// Which side(s) of paired targets a track drives. Paired motion is mirrored, so positive
/// sideways values always mean "outward".
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Sides {
    #[default]
    Both,
    Left,
    Right,
}

impl Sides {
    #[must_use]
    pub fn includes(self, side: Side) -> bool {
        match self {
            Self::Both => true,
            Self::Left => side != Side::Right,
            Self::Right => side != Side::Left,
        }
    }
}

/// Which legs a leg track drives.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LegSel {
    #[default]
    All,
    /// The front-most pair.
    Fore,
    /// The rear-most pair.
    Hind,
    Side(Sides),
}

/// Anatomical roles a track can drive. Resolved per body; a role the body lacks is skipped
/// (and [`Fit`] can require it).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The whole body (not the mob's `Transform`): bob, sway, tilt.
    Root,
    /// Spread over the torso bones.
    Spine,
    Neck,
    Head,
    Jaw,
    /// Spread along every tail (an insect's abdomen counts).
    Tail,
    Ears(Sides),
    Antennae(Sides),
    Legs(LegSel),
    /// Manipulator limbs (arms, a mantis's blades).
    Arms(Sides),
    Wings(Sides),
    /// Fingers and toes on manipulator limbs.
    Digits(Sides),
}

/// A limb joint by anatomy, resolved per limb (hip/shoulder, knee/elbow, ankle/hock/wrist,
/// fetlock/pastern).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Joint {
    #[serde(alias = "Hip", alias = "Shoulder")]
    Upper,
    #[serde(alias = "Knee", alias = "Elbow", alias = "Stifle")]
    Middle,
    #[serde(alias = "Ankle", alias = "Hock", alias = "Wrist")]
    Lower,
    #[serde(alias = "Fetlock", alias = "Pastern")]
    Tip,
}

/// What a track changes. Units and ranges are fixed per channel (see [`Channel::range`]).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Root: up/down, leg lengths.
    Bob,
    /// Root: side to side, leg lengths.
    Sway,
    /// Root: forward/back, leg lengths.
    Surge,
    /// Degrees; positive raises the front (root) or the chain's tip.
    Pitch,
    /// Degrees; positive turns left (paired targets: outward).
    Yaw,
    /// Degrees; positive rolls right side down (paired targets: outward edge down).
    Roll,
    /// Jaw: 0 shut .. 1 wide open.
    Open,
    /// Ears: -1 flat back .. 1 pricked up.
    Perk,
    /// Limbs: extra bend at a joint, degrees (legs: `Lower` and `Tip` only; IK sets the rest).
    Flex(Joint),
    /// Legs: foot raised, leg lengths.
    FootLift,
    /// Legs: foot moved forward, leg lengths.
    FootReach,
    /// Arms: forward/back at the shoulder, degrees.
    Swing,
    /// Arms: out to the side at the shoulder, degrees.
    Raise,
    /// Wings: 0 spread .. 1 folded (overrides the body's rest fold).
    Fold,
    /// Wings: up/down stroke at the shoulder, degrees.
    Flap,
    /// Wings: swept back, degrees.
    Sweep,
    /// Digits: 0 straight .. 1 closed fist.
    Curl,
    /// Digits: fan out, degrees.
    Spread,
}

impl Channel {
    /// Allowed value range (after `weight`).
    #[must_use]
    pub const fn range(self) -> (f32, f32) {
        match self {
            Self::Bob | Self::Sway | Self::Surge => (-0.5, 0.5),
            Self::Pitch | Self::Yaw | Self::Roll | Self::Flap => (-90.0, 90.0),
            Self::Open | Self::Fold | Self::Curl => (0.0, 1.0),
            Self::Perk => (-1.0, 1.0),
            Self::Flex(_) => (-120.0, 120.0),
            Self::FootLift | Self::FootReach => (-0.6, 0.6),
            Self::Swing | Self::Raise => (-150.0, 150.0),
            Self::Sweep => (-60.0, 60.0),
            Self::Spread => (-45.0, 45.0),
        }
    }
}

impl Target {
    /// Channels this target accepts.
    #[must_use]
    pub const fn accepts(self, channel: Channel) -> bool {
        use Channel as Ch;
        match self {
            Self::Root => matches!(channel, Ch::Bob | Ch::Sway | Ch::Surge | Ch::Pitch | Ch::Yaw | Ch::Roll),
            Self::Spine | Self::Neck | Self::Head | Self::Tail | Self::Antennae(_) => {
                matches!(channel, Ch::Pitch | Ch::Yaw | Ch::Roll)
            }
            Self::Jaw => matches!(channel, Ch::Open),
            Self::Ears(_) => matches!(channel, Ch::Pitch | Ch::Yaw | Ch::Roll | Ch::Perk),
            Self::Legs(_) => matches!(channel, Ch::Flex(Joint::Lower | Joint::Tip) | Ch::FootLift | Ch::FootReach),
            Self::Arms(_) => matches!(channel, Ch::Flex(_) | Ch::Swing | Ch::Raise | Ch::Roll),
            Self::Wings(_) => matches!(channel, Ch::Fold | Ch::Flap | Ch::Sweep | Ch::Roll),
            Self::Digits(_) => matches!(channel, Ch::Curl | Ch::Spread),
        }
    }
}

/// A value over the cycle's time `t` (0..1).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Curve {
    Constant(f32),
    /// `offset + amplitude·sin(2π(t + phase)) + Σ amp·amplitude·sin(2π(n·t + phase))`. Always
    /// loops seamlessly: the default for gaits and idles.
    Wave { amplitude: f32, phase: f32, offset: f32, harmonics: Vec<(u8, f32, f32)> },
    /// Keys `(t, value)`, smooth (Catmull-Rom-like ease) or linear. Loops must end where they
    /// start.
    Keys { keys: Vec<(f32, f32)>, smooth: bool },
    /// Rises to `peak` over `attack`, holds, falls over `release` (fractions of the cycle; the
    /// rest is zero): the shape of most actions.
    Envelope { peak: f32, attack: f32, hold: f32, release: f32 },
    /// A half-sine hump of `peak` from `from` to `to`, zero elsewhere: foot arcs, loads, landings.
    Bump { peak: f32, from: f32, to: f32 },
}

impl Default for Curve {
    fn default() -> Self {
        Self::Constant(0.0)
    }
}

impl Curve {
    /// A sine wave of `amplitude`.
    #[must_use]
    pub const fn wave(amplitude: f32) -> Self {
        Self::Wave { amplitude, phase: 0.0, offset: 0.0, harmonics: Vec::new() }
    }

    /// Smooth keys.
    pub fn keys(keys: impl IntoIterator<Item = (f32, f32)>) -> Self {
        Self::Keys { keys: keys.into_iter().collect(), smooth: true }
    }

    #[must_use]
    pub const fn envelope(peak: f32, attack: f32, hold: f32, release: f32) -> Self {
        Self::Envelope { peak, attack, hold, release }
    }

    #[must_use]
    pub const fn bump(peak: f32, from: f32, to: f32) -> Self {
        Self::Bump { peak, from, to }
    }

    /// Straight-line keys.
    pub fn linear(keys: impl IntoIterator<Item = (f32, f32)>) -> Self {
        Self::Keys { keys: keys.into_iter().collect(), smooth: false }
    }

    /// Waves: shifts the wave by `phase` of a cycle.
    #[must_use]
    pub const fn phase(mut self, p: f32) -> Self {
        if let Self::Wave { phase, .. } = &mut self {
            *phase = p;
        }
        self
    }

    /// Waves: adds a constant.
    #[must_use]
    pub const fn offset(mut self, o: f32) -> Self {
        if let Self::Wave { offset, .. } = &mut self {
            *offset = o;
        }
        self
    }

    /// Waves: adds harmonic `n` at `amp` of the base amplitude.
    #[must_use]
    pub fn harmonic(mut self, n: u8, amp: f32) -> Self {
        if let Self::Wave { harmonics, .. } = &mut self {
            harmonics.push((n, amp, 0.0));
        }
        self
    }

    #[must_use]
    pub fn sample(&self, t: f32) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Wave { amplitude, phase, offset, harmonics } => {
                let base = (TAU * (t + phase)).sin();
                let extra: f32 = harmonics.iter().map(|(n, a, p)| a * (TAU * (f32::from(*n) * t + p)).sin()).sum();
                offset + amplitude * (base + extra)
            }
            Self::Keys { keys, smooth } => sample_keys(keys, t, *smooth),
            Self::Bump { peak, from, to } => {
                if t <= *from || t >= *to { 0.0 } else { peak * (PI * (t - from) / (to - from).max(1e-6)).sin() }
            }
            Self::Envelope { peak, attack, hold, release } => {
                let ease = |x: f32| x * x * 2.0f32.mul_add(-x, 3.0);
                if t < *attack {
                    peak * ease((t / attack.max(1e-4)).clamp(0.0, 1.0))
                } else if t < attack + hold {
                    *peak
                } else {
                    let r = ((t - attack - hold) / release.max(1e-4)).clamp(0.0, 1.0);
                    peak * (1.0 - ease(r))
                }
            }
        }
    }

    /// Conservative bounds of the curve's values.
    fn bounds(&self) -> (f32, f32) {
        match self {
            Self::Constant(v) => (*v, *v),
            Self::Wave { amplitude, offset, harmonics, .. } => {
                let a = amplitude.abs() * (1.0 + harmonics.iter().map(|h| h.1.abs()).sum::<f32>());
                (offset - a, offset + a)
            }
            Self::Keys { keys, .. } => keys.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), k| (lo.min(k.1), hi.max(k.1))),
            Self::Envelope { peak, .. } | Self::Bump { peak, .. } => (peak.min(0.0), peak.max(0.0)),
        }
    }

    fn closes(&self) -> bool {
        match self {
            Self::Keys { keys, .. } => match (keys.first(), keys.last()) {
                (Some(a), Some(b)) => (a.1 - b.1).abs() < 1e-3,
                _ => true,
            },
            Self::Envelope { attack, hold, release, .. } => attack + hold + release <= 1.0 + 1e-4,
            _ => true,
        }
    }
}

fn sample_keys(keys: &[(f32, f32)], t: f32, smooth: bool) -> f32 {
    let (Some(&first), Some(&last)) = (keys.first(), keys.last()) else { return 0.0 };
    if t <= first.0 {
        return first.1;
    }
    if t >= last.0 {
        return last.1;
    }
    let Some(&[from, to]) = keys.array_windows().find(|[_, next]| next.0 > t) else { return last.1 };
    let mut f = (t - from.0) / (to.0 - from.0).max(1e-6);
    if smooth {
        f = f * f * 2.0f32.mul_add(-f, 3.0);
    }
    f32::mul_add(to.1 - from.1, f, from.1)
}

/// A named moment (`at` = 0..1 of the cycle) delivered as a [`MotionEvent`](super::MotionEvent).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CycleEvent {
    pub at: f32,
    pub name: String,
}

/// Displacement of the whole mob over a cycle: forward/up/side in leg lengths, yaw in degrees.
/// Curves give the *total* moved by time `t`; the per-frame delta is their difference.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct RootMotion {
    pub forward: Curve,
    pub up: Curve,
    pub side: Curve,
    pub yaw: Curve,
}

impl RootMotion {
    #[must_use]
    pub fn forward(curve: Curve) -> Self {
        Self { forward: curve, ..default() }
    }

    /// Displacement (leg lengths; side positive = left) and yaw (degrees) at `t`.
    #[must_use]
    pub fn at(&self, t: f32) -> (Vec3, f32) {
        (Vec3::new(self.side.sample(t), self.up.sample(t), self.forward.sample(t)), self.yaw.sample(t))
    }
}

/// Problems that keep a cycle from loading.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum CycleError {
    #[error("track {index}: {target:?} has no {channel:?} channel")]
    BadChannel { index: usize, target: Target, channel: Channel },
    #[error("track {index}: {channel:?} reaches {value} (allowed {lo}..{hi})")]
    OutOfRange { index: usize, channel: Channel, value: f32, lo: f32, hi: f32 },
    #[error("track {index}: a looping cycle's curve must end where it starts")]
    OpenLoop { index: usize },
    #[error("{0}")]
    Invalid(String),
}

impl Cycle {
    fn new(name: &str, kind: CycleKind) -> Self {
        Self { name: name.into(), kind, ..default() }
    }

    /// A gait between `min_speed` and `max_speed` leg lengths per second, `stride` leg lengths per
    /// cycle, with a foot plan.
    #[must_use]
    pub fn gait(name: &str, min_speed: f32, max_speed: f32, stride: f32, feet: FootPlan) -> Self {
        Self { feet: Some(feet), ..Self::new(name, CycleKind::Gait { min_speed, max_speed, stride }) }
    }

    /// A loop of `period_s` seconds.
    #[must_use]
    pub fn idle(name: &str, period_s: f32) -> Self {
        Self::new(name, CycleKind::Idle { period_s })
    }

    /// A one-shot of `duration_s` seconds.
    #[must_use]
    pub fn action(name: &str, duration_s: f32) -> Self {
        Self::new(name, CycleKind::Action { duration_s, hold: false })
    }

    /// Actions: keep the last pose until the state is left.
    #[must_use]
    pub const fn hold(mut self) -> Self {
        if let CycleKind::Action { hold, .. } = &mut self.kind {
            *hold = true;
        }
        self
    }

    #[must_use]
    pub fn fits(mut self, fit: Fit) -> Self {
        self.fits = fit;
        self
    }

    #[must_use]
    pub fn track(mut self, target: Target, channel: Channel, curve: Curve) -> Self {
        self.tracks.push(Track { target, channel, curve, ..default() });
        self
    }

    /// Adds a fully specified track (lag, per-leg timing, weight).
    #[must_use]
    pub fn with(mut self, track: Track) -> Self {
        self.tracks.push(track);
        self
    }

    #[must_use]
    pub fn event(mut self, at: f32, name: &str) -> Self {
        self.events.push(CycleEvent { at, name: name.into() });
        self
    }

    #[must_use]
    pub fn root(mut self, root: RootMotion) -> Self {
        self.root = Some(root);
        self
    }

    /// Length of one cycle in seconds at `speed` leg lengths per second (gaits), or its fixed
    /// period/duration.
    #[must_use]
    pub fn period(&self, speed: f32) -> f32 {
        match self.kind {
            CycleKind::Gait { stride, .. } => (stride / speed.max(0.05)).clamp(0.15, 4.0),
            CycleKind::Idle { period_s } => period_s.max(0.05),
            CycleKind::Action { duration_s, .. } => duration_s.max(0.05),
        }
    }

    /// Checks every track's target/channel pair, value range and loop closure.
    ///
    /// # Errors
    ///
    /// Every problem found.
    pub fn validate(&self) -> Result<(), Vec<CycleError>> {
        let mut errors = Vec::new();
        match &self.kind {
            CycleKind::Gait { min_speed, max_speed, stride } => {
                if !(0.0 <= *min_speed && min_speed < max_speed) || *stride <= 0.0 {
                    errors.push(CycleError::Invalid("gait speeds must satisfy 0 <= min < max, stride > 0".into()));
                }
                if self.feet.is_none() {
                    errors.push(CycleError::Invalid("a gait needs a foot plan".into()));
                }
            }
            CycleKind::Idle { period_s } | CycleKind::Action { duration_s: period_s, .. } if *period_s <= 0.0 => {
                errors.push(CycleError::Invalid("period/duration must be positive".into()));
            }
            _ => {}
        }
        if let Some(f) = &self.feet
            && (!(0.2..=0.9).contains(&f.duty) || !(0.0..=0.6).contains(&f.lift) || !(0.0..=0.2).contains(&f.bob)) {
                errors.push(CycleError::Invalid("foot plan: duty 0.2..0.9, lift 0..0.6, bob 0..0.2".into()));
            }
        for (index, t) in self.tracks.iter().enumerate() {
            if !t.target.accepts(t.channel) {
                errors.push(CycleError::BadChannel { index, target: t.target, channel: t.channel });
                continue;
            }
            let (lo, hi) = t.channel.range();
            let (a, b) = t.curve.bounds();
            for value in [a * t.weight, b * t.weight] {
                if value < lo - 1e-4 || value > hi + 1e-4 {
                    errors.push(CycleError::OutOfRange { index, channel: t.channel, value, lo, hi });
                    break;
                }
            }
            if self.kind.loops() && !t.curve.closes() {
                errors.push(CycleError::OpenLoop { index });
            }
            if !(0.0..=1.0).contains(&t.weight) || !(0.0..=1.0).contains(&t.lag) {
                errors.push(CycleError::Invalid(format!("track {index}: weight and lag must be 0..1")));
            }
        }
        if self.events.iter().any(|e| !(0.0..=1.0).contains(&e.at)) {
            errors.push(CycleError::Invalid("events must be at 0..1".into()));
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}

/// Loads `*.cycle.ron`, rejecting cycles that fail [`Cycle::validate`].
#[derive(Default, TypePath)]
pub struct CycleLoader;

#[derive(Debug, Error)]
pub enum CycleLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
    #[error("invalid cycle: {0:?}")]
    Invalid(Vec<CycleError>),
}

impl AssetLoader for CycleLoader {
    type Asset = Cycle;
    type Settings = ();
    type Error = CycleLoaderError;

    async fn load(&self, reader: &mut dyn Reader, (): &(), _: &mut LoadContext<'_>) -> Result<Cycle, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let cycle: Cycle = ron::de::from_bytes(&bytes)?;
        cycle.validate().map_err(CycleLoaderError::Invalid)?;
        Ok(cycle)
    }

    fn extensions(&self) -> &[&str] {
        &["cycle.ron"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_catches_bad_tracks() {
        let bad = Cycle::idle("bad", 1.0)
            .track(Target::Jaw, Channel::Swing, Curve::Constant(1.0))
            .track(Target::Head, Channel::Pitch, Curve::wave(200.0))
            .track(Target::Tail, Channel::Yaw, Curve::keys([(0.0, 0.0), (1.0, 30.0)]));
        let errors = bad.validate().unwrap_err();
        assert!(matches!(errors[0], CycleError::BadChannel { .. }));
        assert!(matches!(errors[1], CycleError::OutOfRange { .. }));
        assert!(matches!(errors[2], CycleError::OpenLoop { .. }));
    }

    #[test]
    fn curves_sample() {
        let env = Curve::envelope(10.0, 0.2, 0.2, 0.2);
        assert_eq!(env.sample(0.0), 0.0);
        assert_eq!(env.sample(0.3), 10.0);
        assert_eq!(env.sample(0.9), 0.0);
        assert!((Curve::wave(5.0).sample(0.25) - 5.0).abs() < 1e-4);
        assert!((Curve::keys([(0.0, 0.0), (1.0, 2.0)]).sample(0.5) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn cycles_round_trip_through_ron() {
        let c = Cycle::action("lunge", 0.8)
            .track(Target::Spine, Channel::Pitch, Curve::envelope(-20.0, 0.3, 0.1, 0.4))
            .root(RootMotion::forward(Curve::keys([(0.0, 0.0), (0.4, 0.5), (1.0, 0.5)])))
            .event(0.4, "hit");
        let text = ron::to_string(&c).unwrap();
        let back: Cycle = ron::from_str(&text).unwrap();
        assert_eq!(c, back);
        assert!(back.validate().is_ok());
    }
}
