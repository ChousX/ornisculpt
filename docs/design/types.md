# Ornisculpt: types and traits

The crate's reference: what each subsystem is made of and why. Checked against the code at
e1f355d (2026-10-31). Where another design doc covers a subsystem in depth, the section here
summarises it and links to it.

> How to read: decisions are tagged **[T-x]**. A tag marked *revised* kept its number but the
> decision changed; the text says what it is now. Measured numbers carry the date they were
> taken; `bench/LOG.md` has the current ones.

---

## 0. Settled direction

| Topic | Decision |
|---|---|
| Crate | **One crate**, `ornisculpt`, on Bevy 0.20. Modules are listed in §1. |
| Math | `bevy::math` (`Vec3`, `Vec3A`, `Quat`, `Affine3A`, `Mat4`) and `Transform`. No custom transform type. |
| Composition | A mob = **`BodyPlan` + `Form` + coat + outfit + gait + motion graph** (+ held tools). There is no species concept. Sharing comes from **content hashes** (§7). |
| Part tree | **Hybrid.** The `BodyPlan` asset is the authored source. A spawned mob keeps its own expanded copy (`LiveBody`) and **Part entities** over it. Live edits trigger regeneration. |
| Regions | **Derived from parts.** Each part kind emits its own regions. Coats, markings, clothing and masks pick them with **selectors**. |
| Symmetry | `mirror` and `repeat` on a part. `expand_symmetry` makes every copy an explicit node, so one copy can be patched, severed or made to differ. |
| Variation | `Form` is a list of **levers** (named knobs with fixed meanings), never reflect paths. The **game always supplies it**; the plugin does no random sampling. |
| Determinism | Anatomy is a pure function of the plan. Seeds only feed surface noise. RNG is `rand_pcg::Pcg64Mcg`, forked by `xxh3(seed, label)`. Noise is `fastnoise-lite` through a serializable `NoiseSettings` (`src/random.rs`). |
| Runtime change | **Levers** the game drives: per-bone scale, part edits, sever and graft (async regen). The plugin never defines age, fat or injury. |
| Control | A `MobIntent` component, and optionally motion graphs. The plugin owns all bone motion. |
| Ground | The game's physics. The plugin ships none; it writes `FootProbes` and reads `FootGround` hits. |
| Cameras | Top-down and first-person. Tiering uses **screen-space size** over `TierCamera`s, not distance. |
| Hardware | Runs on low-end and scales up through a `QualityProfile` resource. The Steam Deck is the measured target (`bench/LOG.md`, 2026-10-31). |
| Bones | **≤ 128** per hero mob (fingers included; 64 until 2026-10-31). **≤ 32** in the crowd skeleton, where fine parts fold into their parent. Crowd bones are GPU data, not entities. |
| Region cap | **≤ 64** per mob (a `u64` mask). Hard parts take none of their own. |
| Tools | **Hold only.** A held tool is any entity; the plugin places it. |
| Coverings | Scales in the shader, fur as shells and hair cards, feathers as cards, markings baked per vertex. |
| Wings | Built (§2.11, `wings.md`). Poses now, flight later. |
| Genetics | Built behind the `genetics` feature (§2.12). The data stays plain, ordered and reflectable so a genome can hold it. |
| Part LOD | Parts have detail levels. Small parts drop out of the mesh, the skeleton and the ECS at coarser tiers (§2.9). |
| Cache | In memory, by `BodyKey`, for the session. No disk cache yet (`roadmap.md`, Phase 2). |
| Tooling | Authoring tools live in **ornisculpter**, a separate app: the live editor, glTF export and convert, the Blender add-on. It draws with ornisculpt's `preview` feature. **Examples** test by eye and **the bench** guards performance (§12). |

---

## 1. Module map

`README.md` has the module table, the feature flags, the examples and the consumers. In short:

```
ornisculpt/src/
├─ lib.rs          OrnisculptPlugin (adds every sub-plugin)
├─ anatomy/        BodyPlan, parts, Form and levers, regions, build → Anatomy, tone stamps,
│                  viability, wings, genomes (genetics)
├─ surface/        Field, BodyField, meshers, binding, LODs, centres of rotation, muscle flex,
│                  contacts, hard parts, the GPU preview (preview, *.wesl)
├─ coat/           Coat asset, markings, fur shells, feather and hair cards, MobMaterial, shaders
├─ clothing/       Outfit asset, garment fitting, hidden skin
├─ equip/          sockets, HeldBy, tool placement
├─ motion/         MobIntent, gaits, pose stages, IK, cycles, graphs, the built-in library,
│                  hands, baked crowd motion, the crowd ground fit
├─ mob/            Mob, generation and its cache, tiers, crowd skinning (crowd_blend.wesl), levers
├─ soft.rs         soft tissue (soft-tissue)
├─ interchange/    glTF in and out (interchange)
├─ ron_loader.rs   the one loader behind every *.ron asset
└─ num.rs, random.rs
examples/          visual test beds and the bench (§12)
assets/            mobs/*.body.ron, coats/*.coat.ron, outfits/*.outfit.ron, parts/*.gltf, garments/
tools/             asset generators (Python), web.sh, deck-bench.sh
```

Cargo features, all off by default: `joint-limits`, `soft-tissue`, `genetics`, `preview`,
`interchange`, `web`.

- **T-1** Plain functions and slices are used **only** for hot inner loops: SDF evaluation,
  meshing, IK math and skinning math. Everything else is ECS: components, systems, observers,
  assets and relationships.
- **T-2** (*revised*) All authored data derives `Reflect + Serialize + Deserialize + Clone`, so it
  works with the inspector, RON assets, levers and genomes. Overrides go through levers, not
  reflect paths (§2.4).
- **T-3** One Bevy version, pinned: 0.20. Required components, relationships and observers are
  used throughout.
- Every `*.ron` asset loads through `ron_loader::RonLoader<A>`, keyed by extension
  (`body.ron`, `coat.ron`, `outfit.ron`, `gait.ron`, `cycle.ron`, `graph.ron`).
- `num.rs` holds the crate's only `as` casts, as checked conversions (`Idx`, `ToF32`,
  `Saturate`, `Clamp`). Lints deny `unwrap`, `expect`, indexing, panics and `as`.

---

## 2. Anatomy (authored data)

Data flow: `BodyPlan` + `Form` → `apply_form` → `build_anatomy` → `Anatomy`.

### 2.1 Body plan: the anatomical topology

```rust
pub struct BodyPlan { pub root: PartNode, pub tone: Tone }   // *.body.ron; never mutated at runtime

pub struct PartNode {
    pub name: String,           // unique among siblings; joined into the PartPath
    pub kind: PartKind,
    pub attach: AttachSlot,     // where on the parent (default End); ignored for the root
    pub offset: PartOffset,
    pub mirror: bool,           // left/right copies across the parent's local X, named .l / .r
    pub repeat: Repeat,         // copies named .0, .1, ... (before the mirror suffix: leg.3.l)
    pub side: Side,             // set by expand_symmetry; authored plans leave Center
    pub detail: PartDetail,     // part LOD (§2.9)
    pub tone: PartTone,         // multipliers on the body Tone (the Muscle/Fat/Folds levers)
    pub stamps: StampSet,       // Default | None | Custom([...]) (§2.7)
    pub density: f32,           // relative to water; the gait feels the weight (motion::Heft)
    pub children: Vec<PartNode>,
    pub left: SideOverride, pub right: SideOverride,   // how a mirrored copy differs
    pub share_regions: bool,    // a repeated part's copies share the first copy's regions
}
pub enum Repeat { Once, Series { count: u8, from: f32, to: f32 }, Radial { count: u8 } }
pub enum AttachSlot { Start, End, Along(f32), Surface { t: f32, around_deg: f32 },
                      Jaw, Eye, Ear, Horn, Mouth, Nose, Crown }
pub struct PartOffset { pub translate: Vec3, pub rotate_deg: Vec3 /* Euler XYZ */, pub scale: f32 }
pub struct SideOverride { pub absent: bool, pub kind: Option<PartKind>,
                          pub offset: Option<PartOffset>, pub children: Option<Vec<PartNode>> }
pub struct Tone { pub muscle: f32, pub fat: f32, pub folds: f32, pub landmarks: f32, pub fold_noise: NoiseSettings }
```

- `AttachSlot` is a closed enum, so a typo fails at parse time. Each part kind documents the
  slots it exposes (§2.2). Attaching to a missing slot is `AnatomyError::MissingAttachSlot`.
- `Surface { t, around_deg }`: `t` along the part, `around_deg` 0 = dorsal, 90 = left, 180 =
  ventral. `Series` replaces `t` on an `Along` or `Surface` attach; `Radial` turns the offset
  about Y.
- `PartOffset` keeps quaternions out of RON.
- **`expand_symmetry`** returns a plan where every mirrored and repeated copy is an explicit node
  (`mirror: false`, `repeat: Once`), named with its suffixes. Geometry is not flipped there: a
  right copy is built like its left twin and reflected by the builder, so bones never carry a
  negative scale.
- **Per-side overrides** (2026-10-31): a mirrored part may carry `left` and `right`. `absent`
  drops that copy (a one-tusked boar). `kind` replaces the parameters whole, a limb's end among
  them. `offset` and `children` replace theirs; the children expand in turn. On a mirrored
  series they apply to every copy on that side. The expanded plan is ordinary nodes, so a
  suffixed `FormPatch` still reaches one copy. Plans without overrides expand exactly as before
  (`existing_plans_expand_as_before`). The lobster's crusher and cutter claws use it.
  - Not in the format: per-side tone, stamps or attach (restate the part, or patch the copy),
    and partial parameter merges.
  - The old workaround, a `Side::Center` part at `around_deg` 270, lands where the mirrored
    copy would. But only a pitch offset mirrors, and the part has no side
    (`center_part_at_270_is_not_a_right_copy`).
- **Shared regions in a series** (2026-10-31): with `share_regions: true`, a repeated part's
  copies take one set of regions per side, the first copy's. `expand_symmetry` sets
  `regions_with` on the later copies.
  - A limb takes three regions, so a centipede's 32 legs would need 96 of the body's 64. Shared,
    they take six, and a coat marks them as one anyway.
  - A `Part` selector reaches the series by the first copy's path (`trunk/leg.0.l`).
  - It's off by default: the lobster's legs keep a region set each.
- Default tone: muscle 0.5, fat 0.3, folds 0.3, landmarks 0.3.

### 2.2 Part kinds (closed enum, all values plain numbers)

| Kind | Params | Slots exposed | Notes |
|---|---|---|---|
| `Torso` | `TorsoParams { chain, arch: Vec<(t, m)>, belly }` | Start (rear), End (front), Along, Surface | Also a legless body. Emits the body sockets (§3.1). |
| `Chain` | `ChainPart { role: ChainRole, chain }` | Start, End, Along, Surface | Roles: Neck, Tail, Ear, Antenna, Tentacle, Trunk, Tongue, Whisker. Neck drives look-at and emits `neck`; Tail emits `tail_tip`; the rest get spring motion. |
| `Head` | `HeadParams` (below) | End, Surface, Jaw, Eye, Ear, Horn, Mouth, Nose, Crown | Emits `head` and `mouth`. |
| `Limb` | `LimbParams` (below) | Start, End, Along, Surface | Locomotor and Both limbs become legs; a grasping Manipulator emits `grip`. |
| `Digits` | `DigitParams { count, chain, spread_deg, opposable, webbing, tip: Option<HardParams> }` | End (middle digit tip) | Hero only by default. `tip` grows a claw or nail on every digit. |
| `Mass` | `MassParams { shape: Sphere\|Ellipsoid\|Crest\|Hump\|Flap\|Sac, size: Vec3, blend }` | End | Humps, crests, dewlaps, pouches. |
| `Hard` | `HardParams` (below) | none | A separate mesh, not SDF. |
| `Sculpt` | `SculptParams { mesh, scale, blend, resolution, orient, region, own_bone }` | Start, End, Along, Surface | An authored watertight mesh blended into the body as an SDF (§2.10). |
| `Wing` | `WingParams { kind, span, chord, fold, sweep_deg, radius }` | End (wing tip) | Membrane, feathered or insect blade (§2.11). |

Adding a variant is backward compatible; renaming or removing one is not. Reserved for later:
`Fin`, `Shell`.

**Chains** (shared by torsos, chains, limbs, digits and jaws):

```rust
pub struct ChainParams {
    pub segments: u8,          // one bone each; changing it changes the skeleton (default 3)
    pub length: f32,           // metres (default 0.5)
    pub radius: Profile,
    pub curl: f32,             // degrees per metre toward local +Z
    pub blend: f32,            // smooth-min radius against the parent (default 0.05)
    pub ratios: Vec<f32>,      // relative segment lengths, root first; empty = equal
    pub segmented: f32,        // 0..1: exoskeleton joints (arthropod legs, a lobster's tail)
}
pub struct Profile { pub curve: Vec<(f32, f32)>, pub scale: f32 /* metres, 0.08 */, pub cross: CrossSection,
                     pub width: Vec<(f32, f32)>, pub depth: Vec<(f32, f32)>, pub drop: Vec<(f32, f32)> }
pub enum CrossSection { Round, Ellipse { aspect }, Flat { aspect }, Teardrop { aspect } }
```

- `width`, `depth` and `drop` shape the section along `t` on top of `cross`. Where they vary
  inside one bone, that bone's surface is built from two or three pieces. The skeleton is
  unchanged.
- `drop` hangs the section below the axis (a deep chest under a flat topline).
- `Teardrop` is an ellipse with a narrower keel blended under it (bird chests, dorsal ridges).

**Limbs:**

```rust
pub struct LimbParams {
    pub chain: ChainParams, pub role: LimbRole, pub stance: Stance, pub end: LimbEnd,
    pub splay_deg: f32,
    pub bends: Vec<(f32, f32)>,        // raw (forward, up) rest bends; for limbs without joints
    pub clavicle: Option<f32>,         // manipulators: None = automatic, Some(0.0) = none
    pub joints: Option<LimbJoints>,    // the anatomical joints, in place of bends
}
pub struct LimbJoints { pub plan: JointPlan, pub swing: f32, pub lift: f32, pub flex: Vec<f32> }
pub enum JointPlan { Foreleg, Hindleg, Leg, Arm, Arched }
pub enum LimbRole { Locomotor, Manipulator, Both, Swimmer, Display }
pub enum Stance { Plantigrade, Digitigrade, Unguligrade, Sprawl, Arched }
pub enum LimbEnd { None, Hand { palm }, Foot { length }, Hoof { split: bool }, Pad,
                   Pincer { size, shape }, Blade { length }, Point, Paddle { width } }
```

- Every sample limb declares its `joints` (test `sample_limbs_declare_their_joints`). Each
  `JointPlan` names its hinges, which way each closes (`Flexes`) and its range
  (`JointPlan::hinges`), so a reversed knee does not build (`a_reversed_knee_does_not_build`).
- The leg's IK pole comes from the first declared hinge: elbows back, stifles forward, insect
  knees up.
- Hinge ranges, degrees of flexion (most extended, most flexed):

  | Plan | Root cone | Hinges |
  |---|---|---|
  | Foreleg | 75° | elbow forward (−10, 140), carpus back (−30, 150), fetlock back (−60, 120) |
  | Hindleg | 75° | stifle back (−10, 140), hock forward (−15, 140), fetlock back (−60, 120) |
  | Leg | 110° | knee back (−5, 150), ankle forward (−50, 30) |
  | Arm | 160° | elbow forward (−5, 150), wrist forward (−70, 80) |
  | Arched | 75° | knee down (−10, 170), tarsus down (−45, 120) |

- `Hand` and `Pincer` grasp (`LimbEnd::grasps`), so they take a `grip` socket.
- `Pincer`'s `shape` (`PincerShape`: fingers, width, depth, thickness, gape) scales the
  default claw. Every field defaults to 1.0, which builds the old claw
  (`default_pincers_build_as_before`).

**Heads:**

```rust
pub struct HeadParams {
    pub skull: MassParams, pub snout: f32, pub jaw: Option<ChainParams>,
    pub eye_spacing: f32, pub eye_lateral: f32 /* 0 predator .. 1 prey */, pub pitch: f32,
    pub face: Option<FaceParams>,      // a flat primate face in place of a muzzle
    pub muzzle: MuzzleParams,          // an animal's snout and jaw (ignored under a face)
    pub sculpt: Option<SculptHead>,    // an authored head mesh in place of cranium, face or muzzle
}
pub struct FaceParams { nose, brow, cheekbones, lips, chin, jaw_width, cheeks, eyelids }   // 1.0 = a typical adult
pub struct MuzzleParams { base: Vec2, tip: Vec2, rise, drop, dish, cheek, nostrils, lips, jaw_depth, jaw_width }
pub struct SculptHead { mesh, resolution, blend, eye: Vec3, eye_radius, nostril, ear, nose, mouth, crown }
```

- A sculpted head is scaled to stand `skull.size.y` high and places its slots at the mesh's own
  landmarks. Eyes, hair and anything else still attach by slot. The defaults are the MakeHuman
  head's (`parts/head_mh.gltf`); the orc, gnome and goblin wear warps of it
  (`tools/head_warp.py`).

**Hard parts:**

```rust
pub struct HardParams { pub kind: HardKind, pub size: f32, pub curve: f32, pub twist: f32,
                        pub sclera: f32 /* eyes */, pub shape: HardShape }
pub enum HardKind { Claw, Nail, Horn, Antler { tines: u8 }, Tusk, Tooth, Fang, Beak, Spike, Plate, Eye,
                    Mesh { path: String, orient: Orient, scale: f32 } }
pub struct HardShape {
    pub base: Option<f32>,   // base radius as a fraction of size; None = the kind's own
    pub taper: f32,          // radius = base × (1 − t)^taper; 0.8
    pub bend_deg: f32,       // where curve bends, about the axis: 0 = local +Z, 90 = +X
    pub ridges: u8, pub ridge_depth: f32,   // lobes round the section, spiralling with twist
    pub rings: u8, pub ring_depth: f32,     // bumps along the length
}
```

- `HardShape` (2026-10-31) shapes horns, tusks, spikes, claws, teeth, fangs, beaks and an
  antler's beam over each kind's fixed proportions (`surface::hard::horn_dims`, which the SDF
  preview shares). Every default is the kind's own, so plans without one build identically
  (`default_shapes_build_as_before`). Eyes, plates and authored meshes ignore it. Ridges and
  rings refine the mesh at hero and near detail. The unicorn's spiral horn is 2 ridges at 0.13
  over 4.5 turns.
- `Plate` is a scute lying on the skin (`size` = length, `curve` = doming).

### 2.3 Part paths

```rust
pub struct PartPath(pub String);   // "torso/limb.front.l/digits"
```

- **T-4** The path is a stable address. Form patches, region definitions, garments (`Garment::on`),
  sever and graft, and genomes all use it.
- A pattern matches a path segment by segment. A segment matches exactly, or with its expansion
  suffixes removed (`path_matches`), so `torso/leg` hits every `leg.3.l`, and `torso/leg.3.r`
  hits one copy.

### 2.4 Form and levers: an individual's proportions

```rust
#[derive(Component)]   // a component only, never an asset
pub struct Form { pub overall_scale: f32, pub patches: Vec<FormPatch> }   // ordered; later patches win
pub struct FormPatch { pub part: PartPath, pub lever: Lever, pub value: f32 }
pub enum LeverKind { Factor /* × */, Delta /* + */, Count /* = */ }
pub fn apply_form(plan: &BodyPlan, form: &Form) -> Result<BodyPlan, FormError>;   // expands symmetry first
```

- **T-5** (*revised*) Overrides are **levers**: named knobs with a fixed `LeverKind`, a fixed
  range, and a list of part kinds that support them (`PartKind::levers`, `src/anatomy/lever.rs`).
  Reflect paths were dropped: saved forms and genomes must not depend on field names.
- **T-5a** The game always supplies the `Form`. An empty form means the plan as authored. The
  plugin has no randomizer and no ranges of its own.
- Placement levers work on every part: `Scale`, `Pitch`, `Yaw`, `Roll`, `Position` (`t` along the
  parent), `Around`. Surface levers work on every SDF part: `Muscle`, `Fat`, `Folds`, `Blend`.
- Per kind, on top of those:

  | Kind | Levers |
  |---|---|
  | Torso | chain levers, `Arch`, `Belly` |
  | Chain | chain levers |
  | Head | `Skull`, `Snout`, `Jaw`, `EyeSpacing`, `EyeLateral` |
  | Limb | chain levers, `EndSize`, `Splay` |
  | Digits | chain levers, `Count`, `Spread`, `Webbing` |
  | Mass | `Width`, `Height`, `Depth` |
  | Hard | `Size`, `Curve`, `Twist` (+ `Tines` on antlers) |
  | Sculpt | `Blend` only |
  | Wing | `Size` |

  Chain levers: `Length`, `Girth`, `Taper`, `Curl`, `Flatten`, `Width`, `Depth`, `Segments`.
- `Segments`, `Count` and `Tines` are counts and change topology. `Segments` and `Count` change
  the skeleton, so mobs that differ in them share no baked motion.
- Ranges hold a body together: factors 0.25–4 (sizes), angles ±90° (`Pitch`/`Yaw`/`Roll`) or
  ±60° (`Around`, `Splay`), counts 1–8 (`Segments`, `Tines`) or 1–6 (`Count`). Each patch must
  lie in range, and so must all the patches on one part combined (`FormError::Combined`).
- A `Jaw` lever on a head without a jaw is `LeverError::Unsupported`.

### 2.5 Regions (derived)

```rust
pub struct RegionId(pub u8);                 // ≤ 64 per mob (RegionId::MAX): a u64 mask
pub struct RegionDef { pub part: PartPath, pub kind: RegionKind, pub side: Side }
pub enum RegionKind {
    TorsoFront, TorsoMid, TorsoRear, Belly, Neck,
    Skull, Face, Brow, Eyelid, Lip, Snout, Jaw,
    LimbUpper, LimbLower, LimbEnd, Digit,
    TailBase, TailTip, Appendage, Mass, Hard, Wing, WingMembrane,
}
pub enum Side { Center, Left, Right }
pub struct RegionMask(pub u64);
pub enum RegionSelector { All, Kind(RegionKind), Part(PartPath), Role(LimbRole), Side(Side),
                          And(Vec<Self>), Or(Vec<Self>), AllExcept(Vec<Self>) }
impl RegionSelector {
    pub fn mask(&self, a: &Anatomy) -> RegionMask;
    pub fn hard_mask(&self, a: &Anatomy) -> Vec<bool>;   // one per hard part
}
```

- "Fur on everything except limb ends and the face" is `AllExcept([Kind(LimbEnd), Kind(Face)])`.
  It works on a biped and a hexapod alike. `AllExcept` stands in for `Not`, which would need a
  `Box` that Bevy reflection cannot represent.
- `Part(path)` matches the part and its descendants; `Role` matches limbs with that role and
  their descendants.
- `Face`, `Brow`, `Eyelid` and `Lip` (2026-10-27) exist on a primate face, for brows, lashes and
  lips. On a sculpted head `Face` covers only the nostrils (`roadmap.md`, Phase 3).
- A region is owned by the nearest primitive whose blend reaches the surface (2026-10-27).
- **Hard parts take no region** (2026-10-31). A claw, horn or tooth belongs to the region of the
  skin it grows from (`HardPart::region`, its bone's), and carries the part that made it and its
  side. `Anatomy::hard_region` stands each one in as a `Hard` region of its own part, so
  `Kind(Hard)`, `Part(...)`, `Side(...)` and `Role(...)` pick hard parts as before, through
  `hard_mask`. The markings read a hard part's index from the vertex data (§4.4), up to 252 hard
  parts. The dragon's 45 hard parts had taken 45 of the 64 regions; it went from 64 regions to 33.
- A selector that matches nothing on the body it is baked onto warns once per body and coat
  (`Coat::warn_unmatched`, 2026-10-31).

### 2.6 Built anatomy (resolved, CPU-side)

```rust
pub struct Anatomy {
    pub skeleton: Skeleton,
    pub primitives: Vec<Primitive>, pub hard_parts: Vec<HardPart>, pub sockets: Vec<SocketDef>,
    pub regions: RegionTable,
    pub legs: Vec<LegInfo>,             // from Locomotor and Both limbs
    pub clavicles: Vec<Clavicle>,       // shoulder girdles of manipulator arms
    pub spine: Vec<BoneId>,             // torso bones, rear to front
    pub springs: Vec<SpringChainDef>,   // tails, ears, antennae
    pub cloth_follow: Vec<ClothFollow>, // loose-cloth chains that swing with a leg
    pub sheets: Vec<Sheet>,             // wing membranes, flight feathers, insect wings
    pub wings: Vec<WingRig>,            // fold rigs
    pub look: Option<LookDef>,          // neck and head bones that turn toward a target
    pub hands: Vec<HandDef>,            // digits parts, for posing
    pub joint_limits: Vec<(BoneId, JointLimit)>,
    pub mass: BodyMass,                 // kilograms per bone and the centre of mass
    pub parts: Vec<BuiltPart>,          // depth-first; a parent precedes its children
    pub ground: f32,                    // how far the body was lowered to rest on y = 0
}
pub struct BuiltPart {
    pub path: PartPath, pub parent: Option<u16>, pub role: Option<LimbRole>,
    pub apart: bool,                    // meshed as its own surface (limbs, wings, tails)
    pub detail: PartDetail, pub lod: LodLevel, pub tone: Tone,
    pub bones: Range<u8>, pub primitives: Range<u32>, pub hard_parts: Range<u16>, pub regions: Range<u8>,
    pub axis: SmallVec<[AxisSample; 9]>, // samples along the part, grounded with the body
    pub attach: Transform, pub mirrored: bool, pub density: f32,
}
pub fn build_anatomy(live: &BodyPlan, lod_rules: &PartLodRules, assets: &PartAssets) -> Result<Anatomy, AnatomyError>;
impl Anatomy { pub fn from_plan(plan, lod_rules, assets) -> Result<Self, AnatomyError>; }   // default form
```

- **`build_anatomy` is pure.** It takes the expanded, form-applied plan and the authored meshes
  it references (`referenced_assets`), and has no RNG and no seed. Equal plans always share one
  anatomy and one base mesh. Randomness belongs to later stages.
- `AxisSample` carries lateral and dorsal radii and the section's `drop`, so surface placement
  follows elliptical sections.
- Errors: `BoneBudget`, `RegionBudget`, `MissingAttachSlot`, `DuplicatePath`, `InvalidParams`,
  `MissingAsset`, and `Internal` for a builder invariant that did not hold (reported, never a
  panic).

**`PartBuild` and `AnatomyBuilder`:**

```rust
pub trait PartBuild {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError>;
}
pub struct BuildCx { pub path: PartPath, pub side: Side, pub tone: Tone, pub offset: Transform }
pub struct AttachPoint { pub bone: Option<BoneId>, pub model: Transform }   // +Y away from the parent
pub struct PartSlots { pub fixed: SmallVec<[(AttachSlot, AttachPoint); 6]>,
                       pub axis: SmallVec<[AxisSample; 9]>, pub skull: Option<SkullFrame> }
impl AnatomyBuilder {
    pub fn bone(&mut self, parent: Option<BoneId>, name: &str, model: Transform, length: f32, region: RegionId) -> Result<BoneId, AnatomyError>;
    pub fn region(&mut self, kind: RegionKind, side: Side) -> Result<RegionId, AnatomyError>;
    pub fn primitive(..); pub fn hard_part(..); pub fn socket(..); pub fn leg(..); pub fn clavicle(..);
    pub fn spring_chain(..); pub fn look(..); pub fn limit(..); pub fn follow(..);
    pub fn finish(self) -> Result<Anatomy, AnatomyError>;
}
```

- Parts build in the **logical** model frame: Y up, mob facing +Z, left = +X. A mirrored copy is
  built like its left twin, and the builder reflects it on storage.
- `finish` grounds the body, measures its mass, resolves part LODs and calls `finish_skeleton`
  (inverse binds, the cached rest pose and the crowd skeleton).

### 2.7 Tone stamps

Deterministic, seed-free stamps from built-in per-part tables (`src/anatomy/tone.rs`): muscle
bellies, fat pads, bone landmarks and hollows on top of a part's base shape.

```rust
pub enum StampSet { Default, None, Custom(Vec<Stamp>) }
pub struct Stamp { pub at: StampAt, pub kind: StampKind, pub size: Vec3, pub depth: f32,
                   pub mirror: bool, pub form: StampForm, pub hang: f32, pub soft: SoftTune }
pub enum StampAt { Surface { t, around_deg }, Skull { dir: Vec3 }, Slot { slot: AttachSlot, offset: Vec3 } }
pub enum StampKind { Muscle /* Near+ */, Fat /* every level */, Landmark /* Hero */, Hollow /* Hero */ }
pub enum StampForm { Raise, Mass, Drop }
pub struct SoftTune { pub firmness: f32, pub damping: f32, pub travel: f32 }   // soft-tissue feature
```

- Sizes are relative to the local body radius, so one table works on any body plan. Intensity
  comes from `Tone` × the part's levers.
- `Raise` displaces the surface (`PrimOp::Displace`). `Mass` and `Drop` are bodies of tissue
  blended on, which can hang and overhang (breasts, a belly apron).
- Muscle stamps also give the mesh its flex data (§4.3) and, with `soft-tissue`, its springs
  (§14).
- Joint creases are field displacement masked to joints, hero only, and off by default
  (`LodSpec::geometric_folds`): at the hero voxel size they mesh as scratches.
- Still open: per-limb muscle and tendon tables tuned on reference meshes (`roadmap.md`,
  Phase 3).

### 2.8 Viability checks

```rust
pub trait ViabilityCheck: Send + Sync + 'static { fn name(&self) -> &'static str; fn check(&self, a: &Anatomy) -> Viability; }
pub enum Viability { Ok, Warn(String), Fail(String) }
#[derive(Resource)] pub struct ViabilityChecks(pub Vec<Box<dyn ViabilityCheck>>);   // with_builtins(); games may push their own
```

Built-ins: `bone_budget` (hero and crowd caps), `feet_reach_ground`, `static_balance` (centre of
mass over the feet) and `rest_self_intersection` (contacts deeper than 2 cm at rest, §4.3). Gait
compatibility lives in motion, as a cycle's `Fit` (§6.6). Genomes use the checks as their
fitness gate (§2.12); `examples/viability.rs` runs them over every sample plan.

### 2.9 Part LOD

Each part has a detail level: the **coarsest tier at which the part still exists** in the mesh,
the skeleton and the ECS. Small parts drop out as the mob shrinks on screen.

```rust
pub enum PartDetail { Auto, Always, UpTo(LodLevel) }
pub enum LodLevel { Hero = 0, Near = 1, Far = 2 }       // MobTier wraps it; "pending" = no MobTier yet
#[derive(Resource)] pub struct PartLodRules { pub near_min_ratio: f32 /* 0.08 */, pub far_min_ratio: f32 /* 0.2 */ }
```

- `Auto` compares the part's bounding radius to the mob's against the rules.
- **T-20** Part LOD applies at three layers:
  1. **Mesh.** Each level is **re-meshed** from the SDF without the culled parts, at a coarser
     voxel size, then trimmed by `meshopt`. Culled hard parts are omitted. This beats decimating,
     which leaves lumps where small parts were.
  2. **Skeleton.** The crowd skeleton keeps the bones of parts present at `Near` or coarser,
     plus hanging-cloth bones while the 32-bone budget allows. Other bones fold into the nearest
     kept ancestor.
  3. **ECS.** Part entities exist only while their part is present at the mob's tier
     (`sync_part_entities`). `Always` parts keep their entities in every tier, so gameplay always
     has hit locations.
- Edits are preserved: a culled part's change lives in the mob's `LiveBody`, not its entity.

### 2.10 Authored parts

Two forms, both resolved before building so `build_anatomy` stays pure:

- `PartKind::Sculpt`: a watertight glTF mesh primitive converted to an SDF grid
  (`SdfGrid::from_mesh`, in `anatomy::mesh_sdf`: exact distances in a band a cell round every
  triangle, the rest by jump flooding, angle-weighted pseudo-normal signs; either winding;
  enclosed voids filled) and smooth-blended into the body (`PrimShape::Grid`).
- `HardKind::Mesh { path, orient, scale }`: an authored mesh attached rigidly to one bone.
- `HeadParams::sculpt`: a sculpted head in place of the procedural one (§2.2).

Conventions: glTF's (Y up, +Z forward, metres), origin at the attach point. `Orient::Model` keeps
the authored orientation; `Orient::Slot` aligns +Y to the slot. Right copies sample the grid or
mesh reflected. Meshes are loaded through `PartAssets` (converted SDFs shared by mesh hash and
resolution; builds asking at once share one conversion) and their content hashes are in the
`BodyKey`. Plans and part meshes hot-reload. The glTF side of authoring is in
[`interchange.md`](interchange.md); the artist checklist and the gaps are in
[`authored-parts-and-clothes.md`](authored-parts-and-clothes.md).

### 2.11 Wings

[`wings.md`](wings.md) has the anatomy and the implementation notes. In short:

- One part kind, `Wing`, with three kinds: `Membrane` (bat and dragon with 2–4 fingers,
  pterosaur with 1), `Feathered` (bird: tracts of flight feathers on the arm bones) and `Blade`
  (insect: a rigid blade on a hinge, optionally a hardened elytron).
- Bones are built in the **spread** pose (the bind pose). `fold` blends every wing bone toward
  its folded rotation at rest (`WingRig`, applied by the pose solver and the crowd bake), through
  a lifted `half` pose for membranes.
- Arm and finger bones are thin SDF tubes in the body. Everything thinner than a voxel is a
  generated `Sheet`, skinned across the bones and drawn by `MaterialSlot::Wing` with its kind in
  the vertex colour.
- Samples: dragon, wyvern, bat, bird, beetle, dragonfly. Not yet: `WingCoat` colours, the
  propatagium and uropatagium, flight.

### 2.12 Genomes (`genetics` feature)

`anatomy::genome`: a genome holds `CreatureGenes { body: BodyPlan, coat: Coat, form: Form }`.
Everything a design needs just so is a fixed gene; the values a `CreatureDesign` lists as traits
(a lever on a part, within a range) vary. `CreatureDesign::founder` gives a lineage's first
genome, and `express` turns any genome back into a body, gated by the viability checks.
Individuals that differ only in form share one lineage and one body plan asset. domain-expance
breeds its creatures this way.

---

## 3. Skeleton, bones and sockets

```rust
pub struct Skeleton { pub bones: Vec<BoneDef>, pub inverse_bind: Vec<Mat4>, pub rest_model: Vec<Affine3A>, pub crowd: CrowdSkeleton }
pub struct BoneDef { pub name: String, pub parent: Option<BoneId>, pub rest: Transform, pub length: f32 /* along +Y */,
                     pub region: RegionId, pub part: u16 }
pub struct CrowdSkeleton { pub map: Vec<u8> /* full → crowd */, pub bones: Vec<BoneId> /* crowd → full */ }
pub struct BoneId(pub u8);

pub const MAX_BONES: usize = 128;         // 64 until 2026-10-31
pub const MAX_CROWD_BONES: usize = 32;
pub const CROWD_SLOT: usize = 32;         // 40 with soft-tissue: room for 8 soft regions

pub enum JointLimit { Hinge { flexes: Flexes, flex: (f32, f32) }, Ball { cone: f32 } }
pub enum Flexes { Forward, Back, Up, Down }   // which way the bone below swings as the hinge closes
```

- Bones are in topological order: a parent's index is always lower than its children's.
- **T-6** (*revised*) 128 hero bones is a **budget, not a limit anything is sized by**: `BoneId` is
  a byte, Bevy skins 256 joints, the vertex data carries the bone in a byte, and the crowd folds
  to 32. It guards a hero mob's per-bone CPU work (pose, IK, contacts) and hanging cloth, which
  takes what is left. 64 ran out on a dragon with two-segment clawed toes, spines and horns
  (about 80 bones). The crowd cap stays at 32: a crowd slot is 32 matrices in the bone buffer.
- `JointLimit::clamp` holds a pose within a limit. A hinge clamps only its turn about its axis
  and keeps any turn off it, so it never fights an IK solve except to stop a joint folding the
  wrong way. Motion applies the limits as its last stage with the `joint-limits` feature.
- The skeleton has no per-bone detail flag. Which bones the crowd keeps follows from part LOD
  (§2.9).

### 3.1 Sockets

```rust
pub struct SocketDef { pub name: String, pub kind: SocketKind, pub side: Side, pub bone: BoneId, pub local: Transform, pub size: f32 }
pub enum SocketKind { Grip, Mouth, Head, Neck, Shoulder, Chest, Back, Saddle, Hip, Wrist, TailTip }
```

| Socket | Emitted by | Where |
|---|---|---|
| `grip.l` / `grip.r` | a Manipulator or Both limb whose end grasps (Hand, Pincer) | the fist; `size` = palm or pincer size |
| `head`, `mouth` | heads (procedural and sculpted) | crown, mouth |
| `neck` | Neck chains | half way along, ventral |
| `tail_tip` | Tail chains | the last bone's tip |
| `back`, `saddle`, `chest`, `hip.l/.r`, `shoulder.l/.r` | the torso | on the skin at fixed `t` and `around` |

- Frame convention: origin at the hold point, **+Y along the held tool's main axis** (across the
  fist, so forward when the arm hangs; out of the skin for body sockets; forward out of the
  mouth). Author tools with their main axis along +Y.
- `Wrist` is declared but no part emits it yet.

---

## 4. Surface (mesh generation)

Data flow per level: `Anatomy` → `BodyField` (parts culled for the level) → `Mesher` → `RawMesh`
→ `weld` / `smooth` → `skin_and_attrs` → `simplify` → hard parts merged → (crowd remap for
`Near`/`Far`) → `BodyMeshData` → Bevy `Mesh`.

### 4.1 Field

```rust
pub trait Field: Send + Sync {
    fn bounds(&self) -> Aabb3d;
    fn distance(&self, p: Vec3A) -> f32;                                   // the hot path
    fn distance_batch(&self, ps: &[Vec3A], out: &mut [f32]) { /* default loop */ }
    fn gradient(&self, p: Vec3A) -> Vec3A { /* central differences */ }
    fn surface_info(&self, p: Vec3A) -> SurfaceInfo;                       // once per output vertex
}
pub struct SurfaceInfo { pub influences: Influences /* 4 bones, sorted by weight */, pub region: RegionId, pub layer: PrimLayer }
pub struct BodyField { .. }   // the whole-body SDF at one level: BodyField::new(anatomy, &BodyFieldOptions)
pub struct BodyFieldOptions { pub level: LodLevel, pub seed: u64, pub geometric_folds: bool, pub spread: bool }
```

- `BodyField` is a smooth union of the level's primitives plus crease displacement near the
  surface. Primitives are binned per chunk (`BodyField::view → ChunkView`), with no BVH crate.
- **Limbs mesh apart** (2026-10-23). A part that moves far from where it rests (`BuiltPart::apart`:
  limbs, wings, tails) blends into the body only where it attaches and only *touches* anything
  else. Where it touches the body or another limb, the mesher builds each surface through the
  other as its own **sheet** (`RawMesh::sheets`), so they part cleanly when they move. The field
  keeps every limb apart: its per-point list holds `LIMB_SLOTS` = 12 inline and grows past that
  (2026-10-31). The preview shader keeps 12 as a per-cell cap, which the packer checks and
  `every_mob_fits_the_preview_cells` tests. The centipede has 32 legs.
- **Meshing pose** (`anatomy::Spread`): arms raised 45° clear of the body, fingers fanned 10°,
  and legs that overlap at rest swung apart by the least that parts them. The animation rest
  pose is unchanged; the mesh is skinned back into it.
- Combinators (`surface::combinators`): `Intersect`, `RegionClip` (a torso-only shell), `Displace`.
- Garments see the field too (`BodyField::garment_info`, §13).

### 4.2 Primitives

```rust
pub struct Primitive { pub bone: BoneId, pub shape: PrimShape, pub local: Transform, pub blend: f32,
                       pub blend_anchor: Option<(Vec3, f32)>, pub op: PrimOp, pub layer: PrimLayer, pub region: RegionId,
                       pub lod: LodLevel, pub weigh: Weigh, pub soft: Option<SoftTune> }
pub enum PrimShape { Sphere { r }, Ellipsoid { radii }, RoundCone { r0, r1, len }, RoundBox { half, round },
                     Grid { grid: Arc<SdfGrid>, mirror_x: bool } }
pub enum PrimOp { Union, Subtract, Displace { height } }
pub enum PrimLayer { Base, Muscle, Landmark, Fat }
pub struct HardPart { pub bone: BoneId, pub local: Transform, pub params: HardParams, pub region: RegionId /* the skin's */,
                      pub part: u16, pub side: Side, pub mesh: Option<Arc<TriMesh>>, pub mirror: bool }
```

- Shapes are aligned to local +Y where they have an axis. Non-round sections are built as
  sweeps of elliptic tubes (`surface::sdf::sweep`), a teardrop with its keel blended on.
- `blend_anchor` limits a limb's blend to where it attaches, so it does not web onto a body it
  hangs beside.
- `Displace` pushes the surface out inside an ellipsoid footprint, fading at its rim. Stamps use
  it: a smooth union with a flush shape would inflate the whole footprint and leave a ledge.
- `Weigh` corrects the mass measure for overlaps. Hard parts and sheets weigh nothing.
- Hard parts are separate meshes (`surface::hard::hard_part_mesh`), 100 % weighted to their
  bone: curved tapered horns, antlers, flattened horns (beaks, nails), plates, spheres (eyes) or
  the authored mesh. Tessellation drops with the level; `Far` builds none.

### 4.3 Meshing and processing

```rust
pub trait Mesher { fn mesh(&self, field: &BodyField, settings: &MeshSettings) -> Result<RawMesh, SurfaceError>; }
pub struct SurfaceNets;        // fast-surface-nets over sparse chunks, in parallel
pub struct AdaptiveContour;    // octree dual contouring; what generation uses
pub struct MeshSettings { pub voxel_size: f32, pub chunk_voxels: u32 /* 30 */, pub max_triangles: u32 /* 200k */,
                          pub detail_levels: u8, pub cells_per_radius: f32 /* 2.0 */ }
pub struct LodSpec { pub mesh: MeshSettings, pub simplify_ratio: f32, pub max_error: f32,
                     pub geometric_folds: bool, pub hard_parts: bool, pub deformation: bool }
#[derive(Resource)] pub struct SurfaceSettings { pub levels: [LodSpec; 3] }

pub fn mesh_lod<M: Mesher>(m: &M, a: &Anatomy, level: LodLevel, spec: &LodSpec, seed: u64) -> Result<BodyMeshData, SurfaceError>;
pub fn mesh_all_lods_keeping<M: Mesher>(m, a, settings, seed, keep: Option<KeepFn>) -> Result<[BodyMeshData; 3], SurfaceError>;
pub fn weld(..); pub fn smooth(..) /* Taubin */; pub fn skin_and_attrs(..); pub fn simplify(..); pub fn simplify_keeping(..);
pub fn remap_to_crowd(..); pub fn merge(..);
pub fn centres_of_rotation(mesh: &BodyMeshData) -> Vec<Vec3>;
pub fn muscle_flex(mesh: &BodyMeshData, a: &Anatomy, level: LodLevel) -> Vec<MuscleFlex>;
```

Default `SurfaceSettings` (tuned for a 1.8 m body; voxel sizes scale up with extent, and a mesh
over its triangle budget re-meshes coarser instead of failing):

| Level | Voxel | Detail levels | Simplify to | Max error | Hard parts | Deformation |
|---|---|---|---|---|---|---|
| Hero | 8 mm | 2 (cells halve to 2 mm where the body is thin) | 50 % | 0.0012 | yes | yes |
| Near | 20 mm | 1 | 15 % | 0.01 | yes | yes |
| Far | 50 mm | 0 | 35 % | 0.01 | no | yes |

- **Adaptive meshing** (`surface::octree`): cells refine where the body is thin or bends
  sharply, so fingers and ears keep their shape without meshing the whole body finer. Dual
  contouring joins cell sizes without cracks. Each finest cell can hold several sheets, so two
  fingers in one cell get a vertex each.
- **Binding** (`surface::binding`): geodesic voxel binding. Skin weights come from distances
  measured through the body, so a hand hanging by a thigh does not pull on it. Edges between
  cells of different limbs are cut.
- **Centres of rotation** (`surface::cor`, Le and Hodgins 2016): per vertex, the point its
  blended rotation turns about, as `ATTRIBUTE_COR`. The shader (`coat/dqs.wesl`) rotates about
  it, or blends dual quaternions where a mesh has none. Cost: about 1.7 s single-threaded for a
  29k-vertex hero mesh (code comment), parallel over the worker threads; 9.5× slower on the
  Steam Deck than on tir (2026-10-31).
- **Muscle flex** (`surface::muscle`): each muscle stamp has its insertion across the nearest
  joint. The vertex shader bulges the belly by how much the insertion has come closer than at
  rest. Only joint matrices are needed at run time.
- **Contacts** (`surface::contact`, [`contacts.md`](contacts.md), 2026-10-29): rounded-cone
  proxies for the body and up to 15 limbs (`CONTACT_GROUPS` = 16, `MAX_PROXIES` = 96). Each hero
  skin vertex carries the groups it may be pushed out of (§4.4). The hero vertex shader pushes
  skin inside another group's proxy out to the middle, so thighs and arms press flat instead of
  crossing. `ContactSet::contacts` measures the same on the CPU; `clear_plan` turns deep
  rest-pose contacts out. Crowd tiers and garments do not press.
- `simplify_keeping` spends the triangle budget on edges where a kept value (the coat's markings)
  changes.
- Parallel stages share one worker pool (`surface::worker_threads`, capped by `GenBudget`), and
  every stage merges in job order, so meshes do not depend on the thread count
  (`meshing_is_independent_of_thread_count`). With the `web` feature every worker runs on the
  calling thread.
- Errors: `Empty`, `TooManyTriangles`, `NoPartsAtLevel`, `WorkerPanicked`.

### 4.4 Mesh output

```rust
pub struct BodyMeshData {
    pub positions, pub normals, pub joints: Vec<[u16; 4]>, pub weights: Vec<[f32; 4]>, pub mob_data: Vec<[u32; 2]>,
    pub uvs, pub coverage: Vec<[f32; 3]>, pub flow: Vec<[f32; 4]>, pub paint: Vec<[f32; 4]>, pub cloth: Vec<u8>,
    pub cor: Vec<Vec3>, pub muscle: Vec<MuscleFlex>, pub soft: Vec<SoftVertex>,
    pub indices: Vec<u32>, pub submeshes: Vec<Submesh>,
}
pub enum MaterialSlot { Skin, Hard, Eye, Cloth, Wing }
pub struct MobVertexData { pub region: RegionId, pub cavity: u8, pub shade: u8, pub limb_t: u8,
                           pub primary_bone: BoneId, pub layer: PrimLayer, pub hard: Option<u8> }
```

- **T-7** (*revised*) The output is one Bevy `Mesh` per material slot and level. Hero meshes use
  the standard `JOINT_INDEX`/`JOINT_WEIGHT`; crowd meshes carry theirs under
  `ATTRIBUTE_CROWD_JOINT_*`, because Bevy treats any mesh with the standard joint attributes as
  a `SkinnedMesh`. Joint indices refer to the full skeleton at `Hero` and to the crowd skeleton
  below. The custom attributes, with their shader locations (`CoatExt::specialize`):

  | Attribute | Location | Holds |
  |---|---|---|
  | `POSITION`, `NORMAL` | 0, 1 | |
  | `UV_0` | 2 | skin: (fur, feather) weights; shells: (height, fur length); cards: (across, along); eyes: (iris, white) |
  | `TANGENT` | 4 | coat flow (xyz) and distance along it (w) |
  | `COLOR` | 5 | rest position + scale weight; cloth: palette index in w; wings: sheet kind in w |
  | joints, weights | 6, 7 | hero: Bevy's; crowd: `ATTRIBUTE_CROWD_JOINT_INDEX/WEIGHT` |
  | `ATTRIBUTE_COR` | 8 | centre of rotation |
  | `ATTRIBUTE_MUSCLE`, `ATTRIBUTE_MUSCLE_JOINT` | 9, 10 | insertion + amplitude; joint + length |
  | `ATTRIBUTE_SOFT` | 11 | two soft regions and weights (`soft-tissue`) |
  | `ATTRIBUTE_PAINT` | 12 | markings: linear RGB + coverage |
  | `ATTRIBUTE_MOB_DATA` | 13 | the packed `MobVertexData` |

- `ATTRIBUTE_MOB_DATA` is two `u32`s: x = `region | cavity << 8 | shade << 16 | limb_t << 24`;
  y = `primary_bone | layer << 8 | contact mask << 16`. The layer byte holds 0–3 for the tone
  layers and `4 + index` on a hard part's vertices (`MobVertexData::MAX_HARD` = 252). The
  contact mask names the groups the vertex may be pushed out of (§4.3).
- `cavity` is a one-sample occlusion probe along the normal; `shade` is occlusion at the body's
  scale (under the chin, between the thighs).
- Bodies without coverings carry no coverage, flow or paint attributes.

### 4.5 GPU preview (`preview` feature)

`surface::preview` packs a `BodyField` into one word array a shader can ray-march
(`sdf_field.wesl` mirrors `BodyField::eval`; `sdf_bake.wesl` bakes distance bricks that
`sdf_preview.wesl` marches). Only the bricks an edit changes are baked again. ornisculpter draws
with it; games leave the feature off. **Changes to the body field or its primitives must keep
these shaders in step with the CPU field.**

---

## 5. Coat (look: colours, coverings, markings)

[`realism.md`](realism.md) has the creature passes that shaped the coat and the markings.

```rust
pub struct Coat {                       // *.coat.ron, worn through MobCoat(Handle<Coat>); none = plain skin
    pub base: [f32; 3], pub secondary: [f32; 3], pub accent: [f32; 3],   // sRGB
    pub roughness: f32,
    pub coverings: Vec<Covering>,       // later coverings win overlaps
    pub markings: Vec<Marking>,         // in order; later paint over earlier
    pub hard: [f32; 3], pub iris: [f32; 3],
}
pub struct Covering { pub regions: RegionSelector, pub kind: CoveringKind, pub facing: Option<Facing> }
pub enum CoveringKind { Scales(Scales), Fur(Fur), Feathers(Feathers) }
pub struct Scales { pub size: f32, pub relief: f32, pub contrast: f32 }
pub struct Fur { pub length: f32, pub density: f32, pub thickness: f32, pub tip: f32, pub shells: u8 /* 12 */, pub cards: Option<HairCards> }
pub struct HairCards { pub length: f32, pub width: f32, pub density: f32, pub lift_deg: f32, pub droop: f32 }
pub struct Feathers { pub length: f32, pub width: f32, pub density: f32, pub lift_deg: f32, pub tip: f32 }
pub struct Marking { pub color: [f32; 3], pub regions: RegionSelector, pub strength: f32,
                     pub facing: Option<Facing>, pub height: Option<(f32, f32)>, pub along: Option<(f32, f32)>,
                     pub tip: Option<(f32, f32)>, pub bands: Option<Bands>, pub patches: Option<Patches> }
```

- The coat is part of the `BodyKey` and hot-reloads. Generation bakes the coat's per-vertex
  inputs (`coat::annotate`): coverage weights (scales, fur, feathers) smoothed over the skin so
  borders blend, a flow direction (the bone axis, signed by region, tangent to the skin) and a
  distance along the flow integrated over the mesh, so rows stay evenly spaced across bones.
- **Coverings.** `facing` restricts a covering to the surface facing a direction (a hairline, a
  pale belly). Fur coverings after the first set only their regions' strand length (a mane, a
  beard); density, thickness, tip and shells come from the first. `Fur::cards` grows hair cards
  for long hair.
- **Markings** (2026-10-26): colour laid over skin and hard parts by selector and masks
  (`facing`, `height`, `along`, `tip`, `bands`, `patches`; the masks multiply), baked per vertex
  into `ATTRIBUTE_PAINT` and carried to the fragment stage by `paint_io.wesl`. Fur and scales
  keep their secondary and accent brightness relative to the painted base. `Coat::hard` and
  `Coat::iris` colour hard parts and eyes.
- **Per tier:**
  - Scales are procedural in the skin shader: cellular colour and derivative bump at hero; a
    cheaper 2×2×2 search, no relief, covered pixels only, in the crowd.
  - Fur is shells at hero (`Fur::shells` offset copies of a 20 %-simplified hero skin,
    alpha-tested by strand noise, clumping by distance into a pelt), plus hair cards where the
    covering has them. Crowd tiers carry fur in the skin: a two-scale clump mottle, a
    tip-coloured grazing rim, the skin inflated by 0.8 × the fur length.
  - Feathers are cards scattered by area × density over covered skin at hero, laid along the
    flow and skinned to the surface. Crowd tiers draw overlapping rows along the flow in the
    skin shader.
  - Every procedural pattern fades to its average colour by world-space pixel footprint and
    skips its noise once it is below a pixel.
- **Material.** **T-8** (*revised*) One material type for every surface and tier:
  `MobMaterial = ExtendedMaterial<StandardMaterial, CoatExt>`, specialised by
  `CoatKey { crowd: bool, layer: CoatLayer }` into shader defines. `CoatLayer` is Skin, Shell,
  Feather, Hard, Cloth, Wing or Hair. `CoatExt` binds the crowd bone buffer (100), the
  `CoatUniform` (101) and the contact proxies (102). Shaders: `coat_vertex.wesl`,
  `coat_prepass.wesl` and `coat_fragment.wesl`, with the deformation every pass shares in
  `deform.wesl` and dual-quaternion or centre-of-rotation skinning in `dqs.wesl`. Env knobs for
  comparing: `ORNISCULPT_NO_MUSCLE`, `ORNISCULPT_NO_SOFT`, `ORNISCULPT_NO_CONTACT`,
  `ORNISCULPT_LINEAR_SKIN`.
- **T-9** (*revised*) Fur shells exist at hero detail only, `Fur::shells` of them. Crowd tiers
  shade fur on the skin: alpha-tested layers defeat early depth rejection across thousands of
  mobs.
- Skin shading (2026-10-27): low specular, a mottled sheen, cavity and body-scale shade from the
  vertex data. Not yet: subsurface scattering, pores, fur and feather shadows.
- Measured (2026-10-17, `examples/crowd.rs`): 4000 mobs in mixed coats about 15 ms/frame
  against 14–15 ms plain. The versioned bench supersedes these for comparisons (`bench/LOG.md`).

---

## 6. Motion

[`motion-api.md`](motion-api.md) covers the cycle and graph authoring API in depth. This
section is the runtime's shape.

### 6.1 Gait

```rust
pub struct Gait { pub modes: Vec<GaitMode>, pub turn_rate_deg: f32, pub accel: f32, pub heft: HeftResponse }   // *.gait.ron
pub struct GaitMode { pub name: String, pub min_speed: f32 /* leg lengths/s */, pub pattern: PhasePattern,
                      pub duty: f32, pub stride: f32, pub lift: f32, pub bob: f32, pub body: BodyMotion, pub shape: StepShape }
pub enum PhasePattern { Alternate, LateralSequence, Diagonal, Bound, Gallop { lead: Side }, Tripod, Wave { lag: f32 }, Custom(Vec<f32>) }
pub struct BodyMotion { yaw, roll, sway, rock, counter, steady, nod, tail }    // how the torso rides the feet
pub struct LegInfo { pub chain: SmallVec<[BoneId; 4]>, pub foot: BoneId, pub pair: u8, pub side: Side,
                     pub stance: Stance, pub rest_foot: Vec3, pub pole: Vec3, pub length: f32 }
```

- **T-10** A gait assigns phase from `(pair, side)`, never from bone names, so one trot works on
  any quadruped and one tripod on any hexapod. `Gait::for_legs` gives the defaults: bipeds walk
  and run, quadrupeds walk and trot, hexapods tripod, more legs wave. `MobGait` overrides it; a
  gait that fails to load is reported once and the default plays.
- Speeds are in leg lengths per second, so one gait scales from a goblin to an ogre.
- **Heft** (`motion::heft`): the body's weight (`BodyMass`, from the primitives' volume × the
  parts' `density`) changes cadence, duty, lift, landing, waddle and agility through curves in
  the gait (`HeftResponse`); `StepShape` shapes the foot arc and the torso's step signals.

### 6.2 Control (gameplay-facing)

```rust
pub struct MobIntent { pub velocity: Vec3, pub facing: Option<Dir3>, pub look_at: Option<Vec3> }
pub struct ExternalRoot;              // the game moves the Transform; velocity is inferred
pub struct FootProbes(SmallVec<[Vec3; 8]>);                 // one ray origin per leg, cast down, written every frame
#[require(CrowdGround)] pub struct FootGround(SmallVec<[Option<Vec3>; 8]>);   // the game's hits; None = root height
pub struct HandPose { pub left: Option<Hand>, pub right: Option<Hand> }   // per-digit curl and spread, hero only
```

- The plugin moves the root toward the intent, or reads it under `ExternalRoot` (root motion
  then lands in `RootMotionDelta`).
- Ground is the root height unless the game writes `FootGround`. Probes are written for hero
  mobs and for any mob with a `FootGround`: a game adds an empty one to the crowd mobs it wants
  on its terrain. Hero feet plant on the hits; crowd mobs sit on them through `CrowdGround`
  (§6.5).
- Not yet: `Reach` (arm IK toward a target) and fingers closing on a held grip
  (`roadmap.md`, Phase 2).

### 6.3 Hero tier: pose stages

The pose lives in a per-mob buffer (`MobPose { local: Vec<Transform>, .. }`). One system poses
every hero mob in parallel, then one copies the pose to the joint entities.

State, one concern per component, all required by `Mob`: `Locomotion`, `GaitClock`,
`ActiveStep`, `GaitOverride`, `Feet`, `SpringState`, `LookState`, `TorsoState`, `Heft`,
`FootProbes`. `MobPose` and `CyclePose` come with the hero skeleton.

Systems, in order, inside `MobSet::Motion` (`MotionSystems`):

1. **Root:** `tick_motion_time` (frame time evened out, hitches passed through), `move_roots`.
2. **Player:** graphs and direct cycles advance; transitions, events, root motion.
3. **Clock** (every tier): gait mode for the speed, the step with heft applied, the phase.
4. **Pose:** hero: `write_probes` → `evaluate_cycles` → `pose_heroes` → `write_joints`; crowd:
   `bake_graph_clips` → `crowd_ground` → `crowd_animate`. With `soft-tissue`, the springs step on
   the final pose after the joints are written.

`pose_frame` (`motion::stages`): rest pose with the joints' scales → wings fold → feet (planted
in stance, arcs in swing toward home + velocity × stance / 2) → bob and bend into turns → torso
rides the feet (girdles turn, roll and sway; the spine bends between; the head steadies; the
tail swings), standing weight on one leg and a lean into acceleration → springs → cycles →
hands → shoulder rhythm (a third of an arm raise past 30° moves to the clavicle) → leg IK →
look-at (shared across the neck bones, clamped) → joint limits (`joint-limits`).

- **T-11** Layers are functions over components, and the pose is a `Vec<Transform>` rather
  than per-bone queries: IK needs random access to whole chains, and `get_many_mut` across bone
  entities would serialise the work. Joint entities still exist at hero tier for rendering
  (`SkinnedMesh`), attachment and gameplay reads.
- A foot touching down triggers `Footstep` on the mob, once the whole pose is done.

### 6.4 IK (math only)

```rust
pub fn two_bone_soft(joints: &mut [Vec3; 3], target: Vec3, pole: Vec3, soft: f32);
pub fn fabrik(joints: &mut [Vec3], target: Vec3, iterations: u32, tolerance: f32);
pub const SOFT_START: f32 = 0.95;
```

- Legs use the exact two-bone solver for the hip and knee, bending toward the leg's pole and
  soft near full extension: past 95 % of reach the leg approaches straight asymptotically
  instead of locking. A leg that stands nearly straight at rest starts easing just past its rest
  reach instead. Longer chains carry their lower segments below the knee (a pantograph for hind
  legs, less for fore legs), and a root may slide toward a foot it cannot otherwise reach. Only
  a single-bone leg falls back to FABRIK.
- Measured (2026-10-18): the knee snap when a trailing leg passed full extension went from 26 cm
  jumps in one frame to under 3 cm (`knees_move_smoothly`).

### 6.5 Baked motion (crowd tiers)

```rust
pub struct BakedMotion { pub clips: Vec<BakedClip>, pub idle: SkinFrame }   // over the crowd skeleton
pub struct BakedClip { pub start_phase: f32, pub frames: Vec<SkinFrame> }   // BAKE_FRAMES = 24 per cycle
pub type SkinFrame = Vec<[f32; 16]>;                                         // skin matrices (model × inverse bind)
pub fn bake_motion(a: &Anatomy, gait: &Gait) -> BakedMotion;                // at generation; part of GeneratedBody
pub fn bake_cycle(a: &Anatomy, rig: &MotionRig, cycle: &Cycle, mask: &MaskWeights) -> Option<BakedClip>;
pub struct CrowdGround { pub lift: f32, pub tilt: Quat, pub feet: SmallVec<[f32; 8]> }
pub fn ground_fit(legs: &[LegInfo], root: &Affine3A, hits: &[Option<Vec3>], leg: f32) -> Option<CrowdGround>;
```

- **T-12** (*revised*) Clips are derived automatically: the procedural solver walks each gait
  mode in a straight line at a representative speed and, after a warm-up, records one cycle of
  final skin matrices, plus the idle pose. Nothing is authored per mob. There are no turn clips.
- **T-13** (*revised*) Baked gait motion is part of the `GeneratedBody`, so it is shared by every
  mob with the same `BodyKey`. A graph's idles and actions are baked once per (body, graph set)
  (`CrowdCycles`, 2026-10-31) and appended to the clip atlas (§10). With `soft-tissue`, every
  frame also carries one matrix per crowd soft region.
- **Uneven ground** (2026-10-31): baked clips assume flat ground. A crowd mob with `FootGround`
  hits gets a `CrowdGround`: the plane through its hits as a root lift and tilt (clamped to a 0.6
  slope and 0.6 leg lengths), and each foot's distance from that plane (0.4 leg lengths at most),
  eased at 8/s. The crowd blend applies it last, down each leg (`motion-api.md` §7,
  `examples/slope.rs`).

### 6.6 Cycles, graphs and the library

Built 2026-10-23 to 2026-10-31; the design is [`motion-api.md`](motion-api.md).

- **`Cycle`** (`*.cycle.ron`, or built in Rust): one loop or one-shot of body-relative motion. It
  never names bones: it drives **targets** (`Root`, `Spine`, `Neck`, `Head`, `Jaw`, `Tail`,
  `Ears`, `Antennae`, `Legs`, `Arms`, `Wings`, `Digits`) through typed **channels** (`Bob`,
  `Sway`, `Surge`, `Pitch`, `Yaw`, `Roll`, `Open`, `Perk`, `Flex(Joint)`, `FootLift`,
  `FootReach`, `Swing`, `Raise`, `Fold`, `Flap`, `Sweep`, `Curl`, `Spread`) with curves over
  its time. Units are leg lengths and degrees, with fixed ranges per channel, checked at load.
- **`Fit`**: which bodies a cycle may play on, by `BodyFamily` (UprightBiped, Biped, Quadruped,
  Hexapod, Octopod, Myriapod, Serpentine, Other), leg count, stance, required and excluded
  `Feature`s (Neck, LongNeck, Tail, LongTail, Jaw, Ears, Antennae, Horns, Arms, Grip, Wings,
  GroundReach) and size. A body's traits are derived from its anatomy, never authored. A misfit
  never plays; the reason is logged once.
- **`MotionGraph`** (`*.graph.ron`): masked layers (`BodyMask`: All, Upper, Lower, HeadNeck,
  Tail, Wings, Arms, Targets), each a state machine over cycles (`Play`: Empty, Cycle,
  Locomotion, Blend), with transitions on parameter conditions and `Switch` timing. A mob runs
  several graphs composed by name (`MobMotion`). `MotionParams` holds the values transitions
  read; `speed`, `moving` and `turn` are written by the plugin. `PlayCycle` plays one cycle
  directly. Cycle events arrive as `MotionEvent`s.
- **Library** (`motion::library`, in Rust): gaits for every family, idles, one-shot actions, and
  `standard()`, a graph for any body (locomotion, breath, ears, feelers, head, tail, wings and an
  action layer on triggers `attack`, `shake`, `rear`, `wave`, `flare`, `eat`).
- Still open: an action drops the breathing layer in the crowd (three layers in all);
  crossfades inside a crowd layer show only the heavier cycle; a graph's own gaits are not baked
  (`roadmap.md`, Phase 2).

### 6.7 Runtime body levers (the game drives these)

```rust
#[derive(Component)] pub struct BoneScales(pub SmallVec<[BoneScale; 8]>);   // empty = identity
pub struct BoneScale { pub bone: BoneId, pub length: f32, pub girth: f32 }
```

- **T-14** (*revised*) `BoneScale` is **non-inherited**: `length` stretches the bone along its
  axis and moves its children, `girth` widens only that bone's skin, and children divide out
  their parent's scale so it does not compound down a chain. Applied every frame to the joint
  scales (`apply_bone_scales`, `MobSet::Levers`), so at hero tier only. Crowd tiers do not apply
  it yet (`roadmap.md`, Phase 2). It is not how a crowd mob's `Form` is approximated: every form
  has its own body (§7).
- Part-level changes go through the Part entities (§8.2): editing a `Part` or sending
  `SeverPart`/`GraftPart` regenerates.

---

## 7. Generation

### 7.1 Keys

```rust
pub struct BodyKey(pub u128);      // xxh3-128 of GENERATOR_VERSION | live plan | surface settings | seed (if used) |
                                   //   referenced mesh hashes | coat | outfit
pub const GENERATOR_VERSION: u32 = 1;   // bump when the output changes for the same input
```

- Equal keys share one `GeneratedBody`. The seed only counts when some level meshes
  seed-dependent geometry (`geometric_folds`), so identical bodies normally share meshes.
- There is no separate base key or motion key. **T-15** (*revised*): a crowd mob renders the
  body of its own key, not a base body with `BoneScales` from its `Form`. Mobs with the same
  plan, form, coat and outfit share everything; a different form is a different body.

### 7.2 Generated assets

```rust
#[derive(Asset)]
pub struct GeneratedBody {
    pub key: BodyKey, pub anatomy: Arc<Anatomy>,
    pub lods: [LodMeshes; 3],                  // one mesh per material slot; Hero full skeleton, coarser crowd skeleton
    pub full_bindposes: Handle<SkinnedMeshInverseBindposes>,
    pub radius: f32,                           // for tiering
    pub motion: Arc<BakedMotion>,              // the default gait of its leg count, baked
    pub coat_meshes: [CoatMeshHandles; 3],     // shells, feathers, hair (hero only)
    pub materials: [SlotMaterials; 2],         // [hero, crowd], one per slot kind
    pub soft: Arc<SoftTissue>,                 // soft-tissue feature
}
pub struct GenInput { live: BodyPlan, rules: PartLodRules, assets: PartAssets, settings: SurfaceSettings, seed: u64, coat: Coat, outfit: Outfit }
pub struct GenOutput { anatomy, lods: [BodyMeshData; 3], motion: BakedMotion, coat, coat_meshes, outfit, contacts: ContactSet, soft }
pub fn generate_body(input: GenInput, times: &mut StageTimes) -> Result<GenOutput, String>;
```

Stages, in order, each timed (`StageTimes`, a Tracy span each): `anatomy` → `fit_outfit` →
`mesh` (all levels) → `garments` → `annotate` → `centres_of_rotation` → `muscle_flex` →
`soft_tissue` → `cover_skin` → `bake_motion` → `coat_meshes` → `contacts`. The bench reports
them per stage (`bench/LOG.md`; on the Steam Deck the head's SDF conversion and centres of
rotation are the stages furthest behind, 2026-10-31).

### 7.3 Components, resources and flow

```rust
#[derive(Component)] pub struct GenRequest;            // marks a mob that needs (re)generation
#[derive(Component)] pub struct Placeholder;           // shown while the first body generates
#[derive(Resource)] pub struct GenCache { pub bodies: HashMap<BodyKey, Handle<GeneratedBody>>, /* in flight, failures, names */ }
#[derive(Resource)] pub struct GenBudget { pub max_in_flight: usize /* 4 */, pub max_completions_per_frame: usize /* 2 */,
                                           pub reserved_threads: usize /* 2 */ }
#[derive(Resource)] pub struct PartAssetCache { pub assets: PartAssets, .. }   // authored meshes, extracted once
#[derive(EntityEvent)] pub struct BodyReady { pub entity: Entity }
#[derive(EntityEvent)] pub struct BodyFailed { pub entity: Entity, pub error: String }
```

Flow (`MobSet::Generation`):

1. An observer on `Add<Mob>` (and on inserting or removing `MobOutfit`) inserts `GenRequest`.
2. `start_generation`: once the plan, coat, outfit and part meshes are loaded, resolve the live
   body (`apply_form`), compute the key, then reuse a cached or in-flight body or start a task
   on `AsyncComputeTaskPool` within the budget. The task's parallel stages share the cores not
   reserved for the main and render threads.
3. `finish_generation` polls tasks and turns outputs into `GeneratedBody` assets, materials and
   meshes (built off the main thread).
4. `attach_body` spawns the crowd meshes (near and far) under the mob, removes the placeholder
   and triggers `BodyReady`. Hero visuals (joints and skinned meshes) are managed by tier (§9).
5. `register_clips` appends the body's baked clips to the crowd atlas once (§10).

- **Bad assets fail fast** (2026-10-31): a plan, coat, outfit or part mesh that fails to load, or
  a body that cannot be built, triggers `BodyFailed` on each waiting mob with the error, logged
  once per asset. The mob keeps the body it had and is requested again if the asset reloads.
  Headless example runs exit non-zero with the error.
- Hot reload (`reload_changed_assets`): mobs whose plan or authored meshes changed on disk
  regenerate.
- Not yet: a disk cache by `BodyKey`, and freeing a body's atlas space when it goes
  (`roadmap.md`, Phase 2).

---

## 8. The mob in the ECS

### 8.1 Mob root

```rust
#[derive(Component)]
#[require(Transform, Visibility, Form, MobSeed, BoneScales, MobIntent, Locomotion, GaitClock, ActiveStep,
          GaitOverride, Feet, SpringState, LookState, TorsoState, Heft, FootProbes)]
pub struct Mob { pub body: Handle<BodyPlan> }

#[derive(Component)] pub struct MobSeed(pub u64);           // per-mob noise; anatomy never uses it
#[derive(Component)] pub struct LiveBody(pub BodyPlan);     // the expanded, form-applied plan: the source of truth after spawn
#[derive(Component)] pub struct MobBody(pub Handle<GeneratedBody>);
#[derive(Component)] pub struct MobTier(pub LodLevel);      // missing until the first body is attached
// Optional: MobCoat(Handle<Coat>), MobOutfit(Handle<Outfit>), MobGait(Handle<Gait>), MobMotion(Vec<Handle<MotionGraph>>),
//           FootGround, ExternalRoot, HandPose, ForceTier.
```

```rust
commands.spawn((Mob::new(asset_server.load("mobs/ox.body.ron")), MobCoat(asset_server.load("coats/hereford.coat.ron"))));
```

- `Form` is a component only, not an asset. There is no composed "mob description" asset: a
  mob is the components it is spawned with.
- Hero-tier visuals: `MobSkeleton(Vec<Entity>)` indexes the joint entities (`MobJoint { mob,
  bone, rest }`) by `BoneId`; `LodMesh(LodLevel)` marks each level's mesh entities;
  `HeroVisual` marks what exists at hero only; `MobVisual` marks everything replaced on
  regeneration.

### 8.2 Part entities (hybrid part tree)

```rust
#[derive(Component)] pub struct Part { pub path: PartPath, pub kind: PartKind }   // editable view of one LiveBody node
#[derive(Component)] pub struct PartLod(pub LodLevel);
#[derive(Component)] #[relationship(relationship_target = MobParts)] pub struct PartOf(pub Entity);
#[derive(Component)] #[relationship_target(relationship = PartOf)] pub struct MobParts(Vec<Entity>);
#[derive(EntityEvent)] pub struct SeverPart { pub entity: Entity /* the mob */, pub path: PartPath }
#[derive(EntityEvent)] pub struct GraftPart { pub entity: Entity, pub parent: PartPath, pub node: PartNode }
```

- **T-16** Part entities are spawned per **part LOD** (§2.9): core parts at every tier, fine
  parts only at the tiers where they are meshed. Gameplay edits a `Part`'s `kind`, and
  `sync_part_edits` writes it into `LiveBody` and requests a regen. Removing or adding anatomy
  uses `SeverPart` and `GraftPart` (a grafted node's symmetry is expanded, so `mirror: true`
  grafts a pair), so a LOD despawn can never be mistaken for dismemberment.
- Not built: `HiddenRegions`, which would hide a severed part while the regen runs. The part
  disappears when the new body arrives.

### 8.3 Equipment: sockets and tools (`src/equip/`)

```rust
pub enum SocketSel { Kind(SocketKind, Side), Named(String) }
#[derive(Component)] #[relationship(relationship_target = Holding)]
pub struct HeldBy { pub mob: Entity, pub socket: SocketSel, pub grip: Transform /* where the tool is held, in its own space */ }
#[derive(Component)] #[relationship_target(relationship = HeldBy)] pub struct Holding(Vec<Entity>);
```

- A tool is any entity the game spawns (a mesh, a scene, a physics body). Inserting `HeldBy`
  puts it in a socket; removing it lets go, and the tool stays where it was. There is no tool
  asset type.
- **T-17** (*revised*) There are no socket entities and no crowd-pipeline tool draws. Held tools
  stay **root entities** (an observer removes any `ChildOf`), and `place_held` (PostUpdate,
  before transform propagation) writes their world `Transform` every frame. At hero tier the
  socket follows the joint entities. In crowd tiers it comes from the socket bone's skin matrix,
  blended on the CPU from the same baked frames, weights and layers the GPU uses
  (`CrowdSkin::blend_bone`). So tools survive tier changes, regeneration and the holder's
  despawn, and gameplay always has a real transform. Bevy instances identical tool meshes.
- Measured (2026-10-17, `examples/hold.rs`): 2000 mobs holding about 5000 tools, 12–19 ms/frame.
- Not yet: fingers closing round a grip, `Reach`, `BoneScales` in crowd-tier placement, and the
  ground fit's foot offsets (tools take the lift and tilt).

---

## 9. Tiering and quality

```rust
#[derive(Resource)]
pub struct QualityProfile { pub hero_px: f32, pub near_px: f32, pub hysteresis: f32, pub promotions_per_frame: usize, pub max_hero: usize }
#[derive(Component)] pub struct TierCamera;           // cameras considered; the largest on-screen size wins
#[derive(Component)] pub struct ForceTier(pub LodLevel);
```

| Profile | hero at ≥ | near at ≥ | hysteresis | promotions/frame | max hero |
|---|---|---|---|---|---|
| LOW | 500 px | 120 px | 15 % | 4 | 8 |
| MEDIUM (default) | 300 px | 80 px | 15 % | 8 | 32 |
| HIGH | 200 px | 50 px | 15 % | 16 | 64 |

- **T-18** Tier = the mob's projected on-screen height (its bounding radius) across all
  `TierCamera`s, with hysteresis against the current tier. The biggest on screen are served
  first, so the hero cap and the promotion budget favour what the player sees. `ForceTier` pins
  story mobs and first-person targets. With no `TierCamera`, every mob is hero.
- `apply_tier_visibility` shows the mesh entities of the current tier; `sync_hero_visuals`
  spawns joints and skinned hero meshes at `Hero` and removes them below it.
- Not yet: the profile does not scale `SurfaceSettings` (voxel sizes, shell counts); those are
  resources a game sets directly.

---

## 10. Crowd rendering

```rust
pub struct CrowdSlot(pub u32);            // the mob's slot in the bone buffer; also its meshes' MeshTag
#[repr(C)] pub struct CrowdInstance {     // 96 bytes, uploaded only when it changes
    pub idle: u32,                        // atlas index of the idle pose
    pub bones: u32,                       // crowd slot entries to write; 0 disables the slot
    pub legs: u32,                        // atlas index of the body's leg rows (ground fit), MASK_ALL for none
    pub lift: f32, pub tilt: [f32; 4],    // the ground fit's root lift and tilt
    pub feet: [u32; 4],                   // eight foot offsets as unorm16 pairs over FOOT_RANGE = 8 m
    pub layers: [CrowdLayer; CROWD_LAYERS /* 3 */],
}
pub struct CrowdLayer { pub frame_a: u32, pub frame_b: u32, pub mask: u32 /* row index; MASK_ALL; LAYER_ADDITIVE bit */, pub blend: u32 /* (t, weight) unorm16 */ }
pub struct ClipLayout { pub base: u32, pub bones: u32, pub modes: u32, pub legs: u32 }   // where a body's gait clips live
#[derive(Resource)] pub struct CrowdSkin { pub bones: Handle<ShaderBuffer>, /* slot allocator, clip atlas + CPU copy */ }
```

- Joint entities exist only at `Hero`. Every generated body's baked clips are appended once to a
  GPU **clip atlas** (`register_clips`); a graph's idles and actions per (body, graph set) too,
  each with a mask row of the crowd bones it moves (§6.5).
- Each frame `crowd_animate` computes every non-hero mob's `CrowdInstance` in parallel: the
  idle pose plus up to three layers of frame pairs. Layer 0 is the gait at `1 − idle weight`;
  the two heaviest playing graph layers fill the rest (override before additive, then heavier).
  Standing mobs with no graph upload nothing.
- A compute pass (`crowd_blend.wesl`, once per frame in `RenderGraphSystems::Begin`) blends
  the frames into a GPU-only bone buffer: start from the idle, lay each layer over it by its
  weight and mask row (or add its difference from the idle), then tilt and lift for the ground
  fit and move each leg's bones by its foot offset times how far down the leg they sit.
- Near and far meshes use the same `MobMaterial` with `COAT_CROWD`: the vertex shader skins
  from the bone buffer at the slot in the mesh's `MeshTag`. Mobs sharing a body share mesh and
  material, so Bevy batches them into instanced draws. Crowd tiers cast no shadows.
- **T-19** (*revised*) A crowd mob is its root plus one mesh entity per slot per crowd level,
  with no joint entities and no per-bone systems. There are no update buckets: an instance is
  recomputed every frame and uploaded only when it changed.
- Measured (2026-10-17, `examples/crowd.rs`): 4000 mobs about 13.7 ms/frame, about 99 KB/frame
  of instance uploads (was 8 MB of matrices). The versioned bench measures the crowd differently
  (1920×1080, coats on, cold start and steady state): tir 62.8–68.8 ms steady, Steam Deck
  62.7–63.2 ms (`bench/LOG.md`, 2026-10-29 and 2026-10-31).
- **What a tier costs** (Steam Deck, 2026-11-02, 4000 bare mobs at a fixed camera,
  `ORNISCULPT_TIERS` sweeps): about 0.5 ms a **hero** mob, 5 µs a **near** mob and under half a
  microsecond a **far** one, over a base under 1 ms. The hero tier dominates: at the default cap
  of 32 it is 16 of the 27 ms. On tir the balance was the other way round (the near tier at
  24.5 µs a mob); the Deck's near tier is five times cheaper than tir's, its hero tier dearer.
  A 60 fps frame there buys about 30 hero mobs, or thousands of crowd ones. Nearly all of a
  hero's cost is its shadow casting (0.47 of the 0.49 ms; crowd tiers are `NotShadowCaster`):
  its mesh, joints and skinning together come to 24 µs.
- Not yet: crowd shadows, `BoneScales` and custom gaits in crowd tiers, `PlayCycle` in crowd
  tiers, atlas compaction when bodies are freed, dispatch splitting above about 2M crowd bones.

---

## 11. Plugin and system sets

```rust
pub struct OrnisculptPlugin;   // AnatomyPlugin, CoatPlugin, SurfacePlugin, MobPlugin, MotionPlugin, EquipPlugin, ClothingPlugin
pub enum MobSet { Generation, Tiering, Motion, Levers }        // Update, chained
pub enum MotionSystems { Root, Player, Clock, Pose }           // chained inside MobSet::Motion
// PostUpdate: equip::place_held before TransformSystems::Propagate.
// Render world: crowd extract, buffer prepare, bind group, and the blend dispatch in RenderGraphSystems::Begin.
```

The game writes `MobIntent` (and `MotionParams`, `FootGround`) anywhere before `MobSet::Motion`.

---

## 12. Examples, the bench and tools

Examples double as visual tests (`README.md` has the run lines). Every windowed example also runs
headless: `ORNISCULPT_SCREENSHOT=out.png` renders to an image and exits, `ORNISCULPT_CAM`
places the camera, and a bad asset exits non-zero with the error. Early exits sometimes crash in
teardown, so scripts test for a non-zero exit rather than 1 (`roadmap.md`, Phase 2).

| Example | Shows |
|---|---|
| `viewer` | One mob with LOD, tone and rig debugging; hot-reloads its plan, coat and outfit. |
| `walk` | Several bodies walking in their coats. |
| `motion` | The motion API: every body on the standard graph. |
| `hands` | Hand poses on a standing woman. |
| `hold` | Held tools in every socket, at hero and crowd tiers. |
| `mobs` | Shared bodies, tiers by on-screen size, runtime levers. |
| `crowd` | Thousands of wandering mobs across several plans (`ORNISCULPT_COATS`, `ORNISCULPT_OUTFITS`). |
| `slope` | Crowd mobs sitting on uneven ground through `FootGround`. |
| `fields` | The field combinators and the sweep SDF, voxelised. |
| `viability` | The built-in checks over every sample plan, headless. |
| `bench` | The fixed, versioned benchmark (`BENCH_VERSION` = 5). |

- **The bench** (`examples/bench.rs`, [`perf-and-look-plan.md`](perf-and-look-plan.md) §1):
  `gen` (generation per stage), `hero` (stills at full body, head and eye framings), `motion`
  (two bodies walking) and `crowd` (4000 mobs, cold start then steady). Headless at a fixed
  1920×1080, animation stepped exactly 1/60 s, inputs hashed into each report. `compare` prints
  deltas against one or more baselines and fails on a regression beyond the noise floor or an
  image change beyond the gate. There are no criterion benches and no separate bake binary.
- **Rule:** no look or perf change lands without a before/after run and a row in `bench/LOG.md`.
- **T-21** Benches and examples use the same `assets/` set, so a performance number always
  matches something you can look at.
- `tools/`: `gen_garments.py`, `gen_sample_parts.py`, `gen_horns.py`, `gen_head.py` and the
  warps (`gen_head_orc.py`, `gen_head_gnome.py`, `gen_head_goblin.py`, `head_warp.py`),
  `web.sh` and `web-run.sh` (the browser build), `deck-bench.sh` (the bench on a Steam Deck).

---

## 13. Clothing (`src/clothing/`)

[`authored-parts-and-clothes.md`](authored-parts-and-clothes.md) has the requirements and the
gaps; [`interchange.md`](interchange.md) covers garments modelled in a DCC.

```rust
pub struct Outfit { pub garments: Vec<Garment> }    // *.outfit.ron, worn through MobOutfit; inserting or removing it regenerates
pub struct Garment { pub name: String, pub on: String /* part pattern */, pub span: (f32, f32), pub mesh: String,
                     pub color: [f32; 3], pub roughness: f32, pub ease: f32, pub reach: f32, pub hang: Option<Hang> }
pub struct Hang { pub length: f32, pub chains: u8, pub segments: u8, pub droop: f32, pub flare: f32,
                  pub stiffness: f32, pub damping: f32, pub follow_legs: f32 }
pub const MAX_GARMENTS: usize = 8;                  // palette entries; later garments reuse the last colour
```

- Wrapped garments only, **baked into the body**. Garment meshes are authored in **wrap
  space**: a unit cylinder per body part, `y` 0..1 over the span, the angle around Y = the angle
  around the part (−Z dorsal, +X left or inner; mirrored on mirrored parts), `r − 1` = metres
  above the skin.
- **Fitting** (`fit_outfit`, after `build_anatomy`) marches from the part's axis samples out to
  the real body SDF at every vertex, so a garment takes each region's actual shape. It hugs the
  whole body but never sits further out than its own part's surface + `reach` × radius, so
  neighbouring parts emerge through it (arms through a tunic) unless `reach` is raised (breeches
  over leg roots).
- `y` beyond 0..1 either **hangs** (spring-bone chains appended to the skeleton, skinned by angle
  and drop, following the nearest leg's swing by `follow_legs` through `Anatomy::cloth_follow`)
  or, without `hang`, **caps** the part's end (shorts over a pelvis). `finish_skeleton` keeps
  cloth bones in the crowd skeleton while the 32-bone budget allows.
- **Cover** (`cover_skin`, 2026-10-26): cloth close over the skin takes the skin's muscle flex
  and soft-tissue weights, so the two move as one. Skin no one can see under close-fitting cloth
  leaves the mesh, with a band kept at every hem.
- Cloth is one `MaterialSlot::Cloth` submesh per level (simplified and pushed out a little at
  Near and Far), coloured from the outfit's 8-entry palette in `CoatUniform::cloth` (index in
  the vertex colour's w), with a faint weave that fades by pixel footprint.
- Samples: `tools/gen_garments.py` → `assets/garments/garments.gltf`;
  `assets/outfits/{peasant,warrior,harness,orc}.outfit.ron`.
- Measured again (2026-11-02, `examples/crowd 4000 ORNISCULPT_OUTFITS=1`): dressing the crowd
  costs 0.8 ms a frame on toth (15.45 → 16.25 ms) and about 2 ms on the Steam Deck (41.3 → 43.0),
  not the 23–27 against 15 ms read on 2026-10-18 on tir. The cloth submesh is an extra *entity*
  per dressed mob, not an extra draw: crowd mobs are instanced (the v5 main pass runs in 1630
  instances). Merging it into the skin mesh was built and measured and gains nothing — it loses
  0.1 to 0.9 ms on toth, where the cloth branch joins a crowd-skin shader already doing fur,
  feathers and scales, and sits inside the spread on the Deck. Not kept (`bench/LOG.md`).
- Not yet: head and hood mapping, hanging from both span edges, layering beyond `ease`, garments
  in contacts, textures.

---

## 14. Soft tissue (`soft-tissue` feature, `src/soft.rs`)

- Each fat or muscle stamp becomes a `SoftRegion`: one damped spring carrying a point of tissue
  behind its bone, driven by its anchor's acceleration, resting where gravity pulls it. A
  muscle's spring stiffens and swells as it works. `Stamp::soft` tunes firmness, damping and
  travel.
- Decided at generation and kept in the `GeneratedBody`: the regions (at most 32 on a hero
  body, 8 in the crowd), each vertex's share of up to two regions (`ATTRIBUTE_SOFT`), and the
  crowd's jiggle, baked as one extra matrix per region after the crowd bones (`CROWD_SLOT` = 40).
- Hero mobs run the springs on the final pose each frame and get one `SoftJoint` entity per
  region, a child of its bone. On the GPU a region is a pair of joints: the soft joint is its
  bone's skin matrix moved by the spring, so `S·p − B·p` is the offset (`deform.wesl`,
  `COAT_SOFT`).

---

## 15. Interchange (`interchange` feature, `src/interchange/`)

[`interchange.md`](interchange.md) is the design. In short: body plans, authored parts and
garments round-trip through glTF 2.0 with Ornisculpt's data in `extras.ornisculpt`
(`FORMAT` = 1). A `*.body.glb` or `*.body.gltf` holds one empty per part, nested as the part
tree, plus a reference skeleton and skin to model against; it loads directly as a `BodyPlan`
(`BodyGltfLoader`), and an unedited file comes back as an equal plan
(`round_trip_every_sample_plan`). Authored parts are mesh nodes modelled in place; garments are
fitted meshes solved back into wrap space on import. The tools that use it (export, convert,
templates, the Blender add-on) live in ornisculpter.

---

## 16. What is not built

The order work goes in is [`roadmap.md`](roadmap.md). Gathered from the sections above:

- A disk cache of generated bodies; freeing a body's atlas space when it goes (§7, §10).
- `Reach` arm IK; fingers closing on a grip; `BoneScales` and custom gaits in crowd tiers; crowd
  shadows (§6, §8.3, §10).
- `HiddenRegions` (§8.2); `Wrist` sockets (§3.1); `QualityProfile` scaling the surface settings
  (§9).
- Clothing: hoods, both-edge hangs, layering, garments in contacts (§13). Merging the cloth
  submesh into the skin mesh (§10).
- Wings: `WingCoat`, the propatagium and uropatagium, flight (§2.11).
- Skin: subsurface scattering, pores, fur and feather shadows (§5). Per-limb muscle tables and
  real hands (§2.7).
- Crowd motion: an action drops the breathing layer; crossfades inside a layer; a graph's own
  gaits (§6.6).
