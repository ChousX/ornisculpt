# Ornisculpt: Types & Traits (draft v1)

> How to review: decisions are tagged **[T-x]** and open questions **[TQ-x]**. Reply by tag.
> v1 changes: one crate, Bevy math, no species (mobs are composed from anatomy pieces), ECS-first, no genetics, no wings.

> **v2 changes (anatomy stubs review)** — these supersede the sections below where they conflict:
> - RNG = `rand` + `rand_pcg::Pcg64Mcg`, forked by `xxh3(seed, label)`; noise = `fastnoise-lite` via a serializable `NoiseSettings` (`src/random.rs`). No custom RNG.
> - **Anatomy has no RNG and no seed.** `build_anatomy(live_plan, lod_rules)` is a pure function, so equal bodies always share one anatomy and base mesh. Seeds only feed surface noise and coat variation.
> - `BodyPlan::expand_symmetry` makes every mirrored/radial copy an explicit node (with `side`). `LiveBody` stores the expanded plan, so one copy can be patched or severed alone. A `FormPatch` on an unsuffixed path hits all copies.
> - `Neck`/`Tail` merged into `Chain(ChainPart { role: Neck|Tail|Ear|Antenna|Tentacle, chain })`; new `RegionKind::Appendage`.
> - `Attach { slot, t }` → closed enum `AttachSlot { Start, End, Along(t), Jaw, Eye, Ear, Horn, Mouth }` (typos fail at parse time).
> - Part offsets are `PartOffset { translate, rotate_deg, scale }` instead of `Transform` (no quaternions in RON).
> - `Form` is a component only (not an asset). `RegionSelector::Not(Box)` → `AllExcept(Vec)`.
> - `MobTier` will reuse `LodLevel`; "pending" = no tier component yet (one enum instead of two).
> - **Levers replace reflect paths.** `FormPatch { part, lever: Lever, value }`. Each `Lever` has a fixed `LeverKind` (Factor ×, Delta +, Count =); part kinds declare supported levers (`PartKind::levers`), placement levers work on every part. `Segments`/`Count`/`Tines` change topology (`Segments` and `Count` break skeleton/motion sharing). See `src/anatomy/lever.rs`.
> - **Symmetry → `mirror: bool` + `repeat: Repeat { Once, Series { count, from, to }, Radial { count } }`**, so mirrored series (centipede legs) work. New attach slots `Surface { t, around_deg }`, `Nose`, `Crown`.
> - **Equipment (implemented, `src/equip/`)**: hold only. Sockets are emitted by parts: `grip.l/.r` (grasping manipulator limbs), `head`, `mouth`, `neck` (neck chains), `tail_tip`, and on the torso `back`, `saddle`, `chest`, `hip.l/.r`, `shoulder.l/.r`. Socket frame convention: origin at the hold point, +Y along the held tool's main axis (across the fist, forward when the arm hangs; out of the skin for body sockets; forward out of the mouth). A tool is any entity the game spawns (mesh, scene, physics body); `HeldBy { mob, socket: SocketSel::Kind(kind, side) | Named(name), grip }` (relationship, target `Holding`) puts it in a socket, and removing it drops the tool in place. Deviation from T-17: there are no socket entities and no crowd-pipeline tool draws. Held tools stay root entities and `place_held` (PostUpdate, before transform propagation) writes their world `Transform` every frame. At hero tier this comes from the joint entities; in crowd tiers it comes from the socket bone's skin matrix, blended on the CPU from the same baked frames and weights the GPU uses. So tools survive tier changes, regeneration and the holder's despawn, and gameplay always has a real transform. Bevy instances identical tool meshes itself. Measured: 2000 mobs holding ~5000 tools, 12–19 ms/frame. Not yet: fingers closing around a held grip, `Reach` (arm IK toward a target), BoneScales in crowd-tier placement.
> - **Clothing (implemented, `src/clothing/`)**: wrapped garments only, baked into the body. An `Outfit` asset (`*.outfit.ron`, worn via `MobOutfit`; inserting/removing it regenerates) lists `Garment { on: part pattern, span: (t0, t1), mesh, color, roughness, ease, reach, hang }`. Garment meshes are authored in **wrap space**: a unit cylinder per body part, `y` 0..1 over the span, angle around Y = angle around the part (-Z dorsal, +X left/outer, mirrored on mirrored parts), `r - 1` = metres above the skin. Fitting (`fit_outfit`, in the generation task after `build_anatomy`) marches from the part's stored axis samples (`BuiltPart::axis`, now kept and grounded with the body) out to the real body SDF at every vertex, so a garment takes each region's actual shape (muscle, fat, ellipse). It hugs the whole body but never sits further out than its own part's surface + `reach` × radius, so neighbouring parts emerge through it (arms through a tunic) unless `reach` is raised (breeches over leg roots). `y` beyond 0..1 either **hangs** (`Hang`: spring-bone chains appended to the skeleton, skinned by angle and drop, optionally following the nearest leg's swing via `Anatomy::cloth_follow`) or, without `hang`, **caps** the part's end (shorts over a pelvis). `Anatomy::finish_skeleton` rebuilds inverse binds and the crowd skeleton (cloth bones kept while the 32-bone crowd budget allows). Cloth is one `MaterialSlot::Cloth` submesh per LOD (simplified and pushed out a little at Near/Far), coloured from an 8-entry palette in `CoatUniform::cloth` (index in vertex colour w), with a faint weave that fades by pixel footprint. Samples: `tools/gen_garments.py` → `assets/garments/garments.gltf`; `assets/outfits/{peasant,warrior,harness}.outfit.ron`; `viewer [body] [coat|-] [outfit]`, `crowd` with `ORNISCULPT_OUTFITS=1`. Measured: 4000 mobs, bipeds and quadrupeds dressed, ~23–27 ms/frame vs ~15 ms plain (the cloth submesh is an extra crowd draw entity per dressed mob; merging it into the skin mesh is the obvious next optimisation). Not yet: hiding skin under opaque garments, head/hood mapping (heads have no axis), hanging from both span edges, garment-to-garment layering beyond `ease`.
> - **Real proportions (`docs/design/anatomy-reference.md`)**: the sample plans are rebuilt from measured or conformation proportions: `biped` (human), `quadruped` (dog), `beast` (bear, plantigrade, keeps the authored feet and crest), `horse` (new, unguligrade) and `hexapod` (insect). Format additions:
>   - `ChainParams::ratios` gives relative segment lengths, and radius profiles now run along real length.
>   - `LimbParams::joints` names the limb's joints (`JointPlan`) and their rest flexion; each hinge knows which way it closes and its range, so a reversed knee does not build. Raw `bends` (`(forward, up)` degrees) remain for limbs without joints.
>   - Leg IK poles come from the first declared hinge (elbows back, stifles forward, insect knees up).
>   - Legs with more than two segments solve their upper two bones exactly and carry the lower segments at rest offsets, which keeps cannons and metatarsals steady.
>   - Mesh voxel sizes scale with body extent beyond 1.8 m, so a horse fits the triangle budget.
>   - `finish_skeleton` now enforces the 32-bone crowd cap.
>   - Test `sample_plans_walk_without_snaps` walks every sample plan.
> - **Leg IK (revised)**: two-bone legs use an exact solver (`two_bone`) bending toward the leg's pole (plus some up, so a forward-swinging foot doesn't degenerate it) and soft near full extension (from 95% of reach the leg approaches straight asymptotically instead of locking). Longer chains use FABRIK with `orient_to_pole` before and after. This removed a knee snap when a trailing leg passed full extension or FABRIK found the other bend side (26 cm knee jumps in one frame → under 3 cm; `motion::pose::tests::knees_move_smoothly`).
> - **Coat (implemented, `src/coat/`)**: `Coat` asset (`*.coat.ron`, worn via `MobCoat`; none = plain skin) = sRGB base/secondary/accent + `coverings: [(regions: RegionSelector, kind: Scales|Fur|Feathers)]` (later coverings win overlaps). Coat is part of the `BodyKey`, hot-reloads. One material type `MobMaterial = ExtendedMaterial<StandardMaterial, CoatExt>` for every surface and tier, specialised by `CoatKey { crowd, layer: Skin|Shell|Feather|Hard }` (shader defs); generation bakes per-vertex coat inputs (`coat::annotate`): coverage weights (scales, fur, feathers) smoothed over the skin so borders blend, plus a flow direction (primary bone axis, signed by region, tangent to the skin) and a distance-along-flow field integrated over the mesh so rows stay evenly spaced across bones. Vertex colour = rest position + scale weight; skin UV0 = (fur, feather) weights; tangent = flow + along (w). Bodies without coverings get none of these attributes. Scales: cellular colour + derivative bump in the skin shader (crowd: cheaper 2×2×2 search, no relief, covered pixels only). Fur: shells built from a 20%-simplified hero skin, `Fur::shells` layers, hero tier only. Feathers: cards scattered by area × density over covered skin, laid along body flow, skinned to the surface, hero tier only. Crowd tiers carry fur and feathers in the skin: fur = matte two-scale clump mottling plus a tip-coloured grazing rim, skin inflated by 0.8 × fur length; feathers = overlapping rows along the flow (3D Voronoi per row, rounded tips, shadow under the row above), skin inflated slightly. Every procedural pattern (scales too, hero included) fades to its average colour by world-space pixel footprint, with no derivatives, and skips its noise entirely once it is below a pixel. `HardKind::Plate` for scutes. `RegionSelector::mask` implemented. Measured: 4000 mobs in mixed coats ~15 ms/frame (plain ~14–15 ms). Not yet: patterns (stripes/spots), fur/feather shadows, multiple coverings of the same kind with different params.
> - **Crowd (implemented)**: joint entities exist only at `Hero` tier (`sync_hero_visuals`). Generation bakes `BakedMotion` (the procedural solver run headlessly per gait mode, 24-frame loops of final crowd-skeleton skin matrices + idle). Each body's clips are appended once to a GPU clip atlas. Non-hero mobs have a `CrowdSlot` and a 32-byte `CrowdInstance` (frame A/B/idle atlas offsets + weights), computed in parallel and uploaded only when it changes (standing mobs stop uploading). A compute pass (`crowd_blend.wesl`, scheduled once per frame in `RenderGraphSystems::Begin`) blends the frames into a GPU-only bone buffer; near/far meshes use `CrowdMaterial` whose vertex shader (`crowd_skin.wesl`) skins from it at `MeshTag` = slot. Crowd joints use custom attributes (`ATTRIBUTE_CROWD_JOINT_*`) because Bevy treats meshes with standard joint attributes as skinned. Measured at 4000 mobs: ~13.7 ms/frame, ~99 KB/frame of instance uploads (was 8 MB of matrices). Not yet: crowd shadows, `BoneScales` and custom `MobGait` in crowd tiers, atlas compaction when bodies are freed, dispatch splitting above ~2M crowd bones.
> - **Motion (implemented, `src/motion/`)**: `MobIntent { velocity, facing, look_at }` drives each mob; the plugin moves the root (or reads it with `ExternalRoot`). `Gait` asset (`*.gait.ron`) = modes picked by speed in leg lengths/s, each with a `PhasePattern` (Alternate, LateralSequence, Diagonal, Bound, Gallop, Tripod, Wave, Custom) assigning phase from `(pair, side)`; `Gait::for_legs` default. Per frame (one system, layered functions): root → phase → feet (planted in stance, arcs in swing toward home + velocity·stance/2) → bob → spine bends into turns → spring chains (accel/turn driven) → FABRIK leg IK → look-at (shared across neck bones, clamped) → joints. Ground = root height or game-written `FootGround` (from `FootProbes`). Runs for every tier for now; crowd tiers will switch to baked motion. `AxisSample` now carries lateral/dorsal radii so surface placement follows elliptical cross-sections.
> - **Authored parts (implemented)**: `PartKind::Sculpt(SculptParams { mesh, scale, blend, resolution, orient, region, own_bone })` converts a watertight glTF mesh primitive to an SDF grid (`mesh_to_sdf`, BVH + pseudo-normal sign) and smooth-blends it into the body (`PrimShape::Grid`). `HardKind::Mesh { path, orient, scale }` attaches an authored mesh rigidly. Authoring convention = glTF's (Y up, +Z forward, origin at the attach point); `Orient::Model` keeps it, `Orient::Slot` aligns +Y to the slot. Right copies sample the grid / mesh reflected. Meshes are resolved before building (`PartAssets`, `referenced_assets`) so `build_anatomy(live, rules, assets)` stays pure; their content hashes are in the `BodyKey`. Body plans and part meshes hot-reload (`reload_changed_assets`). Samples: `assets/mobs/beast.body.ron`, `assets/parts/sample_parts.gltf` (from `tools/gen_sample_parts.py`).
> - **ECS integration (implemented, `src/mob/`)**: `Mob { body: Handle<BodyPlan> }` (+ required `Form`, `MobSeed`, `BoneScales`) → observer inserts `GenRequest` → `LiveBody` resolved → async task on `AsyncComputeTaskPool`, deduplicated by `BodyKey` (xxh3-128 of live plan + surface settings + generator version; seed only if seed-dependent geometry is on) in `GenCache` → `GeneratedBody` asset → joints (`MobJoint`), per-LOD skinned meshes (`LodMesh`), `Placeholder` while waiting → `BodyReady` triggered (`BodyFailed` with the error when a requested generation fails; the mob keeps its body). `MobTier(LodLevel)` from on-screen size over `TierCamera`s with hysteresis, hero cap and promotion budget (`QualityProfile` LOW/MEDIUM/HIGH); `ForceTier` override. Part entities (`Part`, `PartLod`, `PartOf`/`MobParts`) exist per tier; editing `Part.kind` writes `LiveBody` and regenerates. `SeverPart`/`GraftPart` entity events. Not yet: disk cache, `HiddenRegions` (needs coat shader), `MobIntent` (motion), crowd instancing (crowd tiers still use per-mob skinned meshes).
> - **Tone (implemented)**: deterministic, seed-free stamps from built-in per-part tables (`src/anatomy/tone.rs`): muscle bellies, fat pads, landmarks, subtractive hollows, placed by `Surface{t, around}`, `Skull{dir}` or `Slot{..}`, scaled by `Tone` × part levers; per-primitive LOD (fat Far, muscle Near, landmarks/hollows Hero). RON override per part: `stamps: Default | None | Custom([...])`. Joint creases are field displacement masked to joints (Hero only). **Deferred, required later:** muscle flex deformation (stamps currently move rigidly with their bone).
> - **Surface stubs (`src/surface/`)**: `Mesher::mesh` takes a `BodyField` (not generic `F: Field`) so it can cull per chunk via `BodyField::view → ChunkView`; no BVH crate, primitives are binned per chunk. Crates: `fast-surface-nets` (meshing), `meshopt` (simplify/optimize). `BuiltPart` gains a resolved `tone`. `SurfaceSettings` resource holds one `LodSpec` per `LodLevel` (voxel size, simplify ratio, geometric folds, hard parts, deformation = centres of rotation + muscle flex; off for the editor's previews). Bevy 0.20 note: bounding volumes live in `bevy::shape`, not `bevy::math::bounding`.
> - Enum variants filled out (adding later is non-breaking; renaming is): `ChainRole` +Trunk/Tongue/Whisker, `LimbRole` +Swimmer, `Stance` +Arched, `LimbEnd` +Pincer/Blade/Point/Paddle, `MassShape` +Hump/Flap/Sac, `HardKind` +Nail/Antler/Tusk/Fang/Spike, `CrossSection` +Teardrop, `SocketKind` +Neck/Chest/Saddle/Wrist/TailTip, `PrimShape` +RoundBox, `RegionKind` +Snout. Reserved: Wing, Fin, Shell.

---

## 0. Settled direction

| Topic | Decision |
|---|---|
| Crate | **One crate**, `ornisculpt`. Modules are listed in §1. The bake tool lives in `src/bin/`. |
| Math | `bevy::math` (`Vec3`, `Vec3A`, `Quat`, `Affine3A`, `Mat4`) and `bevy::prelude::Transform`. No custom transform type. |
| Composition | A mob = **BodyPlan + Form + Gait + Coat** (+ held tools). There is no species concept. Sharing comes from **content hashes**. |
| Part tree | **Hybrid.** The `BodyPlan` asset is the authored source. Spawning expands it into **Part entities**. Live edits to those entities trigger regeneration. |
| Regions | **Derived from parts.** Each part kind emits its own regions, and clothing, patterns and fur use **selectors** to pick them. |
| Variation | Shape and proportion vary per mob. Crowds share a **base mesh per BodyPlan**, plus a per-bone length/girth scale and a coat on each instance. |
| Runtime change | The plugin provides **levers**: per-bone scale, region hiding, part edits/despawn → async regen. It never defines age, fat, and so on. |
| Control | A `MobIntent` component. The plugin owns all bone motion. |
| Cameras | Top-down and first-person. Tiering uses **screen-space size**, not distance, so it works for both. |
| Hardware | Runs on low-end and scales up through a `QualityProfile` resource. |
| Skin detail | Triplanar detail at runtime. Pre-baked mobs also get baked normal maps. |
| Bones | **≤ 64** per mob at close range (fingers included). **≤ 32** in crowds, where fine bones fold into their parent. Crowd bones are GPU-instanced data, not entities. |
| Tools | **Hold only** for v1. |
| Coverings | **Fur** (shell rendering). Feathers and plates come later. |
| Wings, genetics | Out of scope. The data stays plain, ordered and reflectable so genetics can encode it later. |
| Form source | The **game always supplies `Form`**. The plugin does no random sampling. |
| Physics | **User's choice.** The plugin has no physics dependency, and the game fills foot ground hits. |
| Part LOD | Parts have detail levels. Small parts drop out of the mesh, skeleton and ECS at coarser tiers (§2.10). |
| Cache | postcard + zstd. |
| Tooling | No standalone editor: the `editor` example edits a plan live. **Examples** for testing and **benches** for performance (§12). |

---

## 1. Module map

```
ornisculpt/src/
├─ lib.rs            OrnisculptPlugin, MobSet (system sets), QualityProfile
├─ anatomy/          BodyPlan asset, parts, Form, regions, build → Anatomy, viability checks
├─ surface/          Field trait, SDF body, meshing, skinning, attrs, LODs, hard parts
├─ coat/             Coat asset: palette, pattern, fur; material
├─ motion/           Gait asset, hero pose layers (systems), IK, baked motion
├─ gen/              keys, generation pipeline systems, cache, disk store
├─ tier/             screen-size tiering, hero↔crowd transitions
├─ crowd/            render-world extraction, GPU skinning compute, instanced draw, fur shells
├─ equip/            sockets, tools, holding
└─ bin/bake.rs       headless pre-bake (same crate)
examples/            visual test beds (§12)
benches/             criterion benchmarks (§12)
assets/mobs/         sample BodyPlan / Gait / Coat / MobDesc RON files used by examples and benches
```

- **T-1** Plain functions and slices are used **only** for hot inner loops: SDF evaluation, meshing, IK math and skinning math. Everything else is ECS: components, systems, observers, assets and relationships.
- **T-2** All authored data derives `Reflect + Serialize + Deserialize + Clone`, so it works with the inspector, RON assets, reflect-path overrides and future genetics encoding.
- **T-3** Pin one Bevy version. This assumes at least Bevy 0.16 (required components, relationships, observers).

---

## 2. Anatomy (authored data)

### 2.1 Body plan: the anatomical topology
```rust
#[derive(Asset, Reflect, Serialize, Deserialize, Clone)]
pub struct BodyPlan {
    pub root: PartNode,                 // usually a Torso
    pub tone: Tone,                     // global muscle/fat/fold intensity defaults
}

#[derive(Reflect, Serialize, Deserialize, Clone)]
pub struct PartNode {
    pub name: String,                   // unique among siblings; forms the PartPath
    pub kind: PartKind,
    pub attach: Attach,                 // where on the parent
    pub offset: Transform,              // extra local offset/rotation
    pub symmetry: Symmetry,
    pub detail: PartDetail,             // part-level LOD (§2.10)
    pub children: Vec<PartNode>,
}
pub enum Symmetry { None, Mirror, Radial { count: u8 } }
pub struct Attach { pub slot: AttachSlot, pub t: f32 /* along a segmented slot */ }
pub enum AttachSlot { Start, End, Along, Socket(String) /* head.jaw, head.eye ... */ }
```

### 2.2 Part kinds (closed enum, all values plain `f32`)
```rust
pub enum PartKind {
    Torso(TorsoParams),     // segmented trunk; also the serpent body
    Neck(ChainParams),
    Head(HeadParams),       // skull + optional jaw; exposes eye/ear/mouth/horn slots
    Limb(LimbParams),       // 2–4 segments + end; may be locomotor
    Digits(DigitParams),    // fingers/toes as a group; bones are BoneDetail::Fine
    Tail(ChainParams),
    Mass(MassParams),       // free blob(s): humps, crests, bellies, cheeks
    Hard(HardParams),       // claw, horn, beak, tooth, eye; separate mesh, not SDF
}
pub struct ChainParams { pub segments: u8, pub length: f32, pub radius: Profile, pub curl: f32, pub blend: f32 }
pub struct Profile { pub curve: Vec<(f32, f32)>, pub scale: f32, pub cross: CrossSection }
pub enum CrossSection { Round, Ellipse { aspect: f32 }, Flat { aspect: f32 } }
pub struct TorsoParams { pub chain: ChainParams, pub arch: Vec<(f32, f32)>, pub belly: f32 }
pub struct LimbParams { pub chain: ChainParams, pub role: LimbRole, pub stance: Stance, pub end: LimbEnd }
pub enum LimbRole { Locomotor, Manipulator /* arms */, Both /* knuckle-walkers */, Display }
pub enum Stance { Plantigrade, Digitigrade, Unguligrade, Sprawl }
pub enum LimbEnd { None, Hand { palm: f32 }, Foot { length: f32 }, Hoof, Pad }
pub struct HeadParams { pub skull: MassParams, pub snout: f32, pub jaw: Option<ChainParams>, pub eye_spacing: f32 }
pub struct DigitParams { pub count: u8, pub chain: ChainParams, pub spread_deg: f32, pub opposable: bool }
pub struct MassParams { pub shape: MassShape, pub size: Vec3, pub blend: f32 }
pub struct HardParams { pub kind: HardKind, pub size: f32, pub curve: f32, pub twist: f32 }
pub enum HardKind { Claw, Horn, Beak, Tooth, Eye, Mesh(String /* asset path */) }
pub struct Tone { pub muscle: f32, pub fat: f32, pub folds: f32, pub landmarks: f32 }
```

### 2.3 Part paths
```rust
#[derive(Reflect, Clone, Hash, Eq, PartialEq)]
pub struct PartPath(pub String);   // "torso/limb.front.l/digits"; mirrored copies get .l / .r
```
- **T-4** The path is a stable address. Form overrides, RNG forking and region names all use it, and future genetics will swap sequences by path.

### 2.4 Form: an individual's proportions
```rust
#[derive(Component, Asset, Reflect, Serialize, Deserialize, Clone, Default)]
pub struct Form {
    pub overall_scale: f32,
    pub patches: Vec<FormPatch>,       // ordered, plain data
}
pub struct FormPatch { pub part: PartPath, pub field: String /* reflect path, e.g. "chain.length" */, pub value: f32 }
pub fn apply_form(plan: &BodyPlan, form: &Form) -> Result<BodyPlan, FormError>;   // FormError: bad path / wrong type
```
- **T-5** Overrides use **Bevy reflect paths** on the part's params. That's generic, needs no per-field code, and is already the "interpretable data" a genetics layer would need.
- **T-5a (from TQ-3)** The **game always supplies the `Form`**. The plugin has no randomizer and no ranges. An empty `Form` means the body plan as authored.

### 2.5 Mob description (the composition)
```rust
#[derive(Asset, Reflect, Serialize, Deserialize, Clone)]
pub struct MobDesc {
    pub body: Handle<BodyPlan>,
    pub form: Form,
    pub gait: Handle<Gait>,
    pub coat: Handle<Coat>,
    pub seed: u64,                     // noise/pattern seed
}
```

### 2.6 Regions (derived)
```rust
#[derive(Reflect, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegionId(pub u8);           // ≤ 64 per mob (fits a u64 mask)
pub struct RegionDef { pub part: PartPath, pub kind: RegionKind, pub side: Side }
pub enum RegionKind {
    TorsoFront, TorsoMid, TorsoRear, Belly, Neck,
    Skull, Face, Jaw,
    LimbUpper, LimbLower, LimbEnd, Digit,
    TailBase, TailTip, Mass, Hard,
}
pub enum Side { Center, Left, Right }
pub struct RegionTable { pub regions: Vec<RegionDef> }

/// How coats, fur and clothing pick regions on ANY body plan.
#[derive(Reflect, Serialize, Deserialize, Clone)]
pub enum RegionSelector {
    All, Kind(RegionKind), Part(PartPath), Role(LimbRole), Side(Side),
    And(Vec<RegionSelector>), Or(Vec<RegionSelector>), AllExcept(Vec<RegionSelector>), // no Box: Bevy reflect can't represent it
}
impl RegionSelector { pub fn mask(&self, t: &RegionTable, a: &Anatomy) -> RegionMask; }
#[derive(Reflect, Clone, Copy, Default)] pub struct RegionMask(pub u64);
```
- Example: "fur on everything except limb ends and the face" is `AllExcept([Kind(LimbEnd), Kind(Face)])`. That works for a biped and a hexapod alike.

### 2.7 Built anatomy (resolved, CPU-side, not authored)
```rust
pub struct Anatomy {
    pub skeleton: Skeleton,
    pub primitives: Vec<Primitive>,
    pub hard_parts: Vec<HardPart>,
    pub sockets: Vec<SocketDef>,
    pub regions: RegionTable,
    pub legs: Vec<LegInfo>,            // discovered from Locomotor limbs
    pub spine: Vec<BoneId>,
    pub springs: Vec<SpringChainDef>,  // tails, ears
    pub look: Option<LookDef>,         // neck/head bones
    pub joint_limits: Vec<(BoneId, JointLimit)>,
    pub parts: Vec<BuiltPart>,         // PartPath → bones/primitive/region ranges
}
pub fn build_anatomy(plan: &BodyPlan, form: &Form, seed: u64, lod_rules: &PartLodRules) -> Result<Anatomy, AnatomyError>;
```

### 2.8 `PartBuild` trait and `AnatomyBuilder`
```rust
pub trait PartBuild {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &mut BuildCx) -> Result<PartSlots, AnatomyError>;
}
// implemented for TorsoParams, LimbParams, HeadParams, ...; PartKind dispatches by match.
pub struct BuildCx<'a> { pub path: &'a PartPath, pub rng: MobRng, pub tone: &'a Tone }
pub struct AttachPoint { pub bone: BoneId, pub local: Transform, pub side: Side }
pub struct PartSlots { pub slots: SmallVec<[(AttachSlot, SlotPoints); 4]> }
pub enum SlotPoints { One(AttachPoint), Along(SmallVec<[AttachPoint; 8]>) }

impl AnatomyBuilder {
    pub fn bone(&mut self, parent: Option<BoneId>, name: &str, rest: Transform, length: f32, region: RegionId, detail: BoneDetail) -> BoneId;
    pub fn region(&mut self, def: RegionDef) -> RegionId;
    pub fn primitive(&mut self, p: Primitive);
    pub fn hard_part(&mut self, h: HardPart);
    pub fn socket(&mut self, s: SocketDef);
    pub fn leg(&mut self, l: LegInfo);
    pub fn spring_chain(&mut self, c: SpringChainDef);
    pub fn look(&mut self, l: LookDef);
    pub fn limit(&mut self, b: BoneId, l: JointLimit);
    pub fn finish(self) -> Result<Anatomy, AnatomyError>;
}
```

### 2.9 Viability checks (designed now for future genetics; useful for authoring too)
```rust
pub trait ViabilityCheck: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self, a: &Anatomy) -> Viability;   // gait compatibility moves to motion (needs a Gait)
}
pub enum Viability { Ok, Warn(String), Fail(String) }
#[derive(Resource, Default)] pub struct ViabilityChecks(pub Vec<Box<dyn ViabilityCheck>>);
// Built-ins: BoneBudget, FeetReachGround, StaticBalance (COM over support polygon),
//            GaitCompatible (leg count/pattern), RestSelfIntersection.
```

### 2.10 Part LOD (from TQ-4)
Each part has a detail level. It decides the **lowest tier at which the part still exists** in the mesh, the skeleton and the ECS. Small parts drop out as the mob shrinks on screen.
```rust
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Default)]
pub enum PartDetail {
    #[default] Auto,          // from size: part bounding radius / mob bounding radius vs PartLodRules
    Always,                   // never culled (torso, main limbs, head)
    UpTo(LodLevel),           // present at this level and finer
}
#[derive(Reflect, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LodLevel { Hero = 0, Near = 1, Far = 2 }   // matches MobTier, excluding Pending

pub struct BuiltPart {
    pub path: PartPath, pub parent: Option<u16>,
    pub bones: Range<u8>, pub primitives: Range<u32>, pub hard_parts: Range<u16>, pub regions: Range<u8>,
    pub lod: LodLevel,                    // resolved: coarsest level this part appears in
}
#[derive(Resource, Reflect)]
pub struct PartLodRules { pub near_min_ratio: f32, pub far_min_ratio: f32 }   // e.g. 0.08, 0.2
```
- **T-20** Part LOD applies at three layers:
  1. **Mesh.** Each LOD level is **re-meshed** from the SDF without the culled parts, at a coarser voxel size. `meshopt` then trims the rest. Culled hard parts (teeth, claws, eyes) are simply omitted. This beats decimating, which leaves lumps where small parts used to be.
  2. **Skeleton.** Bones of culled parts fold into the nearest surviving ancestor in the crowd skeleton. This generalizes `BoneDetail::Fine` from §3: the crowd skeleton is now "bones of parts with `lod ≥ Near`".
  3. **ECS.** Part entities exist only while their part is present at the mob's current tier. On demotion, an observer despawns the finer part entities. On promotion they respawn from the `Anatomy`, cheap because it's data only. Core parts (`Always`) keep their entities in every tier, so gameplay always has hit locations.
- Edits are preserved: changing a part that is currently culled is stored on the mob's live `BodyPlan` snapshot component (§8.2), not lost with the entity.

---

## 3. Skeleton, bones and sockets

```rust
pub struct Skeleton { pub bones: Vec<BoneDef>, pub inverse_bind: Vec<Mat4>, pub crowd: CrowdSkeleton }
pub struct BoneDef {
    pub name: String, pub parent: Option<BoneId>, pub rest: Transform, pub length: f32,
    pub region: RegionId, pub part: u16 /* index into Anatomy.parts */, pub detail: BoneDetail,
}
pub enum BoneDetail { Core, Fine }                   // Fine: digits, ear tips, tail tips
pub struct CrowdSkeleton { pub map: Vec<u8> /* full bone → crowd bone (Fine folds to Core parent) */, pub bones: Vec<BoneId> }
#[derive(Reflect, Clone, Copy, PartialEq, Eq, Hash, Debug)] pub struct BoneId(pub u8);

pub const MAX_BONES: usize = 64;
pub const MAX_CROWD_BONES: usize = 32;
```
- **T-6** 64 bones is enough for a torso, neck, head, jaw, four limbs, two hands of fingers and a tail. 32 crowd bones = 64 texels per frame row in the baked motion texture.

### 3.1 Sockets
```rust
pub struct SocketDef { pub name: String, pub kind: SocketKind, pub bone: BoneId, pub local: Transform, pub size: f32 }
pub enum SocketKind { Grip, Back, Hip, Head, Mouth, Shoulder }
```

---

## 4. Surface (mesh generation)

### 4.1 Field
```rust
pub trait Field: Send + Sync {
    fn bounds(&self) -> Aabb3d;
    fn distance(&self, p: Vec3A) -> f32;                                   // hot path
    fn distance_batch(&self, ps: &[Vec3A], out: &mut [f32]) { /* default loop */ }
    fn gradient(&self, p: Vec3A) -> Vec3A { /* central differences */ }
    fn surface_info(&self, p: Vec3A) -> SurfaceInfo;                       // per output vertex only
}
pub struct SurfaceInfo { pub bones: [BoneId; 4], pub weights: [f32; 4], pub region: RegionId, pub layer: PrimLayer }
pub struct BodyField { /* primitives + BVH; built from Anatomy */ }
// combinators: Offset<F>, Intersect<A,B>, Subtract<A,B>, RegionClip<F>, Displace<F>
```

### 4.2 Primitives
```rust
pub struct Primitive { pub bone: BoneId, pub shape: PrimShape, pub local: Transform, pub blend: f32,
                       pub op: PrimOp, pub layer: PrimLayer, pub region: RegionId }
pub enum PrimShape { Sphere { r: f32 }, Ellipsoid { radii: Vec3 }, Capsule { r0: f32, r1: f32, len: f32 },
                     RoundCone { r0: f32, r1: f32, len: f32 }, Sweep { points: Vec<Vec3>, radii: Vec<f32>, cross: CrossSection } }
pub enum PrimOp { Union, Subtract }
pub enum PrimLayer { Base, Muscle, Landmark, Fat }
pub struct HardPart { pub bone: BoneId, pub local: Transform, pub params: HardParams, pub region: RegionId }
```

### 4.3 Meshing and output
```rust
pub trait Mesher { fn mesh<F: Field>(&self, f: &F, s: &MeshSettings) -> RawMesh; }
pub struct SurfaceNets;                               // v1; a GPU mesher can come later
pub struct MeshSettings { pub voxel_size: f32, pub chunk: u32 }
pub struct RawMesh { pub positions: Vec<Vec3>, pub normals: Vec<Vec3>, pub indices: Vec<u32> }

// Free functions (fixed pipeline steps):
pub fn weld(m: &mut RawMesh, eps: f32);
pub fn smooth(m: &mut RawMesh, iters: u32);
pub fn skin_and_attrs<F: Field>(m: RawMesh, f: &F, a: &Anatomy) -> BodyMeshData;
pub fn mesh_lod<M: Mesher>(m: &M, a: &Anatomy, level: LodLevel, s: &MeshSettings) -> BodyMeshData; // re-mesh without culled parts (T-20)
pub fn simplify(d: &BodyMeshData, target_ratio: f32) -> BodyMeshData;                          // meshopt trim after re-mesh
pub fn to_bevy_mesh(d: &BodyMeshData) -> Mesh;
```
- **T-7** The output is a **Bevy `Mesh`** with standard `POSITION`, `NORMAL`, `JOINT_INDEX` and `JOINT_WEIGHT`, plus one custom attribute:
  ```rust
  pub const ATTRIBUTE_MOB_DATA: MeshVertexAttribute = /* Uint32x2 */;
  // x: region u8 | cavity u8 | curvature u8 | limb_t u8
  // y: primary_bone u8 | layer u8 | reserved u16
  ```

---

## 5. Coat (look: palette, pattern, fur)

```rust
#[derive(Asset, Reflect, Serialize, Deserialize, Clone)]
pub struct Coat {
    pub base: Color, pub secondary: Color, pub accent: Color,
    pub variation: ColorVariation,             // per-mob hue/value jitter from MobDesc.seed
    pub layers: Vec<PatternLayer>,             // ordered, blended
    pub fur: Option<Fur>,
    pub skin_detail: SkinDetail,               // triplanar scale/wrinkle/pore settings
}
pub struct PatternLayer { pub pattern: Pattern, pub color: CoatColor /* Secondary|Accent|Custom */, pub regions: RegionSelector, pub strength: f32 }
pub enum Pattern { Countershade { sharpness: f32 }, Stripes { freq: f32, warp: f32 }, Spots { density: f32, size: f32 },
                   Rosettes { density: f32 }, Mottle { scale: f32 }, Gradient { along: GradientAxis } }
pub struct Fur { pub regions: RegionSelector, pub length: f32, pub density: f32, pub clump: f32, pub stiffness: f32,
                 pub tip_color: Option<CoatColor> }

/// GPU form, shared by the hero material and the crowd pipeline.
#[repr(C)] #[derive(ShaderType, Clone, Copy)]
pub struct CoatGpu { pub colors: [Vec4; 3], pub layers: [PatternGpu; 4], pub fur_mask: UVec2, pub fur: Vec4 }
```
- **T-8** The hero mesh uses `ExtendedMaterial<StandardMaterial, MobMaterialExt>`. The crowd pipeline reuses the same WGSL coat functions as a shared shader module.
- **T-9** Fur uses **shell rendering**: N extruded copies of the skinned surface, alpha-tested by a noise texture and masked by `fur_mask`. The shell count comes from tier and `QualityProfile`, for example 16 for hero, 6 near, 0 far. At 0 shells a fur-tinted base is drawn instead.

---

## 6. Motion

### 6.1 Gait (independent of any specific body plan)
```rust
#[derive(Asset, Reflect, Serialize, Deserialize, Clone)]
pub struct Gait { pub modes: Vec<GaitMode>, pub idle: IdleParams, pub turn_rate_deg: f32 }
pub struct GaitMode {
    pub name: String, pub speed: (f32, f32), pub pattern: PhasePattern,
    pub duty: f32, pub stride: f32 /* × leg length */, pub lift: f32,
    pub bob: f32, pub spine_flex: f32, pub lean: f32,
}
pub enum PhasePattern {
    Alternate,                // bipeds
    LateralSequence,          // walk
    Diagonal,                 // trot
    Bound, Gallop { lead: Side },
    Tripod,                   // hexapods
    Wave { lag: f32 },        // many legs
    Undulate { waves: f32 },  // legless: spine wave
    Custom(Vec<f32>),         // explicit per-leg phases (by LegInfo order)
}
pub struct LegInfo { pub chain: SmallVec<[BoneId; 4]>, pub foot: BoneId, pub pair: u8 /* front→back */, pub side: Side,
                     pub stance: Stance, pub rest_foot: Vec3, pub pole: Vec3, pub length: f32 }
```
- **T-10** A gait assigns phase from `(pair, side)`, so one "trot" gait works on any quadruped and "tripod" works on any hexapod. `GaitCompatible` (§2.9) flags mismatches.

### 6.2 Control (gameplay-facing)
```rust
#[derive(Component, Reflect, Default, Clone)]
pub struct MobIntent {
    pub velocity: Vec3,                 // desired, world space
    pub facing: Option<Dir3>,
    pub look_at: Option<Vec3>,
    pub reach: SmallVec<[Reach; 2]>,    // "put grip.r here" (hold-only tools still want this)
}
pub struct Reach { pub socket: SocketKind, pub side: Side, pub target: Vec3, pub weight: f32 }
```

### 6.3 Hero tier: pose layers as ECS systems
The pose lives in a **per-mob buffer component**. Layer systems run in parallel across mobs with `par_iter_mut`, then one sync system writes bone entity `Transform`s.
```rust
#[derive(Component)] pub struct PoseBuffer { pub local: Vec<Transform>, pub model: Vec<Affine3A> }
#[derive(Component)] pub struct MobAnatomy(pub Arc<Anatomy>);      // shared, immutable
#[derive(Component, Default)] pub struct GaitState { pub phase: f32, pub mode: u8, pub blend: f32, pub speed: f32 }
#[derive(Component, Default)] pub struct FeetState { pub feet: SmallVec<[FootState; 8]> }
#[derive(Component, Default)] pub struct SpringState { pub chains: SmallVec<[SpringChainState; 4]> }
#[derive(Component, Default)] pub struct LookState { pub current: Quat }

// Ground is decoupled through components (TQ-2: physics is the user's choice). The game adds a
// system in MobSet::Ground that reads FootProbes and writes FootHits using whatever physics it uses.
// The plugin ships NO physics dependency. Examples use a tiny flat-ground system.
// If no provider writes FootHits, feet fall back to the mob's root height plane.
#[derive(Component, Default)] pub struct FootProbes(pub SmallVec<[FootProbe; 8]>);   // written by plan_feet
#[derive(Component, Default)] pub struct FootHits(pub SmallVec<[Option<GroundHit>; 8]>); // written by ground provider
pub struct FootProbe { pub origin: Vec3, pub max_drop: f32 }
pub struct GroundHit { pub point: Vec3, pub normal: Dir3 }

// Bone entities (hero tier only):
#[derive(Component)] pub struct MobBone { pub id: BoneId }
// Systems, in MobSet::HeroMotion order:
//   advance_gait → plan_feet → (MobSet::Ground: provider) → solve_legs → balance_body
//   → spine_follow → springs → look_at → reach → apply_bone_scale → sync_bones
```
- **T-11 (ECS vs performance):** layers are systems and state is components, which is fully ECS. The pose itself is a `Vec<Transform>` component rather than per-bone queries. IK needs random access to whole chains, and `get_many_mut` across bone entities would serialize the work and break cache locality. Bone entities still exist for rendering (Bevy `SkinnedMesh`), attachment, and gameplay reads.

### 6.4 IK (math only)
```rust
pub fn two_bone_ik(pose: &mut PoseBuffer, sk: &Skeleton, chain: [BoneId; 3], target: Vec3, pole: Vec3);
pub fn fabrik(pose: &mut PoseBuffer, sk: &Skeleton, chain: &[BoneId], target: Vec3, iters: u8, limits: &[JointLimit]);
pub fn forward_kinematics(sk: &Skeleton, local: &[Transform], root: Affine3A, out: &mut [Affine3A]);
```

### 6.5 Baked motion (crowd tiers)
```rust
#[derive(Asset, TypePath)]
pub struct BakedMotion {
    pub key: MotionKey,
    pub clips: Vec<BakedClip>,
    pub texture: Handle<Image>,          // RGBA32F; per frame per crowd bone: [quat][translation.xyz, _]
    pub bones: u8,
}
pub struct BakedClip { pub kind: ClipKind, pub first_frame: u32, pub frames: u32, pub fps: f32, pub looping: bool, pub root_speed: f32 }
pub enum ClipKind { Idle, Mode(u8 /* GaitMode index */), TurnLeft, TurnRight }
pub fn bake_motion(a: &Anatomy, gait: &Gait, fps: f32) -> (Vec<BakedClip>, Image);   // runs the hero layers headless on flat ground
```
- **T-12** Clips are derived **automatically from the gait**: idle, one per mode, and turns. Nothing is authored per mob.
- **T-13** Motion is shared by **`MotionKey = hash(BodyPlan at base form, Gait)`**. Every mob with that body plan and gait shares one texture, whatever its individual form.

### 6.6 Runtime body levers (the game drives these)
```rust
#[derive(Component, Reflect, Default)]
pub struct BoneScales(pub SmallVec<[BoneScale; 32]>);   // empty = identity
pub struct BoneScale { pub bone: BoneId, pub length: f32, pub girth: Vec2 }
#[derive(Component, Reflect, Default)] pub struct HiddenRegions(pub RegionMask);
```
- **T-14** `BoneScale` is **non-inherited**. Length moves children along the bone, and girth scales only that bone's own skin, so there's no shear. Both tiers apply it: the hero in `apply_bone_scale`, the crowd in the GPU skinning pass. This is also how a crowd mob's individual `Form` is approximated on the shared base mesh (§7.2).
- Part-level changes go through the Part entities (§8.2): editing a `Part` component or despawning one triggers regeneration.

---

## 7. Generation

### 7.1 Keys (content hashes, xxh3-128, include `GENERATOR_VERSION`)
```rust
pub struct BaseKey(pub u128);      // BodyPlan + default Form       → shared base mesh (crowd)
pub struct BodyKey(pub u128);      // BodyPlan + Form + seed + parts → unique mesh (hero)
pub struct MotionKey(pub u128);    // BodyPlan base + Gait           → BakedMotion
pub const GENERATOR_VERSION: u32 = 1;
```

### 7.2 Generated assets
```rust
#[derive(Asset, TypePath)]
pub struct GeneratedBody {
    pub key: u128,
    pub anatomy: Arc<Anatomy>,
    pub lods: Vec<Handle<Mesh>>,                    // LOD0 full skeleton; LOD1+ crowd skeleton
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    pub hard_parts: Vec<Handle<Mesh>>,
    pub normal_map: Option<Handle<Image>>,          // baked mobs only
}
```
- **T-15** Crowd mobs render the **base body** (`BaseKey`) with `BoneScales` derived from their `Form`, using `form_to_bone_scales(base: &Anatomy, mine: &Anatomy) -> BoneScales`. Hero mobs get their own `BodyKey` mesh. When a mob is promoted to hero, its unique mesh is requested, and the base mesh is shown until it arrives.

### 7.3 Generation components, resources and flow
```rust
#[derive(Component)] pub struct GenRequest { pub what: GenWhat, pub priority: f32 }
pub enum GenWhat { Base, Unique, Motion }
#[derive(Component)] pub struct GenTask(pub Task<GenResult>);
#[derive(Component)] pub struct Placeholder;

#[derive(Resource, Default)] pub struct GenCache {
    pub bodies: HashMap<u128, Handle<GeneratedBody>>,
    pub motions: HashMap<MotionKey, Handle<BakedMotion>>,
    pub in_flight: HashMap<u128, Vec<Entity>>,      // dedupe: many mobs, one task
}
#[derive(Resource)] pub struct GenStore { pub root: PathBuf }   // postcard + zstd on disk (confirmed)
#[derive(Event)] pub struct BodyReady { pub mob: Entity }          // observer trigger

// Flow:
//   observer On<Add, Mob> → insert GenRequest(Base, Motion) + Placeholder
//   system schedule_gen (MobSet::Generation): cache hit? attach handle : spawn AsyncCompute task (budgeted)
//   system poll_gen: completed → insert asset handles, trigger BodyReady, remove Placeholder
//   system detect_body_edits: Changed<Part>/RemovedComponents<Part>/Changed<Form> → GenRequest(Unique)
```

---

## 8. The mob in the ECS

### 8.1 Mob root
```rust
#[derive(Component, Reflect)]
#[require(Transform, Visibility, MobIntent, MobTier, BoneScales, HiddenRegions)]
pub struct Mob { pub desc: Handle<MobDesc> }

#[derive(Component, Reflect, Default)] pub enum MobTier { #[default] Pending, Hero, Near, Far }
#[derive(Component)] pub struct MobBody(pub Handle<GeneratedBody>);
#[derive(Component)] pub struct MobMotion(pub Handle<BakedMotion>);
#[derive(Component)] pub struct MobSeed(pub u64);
```

### 8.2 Part entities (hybrid part tree)
```rust
/// The mob's own copy of its anatomy after spawn (BodyPlan with Form applied). Source of truth for regen and saving.
#[derive(Component, Reflect, Clone)] pub struct LiveBody(pub BodyPlan);

#[derive(Component, Reflect)] pub struct Part { pub path: PartPath, pub kind: PartKind }   // editable view of one LiveBody node
#[derive(Component)] pub struct PartLod(pub LodLevel);
#[derive(Component)] #[relationship(relationship_target = MobParts)] pub struct PartOf(pub Entity);
#[derive(Component)] #[relationship_target(relationship = PartOf)] pub struct MobParts(Vec<Entity>);

/// Deliberate anatomy changes go through events, never through despawning (despawn = LOD cull).
#[derive(Event)] pub struct SeverPart { pub mob: Entity, pub path: PartPath }   // removes from LiveBody → regen
#[derive(Event)] pub struct GraftPart { pub mob: Entity, pub parent: PartPath, pub node: PartNode }
```
- **T-16** Part entities are spawned per **part LOD** (§2.10). Core parts exist at every tier, and fine parts only at the tiers where they're meshed. Gameplay edits a `Part` component, and a sync system writes the change into `LiveBody` and requests a regen. Removing or adding anatomy uses `SeverPart`/`GraftPart`, so LOD despawns can never be mistaken for dismemberment. `HiddenRegions` hides a severed part immediately while regen runs.

### 8.3 Sockets and tools
```rust
#[derive(Component)] pub struct Socket { pub kind: SocketKind, pub side: Side, pub bone: BoneId, pub size: f32 }
// Socket entities are children of the bone entity (hero) or of the mob root (crowd).
#[derive(Component)] pub struct SocketTracking;   // opt-in: keep Transform updated in crowd tiers (CPU sample of baked clip)

#[derive(Asset, Reflect, Serialize, Deserialize, Clone)]
pub struct ToolDef { pub mesh: Handle<Mesh>, pub material: Handle<StandardMaterial>, pub grip: Transform,
                     pub fits: SocketKind, pub nominal_size: f32 }
#[derive(Component)] #[relationship(relationship_target = Holding)] pub struct HeldBy(pub Entity);   // tool → socket
#[derive(Component)] #[relationship_target(relationship = HeldBy)] pub struct Holding(Vec<Entity>);
#[derive(Component)] pub struct Tool(pub Handle<ToolDef>);
```
- **T-17** Hero tier: the tool is a normal mesh entity following the socket. Crowd tiers: the tool is drawn **in the crowd pipeline** using the socket bone's skinning matrix (one instanced draw per tool mesh), and the tool entity's `Transform` isn't updated unless `SocketTracking` is set.

---

## 9. Tiering and quality

```rust
#[derive(Resource, Reflect)]
pub struct QualityProfile {
    pub max_hero: u32, pub tier_px: [f32; 3] /* screen-height thresholds hero/near/far */,
    pub hysteresis: f32, pub promotions_per_frame: u32,
    pub lod_bias: f32, pub voxel_scale: f32, pub fur_shells: [u8; 3],
    pub crowd_update_divisor: [u8; 2] /* near, far */, pub gen_max_in_flight: u32,
}
impl QualityProfile { pub const LOW: Self; pub const MEDIUM: Self; pub const HIGH: Self; pub const ULTRA: Self; }
#[derive(Component)] pub struct TierCamera;   // cameras considered for tiering (top-down + first-person)
```
- **T-18** Tier = the mob's projected screen height across all `TierCamera`s (the largest wins), plus hysteresis. A forced-hero override component handles story mobs and first-person targets:
  ```rust
  #[derive(Component)] pub struct ForceTier(pub MobTier);
  ```

---

## 10. Crowd rendering (render world)

```rust
#[derive(Component, Clone, Default)] pub struct CrowdMotion { pub clip: [u16; 2], pub time: [f32; 2], pub blend: f32, pub bucket: u8 }

#[repr(C)] #[derive(ShaderType, Clone, Copy)]
pub struct CrowdInstanceGpu {
    pub world: Mat3x4, pub frames: UVec2, pub frame_t: Vec2, pub blend: f32,
    pub bone_scale_offset: u32, pub skin_offset: u32, pub hidden: UVec2, pub coat: u32 /* index into coat buffer */,
}
pub struct CrowdBatchKey { pub body: AssetId<GeneratedBody>, pub lod: u8 }   // one draw (+shadow, +fur shells) per key
pub struct CrowdSkinningNode;   // compute: BAT local poses + BoneScales → skin matrices (storage buffer)
pub struct CrowdDrawPipeline;   // vertex skin from storage buffer; coat fragment; shadow + fur-shell variants
```
- **T-19** Crowd mobs are **one entity each**, with no bone or part-update systems. `CrowdMotion` advances in staggered buckets according to `crowd_update_divisor`. Extraction uses `ExtractComponent`.

---

## 11. Plugin and system sets

```rust
pub struct OrnisculptPlugin { pub quality: QualityProfile, pub store_root: PathBuf }
#[derive(SystemSet, Hash, Eq, PartialEq, Clone, Debug)]
pub enum MobSet { Generation, Intent /* game writes MobIntent */, Tiering, HeroMotion, Ground, CrowdMotion, Sync }
// Update: Generation → Intent → Tiering → HeroMotion(⊃ Ground) ∥ CrowdMotion → Sync → (TransformSystems::Propagate)
```

---

## 12. Examples and benchmarks (from TQ-5)

There is no standalone editor; the `editor` example adjusts a plan's values live and saves them back. Examples double as visual tests, and benches guard performance.

| Example | Shows / tests |
|---|---|
| `anatomy` | BodyPlan → skeleton gizmos, regions coloured, sockets drawn; reload RON live |
| `surface` | SDF mesh with weight heatmap and region overlay; toggle part LODs |
| `rig` | Manually rotate bones (keys/sliders via gizmo) to check skinning |
| `walk` | Biped, quadruped, hexapod and serpent walking on flat ground; switch gaits |
| `coat` | Patterns + fur shells on several body plans; quality profile switch |
| `levers` | BoneScales growth, HiddenRegions, SeverPart/GraftPart regen |
| `hold` | Tools held in grip sockets, hero ↔ crowd tier |
| `crowd` | N mobs (CLI arg, default 4000), mixed body plans, top-down + first-person cameras, Tracy spans |

| Bench (`criterion`) | Measures |
|---|---|
| `build_anatomy` | BodyPlan + Form → Anatomy |
| `sdf_eval` | `distance_batch` throughput (samples/sec) |
| `mesh_lod` | Re-mesh per LodLevel, plus skinning and attrs |
| `bake_motion` | Baked motion per gait |
| `hero_motion` | One frame of hero layer systems for 64 mobs (headless `App`) |
| `crowd_update` | One frame of crowd CPU systems for 4000 mobs (headless `App`) |

- **T-21** Benches and examples use the same `assets/mobs/` set, so a performance number always matches something you can look at.

---

## 13. Open questions

None blocking. Next step: I'll write these types as compiling Rust stubs (`todo!()` bodies), module by module, starting with `anatomy`, and review each with you before moving to the next.
