# Motion authoring API (proposal)

Status: **design for review, nothing implemented yet.** It extends §6 of `types.md`. Today's
`Gait` keeps working and becomes one locomotion cycle in this model.

## Goals and decisions so far

- **Users describe cycles as data.** Descriptions are body-relative and work on bodies they
  were never authored for.
- **Strict format.** A closed vocabulary of targets and channels with units and allowed ranges,
  and explicit fit requirements, so cycles that would look wrong are rejected rather than
  played.
- **Three kinds of cycle:**
  - **Locomotion gaits:** speed-driven and foot-planted.
  - **Looping idles.**
  - **One-shot actions:** triggered, with events.
- **Runtime:** a state machine per masked layer. Locomotion is the base layer, and actions and
  idles play on layers masked to parts of the body.
- **Crowd:** each body bakes the cycles its graph actually uses.

## 1. Assets

| Asset | File | What it is |
|---|---|---|
| `Cycle` | `*.cycle.ron` | One loop or one-shot: requirements, feet plan (gaits), tracks, events |
| `MotionGraph` | `*.graph.ron` | Layers, each with a mask and a state machine over cycles |
| `MotionLibrary` | `*.motionlib.ron` | Optional bundle: named cycles + one graph, shared by a family of bodies |

A mob uses `MobMotion(Handle<MotionGraph>)`. Without it, a default graph is chosen from the
body's family (§3), as `DefaultGaits` does today.

## 2. `Cycle`

```rust
#[derive(Asset, Reflect, Serialize, Deserialize, Clone, Debug)]
pub struct Cycle {
    pub name: String,
    pub kind: CycleKind,
    /// Bodies this cycle may play on (§3). Checked once per (cycle, body); misfits never play.
    pub fits: Fit,
    /// Gaits only: when each foot is planted and how it swings.
    pub feet: Option<FootPlan>,
    /// Everything else that moves, on the closed target/channel vocabulary (§4).
    pub tracks: Vec<Track>,
    /// Named moments for gameplay: footsteps, "hit", "bite", "release".
    pub events: Vec<CycleEvent>,
}

pub enum CycleKind {
    /// Speed-driven loop. One cycle = one stride; speed range in leg lengths per second.
    Gait { speed: RangeInclusive<f32>, stride: f32 },
    /// Loop in seconds; plays standing still or on top of moving (via a masked layer).
    Idle { period_s: f32 },
    /// Plays once; `hold` keeps the last pose until released (rear up, sit).
    Action { duration_s: f32, hold: bool },
}

pub struct FootPlan {
    pub pattern: PhasePattern,      // today's patterns: Alternate, Diagonal, Gallop{lead}, Tripod, ...
    pub duty: Bounded<0.2, 0.9>,    // fraction of the cycle on the ground
    pub lift: Bounded<0.0, 0.5>,    // leg lengths at mid-swing
    pub swing: SwingShape,          // Arc | HighStep | Shuffle | Hop
    pub placement: Placement,       // foot lands ahead of the hip by stride/2 (default), or offset
}

pub struct CycleEvent { pub at: f32 /* 0..1 of the cycle */, pub name: String }
```

`Bounded<LO, HI>` is an `f32` that fails to load outside its range. Ranges live in the type, so
an asset can't ask for a 300° knee or a duty of 1.5.

## 3. Fit: which bodies a cycle may play on

```rust
pub struct Fit {
    pub family: Vec<BodyFamily>,            // empty = any
    pub legs: RangeInclusive<u8>,           // locomotor limbs
    pub stance: Vec<Stance>,                // Plantigrade, Digitigrade, Unguligrade, Sprawl, Arched
    pub needs: Vec<Feature>,                // body features that must exist
    pub excludes: Vec<Feature>,             // features that must not (no tail-swish on a manx)
    pub size: Option<RangeInclusive<f32>>,  // leg length in metres (a gallop for horses, not mice)
}

/// Derived from the anatomy, never authored: how the body is laid out for moving.
pub enum BodyFamily {
    UprightBiped,   // humans, goblins, elves: same family, different proportions
    Quadruped,      // dogs, horses, bears, and dragons (wings are a feature, not a family)
    Hexapod,        // insects
    Octopod,        // spiders, crabs
    Myriapod,       // 10+ legs in a row: millipedes, centipedes (wave gaits)
    Serpentine,     // no legs: snakes, worms, eels
    Other,          // anything the classifier can't place; only `family: []` cycles fit
}

/// Add-ons that cycles can require or exclude, independent of family.
pub enum Feature {
    Neck, LongNeck, Tail, LongTail, Jaw, Ears, Antennae, Horns,
    Arms,                   // manipulator limbs besides the locomotor ones (centaurs, mantises)
    Grip(Side),
    Wings,                  // reserved until wings exist: flight cycles will require it
}
```

Families are about **locomotion layout**. Everything else is a feature. The examples you named
map like this:

| Creature | Family | Notable features | Cycles that fit |
|---|---|---|---|
| Human, elf, goblin | UprightBiped | Arms, Grip(L/R), Neck | the same biped library; proportions alone make a goblin's walk read differently (shorter legs → faster cadence at the same speed) |
| Millipede | Myriapod | Antennae | wave gait (`PhasePattern::Wave`), body undulation via `Spine` `Bend` with `Lag` |
| Dragon | Quadruped | LongNeck, LongTail, Wings, Horns, Jaw | quadruped walk/trot (sprawling or digitigrade by stance), plus dragon-only cycles that `need: [Wings]` (wing fold/flare idle; flight once wings exist) |

What the plan format still can't build: **wings**. They're the reserved `PartKind` you deferred.
A dragon today is a wingless quadruped with a long neck and tail. Upright bipeds, goblins and
elves are fully buildable. Millipedes need a `Repeat::Series` of 10+ leg pairs, which the
plan format supports. The crowd skeleton's 32-bone cap means a millipede's crowd tier folds
leg segments: one bone per leg works, two doesn't. (The `Myriapod` gait is designed for that.)

- **Fit check.** `Cycle::fit(&Anatomy) -> Result<FitReport, Misfit>` runs once per
  (cycle, body), cached by `BodyKey`.
- **Misfits.** A misfit lists every unmet requirement ("needs Tail"; "stance Sprawl not in
  [Digitigrade]"). A state whose cycle doesn't fit is **skipped**: its transitions behave as if
  it weren't there, and a `MotionMisfit` message tells the game.
- **Classifying bodies.** `BodyFamily` comes from leg count, torso orientation and manipulator
  limbs. It is what default graphs and libraries key on.

## 4. Tracks: the closed vocabulary

A track drives one **channel** on one **target** with a curve over the cycle (0..1).

```rust
pub struct Track { pub target: Target, pub channel: Channel, pub curve: Curve, pub weight: f32 }

pub enum Target {
    Root,                       // whole-body offsets (the body, not the mob's Transform)
    Spine,                      // spread over spine bones
    Neck, Head, Jaw,
    Tail,                       // spread along the chain with a travelling lag
    Ears, Antennae,
    Legs(LegSel),               // LegSel: All | Fore | Hind | Pair(n) | Side(Left|Right)
    Arms(Side),                 // manipulator limbs
    Grip(Side),                 // the grip socket, for reach targets
}

pub enum Channel {
    // Root
    Bob, Sway, Surge,                    // leg lengths
    Pitch, Roll, Yaw,                    // degrees, ±30
    // Spine / neck / head / tail (rotations spread over the chain)
    Flex, Bend, Twist,                   // degrees per chain, ±60
    Lag,                                 // tail/neck wave delay along the chain, 0..1 of a cycle
    // Jaw / ears
    Open,                                // 0..1 of the jaw's range
    Perk,                                // ears: -1 flat .. 1 up
    // Legs: named joints, resolved per stance (§5)
    JointFlex(LegJoint),                 // degrees added to the rest bend, ±45
    FootLift, FootReach,                 // leg lengths, on top of the FootPlan
    // Arms
    Swing,                               // degrees, counter-phase to the same-side leg by default
    Raise,                               // degrees at the shoulder
    ReachTo(Anchor),                     // blend the grip toward an anchor: Mouth, Head, Chest,
                                         // Ground, Forward(dist), Target (a point the game sets)
}

pub enum Curve {
    /// Sum of sines: the default for loops (never pops at the seam).
    Wave { amplitude: f32, phase: f32, harmonics: Vec<(u8, f32, f32)> },
    /// Keys over 0..1, smooth or linear; loops must start and end equal.
    Keys { keys: Vec<(f32, f32)>, interp: Interp },
    /// Rises over `attack`, holds, falls over `release` (actions).
    Envelope { peak: f32, attack: f32, hold: f32, release: f32 },
}
```

**Rules checked at load:**
- Every (target, channel) pair must be valid. `Jaw` has `Open` but not `Swing`.
- Values must be inside the channel's range.
- Loop curves must close: first and last key equal, or use `Wave`.
- Speeds must be inside the gait's range.

**Unchecked escape hatch: `Target::Part(PartPath)`.** It drives a named part's bones
directly with `Rotate { axis, degrees }`, and is only available when both of these hold:
- the crate is built with the `unchecked-motion` Cargo feature, and
- the cycle file sets `unchecked: true`.

Such a cycle skips the range checks for those tracks, and its `fits` must name the exact body
plan (`plan: "mobs/dragon_boss.body.ron"`), because a part path means nothing on other bodies.
(Not Rust's `unsafe`: nothing here is memory-unsafe. It's "unchecked" in the sense of not
validated.)

When it's needed, roughly in order of likelihood:
- **One-off hero creatures** whose authored parts have their own bones (`Sculpt { own_bone }`,
  a boss's frill or crest flaring). The vocabulary has no generic target for "that sculpt".
- **Masses and sacs:** a frog's throat sac, a dewlap. Better solved by adding
  `Target::Mass(selector)` with an `Inflate` channel.
- **Individual digits:** a hand closing finger by finger, or a claw spread. Better solved by
  `Target::Digits(side)` with `Curl` and `Spread`.
- **Prototyping** a motion before deciding whether it deserves a real channel.

So the hatch exists for bespoke creatures and prototyping. Anything that recurs should become a
proper target/channel instead. **Recommendation:** add `Mass`, `Digits` and `Sculpt` targets
up front and leave the hatch out of the first version.

## 5. Named leg joints instead of indices

`LegJoint` is anatomical, resolved per limb from its stance and segment count:

| LegJoint | Plantigrade (2 seg) | Digitigrade (3 seg) | Unguligrade (4 seg) |
|---|---|---|---|
| `Hip` / `Shoulder` | segment 0 | segment 0 | segment 0 |
| `Knee` / `Elbow` | 1 | 1 | 1 |
| `Ankle` / `Hock` / `Wrist` | foot end | 2 | 2 |
| `Fetlock` | — | — | 3 |

A track on a joint the limb doesn't have fits nothing on that limb: it's skipped, and the fit
report says so. This is where fetlock flex and hock folding during the swing come from.

## 6. `MotionGraph`: masked layers × state machines

```rust
#[derive(Asset)]
pub struct MotionGraph {
    pub params: Vec<ParamDef>,          // float / bool / trigger, with defaults
    pub layers: Vec<Layer>,             // evaluated bottom to top
}

pub struct Layer {
    pub name: String,
    pub mask: BodyMask,                 // which bones this layer may move
    pub mode: LayerMode,                // Override (replace) | Additive (add to below)
    pub states: Vec<State>,
    pub transitions: Vec<Transition>,
    pub entry: String,
}

pub enum BodyMask {
    All,
    Upper,          // everything above the hips: spine front half, neck, head, arms
    Lower,          // legs + rear spine
    HeadNeck, Tail, Arm(Side),
    Regions(RegionSelector),            // the selectors coats and clothing already use
}

pub struct State {
    pub name: String,
    pub play: Play,
    pub speed: f32,                     // playback rate
}

pub enum Play {
    /// Base locomotion: picks and crossfades gait cycles by the `speed` param (a 1D blend).
    Locomotion { gaits: Vec<String> },
    Cycle(String),
    /// Several cycles mixed by a param (idle variations, injured limp amount).
    Blend { by: String, cycles: Vec<(f32, String)> },
    Empty,                              // layer contributes nothing (weight fades to 0)
}

pub struct Transition {
    pub from: From,                     // State(name) | Any
    pub to: String,
    pub when: Condition,
    pub blend_s: f32,
    pub interrupt: bool,                // may cut an action mid-play
}

pub enum Condition {
    Trigger(String), Bool(String, bool), Float(String, Cmp, f32),
    Finished,                           // the current action ended
    All(Vec<Condition>), Any(Vec<Condition>), Not(Box<Condition>),
}
```

Example: a biped can attack while walking.

```ron
(
  params: [Float("speed", 0.0), Trigger("attack"), Bool("alert", false)],
  layers: [
    (name: "base", mask: All, mode: Override, entry: "move",
     states: [(name: "move", play: Locomotion(gaits: ["walk", "run"]))]),
    (name: "idle", mask: HeadNeck, mode: Additive, entry: "calm",
     states: [(name: "calm", play: Cycle("look_around")), (name: "alert", play: Cycle("scan"))],
     transitions: [(from: Any, to: "alert", when: Bool("alert", true), blend_s: 0.3),
                   (from: Any, to: "calm", when: Bool("alert", false), blend_s: 0.6)]),
    (name: "action", mask: Upper, mode: Override, entry: "none",
     states: [(name: "none", play: Empty), (name: "swing", play: Cycle("overhead_strike"))],
     transitions: [(from: State("none"), to: "swing", when: Trigger("attack"), blend_s: 0.1),
                   (from: State("swing"), to: "none", when: Finished, blend_s: 0.25)]),
  ],
)
```

### Composing graphs

A mob runs **several graphs at once**, combined into one at load time:
`MobMotion(vec![locomotion, combat, emotes])`.

```rust
#[derive(Component)]
pub struct MobMotion(pub Vec<Handle<MotionGraph>>);
```

**How graphs combine (union):**
- **Params** are unioned by name. A name declared twice with different types is a load
  error.
- **Layers** are matched by name.
  - Same name: their states and transitions are merged. A duplicate state name is an error
    unless the later graph marks it `override: true`.
  - New names: the layer is stacked. Order is graph order, then the graph's own layer order.
  - A layer may also set `order: i32` to slot itself between others.
- **Connecting.** A transition may target a state in another graph's layer by
  `"graph/layer/state"`. That's how an emotes graph can say "only from `base/move`".

**Hierarchy:** `Play::Graph(handle)` lets a state run a whole sub-graph. Example: a "mounted"
state that runs a riding graph. This is a hierarchical state machine, which keeps big
behaviours modular.

**Composition is resolved once per (set of graphs, body) and cached**, the same way bodies are
cached by `BodyKey`:
- Fit checks, mask resolution and transition target lookups all become integer indices.
- At runtime, a mob's state per layer is a few `u16`s and timers, with no string or hash work.

**Crates considered:**
- [Bevy's `AnimationGraph`](https://dev-docs.bevyengine.org/bevy/prelude/struct.AnimationGraph.html)
  (with [mask groups and additive blends](https://bevyengine.org/examples-webgpu/animation/animation-masks))
  blends recorded `AnimationClip` curves on entities.
- [`bevy_animation_graph`](https://github.com/mbrea-c/bevy_animation_graph) adds state-machine
  nodes and an editor, on the same clip-based model.
- Neither can run procedural cycles (foot planting, IK, springs), nor drive the GPU crowd tier.
- General state-machine crates ([`seldom_state`](https://awesome.ecosyste.ms/lists/nolantait%2Fawesome-bevy?topic=2d),
  `statig`, `moonshine_behavior`) model gameplay states as ECS components. They don't support
  per-mob blending or layer weights.
- The state machine itself is tiny next to pose evaluation (a few comparisons per layer per
  mob). A fast graph crate wouldn't change frame time.
- `petgraph` could still be useful at **load time**, to validate composed graphs (unreachable
  states, transitions to missing states, cycles of instant transitions).

**Gameplay side.**
- `MobIntent` stays as it is. The plugin writes `speed` from velocity.
- `MotionParams` (component) sets bools, floats and triggers.
- `MotionEvent { mob, name }` messages carry footsteps and action events.
- `MotionState` exposes the active state per layer.

**Order per frame:**
1. Each layer's state machine advances.
2. Each layer evaluates its cycle(s) into joint offsets and foot targets, in its mask.
3. Layers are composed bottom to top.
4. Leg IK runs last, so a feet plan from any layer is honoured and feet stay planted under
   upper-body actions.

## 7. Crowd tier

- **What gets baked.** Each body bakes only the cycles its graph references and fits, into the
  existing clip atlas:
  - one clip per gait;
  - one per idle;
  - one per action, as a non-looping clip.
- **Instance data.** `CrowdInstance` grows from "A/B/idle" to **up to 3 layers**. Each layer
  has a frame pair and a weight, and points at a per-body **mask row** (one weight per crowd
  bone) baked with the clips. The compute pass blends per bone: `Σ layer_weight × mask[bone] ×
  frame`.
- **Coarser masks.** Masks resolve against the crowd skeleton, so `Arm(Left)` on a body whose
  forearm folded into the upper arm moves the whole arm.
- **Events** still fire for crowd mobs: they come from the state machine on the CPU, not from
  the GPU.

## 8. Built-in library

Ships as data, so it doubles as examples:
- `upright_biped.motionlib.ron`: walk, run, idle_breathe, look_around, overhead_strike,
  eat (`ReachTo(Mouth)`).
- `quadruped.motionlib.ron`: walk, trot, canter, gallop, sniff, shake, sit (hold).
  - Variants by stance via `fits`: ungulates get fetlock flex, plantigrades a rolling walk.
- `hexapod.motionlib.ron`: tripod, wave, antenna_twitch.

## Root motion

Actions (and some gaits, like hops) may move the mob, with a `root` block in the cycle:

```rust
pub struct RootMotion {
    /// Displacement over the cycle, in leg lengths (forward, up, side), and yaw in degrees.
    pub forward: Curve, pub up: Curve, pub side: Curve, pub yaw: Curve,
}
```

The plugin turns it into a per-frame delta.
- **Default:** the plugin applies the delta to the mob's `Transform`, just as it applies
  `MobIntent` velocity.
- **Under `ExternalRoot`:** the plugin only reports the delta as
  `RootMotionDelta { translation, yaw }` on the mob. The game's physics or navmesh controller
  applies it or vetoes it.

**Pros:**
- Feet don't slide in actions that travel (lunge, pounce, dodge, rear-up step-back,
  sit-down shuffle, turn-in-place steps). Today those would skate.
- Gameplay can trust the motion: a lunge covers exactly the reach the animation shows.
  Because it's in leg lengths, a goblin's lunge is shorter than an ogre's.
- Hops, jumps and bounding gaits become expressible (`up` curve).

**Cons:**
- **Ownership.** Position now has two writers. Without `ExternalRoot` the plugin can push a mob
  into a wall; with it, the game must apply the delta itself.
- **Networking and determinism.** A server must run the same cycle (or receive the delta) to
  stay in sync. That's fine, because cycles are data and deterministic, but it's one more thing
  to replicate.
- **AI and nav.** A pathfinder planning "walk to X" must allow for an action carrying the mob
  forward. `Cycle::root_displacement(body)` exposes the total so AI can plan with it.
- **Crowd.** The crowd tier applies root motion on the CPU from the same curve (cheap, no GPU
  change), so distant mobs move identically.

## Decisions (from review)

1. **Families:** layout families (UprightBiped, Quadruped, Hexapod, Octopod, Myriapod,
   Serpentine, Other) plus `Feature`s for add-ons. Humans, goblins and elves share a family.
   A dragon is a quadruped with `Wings`/`LongNeck`/`LongTail`. Wings themselves remain
   unbuilt.
2. **Escape hatch:** possible as an `unchecked-motion` feature plus a per-file `unchecked: true`,
   plan-specific. Not in v1. Add `Mass`/`Digits`/`Sculpt` targets instead.
3. **Code-defined channels:** no. They'd need trait objects in the hot path, string-keyed
   serialization and a determinism contract for baking, all for little gain. Adding a channel
   means editing the crate.
4. **Graphs:** compose by union (params and layers by name, explicit overrides) plus
   hierarchical sub-graphs, resolved and cached at load. Built in, not from a crate.
5. **Root motion:** yes, per cycle. The plugin applies it by default, or reports it under
   `ExternalRoot`.

## Build order

Decided: **wings first** (`docs/design/wings.md`), then this API, in this order:

1. **`Cycle` with `Fit` and the track vocabulary.**
   - Includes the `Mass`, `Digits`, `Sculpt` and `Wings` targets, plus loaders and validation.
   - Includes a direct `play_cycle` API for testing.
2. **Root motion.** It's only more curves on a cycle, so it needs nothing but cycle playback,
   and it lets a lunge or a hop be tested end to end before graphs exist. When graphs arrive
   they only decide *which* layer's root motion counts: the base layer, or an action layer
   flagged `root_motion: true`.
3. **Built-in libraries:** biped, quadruped, hexapod, myriapod, and winged (fold, flare,
   flap).
4. **`MotionGraph`** with layers, state machines and composition.
5. **Crowd baking** of used cycles, with three masked layers on the GPU.
