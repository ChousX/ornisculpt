//! Built-in cycles and a standard graph that works on any body.
//!
//! Every cycle states what it fits, so a dog gets the quadruped gaits, a human the biped ones, and an action a body can't do
//! simply doesn't play.
//!
//! ```ignore
//! let graph = graphs.add(library::standard());
//! commands.spawn((Mob::new(body), MobMotion::new(graph)));
//! // later
//! params.trigger("attack");
//! ```

use super::cycle::{Channel as C, Curve, Cycle, FootPlan, Joint, LegSel, RootMotion, Sides, Target as T, Track};
use super::fit::{BodyFamily as F, Feature, Fit};
use super::graph::{BodyMask, Condition, Layer, MotionGraph, Play};
use super::gait::{BodyMotion, PhasePattern};
use super::heft::StepShape;
use crate::anatomy::Side;

fn plan(pattern: PhasePattern, duty: f32, lift: f32, bob: f32, body: BodyMotion) -> FootPlan {
    FootPlan { pattern, duty, lift, bob, body, shape: StepShape::default() }
}

/// Arms swing against the opposite leg; the wave peaks (arm forward) as that foot lands.
fn arm_swing(degrees: f32) -> Track {
    Track::new(T::Arms(Sides::Both), C::Swing, Curve::wave(degrees).phase(0.25)).per_leg()
}

/// Gaits for every family. Speeds are leg lengths per second.
#[must_use]
pub fn gaits() -> Vec<Cycle> {
    let upright = || Fit::family(F::UprightBiped);
    let biped = || Fit::family(F::Biped);
    let quad = || Fit::family(F::Quadruped);
    vec![
        // Humans, goblins, elves.
        Cycle::gait("biped.walk", 0.0, 2.2, 1.0, plan(PhasePattern::Alternate, 0.6, 0.12, 0.03, BodyMotion::walk()))
            .fits(upright())
            .with(arm_swing(22.0))
            .track(T::Arms(Sides::Both), C::Flex(Joint::Middle), Curve::Constant(12.0)),
        Cycle::gait("biped.run", 2.2, 7.0, 1.8, plan(PhasePattern::Alternate, 0.38, 0.22, 0.05, BodyMotion::run()))
            .fits(upright())
            .with(arm_swing(40.0))
            .track(T::Arms(Sides::Both), C::Flex(Joint::Middle), Curve::Constant(75.0))
            .track(T::Spine, C::Pitch, Curve::Constant(-8.0)),
        // Birds, theropods, wyverns: the head bobs, the tail balances.
        Cycle::gait("biped.strut", 0.0, 2.0, 0.9, plan(PhasePattern::Alternate, 0.6, 0.15, 0.03, BodyMotion::strut()))
            .fits(biped())
            .track(T::Neck, C::Pitch, Curve::wave(6.0).harmonic(2, 0.5))
            .with(Track::new(T::Tail, C::Yaw, Curve::wave(5.0).phase(0.5)).lag(0.25)),
        Cycle::gait("biped.sprint", 2.0, 7.0, 1.7, plan(PhasePattern::Alternate, 0.4, 0.22, 0.05, BodyMotion { counter: 0.0, rock: 0.0, steady: 0.8, ..BodyMotion::run() }))
            .fits(biped())
            .track(T::Spine, C::Pitch, Curve::Constant(-6.0))
            .track(T::Neck, C::Pitch, Curve::Constant(-12.0))
            .with(Track::new(T::Tail, C::Pitch, Curve::wave(4.0)).lag(0.3)),
        // Dogs, horses, bears, dragons.
        Cycle::gait("quadruped.walk", 0.0, 1.6, 1.0, plan(PhasePattern::LateralSequence, 0.65, 0.15, 0.025, BodyMotion::walk()))
            .fits(quad())
            .track(T::Neck, C::Pitch, Curve::wave(4.0).harmonic(2, 0.5))
            .with(Track::new(T::Tail, C::Yaw, Curve::wave(8.0)).lag(0.3))
            .with(Track::new(T::Legs(LegSel::All), C::Flex(Joint::Tip), Curve::keys([(0.0, 0.0), (0.7, 0.0), (0.82, 35.0), (1.0, 0.0)])).per_leg()),
        Cycle::gait("quadruped.trot", 1.6, 3.5, 1.5, plan(PhasePattern::Diagonal, 0.5, 0.2, 0.04, BodyMotion::trot()))
            .fits(quad())
            .track(T::Neck, C::Pitch, Curve::Constant(-4.0))
            .track(T::Tail, C::Pitch, Curve::Constant(8.0))
            .with(Track::new(T::Tail, C::Yaw, Curve::wave(5.0)).lag(0.3))
            .with(Track::new(T::Legs(LegSel::All), C::Flex(Joint::Tip), Curve::keys([(0.0, 0.0), (0.55, 0.0), (0.7, 45.0), (1.0, 0.0)])).per_leg()),
        Cycle::gait("quadruped.gallop", 3.5, 10.0, 2.4, plan(PhasePattern::Gallop { lead: Side::Left }, 0.35, 0.25, 0.07, BodyMotion::gallop()))
            .fits(quad())
            .track(T::Spine, C::Pitch, Curve::wave(8.0))
            .track(T::Neck, C::Pitch, Curve::wave(10.0).phase(0.5).offset(-8.0))
            .with(Track::new(T::Tail, C::Pitch, Curve::wave(10.0).offset(10.0)).lag(0.2))
            .with(Track::new(T::Legs(LegSel::All), C::Flex(Joint::Tip), Curve::keys([(0.0, 0.0), (0.4, 0.0), (0.6, 55.0), (1.0, 0.0)])).per_leg()),
        // Insects.
        Cycle::gait("hexapod.tripod", 0.0, 5.0, 0.9, plan(PhasePattern::Tripod, 0.55, 0.2, 0.01, BodyMotion::rigid()))
            .fits(Fit::family(F::Hexapod))
            .with(Track::new(T::Antennae(Sides::Both), C::Pitch, Curve::wave(8.0)).lag(0.3))
            .track(T::Tail, C::Pitch, Curve::wave(2.0).harmonic(2, 0.5)),
        // Spiders, millipedes, centipedes: a wave travels down the body.
        Cycle::gait("many.wave", 0.0, 4.0, 0.8, plan(PhasePattern::Wave { lag: 0.15 }, 0.7, 0.15, 0.005, BodyMotion::rigid()))
            .fits(Fit::family(F::Myriapod).or_family(F::Octopod))
            .with(Track::new(T::Spine, C::Yaw, Curve::wave(6.0)).lag(0.6))
            .with(Track::new(T::Antennae(Sides::Both), C::Yaw, Curve::wave(10.0)).lag(0.2)),
    ]
}

/// Looping idles, layered on top of everything.
#[must_use]
pub fn idles() -> Vec<Cycle> {
    vec![
        Cycle::idle("breathe", 3.6).track(T::Spine, C::Pitch, Curve::wave(1.5)).track(T::Root, C::Bob, Curve::wave(0.004)),
        Cycle::idle("look_around", 8.0)
            .fits(Fit::any().needs(Feature::Neck))
            .track(T::Neck, C::Yaw, Curve::keys([(0.0, 0.0), (0.15, 28.0), (0.4, 28.0), (0.55, -22.0), (0.8, -22.0), (1.0, 0.0)]))
            .track(T::Head, C::Pitch, Curve::keys([(0.0, 0.0), (0.2, 6.0), (0.5, -4.0), (0.8, 4.0), (1.0, 0.0)])),
        Cycle::idle("tail_wag", 0.45)
            .fits(Fit::any().needs(Feature::Tail))
            .with(Track::new(T::Tail, C::Yaw, Curve::wave(25.0)).lag(0.3))
            .track(T::Tail, C::Pitch, Curve::Constant(15.0)),
        Cycle::idle("ear_flick", 4.0)
            .fits(Fit::any().needs(Feature::Ears))
            .track(T::Ears(Sides::Left), C::Pitch, Curve::keys([(0.0, 0.0), (0.05, -25.0), (0.12, 0.0), (1.0, 0.0)]))
            .track(T::Ears(Sides::Right), C::Pitch, Curve::keys([(0.0, 0.0), (0.5, 0.0), (0.55, -20.0), (0.62, 0.0), (1.0, 0.0)])),
        Cycle::idle("antennae_sweep", 2.2)
            .fits(Fit::any().needs(Feature::Antennae))
            .with(Track::new(T::Antennae(Sides::Both), C::Yaw, Curve::wave(15.0)).lag(0.25))
            .with(Track::new(T::Antennae(Sides::Left), C::Pitch, Curve::wave(8.0).phase(0.3))),
        Cycle::idle("wings.flap", 0.55)
            .fits(Fit::any().needs(Feature::Wings))
            .track(T::Wings(Sides::Both), C::Fold, Curve::Constant(0.0))
            .track(T::Wings(Sides::Both), C::Flap, Curve::wave(50.0).offset(10.0).harmonic(2, 0.15))
            .track(T::Wings(Sides::Both), C::Sweep, Curve::wave(10.0).phase(0.25))
            .track(T::Root, C::Bob, Curve::wave(0.03).phase(0.5))
            .event(0.25, "wingbeat"),
    ]
}

/// One-shot actions.
#[must_use]
pub fn actions() -> Vec<Cycle> {
    vec![
        Cycle::action("attack", 0.7)
            .track(T::Spine, C::Pitch, Curve::envelope(-8.0, 0.3, 0.1, 0.5))
            .track(T::Neck, C::Pitch, Curve::envelope(-18.0, 0.3, 0.1, 0.5))
            .track(T::Jaw, C::Open, Curve::keys([(0.0, 0.0), (0.25, 1.0), (0.35, 0.1), (0.6, 0.0), (1.0, 0.0)]))
            .track(T::Arms(Sides::Right), C::Swing, Curve::keys([(0.0, 0.0), (0.2, -40.0), (0.38, 70.0), (0.7, 0.0), (1.0, 0.0)]))
            .track(T::Arms(Sides::Right), C::Raise, Curve::envelope(35.0, 0.2, 0.2, 0.5))
            .root(RootMotion::forward(Curve::keys([(0.0, 0.0), (0.2, -0.05), (0.38, 0.35), (1.0, 0.35)])))
            .event(0.36, "hit"),
        Cycle::action("shake", 1.1)
            .fits(Fit::family(F::Quadruped))
            .track(T::Spine, C::Roll, Curve::keys([(0.0, 0.0), (0.1, 20.0), (0.2, -22.0), (0.3, 22.0), (0.4, -20.0), (0.5, 16.0), (0.6, -12.0), (0.7, 6.0), (0.85, 0.0), (1.0, 0.0)]))
            .track(T::Neck, C::Roll, Curve::keys([(0.0, 0.0), (0.12, -25.0), (0.22, 25.0), (0.32, -25.0), (0.42, 22.0), (0.52, -16.0), (0.65, 8.0), (0.85, 0.0), (1.0, 0.0)]))
            .with(Track::new(T::Tail, C::Yaw, Curve::keys([(0.0, 0.0), (0.15, 30.0), (0.25, -30.0), (0.35, 30.0), (0.45, -25.0), (0.6, 10.0), (0.85, 0.0), (1.0, 0.0)])).lag(0.05))
            .event(0.3, "spray"),
        Cycle::action("rear", 1.8)
            .fits(Fit::family(F::Quadruped).size(0.5..=5.0))
            .track(T::Root, C::Pitch, Curve::envelope(30.0, 0.3, 0.3, 0.35))
            .track(T::Root, C::Bob, Curve::envelope(0.12, 0.3, 0.3, 0.35))
            .track(T::Legs(LegSel::Fore), C::FootLift, Curve::envelope(0.45, 0.3, 0.3, 0.35))
            .track(T::Legs(LegSel::Fore), C::FootReach, Curve::envelope(0.2, 0.3, 0.3, 0.35))
            .track(T::Neck, C::Pitch, Curve::envelope(20.0, 0.35, 0.2, 0.4))
            .event(0.95, "stomp"),
        Cycle::action("wave", 1.8)
            .fits(Fit::family(F::UprightBiped).needs(Feature::Arms))
            .track(T::Arms(Sides::Right), C::Raise, Curve::envelope(130.0, 0.2, 0.6, 0.2))
            .track(T::Arms(Sides::Right), C::Flex(Joint::Middle), Curve::keys([(0.0, 0.0), (0.2, 40.0), (0.3, 80.0), (0.4, 40.0), (0.5, 80.0), (0.6, 40.0), (0.7, 80.0), (0.85, 20.0), (1.0, 0.0)]))
            .track(T::Head, C::Roll, Curve::envelope(6.0, 0.2, 0.6, 0.2)),
        Cycle::action("flare", 1.6)
            .fits(Fit::any().needs(Feature::Wings))
            .track(T::Wings(Sides::Both), C::Fold, Curve::keys([(0.0, 1.0), (0.25, 0.0), (0.75, 0.0), (1.0, 1.0)]))
            .track(T::Wings(Sides::Both), C::Flap, Curve::envelope(25.0, 0.25, 0.5, 0.25))
            .track(T::Neck, C::Pitch, Curve::envelope(25.0, 0.25, 0.5, 0.25))
            .track(T::Jaw, C::Open, Curve::envelope(0.8, 0.25, 0.5, 0.25))
            .event(0.3, "roar"),
    ]
}

/// A graph for any body:
///
/// - `locomotion`: the fitting gait for the current `speed`.
/// - `breath`: additive breathing, always.
/// - `ears`, `feelers`: ear flicks and antenna sweeps, always.
/// - `head`: looking around while standing (`moving` false).
/// - `tail`: wagging while `happy` is set.
/// - `wings`: flapping while `flying` is set.
/// - `action`: one-shots on triggers `attack`, `shake`, `rear`, `wave`, `flare`.
#[must_use]
pub fn standard() -> MotionGraph {
    let mut g = MotionGraph::new()
        .param("happy", super::graph::ParamValue::Bool(false))
        .param("flying", super::graph::ParamValue::Bool(false));
    for c in [&gaits()[..], &idles()[..], &actions()[..]].concat() {
        g = g.cycle(c);
    }
    let gait_names: Vec<String> = gaits().into_iter().map(|c| c.name).collect();
    let mut action = Layer::new("action").state("none", Play::Empty);
    for name in ["attack", "shake", "rear", "wave", "flare"] {
        g = g.param(name, super::graph::ParamValue::Trigger);
        action = action
            .state(name, Play::Cycle(name.into()))
            .transition("none", name, Condition::trigger(name), 0.15)
            .transition(name, "none", Condition::Finished, 0.25);
    }
    g.layer(Layer::new("locomotion").state("move", Play::Locomotion(gait_names)))
        .layer(Layer::new("breath").additive().state("breathe", Play::Cycle("breathe".into())))
        .layer(Layer::new("ears").additive().state("flick", Play::Cycle("ear_flick".into())))
        .layer(Layer::new("feelers").additive().state("sweep", Play::Cycle("antennae_sweep".into())))
        .layer(
            Layer::new("head")
                .mask(BodyMask::HeadNeck)
                .additive()
                .state("still", Play::Empty)
                .state("look", Play::Cycle("look_around".into()))
                .transition("still", "look", Condition::is("moving", false), 1.0)
                .transition("look", "still", Condition::is("moving", true), 0.3),
        )
        .layer(
            Layer::new("tail")
                .mask(BodyMask::Tail)
                .state("rest", Play::Empty)
                .state("wag", Play::Cycle("tail_wag".into()))
                .transition("rest", "wag", Condition::is("happy", true), 0.3)
                .transition("wag", "rest", Condition::is("happy", false), 0.5),
        )
        .layer(
            Layer::new("wings")
                .mask(BodyMask::Wings)
                .state("rest", Play::Empty)
                .state("flap", Play::Cycle("wings.flap".into()))
                .transition("rest", "flap", Condition::is("flying", true), 0.4)
                .transition("flap", "rest", Condition::is("flying", false), 0.6),
        )
        .layer(action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::testing::sample_body;
    use crate::motion::{BodyFamily, BodyTraits, compose};

    #[test]
    fn library_cycles_validate_and_compose() {
        for c in [gaits(), idles(), actions()].concat() {
            assert!(c.validate().is_ok(), "{}: {:?}", c.name, c.validate());
        }
        let g = standard();
        let composed = compose(&[&g]).expect("standard graph composes");
        assert_eq!(composed.layers.len(), 8);
    }

    #[test]
    fn every_sample_body_gets_a_gait() {
        let gaits = gaits();
        for (name, src, family) in [
            ("biped", include_str!("../../assets/mobs/biped.body.ron"), BodyFamily::UprightBiped),
            ("quadruped", include_str!("../../assets/mobs/quadruped.body.ron"), BodyFamily::Quadruped),
            ("horse", include_str!("../../assets/mobs/horse.body.ron"), BodyFamily::Quadruped),
            ("dragon", include_str!("../../assets/mobs/dragon.body.ron"), BodyFamily::Quadruped),
            ("hexapod", include_str!("../../assets/mobs/hexapod.body.ron"), BodyFamily::Hexapod),
            ("bird", include_str!("../../assets/mobs/bird.body.ron"), BodyFamily::Biped),
        ] {
            let a = sample_body(src);
            let traits = BodyTraits::of(&a);
            assert_eq!(traits.family, family, "{name}: {traits:?}");
            let fitting: Vec<&str> = gaits.iter().filter(|g| g.fits.check(&traits).is_empty()).map(|g| g.name.as_str()).collect();
            println!("{name}: {:?} gaits {fitting:?}", traits.features);
            assert!(!fitting.is_empty(), "{name} has no gait");
        }
    }
}
