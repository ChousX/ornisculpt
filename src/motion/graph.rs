//! Motion graphs: masked layers, each running a state machine over cycles.
//!
//! - **Layers** stack bottom to top. Each has a [`BodyMask`] (which bones it may move) and a
//!   [`LayerMode`] (`Override` replaces what's below where it touches, `Additive` adds to it),
//!   like Unity's layers + avatar masks or Unreal's slots.
//! - **States** play a cycle, a speed-driven [`Play::Locomotion`] set of gaits (a 1D blend space
//!   over speed), a parameter-driven [`Play::Blend`], or nothing.
//! - **Transitions** fire on [`Condition`]s over parameters (floats, bools, triggers) or when an
//!   action finishes, blending over `blend` seconds; [`Switch`] decides when (immediately, at the
//!   end of the current cycle, or in sync with its phase — Godot's switch modes).
//! - **Composition**: a mob runs several graphs at once (`MobMotion(vec![locomotion, combat])`).
//!   Parameters are unioned by name; layers with the same name merge their states and
//!   transitions; transitions may target `"layer/state"` across graphs.
//!
//! Graphs are data (`*.graph.ron`, cycles inline or by asset path) or built in Rust.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::cycle::{Cycle, CycleError, Sides, Target};

/// A set of layers, each a state machine over cycles.
#[derive(Asset, TypePath, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct MotionGraph {
    pub params: Vec<Param>,
    /// Cycles defined in this graph, referenced by name.
    pub cycles: Vec<Cycle>,
    pub layers: Vec<Layer>,
    /// Cycle assets referenced by path, loaded with the graph.
    #[serde(skip)]
    pub loaded: HashMap<String, Handle<Cycle>>,
}

/// A named value gameplay sets ([`MotionParams`](super::MotionParams)). `speed` (leg lengths
/// per second), `moving` and `turn` (radians per second) are always present and set by the
/// plugin.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub value: ParamValue,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum ParamValue {
    Float(f32),
    Bool(bool),
    /// Set by gameplay, consumed by the first transition it fires.
    Trigger,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Layer {
    pub name: String,
    pub mask: BodyMask,
    pub mode: LayerMode,
    /// 0..1.
    pub weight: f32,
    /// Stacking order across composed graphs (lower first; ties keep graph order).
    pub order: i32,
    /// Whether this layer's cycles may move the mob (root motion).
    pub root_motion: bool,
    pub entry: String,
    pub states: Vec<State>,
    pub transitions: Vec<Transition>,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            name: String::new(),
            mask: BodyMask::All,
            mode: LayerMode::Override,
            weight: 1.0,
            order: 0,
            root_motion: true,
            entry: String::new(),
            states: Vec::new(),
            transitions: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerMode {
    /// Replaces the layers below wherever this layer drives a bone.
    #[default]
    Override,
    /// Adds on top of the layers below.
    Additive,
}

/// Which bones a layer may move.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum BodyMask {
    #[default]
    All,
    /// Front half of the spine, neck, head, jaw, ears, antennae, arms, digits, wings.
    Upper,
    /// Legs, rear half of the spine, tails.
    Lower,
    HeadNeck,
    Tail,
    Wings,
    Arms(Sides),
    /// Exactly these targets.
    Targets(Vec<Target>),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct State {
    pub name: String,
    pub play: Play,
    /// Playback rate.
    pub speed: f32,
    /// When composing graphs, replace a same-named state instead of erroring.
    pub replace: bool,
}

impl Default for State {
    fn default() -> Self {
        Self { name: String::new(), play: Play::Empty, speed: 1.0, replace: false }
    }
}

/// What a state plays. Cycle references are names of inline cycles or asset paths.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum Play {
    /// Nothing: the layer fades out of the pose.
    #[default]
    Empty,
    Cycle(String),
    /// Gait cycles chosen by the `speed` parameter (each gait's speed range); feet are planted by
    /// the chosen gait's foot plan.
    Locomotion(Vec<String>),
    /// Cycles mixed by a float parameter: `(value, cycle)` points, linearly between neighbours.
    Blend { by: String, cycles: Vec<(f32, String)> },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Transition {
    pub from: From,
    /// A state of this layer, or `"layer/state"`.
    pub to: String,
    pub when: Condition,
    /// Crossfade, seconds.
    pub blend: f32,
    pub switch: Switch,
}

impl Default for Transition {
    fn default() -> Self {
        Self { from: From::Any, to: String::new(), when: Condition::Always, blend: 0.2, switch: Switch::Immediate }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum From {
    #[default]
    Any,
    State(String),
}

/// When a transition takes effect once its condition holds.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Switch {
    #[default]
    Immediate,
    /// Waits for the current cycle to finish (or loop).
    AtEnd,
    /// Starts the new cycle at the current one's phase (gait to gait).
    Sync,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub enum Condition {
    #[default]
    Always,
    Trigger(String),
    Bool(String, bool),
    Float(String, Cmp, f32),
    /// The current state's action has finished.
    Finished,
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cmp {
    pub fn test(self, a: f32, b: f32) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------------------------

impl MotionGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn param(mut self, name: &str, value: ParamValue) -> Self {
        self.params.push(Param { name: name.into(), value });
        self
    }

    /// Adds an inline cycle (referenced by its name).
    pub fn cycle(mut self, cycle: Cycle) -> Self {
        self.cycles.push(cycle);
        self
    }

    pub fn layer(mut self, layer: Layer) -> Self {
        self.layers.push(layer);
        self
    }
}

impl Layer {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), ..default() }
    }

    pub fn mask(mut self, mask: BodyMask) -> Self {
        self.mask = mask;
        self
    }

    pub fn additive(mut self) -> Self {
        self.mode = LayerMode::Additive;
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Adds a state; the first state added is the entry unless [`entry`](Self::entry) says
    /// otherwise.
    pub fn state(mut self, name: &str, play: Play) -> Self {
        if self.entry.is_empty() {
            self.entry = name.into();
        }
        self.states.push(State { name: name.into(), play, ..default() });
        self
    }

    pub fn entry(mut self, name: &str) -> Self {
        self.entry = name.into();
        self
    }

    /// `from` → `to` when `when` holds, crossfading over `blend` seconds.
    pub fn transition(mut self, from: &str, to: &str, when: Condition, blend: f32) -> Self {
        let from = if from == "*" { From::Any } else { From::State(from.into()) };
        self.transitions.push(Transition { from, to: to.into(), when, blend, switch: Switch::Immediate });
        self
    }

    /// Like [`transition`](Self::transition) with an explicit [`Switch`].
    pub fn transition_with(mut self, from: &str, to: &str, when: Condition, blend: f32, switch: Switch) -> Self {
        self = self.transition(from, to, when, blend);
        if let Some(t) = self.transitions.last_mut() {
            t.switch = switch;
        }
        self
    }
}

impl Condition {
    pub fn trigger(name: &str) -> Self {
        Condition::Trigger(name.into())
    }

    pub fn is(name: &str, value: bool) -> Self {
        Condition::Bool(name.into(), value)
    }

    pub fn above(name: &str, value: f32) -> Self {
        Condition::Float(name.into(), Cmp::Gt, value)
    }

    pub fn below(name: &str, value: f32) -> Self {
        Condition::Float(name.into(), Cmp::Lt, value)
    }

    pub fn and(self, other: Condition) -> Self {
        Condition::All(vec![self, other])
    }

    pub fn or(self, other: Condition) -> Self {
        Condition::Any(vec![self, other])
    }

    pub fn not(self) -> Self {
        Condition::Not(Box::new(self))
    }
}

// ---------------------------------------------------------------------------------------------
// Composition
// ---------------------------------------------------------------------------------------------

/// Where a composed graph's cycle comes from.
#[derive(Clone, Debug)]
pub enum CycleSource {
    Inline(Arc<Cycle>),
    Asset(Handle<Cycle>),
}

impl CycleSource {
    pub fn get<'a>(&'a self, assets: &'a Assets<Cycle>) -> Option<&'a Cycle> {
        match self {
            CycleSource::Inline(c) => Some(c),
            CycleSource::Asset(h) => assets.get(h),
        }
    }
}

/// A composed state's play with cycle references resolved to indices into `cycles`.
#[derive(Clone, Debug)]
pub enum CPlay {
    Empty,
    Cycle(usize),
    Locomotion(Vec<usize>),
    Blend { by: usize, points: Vec<(f32, usize)> },
}

#[derive(Clone, Debug)]
pub struct CState {
    pub name: String,
    pub play: CPlay,
    pub speed: f32,
}

#[derive(Clone, Debug)]
pub enum CCondition {
    Always,
    Param(usize, CParamTest),
    Finished,
    All(Vec<CCondition>),
    Any(Vec<CCondition>),
    Not(Box<CCondition>),
}

#[derive(Clone, Copy, Debug)]
pub enum CParamTest {
    Trigger,
    Bool(bool),
    Float(Cmp, f32),
}

#[derive(Clone, Debug)]
pub struct CTransition {
    pub from: Option<usize>,
    pub to_layer: usize,
    pub to: usize,
    pub when: CCondition,
    pub blend: f32,
    pub switch: Switch,
}

#[derive(Clone, Debug)]
pub struct CLayer {
    pub name: String,
    pub mask: BodyMask,
    pub mode: LayerMode,
    pub weight: f32,
    pub root_motion: bool,
    pub entry: usize,
    pub states: Vec<CState>,
    pub transitions: Vec<CTransition>,
}

/// Several graphs merged into one, names resolved to indices (cached per set of graphs).
#[derive(Clone, Debug)]
pub struct ComposedGraph {
    pub params: Vec<(String, ParamValue)>,
    pub cycles: Vec<CycleSource>,
    pub layers: Vec<CLayer>,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum GraphError {
    #[error("parameter {0} is declared with two different types")]
    ParamType(String),
    #[error("layer {layer}: state {state} is defined twice (mark the later one `replace: true`)")]
    DuplicateState { layer: String, state: String },
    #[error("layer {layer}: no state named {state}")]
    MissingState { layer: String, state: String },
    #[error("no layer named {0}")]
    MissingLayer(String),
    #[error("no cycle named {0} (inline cycles are referenced by name, others by asset path)")]
    MissingCycle(String),
    #[error("unknown parameter {0}")]
    MissingParam(String),
    #[error("inline cycle {name}: {errors:?}")]
    InvalidCycle { name: String, errors: Vec<CycleError> },
}

/// Built-in parameters, always present.
pub const BUILTIN_PARAMS: [(&str, ParamValue); 3] =
    [("speed", ParamValue::Float(0.0)), ("moving", ParamValue::Bool(false)), ("turn", ParamValue::Float(0.0))];

fn same_kind(a: ParamValue, b: ParamValue) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

/// Merges `graphs` (in order) into one graph. See the module docs for the rules.
pub fn compose(graphs: &[&MotionGraph]) -> Result<ComposedGraph, GraphError> {
    let mut params: Vec<(String, ParamValue)> = BUILTIN_PARAMS.iter().map(|(n, v)| (n.to_string(), *v)).collect();
    for g in graphs {
        for p in &g.params {
            match params.iter().find(|(n, _)| *n == p.name) {
                Some((_, v)) if !same_kind(*v, p.value) => return Err(GraphError::ParamType(p.name.clone())),
                Some(_) => {}
                None => params.push((p.name.clone(), p.value)),
            }
        }
    }
    // Merge layers by name, ordered by `order`, then first appearance.
    let mut merged: Vec<(i32, Layer)> = Vec::new();
    for g in graphs {
        for l in &g.layers {
            match merged.iter_mut().find(|(_, m)| m.name == l.name) {
                Some((_, m)) => {
                    for s in &l.states {
                        match m.states.iter().position(|x| x.name == s.name) {
                            Some(i) if s.replace => m.states[i] = s.clone(),
                            Some(_) => return Err(GraphError::DuplicateState { layer: l.name.clone(), state: s.name.clone() }),
                            None => m.states.push(s.clone()),
                        }
                    }
                    m.transitions.extend(l.transitions.iter().cloned());
                }
                None => merged.push((l.order, l.clone())),
            }
        }
    }
    merged.sort_by_key(|(o, _)| *o);
    let layers: Vec<Layer> = merged.into_iter().map(|(_, l)| l).collect();

    // Cycles: inline by name across all graphs, then loaded asset paths.
    let mut cycles: Vec<CycleSource> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for g in graphs {
        for c in &g.cycles {
            if let Err(errors) = c.validate() {
                return Err(GraphError::InvalidCycle { name: c.name.clone(), errors });
            }
            by_name.entry(c.name.clone()).or_insert_with(|| {
                cycles.push(CycleSource::Inline(Arc::new(c.clone())));
                cycles.len() - 1
            });
        }
    }
    for g in graphs {
        for (path, h) in &g.loaded {
            by_name.entry(path.clone()).or_insert_with(|| {
                cycles.push(CycleSource::Asset(h.clone()));
                cycles.len() - 1
            });
        }
    }
    let cycle = |name: &str| by_name.get(name).copied().ok_or_else(|| GraphError::MissingCycle(name.into()));
    let param = |name: &str| params.iter().position(|(n, _)| n == name).ok_or_else(|| GraphError::MissingParam(name.into()));

    fn condition(
        c: &Condition,
        param: &dyn Fn(&str) -> Result<usize, GraphError>,
    ) -> Result<CCondition, GraphError> {
        Ok(match c {
            Condition::Always => CCondition::Always,
            Condition::Trigger(n) => CCondition::Param(param(n)?, CParamTest::Trigger),
            Condition::Bool(n, v) => CCondition::Param(param(n)?, CParamTest::Bool(*v)),
            Condition::Float(n, cmp, v) => CCondition::Param(param(n)?, CParamTest::Float(*cmp, *v)),
            Condition::Finished => CCondition::Finished,
            Condition::All(v) => CCondition::All(v.iter().map(|c| condition(c, param)).collect::<Result<_, _>>()?),
            Condition::Any(v) => CCondition::Any(v.iter().map(|c| condition(c, param)).collect::<Result<_, _>>()?),
            Condition::Not(c) => CCondition::Not(Box::new(condition(c, param)?)),
        })
    }

    let state_index = |layer: &Layer, name: &str| {
        layer
            .states
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| GraphError::MissingState { layer: layer.name.clone(), state: name.into() })
    };
    let mut out_layers = Vec::with_capacity(layers.len());
    for (li, l) in layers.iter().enumerate() {
        let mut states = Vec::with_capacity(l.states.len());
        for s in &l.states {
            let play = match &s.play {
                Play::Empty => CPlay::Empty,
                Play::Cycle(c) => CPlay::Cycle(cycle(c)?),
                Play::Locomotion(gaits) => CPlay::Locomotion(gaits.iter().map(|g| cycle(g)).collect::<Result<_, _>>()?),
                Play::Blend { by, cycles: points } => CPlay::Blend {
                    by: param(by)?,
                    points: points.iter().map(|(v, c)| cycle(c).map(|i| (*v, i))).collect::<Result<_, _>>()?,
                },
            };
            states.push(CState { name: s.name.clone(), play, speed: s.speed });
        }
        let mut transitions = Vec::with_capacity(l.transitions.len());
        for t in &l.transitions {
            let from = match &t.from {
                From::Any => None,
                From::State(s) => Some(state_index(l, s)?),
            };
            let (to_layer, to) = match t.to.split_once('/') {
                Some((ln, sn)) => {
                    let li2 = layers.iter().position(|x| x.name == ln).ok_or_else(|| GraphError::MissingLayer(ln.into()))?;
                    (li2, state_index(&layers[li2], sn)?)
                }
                None => (li, state_index(l, &t.to)?),
            };
            transitions.push(CTransition { from, to_layer, to, when: condition(&t.when, &param)?, blend: t.blend.max(0.0), switch: t.switch });
        }
        let entry = if l.entry.is_empty() { 0 } else { state_index(l, &l.entry)? };
        out_layers.push(CLayer {
            name: l.name.clone(),
            mask: l.mask.clone(),
            mode: l.mode,
            weight: l.weight.clamp(0.0, 1.0),
            root_motion: l.root_motion,
            entry,
            states,
            transitions,
        });
    }
    Ok(ComposedGraph { params, cycles, layers: out_layers })
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

/// Loads `*.graph.ron`: cycle references that aren't inline cycle names load as assets.
#[derive(Default, TypePath)]
pub struct GraphLoader;

#[derive(Debug, Error)]
pub enum GraphLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
    #[error("invalid inline cycle {name}: {errors:?}")]
    Invalid { name: String, errors: Vec<CycleError> },
}

impl AssetLoader for GraphLoader {
    type Asset = MotionGraph;
    type Settings = ();
    type Error = GraphLoaderError;

    async fn load(&self, reader: &mut dyn Reader, _: &(), ctx: &mut LoadContext<'_>) -> Result<MotionGraph, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mut graph: MotionGraph = ron::de::from_bytes(&bytes)?;
        for c in &graph.cycles {
            c.validate().map_err(|errors| GraphLoaderError::Invalid { name: c.name.clone(), errors })?;
        }
        let inline: Vec<String> = graph.cycles.iter().map(|c| c.name.clone()).collect();
        let mut refs: Vec<String> = Vec::new();
        for l in &graph.layers {
            for s in &l.states {
                match &s.play {
                    Play::Cycle(c) => refs.push(c.clone()),
                    Play::Locomotion(v) => refs.extend(v.iter().cloned()),
                    Play::Blend { cycles, .. } => refs.extend(cycles.iter().map(|(_, c)| c.clone())),
                    Play::Empty => {}
                }
            }
        }
        for r in refs.into_iter().filter(|r| !inline.contains(r) && r.ends_with(".cycle.ron")) {
            let handle = ctx.load(r.clone());
            graph.loaded.insert(r, handle);
        }
        Ok(graph)
    }

    fn extensions(&self) -> &[&str] {
        &["graph.ron"]
    }
}
