//! Playing motion on mobs.
//!
//! - [`MobMotion`]: the graphs a mob runs (composed in order).
//! - [`MotionParams`]: the values their transitions read (`set`, `set_bool`, `trigger`).
//! - [`PlayCycle`]: play one cycle directly on top of everything, no graph needed.
//! - Cycle events arrive as [`MotionEvent`]s, footfalls as [`Footstep`]s.
//! - Root motion moves the mob's `Transform`, or lands in [`RootMotionDelta`] under
//!   [`ExternalRoot`].

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use smallvec::SmallVec;

use super::cycle::{Cycle, CycleKind};
use super::eval::{EvalInput, LayerPose, MaskWeights, MotionRig, evaluate};
use super::graph::{
    BodyMask, CCondition, CLayer, CParamTest, CPlay, ComposedGraph, CycleSource, LayerMode, MotionGraph, ParamValue, Switch, compose,
};
use super::stages::leg_length;
use super::{ActiveStep, CyclePose, ExternalRoot, GaitClock, GaitOverride, Locomotion, MotionEvent};
use crate::anatomy::Anatomy;
use crate::mob::{GeneratedBody, MobBody, MobSkeleton};
use crate::num::smoothstep;

/// The motion graphs this mob runs, composed in order (see [`compose`]).
#[derive(Component, Clone, Debug, Default)]
#[require(MotionParams, MotionRuntime)]
pub struct MobMotion(pub Vec<Handle<MotionGraph>>);

impl MobMotion {
    #[must_use]
    pub fn new(graph: Handle<MotionGraph>) -> Self {
        Self(vec![graph])
    }

    /// Also runs `graph` (its layers merge with same-named layers, parameters are shared).
    #[must_use]
    pub fn and(mut self, graph: Handle<MotionGraph>) -> Self {
        self.0.push(graph);
        self
    }
}

/// How long an unconsumed trigger stays set, seconds.
pub const TRIGGER_LIFETIME: f32 = 0.5;

/// Values the motion graphs' transitions read. `speed` (leg lengths per second), `moving` and
/// `turn` (radians per second, positive left) are written by the plugin every frame.
#[derive(Component, Clone, Debug, Default)]
pub struct MotionParams {
    floats: HashMap<String, f32>,
    bools: HashMap<String, bool>,
    /// Pending triggers and their age.
    triggers: HashMap<String, f32>,
}

impl MotionParams {
    pub fn set(&mut self, name: &str, value: f32) -> &mut Self {
        match self.floats.get_mut(name) {
            Some(v) => *v = value,
            None => {
                self.floats.insert(name.into(), value);
            }
        }
        self
    }

    pub fn set_bool(&mut self, name: &str, value: bool) -> &mut Self {
        match self.bools.get_mut(name) {
            Some(v) => *v = value,
            None => {
                self.bools.insert(name.into(), value);
            }
        }
        self
    }

    /// Fires the first transition waiting on `name` (within [`TRIGGER_LIFETIME`]).
    pub fn trigger(&mut self, name: &str) -> &mut Self {
        self.triggers.insert(name.into(), 0.0);
        self
    }

    #[must_use]
    pub fn float(&self, name: &str) -> Option<f32> {
        self.floats.get(name).copied()
    }

    #[must_use]
    pub fn bool(&self, name: &str) -> Option<bool> {
        self.bools.get(name).copied()
    }

    #[must_use]
    pub fn is_triggered(&self, name: &str) -> bool {
        self.triggers.contains_key(name)
    }
}

impl From<Handle<Cycle>> for CycleSource {
    fn from(h: Handle<Cycle>) -> Self {
        Self::Asset(h)
    }
}

impl From<Cycle> for CycleSource {
    fn from(c: Cycle) -> Self {
        Self::Inline(Arc::new(c))
    }
}

/// Plays one cycle on top of the mob's graphs (or alone). Actions remove this component when
/// they finish; loops play until it's removed. Replacing it crossfades to the new cycle.
///
/// ```ignore
/// commands.entity(mob).insert(PlayCycle::new(asset_server.load("motion/shake.cycle.ron")));
/// ```
#[derive(Component, Clone, Debug)]
#[require(MotionParams, MotionRuntime)]
pub struct PlayCycle {
    pub cycle: CycleSource,
    pub mask: BodyMask,
    pub mode: LayerMode,
    pub weight: f32,
    /// Fade in/out, seconds.
    pub blend: f32,
    /// Playback rate.
    pub speed: f32,
    pub root_motion: bool,
}

impl PlayCycle {
    pub fn new(cycle: impl Into<CycleSource>) -> Self {
        Self { cycle: cycle.into(), mask: BodyMask::All, mode: LayerMode::Override, weight: 1.0, blend: 0.2, speed: 1.0, root_motion: true }
    }

    #[must_use]
    pub fn mask(mut self, mask: BodyMask) -> Self {
        self.mask = mask;
        self
    }

    #[must_use]
    pub const fn additive(mut self) -> Self {
        self.mode = LayerMode::Additive;
        self
    }

    #[must_use]
    pub const fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    #[must_use]
    pub const fn blend(mut self, seconds: f32) -> Self {
        self.blend = seconds;
        self
    }

    #[must_use]
    pub const fn speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }
}

/// This frame's root motion for [`ExternalRoot`] mobs: apply it yourself (world space).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct RootMotionDelta {
    pub translation: Vec3,
    pub rotation: Quat,
}

/// A foot touched down (hero tier).
#[derive(EntityEvent, Clone, Debug)]
pub struct Footstep {
    pub entity: Entity,
    /// Index into `Anatomy::legs`.
    pub leg: usize,
    /// Where, world space.
    pub position: Vec3,
}

// ---------------------------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------------------------

/// Playback state of one layer.
#[derive(Clone, Debug, Default)]
struct LayerRun {
    state: usize,
    /// Cycle time 0..1 (unused by gait-synced states).
    t: f32,
    /// Seconds in the current state.
    elapsed: f32,
    /// Locomotion: the current gait (composed cycle index).
    gait: Option<usize>,
    /// The state (or gait) fading out.
    fade: Option<Fade>,
    /// A transition waiting for the current cycle to end.
    pending: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct Fade {
    state: usize,
    t: f32,
    gait: Option<usize>,
    left: f32,
    total: f32,
}

impl Fade {
    fn weight(&self) -> f32 {
        if self.total <= 0.0 { 0.0 } else { (self.left / self.total).clamp(0.0, 1.0) }
    }
}

/// A [`PlayCycle`] in flight (several while crossfading).
#[derive(Clone, Debug)]
struct DirectRun {
    play: PlayCycle,
    mask: Option<MaskWeights>,
    fits: Option<bool>,
    t: f32,
    elapsed: f32,
    /// Fade envelope 0..1.
    level: f32,
    leaving: bool,
}

/// An index into the composed graph (a cycle or a layer) or into the direct runs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Src {
    Graph(usize),
    Direct(usize),
}

/// One cycle contributing to this frame's pose.
#[derive(Clone, Copy, Debug)]
struct Active {
    src: Src,
    /// `None`: synced to the gait phase.
    t: Option<f32>,
    weight: f32,
    layer: Src,
}

/// Per-mob playback state (added automatically with [`MobMotion`] or [`PlayCycle`]).
#[derive(Component, Default)]
pub struct MotionRuntime {
    key: Vec<AssetId<MotionGraph>>,
    graph: Option<Arc<ComposedGraph>>,
    body: Option<AssetId<GeneratedBody>>,
    rig: Option<Arc<MotionRig>>,
    masks: Vec<MaskWeights>,
    /// Whether each composed cycle fits this body (filled lazily).
    fits: Vec<Option<bool>>,
    values: Vec<f32>,
    layers: Vec<LayerRun>,
    direct: Vec<DirectRun>,
    active: SmallVec<[Active; 8]>,
    /// The playing gait cycle whose foot plan replaces the gait's mode, if any.
    plan: Option<Src>,
    last_phase: f32,
}

impl MotionRuntime {
    /// Whether anything is playing.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        !self.active.is_empty()
    }

    /// Current state name of a layer.
    #[must_use]
    pub fn state(&self, layer: &str) -> Option<&str> {
        let g = self.graph.as_ref()?;
        let (i, l) = g.layers.iter().enumerate().find(|(_, l)| l.name == layer)?;
        Some(&l.states.get(self.layers.get(i)?.state)?.name)
    }

    /// Whether composed cycle `i` fits this body, checked once (misfits are reported quietly:
    /// one graph serves many bodies).
    fn fits(&mut self, i: usize, cycles: &Assets<Cycle>, out: &mut Out) -> bool {
        if let Some(f) = self.fits.get(i).copied().flatten() {
            return f;
        }
        let (Some(rig), Some(cycle)) = (&self.rig, self.cycle(Src::Graph(i), cycles)) else { return false };
        let misfit = misfit(cycle, rig);
        let ok = misfit.is_none();
        out.misfits.extend(misfit.map(|m| (m, false)));
        if let Some(slot) = self.fits.get_mut(i) {
            *slot = Some(ok);
        }
        ok
    }

    fn cycle<'a>(&'a self, src: Src, cycles: &'a Assets<Cycle>) -> Option<&'a Cycle> {
        match src {
            Src::Graph(i) => self.graph.as_ref()?.cycles.get(i)?.get(cycles),
            Src::Direct(i) => self.direct.get(i)?.play.cycle.get(cycles),
        }
    }

    /// The combined pose of every playing cycle at gait phase `phase`.
    pub(crate) fn pose(&self, a: &Anatomy, cycles: &Assets<Cycle>, phase: f32, leg_phases: &[f32]) -> Option<LayerPose> {
        let rig = self.rig.as_ref()?;
        if self.active.is_empty() {
            return None;
        }
        let empty = || LayerPose::identity(a.skeleton.len(), a.legs.len(), a.wings.len());
        let mut total = empty();
        for group in self.active.chunk_by(|x, y| x.layer == y.layer) {
            let Some(layer) = group.first().map(|act| act.layer) else { continue };
            let mut pose = empty();
            for act in group {
                if let Some(cycle) = self.cycle(act.src, cycles) {
                    let input = EvalInput { anatomy: a, rig, t: act.t.unwrap_or(phase), leg_phases };
                    pose.accumulate(&evaluate(cycle, &input), act.weight);
                }
            }
            let (mask, weight, additive) = match layer {
                Src::Graph(l) => {
                    let g = self.graph.as_ref()?;
                    let (Some(mask), Some(gl)) = (self.masks.get(l), g.layers.get(l)) else { continue };
                    (mask, gl.weight, gl.mode == LayerMode::Additive)
                }
                Src::Direct(d) => {
                    let Some(run) = self.direct.get(d) else { continue };
                    let Some(mask) = &run.mask else { continue };
                    (mask, run.play.weight * run.level, run.play.mode == LayerMode::Additive)
                }
            };
            total.compose(&pose, weight, mask, additive);
        }
        Some(total)
    }

    /// (Re)binds to a body: resolves masks, clears fit results.
    fn bind(&mut self, body: AssetId<GeneratedBody>, rig: Arc<MotionRig>) {
        self.body = Some(body);
        self.masks = self.graph.as_ref().map_or(Vec::new(), |g| g.layers.iter().map(|l| rig.mask(&l.mask)).collect());
        for d in &mut self.direct {
            d.mask = Some(rig.mask(&d.play.mask));
            d.fits = None;
        }
        self.fits = self.graph.as_ref().map_or(Vec::new(), |g| vec![None; g.cycles.len()]);
        self.rig = Some(rig);
    }

    fn set_graph(&mut self, key: Vec<AssetId<MotionGraph>>, graph: Option<Arc<ComposedGraph>>) {
        self.key = key;
        self.layers = graph.as_ref().map_or(Vec::new(), |g| {
            g.layers.iter().map(|l| LayerRun { state: l.entry, ..default() }).collect()
        });
        self.values = graph.as_ref().map_or(Vec::new(), |g| {
            g.params
                .iter()
                .map(|(_, v)| match v {
                    ParamValue::Float(f) => *f,
                    ParamValue::Bool(b) => flag(*b),
                    ParamValue::Trigger => 0.0,
                })
                .collect()
        });
        self.graph = graph;
        if let (Some(body), Some(rig)) = (self.body, self.rig.clone()) {
            self.bind(body, rig);
        }
    }
}

/// Composed graphs shared by every mob running the same set, and motion rigs per body.
#[derive(Resource, Default)]
pub struct MotionCache {
    graphs: HashMap<Vec<AssetId<MotionGraph>>, Option<Arc<ComposedGraph>>>,
    rigs: HashMap<AssetId<GeneratedBody>, Arc<MotionRig>>,
}

/// Binds runtimes to their (composed) graphs and bodies: only mobs whose graphs or body changed,
/// or every mob when a graph or body asset did.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn prepare_motion(
    mut cache: ResMut<MotionCache>,
    mut graph_events: MessageReader<AssetEvent<MotionGraph>>,
    mut body_events: MessageReader<AssetEvent<GeneratedBody>>,
    graphs: Res<Assets<MotionGraph>>,
    bodies: Res<Assets<GeneratedBody>>,
    mut mobs: Query<(&mut MotionRuntime, Option<&MobMotion>, &MobBody)>,
    changed: Query<Entity, (With<MotionRuntime>, Or<(Changed<MobMotion>, Changed<MobBody>, Added<PlayCycle>)>)>,
    mut removed: RemovedComponents<MobMotion>,
) {
    let graphs_changed = graph_events.read().any(|e| {
        matches!(e, AssetEvent::Added { .. } | AssetEvent::Modified { .. } | AssetEvent::Removed { .. } | AssetEvent::LoadedWithDependencies { .. })
    });
    if graphs_changed {
        cache.graphs.clear();
    }
    let mut bodies_changed = false;
    for e in body_events.read() {
        if let AssetEvent::Modified { id } | AssetEvent::Removed { id } = e {
            cache.rigs.remove(id);
            bodies_changed = true;
        }
    }
    let mut rebind = |(mut rt, motion, body): (Mut<MotionRuntime>, Option<&MobMotion>, &MobBody)| {
        let key: Vec<AssetId<MotionGraph>> = motion.map_or(Vec::new(), |m| m.0.iter().map(Handle::id).collect());
        if key != rt.key || graphs_changed || (rt.graph.is_none() && !key.is_empty()) {
            let composed = cache.composed(&key, &graphs);
            let same = match (&rt.graph, &composed) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if !same || key != rt.key {
                rt.set_graph(key, composed);
            }
        }
        if rt.body != Some(body.0.id()) || rt.rig.is_none() || bodies_changed {
            let Some(b) = bodies.get(&body.0) else { return };
            let rig = cache.rigs.entry(body.0.id()).or_insert_with(|| Arc::new(MotionRig::new(&b.anatomy))).clone();
            rt.bind(body.0.id(), rig);
        }
    };
    if graphs_changed || bodies_changed {
        mobs.iter_mut().for_each(&mut rebind);
    } else {
        let todo: Vec<Entity> = changed.iter().chain(removed.read()).collect();
        for e in todo {
            if let Ok(item) = mobs.get_mut(e) {
                rebind(item);
            }
        }
    }
}

impl MotionCache {
    /// The composed graph for a set of graphs, composing it once all are loaded.
    fn composed(&mut self, key: &[AssetId<MotionGraph>], graphs: &Assets<MotionGraph>) -> Option<Arc<ComposedGraph>> {
        if let Some(c) = self.graphs.get(key) {
            return c.clone();
        }
        // Not loaded yet: their load event brings us back.
        let list: Vec<&MotionGraph> = key.iter().map(|id| graphs.get(*id)).collect::<Option<_>>()?;
        if list.is_empty() {
            return None;
        }
        let c = match compose(&list) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                error!("motion graph: {e}");
                None
            }
        };
        self.graphs.insert(key.to_vec(), c.clone());
        c
    }
}

/// Misfit warnings already shown (each is logged once).
#[derive(Resource, Default)]
pub(super) struct MotionWarnings(bevy::platform::collections::HashSet<String>);

impl MotionWarnings {
    fn report(&mut self, message: String, loud: bool) {
        if !self.0.contains(&message) {
            if loud {
                warn!("{message}");
            } else {
                debug!("{message}");
            }
            self.0.insert(message);
        }
    }
}

/// What one mob's update produced.
#[derive(Default)]
pub(super) struct Out {
    events: SmallVec<[(String, String); 2]>,
    /// Direct action finished: remove its `PlayCycle`.
    finished: bool,
    /// (message, loud): graph cycles that don't fit are expected (one graph serves many bodies),
    /// a direct `PlayCycle` that doesn't fit is a mistake.
    misfits: SmallVec<[(String, bool); 1]>,
    /// Root displacement (mob space) and yaw (radians) this frame.
    root: Vec3,
    yaw: f32,
}

/// Advances every layer: parameters, transitions, time, root motion and events.
#[allow(clippy::type_complexity)]
pub(super) fn update_motion(
    time: Res<super::MotionTime>,
    cycles: Res<Assets<Cycle>>,
    bodies: Res<Assets<GeneratedBody>>,
    mut mobs: Query<(
        Entity,
        &mut MotionRuntime,
        &mut MotionParams,
        &Locomotion,
        &GaitClock,
        &mut GaitOverride,
        &MobBody,
        &mut Transform,
        Option<Ref<PlayCycle>>,
        Has<ExternalRoot>,
        Option<&mut RootMotionDelta>,
    )>,
    commands: ParallelCommands,
) {
    let dt = time.dt;
    mobs.par_iter_mut().for_each(|(entity, mut rt, mut params, loco, clock, mut gait, body, mut tf, play, external, delta)| {
        let Some(b) = bodies.get(&body.0) else { return };
        let leg = leg_length(&b.anatomy);
        let mut out = Out::default();
        let (offset, yaw) = step(&mut rt, &mut params, loco, clock, play.as_ref().map(|p| (&**p, p.is_changed())), leg, dt, &cycles, &mut out);
        // The gait cycle's foot plan, copied only when it changes.
        let plan = rt.plan.and_then(|src| rt.cycle(src, &cycles)).and_then(|c| c.feet.as_ref().map(|p| (p, stride(c))));
        let same = match (&gait.0, plan) {
            (Some((a, s)), Some((b, t))) => a == b && s.to_bits() == t.to_bits(),
            (None, None) => true,
            _ => false,
        };
        if !same {
            gait.0 = plan.map(|(p, s)| (p.clone(), s));
        }
        let world = tf.rotation * offset;
        let rotation = Quat::from_rotation_y(yaw);
        if external {
            if let Some(mut d) = delta {
                d.translation = world;
                d.rotation = rotation;
            }
        } else if offset != Vec3::ZERO || yaw != 0.0 {
            tf.translation += world;
            tf.rotation = rotation * tf.rotation;
        }
        if out.events.is_empty() && !out.finished && out.misfits.is_empty() {
            return;
        }
        commands.command_scope(|mut commands| {
            for (name, cycle) in out.events {
                commands.trigger(MotionEvent { entity, name, cycle });
            }
            if out.finished {
                commands.entity(entity).try_remove::<PlayCycle>();
            }
            for (message, loud) in out.misfits {
                commands.queue(move |world: &mut World| world.resource_mut::<MotionWarnings>().report(message, loud));
            }
        });
    });
}

fn cycles_per_second(cycle: &Cycle, speed: f32) -> Option<f32> {
    match cycle.kind {
        CycleKind::Gait { .. } => None,
        _ => Some(1.0 / cycle.period(speed)),
    }
}

/// Steps cycle time `t0` by `rate` (cycles per second) over `dt`: a looping cycle wraps, an
/// action stops at its end. Returns the new time and whether it looped.
fn advance(cycle: &Cycle, t0: f32, rate: f32, dt: f32) -> (f32, bool) {
    let t1 = rate.mul_add(dt, t0);
    if !cycle.kind.loops() {
        (t1.min(1.0), false)
    } else if t1 >= 1.0 {
        (t1.fract(), true)
    } else {
        (t1, false)
    }
}

/// What a cycle stepping from `t0` to `t1` emits: its events in `(t0, t1]` (wrapping when
/// `t1 < t0`) and, with a `root` weight, its root motion scaled by it.
fn emit(cycle: &Cycle, (t0, t1, looped): (f32, f32, bool), root: Option<f32>, leg: f32, out: &mut Out) {
    for e in &cycle.events {
        let hit = if t1 >= t0 { e.at > t0 && e.at <= t1 } else { e.at > t0 || e.at <= t1 };
        if hit {
            out.events.push((e.name.clone(), cycle.name.clone()));
        }
    }
    if let Some(weight) = root {
        let (offset, yaw) = root_delta(cycle, t0, t1, looped);
        out.root += offset * leg * weight;
        out.yaw = yaw.to_radians().mul_add(weight, out.yaw);
    }
}

/// Why `cycle` doesn't fit the rig's body, if it doesn't.
fn misfit(cycle: &Cycle, rig: &MotionRig) -> Option<String> {
    let misfits = cycle.fits.check(&rig.traits);
    (!misfits.is_empty()).then(|| format!("cycle {} doesn't fit a {:?} body: {misfits:?}", cycle.name, rig.traits.family))
}

/// Root displacement (leg lengths, mob space) and yaw (degrees) between `t0` and `t1`.
fn root_delta(cycle: &Cycle, t0: f32, t1: f32, wrapped: bool) -> (Vec3, f32) {
    let Some(r) = &cycle.root else { return (Vec3::ZERO, 0.0) };
    let (a, ya) = r.at(t0);
    let (b, yb) = r.at(t1);
    if wrapped {
        let (end, ye) = r.at(1.0);
        let (start, ys) = r.at(0.0);
        (end - a + b - start, ye - ya + yb - ys)
    } else {
        (b - a, yb - ya)
    }
}

/// This frame's inputs, shared by every layer.
struct Frame<'a> {
    cycles: &'a Assets<Cycle>,
    speed: f32,
    dt: f32,
    /// Root motion is authored in leg lengths.
    leg: f32,
    /// The gait clock wrapped this frame.
    phase_wrapped: bool,
}

#[allow(clippy::too_many_arguments)]
fn step(
    rt: &mut MotionRuntime,
    params: &mut MotionParams,
    loco: &Locomotion,
    clock: &GaitClock,
    play: Option<(&PlayCycle, bool)>,
    leg: f32,
    dt: f32,
    cycles: &Assets<Cycle>,
    out: &mut Out,
) -> (Vec3, f32) {
    rt.active.clear();
    rt.plan = None;
    let frame = Frame { cycles, speed: loco.velocity.length() / leg, dt, leg, phase_wrapped: clock.phase < rt.last_phase };
    rt.last_phase = clock.phase;
    for age in params.triggers.values_mut() {
        *age += dt;
    }
    params.triggers.retain(|_, age| *age <= TRIGGER_LIFETIME + dt);
    let Some(rig) = rt.rig.clone() else { return (Vec3::ZERO, 0.0) };

    if let Some(g) = rt.graph.clone() {
        update_params(rt, &g, params, loco, frame.speed);
        take_transitions(rt, &g, params, cycles);
        for li in 0..g.layers.len() {
            advance_layer(rt, &g, li, &frame, out);
        }
    }
    play_direct(rt, play, &rig, &frame, out);
    (out.root, out.yaw)
}

/// A bool as a parameter value.
fn flag(b: bool) -> f32 {
    f32::from(u8::from(b))
}

/// Parameters: built-ins, then the mob's values.
fn update_params(rt: &mut MotionRuntime, g: &ComposedGraph, params: &MotionParams, loco: &Locomotion, speed: f32) {
    for ((name, kind), value) in g.params.iter().zip(&mut rt.values) {
        *value = match (name.as_str(), kind) {
            ("speed", _) => speed,
            ("moving", _) => flag(speed > 0.05),
            ("turn", _) => loco.turn_rate,
            (_, ParamValue::Float(_)) => params.float(name).unwrap_or(*value),
            (_, ParamValue::Bool(_)) => params.bool(name).map_or(*value, flag),
            (_, ParamValue::Trigger) => flag(params.is_triggered(name)),
        };
    }
}

/// Transitions: at most one per layer per frame.
fn take_transitions(rt: &mut MotionRuntime, g: &ComposedGraph, params: &mut MotionParams, cycles: &Assets<Cycle>) {
    for (li, layer) in g.layers.iter().enumerate() {
        let Some(run) = rt.layers.get(li) else { continue };
        let finished = state_finished(g, li, run, cycles);
        let state = run.state;
        let fire = layer.transitions.iter().enumerate().find(|(_, t)| {
            if t.from.is_some_and(|f| f != state) {
                return false;
            }
            if t.from.is_none() && rt.layers.get(t.to_layer).is_some_and(|r| r.state == t.to) {
                return false;
            }
            eval_condition(&t.when, &rt.values, finished)
        });
        if let Some((ti, t)) = fire {
            for_each_trigger(&t.when, &mut |i| {
                if let Some((name, _)) = g.params.get(i) {
                    params.triggers.remove(name);
                }
            });
            if t.switch == Switch::AtEnd && !finished {
                if let Some(run) = rt.layers.get_mut(li) {
                    run.pending = Some(ti);
                }
            } else {
                enter(rt, g, li, ti);
            }
        }
    }
}

/// Advances layer `li`'s fades and time, and collects what it plays.
fn advance_layer(rt: &mut MotionRuntime, g: &ComposedGraph, li: usize, frame: &Frame, out: &mut Out) {
    let Some(layer) = g.layers.get(li) else { return };
    let Some(mut run) = rt.layers.get(li).cloned() else { return };
    let Some(st) = layer.states.get(run.state) else { return };
    let (speed, cycles) = (frame.speed, frame.cycles);
    run.elapsed += frame.dt;
    // Fading-out state.
    if let Some(f) = &mut run.fade {
        f.left -= frame.dt;
        if f.left <= 0.0 {
            run.fade = None;
        }
    }
    if let Some(f) = run.fade {
        push_state(rt, g, li, f.state, Some(f.t), f.gait, f.weight(), speed, cycles, out);
    }
    let fade_in = 1.0 - run.fade.map_or(0.0, |f| f.weight());
    let wrapped = match &st.play {
        CPlay::Empty => false,
        CPlay::Cycle(c) => play_cycle(rt, (li, layer), *c, st.speed, &mut run, frame, out),
        CPlay::Locomotion(gaits) => {
            let choice = choose_gait(rt, gaits, speed, run.gait, cycles, out);
            if choice != run.gait {
                if let Some(old) = run.gait {
                    run.fade = Some(Fade { state: run.state, t: 0.0, gait: Some(old), left: 0.25, total: 0.25 });
                }
                run.gait = choice;
            }
            if let Some(c) = choice {
                let weight = smoothstep(0.0, 1.0, speed / 0.25) * (1.0 - run.fade.map_or(0.0, |f| f.weight()));
                if rt.plan.is_none() && rt.cycle(Src::Graph(c), cycles).is_some_and(|cycle| cycle.feet.is_some()) {
                    rt.plan = Some(Src::Graph(c));
                }
                rt.active.push(Active { src: Src::Graph(c), t: None, weight, layer: Src::Graph(li) });
            }
            frame.phase_wrapped
        }
        CPlay::Blend { by, points } => {
            let weights = blend_weights(points, rt.values.get(*by).copied().unwrap_or_default());
            // Shared normalised time at the weighted rate.
            let rate: f32 = weights
                .iter()
                .zip(points)
                .filter_map(|(w, &(_, c))| Some(w * cycles_per_second(rt.cycle(Src::Graph(c), cycles)?, speed).unwrap_or(0.0)))
                .sum();
            let mut t1 = f32::mul_add(rate * st.speed, frame.dt, run.t);
            let wrapped = t1 >= 1.0;
            if wrapped {
                t1 = t1.fract();
            }
            for (&w, &(_, c)) in weights.iter().zip(points) {
                if w > 0.0 && rt.fits(c, cycles, out) {
                    let synced = rt.cycle(Src::Graph(c), cycles).is_some_and(|c| cycles_per_second(c, speed).is_none());
                    let t = if synced { None } else { Some(t1) };
                    rt.active.push(Active { src: Src::Graph(c), t, weight: w * fade_in, layer: Src::Graph(li) });
                }
            }
            run.t = t1;
            wrapped
        }
    };
    // A transition waiting for the end.
    let take_pending = wrapped || state_finished(g, li, &run, cycles);
    let pending = if take_pending { run.pending.take() } else { None };
    if let Some(slot) = rt.layers.get_mut(li) {
        *slot = run;
    }
    if let Some(ti) = pending {
        enter(rt, g, li, ti);
    }
}

/// A layer state playing one cycle; returns whether its time wrapped (or an action ended).
fn play_cycle(
    rt: &mut MotionRuntime,
    (li, layer): (usize, &CLayer),
    c: usize,
    rate_scale: f32,
    run: &mut LayerRun,
    frame: &Frame,
    out: &mut Out,
) -> bool {
    let fade_in = 1.0 - run.fade.map_or(0.0, |f| f.weight());
    let Some(cycle) = rt.cycle(Src::Graph(c), frame.cycles).cloned() else { return false };
    let Some(rate) = cycles_per_second(&cycle, frame.speed) else {
        // Synced to the gait phase.
        if rt.fits(c, frame.cycles, out) {
            if rt.plan.is_none() && cycle.feet.is_some() {
                rt.plan = Some(Src::Graph(c));
            }
            rt.active.push(Active { src: Src::Graph(c), t: None, weight: fade_in, layer: Src::Graph(li) });
        }
        return frame.phase_wrapped;
    };
    let t0 = run.t;
    let (t1, looped) = advance(&cycle, t0, rate * rate_scale, frame.dt);
    let action = !cycle.kind.loops();
    let hold = matches!(cycle.kind, CycleKind::Action { hold: true, .. });
    // An action "wraps" the frame it reaches its end.
    let wrapped = looped || (action && t1 >= 1.0 && t0 < 1.0);
    if rt.fits(c, frame.cycles, out) {
        emit(&cycle, (t0, t1, looped), layer.root_motion.then_some(fade_in * layer.weight), frame.leg, out);
        let done = action && t0 >= 1.0;
        if !done || hold {
            rt.active.push(Active { src: Src::Graph(c), t: Some(t1), weight: fade_in, layer: Src::Graph(li) });
        }
    }
    run.t = t1;
    wrapped
}

/// Direct play: starts, fades and advances [`PlayCycle`]s.
fn play_direct(rt: &mut MotionRuntime, play: Option<(&PlayCycle, bool)>, rig: &MotionRig, frame: &Frame, out: &mut Out) {
    match play {
        Some((p, changed)) => {
            if changed || rt.direct.iter().all(|d| d.leaving) {
                for d in &mut rt.direct {
                    d.leaving = true;
                }
                rt.direct.push(DirectRun {
                    play: p.clone(),
                    mask: Some(rig.mask(&p.mask)),
                    fits: None,
                    t: 0.0,
                    elapsed: 0.0,
                    level: if p.blend <= 0.0 { 1.0 } else { 0.0 },
                    leaving: false,
                });
            }
        }
        None => {
            for d in &mut rt.direct {
                d.leaving = true;
            }
        }
    }
    let dt = frame.dt;
    for di in 0..rt.direct.len() {
        let Some(d) = rt.direct.get_mut(di) else { continue };
        let Some(cycle) = d.play.cycle.get(frame.cycles).cloned() else { continue };
        let fade = if d.play.blend <= 0.0 { f32::INFINITY } else { dt / d.play.blend };
        d.level = if d.leaving { d.level - fade } else { d.level + fade }.clamp(0.0, 1.0);
        if d.fits.is_none() {
            let misfit = misfit(&cycle, rig);
            d.fits = Some(misfit.is_none());
            out.misfits.extend(misfit.map(|m| (m, true)));
        }
        let t0 = d.t;
        let t = cycles_per_second(&cycle, frame.speed).map(|rate| {
            let (t1, looped) = advance(&cycle, t0, rate * d.play.speed, dt);
            if !cycle.kind.loops() && t1 >= 1.0 && !d.leaving && !matches!(cycle.kind, CycleKind::Action { hold: true, .. }) {
                out.finished = true;
                d.leaving = true;
            }
            if d.fits == Some(true) {
                emit(&cycle, (t0, t1, looped), d.play.root_motion.then_some(d.level), frame.leg, out);
            }
            d.t = t1;
            t1
        });
        d.elapsed += dt;
        if d.fits == Some(true) && d.level > 0.0 {
            if t.is_none() && cycle.feet.is_some() {
                rt.plan = Some(Src::Direct(di));
            }
            rt.active.push(Active { src: Src::Direct(di), t, weight: 1.0, layer: Src::Direct(di) });
        }
    }
    // Drop faded-out runs (indices in `active` refer to the list before this, so only trailing
    // removal is safe mid-frame; drop from the front when nothing references it).
    if rt.direct.first().is_some_and(|d| d.leaving && d.level <= 0.0) && !rt.active.iter().any(|a| a.src == Src::Direct(0)) {
        rt.direct.remove(0);
        if let Some(Src::Direct(i)) = &mut rt.plan {
            *i = i.saturating_sub(1);
        }
        for a in &mut rt.active {
            if let Src::Direct(i) = &mut a.src {
                *i = i.saturating_sub(1);
            }
            if let Src::Direct(i) = &mut a.layer {
                *i = i.saturating_sub(1);
            }
        }
    }
}

const fn stride(cycle: &Cycle) -> f32 {
    match cycle.kind {
        CycleKind::Gait { stride, .. } => stride,
        _ => 1.0,
    }
}

/// Linear weights between the two blend points around `value`.
fn blend_weights(points: &[(f32, usize)], value: f32) -> SmallVec<[f32; 4]> {
    let mut weights: SmallVec<[f32; 4]> = SmallVec::from_elem(0.0, points.len());
    let mut order: SmallVec<[(usize, f32); 4]> = points.iter().map(|p| p.0).enumerate().collect();
    order.sort_by(|x, y| x.1.total_cmp(&y.1));
    let mut set = |i: usize, w: f32| {
        if let Some(slot) = weights.get_mut(i) {
            *slot = w;
        }
    };
    match (order.first(), order.last()) {
        (Some(&(first, lo)), _) if value <= lo => set(first, 1.0),
        (_, Some(&(last, hi))) if value >= hi => set(last, 1.0),
        _ => {
            let around = order.array_windows().find(|&&[(_, lo), (_, hi)]| value >= lo && value <= hi);
            if let Some(&[(below, lo), (above, hi)]) = around {
                let f = (value - lo) / (hi - lo).max(1e-6);
                set(below, 1.0 - f);
                set(above, f);
            }
        }
    }
    weights
}

/// The gait for `speed` among `gaits`, keeping the current one within a small margin.
fn choose_gait(
    rt: &mut MotionRuntime,
    gaits: &[usize],
    speed: f32,
    current: Option<usize>,
    cycles: &Assets<Cycle>,
    out: &mut Out,
) -> Option<usize> {
    let range = |rt: &MotionRuntime, c: usize| match rt.cycle(Src::Graph(c), cycles).map(|c| &c.kind) {
        Some(CycleKind::Gait { min_speed, max_speed, .. }) => Some((*min_speed, *max_speed)),
        _ => None,
    };
    if let Some(c) = current
        && let Some((lo, hi)) = range(rt, c)
        && speed >= lo - 0.1
        && speed < hi + 0.1
    {
        return Some(c);
    }
    let mut best: Option<(usize, f32)> = None;
    for &c in gaits {
        let Some((lo, hi)) = range(rt, c) else { continue };
        if !rt.fits(c, cycles, out) {
            continue;
        }
        let miss = if speed < lo { lo - speed } else if speed > hi { speed - hi } else { 0.0 };
        if best.is_none_or(|(_, m)| miss < m) {
            best = Some((c, miss));
        }
    }
    best.map(|(c, _)| c)
}

#[allow(clippy::too_many_arguments)]
fn push_state(
    rt: &mut MotionRuntime,
    g: &ComposedGraph,
    li: usize,
    state: usize,
    t: Option<f32>,
    gait: Option<usize>,
    weight: f32,
    speed: f32,
    cycles: &Assets<Cycle>,
    out: &mut Out,
) {
    if weight <= 0.0 {
        return;
    }
    let Some(st) = g.layers.get(li).and_then(|l| l.states.get(state)) else { return };
    let list: SmallVec<[(usize, f32); 4]> = match &st.play {
        CPlay::Empty => SmallVec::new(),
        CPlay::Cycle(c) => smallvec::smallvec![(*c, 1.0)],
        CPlay::Locomotion(_) => gait.map(|c| (c, smoothstep(0.0, 1.0, speed / 0.25))).into_iter().collect(),
        CPlay::Blend { by, points } => {
            let w = blend_weights(points, rt.values.get(*by).copied().unwrap_or_default());
            points.iter().zip(w).map(|((_, c), w)| (*c, w)).collect()
        }
    };
    for (c, w) in list {
        if w > 0.0 && rt.fits(c, cycles, out) {
            rt.active.push(Active { src: Src::Graph(c), t, weight: weight * w, layer: Src::Graph(li) });
        }
    }
}

fn state_finished(g: &ComposedGraph, li: usize, run: &LayerRun, cycles: &Assets<Cycle>) -> bool {
    match g.layers.get(li).and_then(|l| l.states.get(run.state)).map(|s| &s.play) {
        None | Some(CPlay::Empty) => true,
        Some(CPlay::Cycle(c)) => g.cycles.get(*c).and_then(|s| s.get(cycles)).is_some_and(|c| !c.kind.loops() && run.t >= 1.0),
        Some(CPlay::Locomotion(_) | CPlay::Blend { .. }) => false,
    }
}

fn eval_condition(c: &CCondition, values: &[f32], finished: bool) -> bool {
    match c {
        CCondition::Always => true,
        CCondition::Finished => finished,
        CCondition::Param(i, test) => {
            let v = values.get(*i).copied().unwrap_or_default();
            match test {
                CParamTest::Trigger => v > 0.5,
                CParamTest::Bool(b) => (v > 0.5) == *b,
                CParamTest::Float(cmp, x) => cmp.test(v, *x),
            }
        }
        CCondition::All(v) => v.iter().all(|c| eval_condition(c, values, finished)),
        CCondition::Any(v) => v.iter().any(|c| eval_condition(c, values, finished)),
        CCondition::Not(c) => !eval_condition(c, values, finished),
    }
}

/// Calls `f` with every trigger parameter `c` reads.
fn for_each_trigger(c: &CCondition, f: &mut impl FnMut(usize)) {
    match c {
        CCondition::Param(i, CParamTest::Trigger) => f(*i),
        CCondition::All(v) | CCondition::Any(v) => v.iter().for_each(|c| for_each_trigger(c, f)),
        CCondition::Not(c) => for_each_trigger(c, f),
        _ => {}
    }
}

/// Takes transition `ti` of layer `li`.
fn enter(rt: &mut MotionRuntime, g: &ComposedGraph, li: usize, ti: usize) {
    let Some(t) = g.layers.get(li).and_then(|l| l.transitions.get(ti)) else { return };
    let Some(target) = rt.layers.get_mut(t.to_layer) else { return };
    let (old_state, old_t, old_gait) = (target.state, target.t, target.gait);
    target.fade = (t.blend > 0.0).then_some(Fade { state: old_state, t: old_t, gait: old_gait, left: t.blend, total: t.blend });
    target.state = t.to;
    target.t = if t.switch == Switch::Sync { old_t } else { 0.0 };
    target.elapsed = 0.0;
    target.pending = None;
    if t.switch != Switch::Sync {
        target.gait = None;
    }
    // Triggers read this frame stay "on" in `values` until the next frame; clear them so another
    // layer doesn't fire on the same press.
    for_each_trigger(&t.when, &mut |i| {
        if let Some(v) = rt.values.get_mut(i) {
            *v = 0.0;
        }
    });
}

/// Hero tier: what the playing cycles add to this frame's pose, at the clock's phase.
pub(super) fn evaluate_cycles(
    cycles: Res<Assets<Cycle>>,
    bodies: Res<Assets<GeneratedBody>>,
    mut mobs: Query<(&MotionRuntime, &MobBody, &GaitClock, &ActiveStep, &mut CyclePose), With<MobSkeleton>>,
) {
    mobs.par_iter_mut().for_each(|(rt, body, clock, step, mut out)| {
        if !rt.is_playing() {
            out.0 = None;
            return;
        }
        let Some(b) = bodies.get(&body.0) else { return };
        let a = &b.anatomy;
        let legs: SmallVec<[f32; 8]> = (0..a.legs.len()).map(|i| step.leg_phase(clock.phase, &a.legs, i)).collect();
        out.0 = rt.pose(a, &cycles, clock.phase, &legs);
    });
}
