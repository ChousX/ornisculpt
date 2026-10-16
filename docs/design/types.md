# Ornisculpt: Types & Traits (draft v0)

This document defines the vocabulary of the system: every major type, every trait, and which crate owns each one. No implementations yet.

> **How to review:** each decision is tagged **[T-x]** and each open question **[TQ-x]**. Reply by tag. The doc follows the order data flows through the pipeline, so reviewing top to bottom makes the most sense.

---

## 0. Design principles for the type system

| # | Principle | Consequence |
|---|---|---|
| **T-1** | **Enums for closed, serialized, hot-path data. Traits for algorithms and environment boundaries.** | Primitive shapes, part kinds and regions are enums: matched, `serde`-able, no vtables in the SDF inner loop. Meshers, IK solvers, ground queries and stores are traits. |
| **T-2** | **Desc / State split.** Shared, immutable, per-species data (`*Desc`, `*Def`, `*Rig`) is kept separate from per-creature mutable data (`*State`). | Thousands of creatures share one `Arc<LocomotionRig>`. Each creature only owns a small state struct. |
| **T-3** | **Only the `ornisculpt_bevy` crate depends on Bevy.** The core crates use `glam` (same version as Bevy's re-export), `serde`, `smallvec`, `smol_str`. | Headless bake CLI, fast unit tests, fast compiles. |
| **T-4** | **Determinism is a type-level concern.** All randomness flows through `GenRng`, which is forked by *label*, not by draw order. | Editing one part of a template doesn't reshuffle every other part's randomness. |
| **T-5** | **Indices, not pointers.** `BoneId(u16)`, `SocketId(u16)`, `GeneId(u16)`, `ClipId(u16)`. | Compact, GPU-friendly, trivially serializable. |
| **T-6** | **Everything produced by generation is `Send + Sync` and plain data.** | Can be generated on any thread, cached, and shared via `Arc`. |

---

## 1. Crate map and dependency direction

```
ornisculpt_core      ← math aliases, ids, Xform, Skeleton, Region, Socket, GenRng, Curve
   ▲
ornisculpt_genome    ← genes, templates, parts, PlanBuilder, BodyPlan
   ▲
ornisculpt_mesh      ← Field, Mesher, mesh types, weights, attrs, LODs, hard parts, garments
ornisculpt_anim      ← Pose, IK, gait, layers, actions, clip baking (depends on core + genome::BodyPlan)
   ▲
ornisculpt_gen       ← pipeline: generate(), CreatureKey, assets, CreatureStore, GenSettings
   ▲
ornisculpt_bevy      ← plugin, components, AssetLoaders, material, crowd render pipeline
ornisculpt_editor / ornisculpt-bake (bins)
```
- **T-7** A new `ornisculpt_core` crate holds the types *every* layer needs. Without it, `mesh` and `anim` would both depend on `genome` just for `Skeleton`.
- **T-8** A new `ornisculpt_gen` crate is the pipeline orchestrator, so neither `mesh` nor `anim` has to know about the other.

---

## 2. `ornisculpt_core`

### 2.1 Math and ids
```rust
pub use glam::{Vec2, Vec3, Vec3A, Vec4, Quat, Mat4, Affine3A};

/// Bevy-free TRS transform (mirrors bevy::Transform; From/Into provided in ornisculpt_bevy).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Xform { pub translation: Vec3, pub rotation: Quat, pub scale: Vec3 }
impl Xform {
    pub const IDENTITY: Self;
    pub fn mul(&self, rhs: &Xform) -> Xform;
    pub fn to_affine(&self) -> Affine3A;
    pub fn mirror_x(&self) -> Xform;          // for bilateral symmetry
}

macro_rules! id { ($n:ident, $t:ty) => {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
    pub struct $n(pub $t);
}}
id!(BoneId, u16); id!(SocketId, u16); id!(GeneId, u16); id!(ClipId, u16);
id!(SpeciesId, u32); id!(ToolId, u32); id!(ActionId, u16);

pub type Name = smol_str::SmolStr;            // bone/part/socket/gene names
pub struct Aabb { pub min: Vec3A, pub max: Vec3A }
```

### 2.2 Skeleton
```rust
pub struct Skeleton {
    pub bones: Vec<BoneDef>,          // topologically sorted: parent index < child index
    pub inverse_bind: Vec<Affine3A>,  // model-space rest → bone space
}
pub struct BoneDef {
    pub name: Name,                   // "spine.3", "arm.l.upper", ...
    pub parent: Option<BoneId>,
    pub rest: Xform,                  // local, relative to parent
    pub length: f32,                  // along local +Y (bone convention, matches Blender/glTF export)
    pub region: Region,
    pub flags: BoneFlags,             // DEFORM | IK_EFFECTOR | SPRING | NO_CROWD (dropped in crowd LOD)
}
impl Skeleton {
    pub fn find(&self, name: &str) -> Option<BoneId>;
    pub fn children(&self, b: BoneId) -> impl Iterator<Item = BoneId>;
    pub fn chain(&self, root: BoneId, tip: BoneId) -> SmallVec<[BoneId; 8]>;
}
```
- **T-9** The bone budget is `MAX_BONES = 128` hard and **64 for crowd tiers**. `BoneFlags::NO_CROWD` bones (fingers, ear tips) get their weights folded into the parent in crowd LODs. Answers Q-1 from the plan without banning fingers.

### 2.3 Regions
```rust
#[repr(u8)] #[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Region { Head, Face, Jaw, Neck, Chest, Belly, Back, Pelvis,
                  UpperArm, Forearm, Hand, Digit, Thigh, Shin, Foot,
                  Tail, Wing, Ear, Appendage, Hard /* claws, horns */ }
bitflags! { pub struct RegionMask: u32 { ... one bit per Region ... } }
```
Regions drive pattern masks, clothing coverage, hide masks and hit locations.

- **TQ-1** Is a fixed region enum (≤32) expressive enough, or do you want designer-defined regions (`RegionId(u8)` + a name table per species)?

### 2.4 Sockets
```rust
pub struct SocketDef {
    pub name: Name,          // "hand.r", "back", "mouth"
    pub kind: SocketKind,
    pub bone: BoneId,
    pub local: Xform,
    pub size: f32,           // characteristic size (palm width, back width) for tool scaling
}
pub enum SocketKind { Grip, Back, Hip, Head, Mouth, Shoulder, Custom(Name) }
```

### 2.5 Deterministic RNG
```rust
pub struct GenRng(rand_pcg::Pcg64Mcg);
impl GenRng {
    pub fn from_seed(seed: u64) -> Self;
    /// Child stream derived from (this stream's seed, label). Stable across template edits.
    pub fn fork(&self, label: impl Hash) -> GenRng;
    pub fn f32(&mut self) -> f32; pub fn range(&mut self, r: Range<f32>) -> f32; pub fn normal(&mut self, mean: f32, sd: f32) -> f32;
}
```
- **T-10** `fork` hashes `(parent_seed, label)` with a fixed hasher (e.g. `rustc_hash`/`xxhash` with fixed seed). It does not use `std` `DefaultHasher`, whose output isn't guaranteed stable.

### 2.6 Curves
```rust
pub struct Curve1 { pub keys: SmallVec<[(f32, f32); 6]>, pub interp: Interp }   // t∈[0,1] → value
pub struct Curve3 { pub keys: SmallVec<[(f32, Vec3); 6]>, pub interp: Interp }
pub enum Interp { Linear, CatmullRom, Step }
```

---

## 3. `ornisculpt_genome`: from template to body plan

### 3.1 Genes (the expressive core)
Every tunable number in a template is either a literal or an **expression over genes**. Genes are normalized `0..1` values. That makes sliders, breeding, mutation and "find me something between these two" trivial.
```rust
pub struct GeneDef {
    pub name: Name,                 // "bulk", "leg_length", "horn_curl", "fat"
    pub dist: GeneDist,             // how wild individuals sample it
    pub default: f32,               // used for the species' canonical/crowd mesh
    pub mesh_affecting: bool,       // false ⇒ only palette/anim (doesn't force a unique mesh)
}
pub enum GeneDist { Fixed, Uniform { lo: f32, hi: f32 }, Normal { mean: f32, sd: f32 } }

/// A number that may depend on genes and per-part jitter. Serializable expression tree.
pub enum Scalar {
    Lit(f32),
    Gene { gene: Name, lo: f32, hi: f32, curve: Option<Curve1> },  // remap gene 0..1 → lo..hi
    Jitter { base: Box<Scalar>, amount: f32 },                     // uses the part's forked rng
    Sum(Vec<Scalar>), Product(Vec<Scalar>),
}
pub struct Genome { pub species: SpeciesId, pub values: Vec<f32> /* indexed by GeneId */, pub seed: u64 }
pub struct EvalCx<'a> { pub genome: &'a Genome, pub gene_index: &'a GeneIndex, pub rng: GenRng }
impl Scalar { pub fn eval(&self, cx: &mut EvalCx) -> f32; }
```
- **T-11** Genes are 0..1 and remapped per use, so the same gene ("bulk") can drive leg radius 0.1–0.2 *and* muscle stamp size 0.5–1.4 at once, without the two drifting apart.
- **T-12** `mesh_affecting` decides whether an individual needs its own mesh (§6.3).
- **TQ-2** Do you want gene **correlation** (e.g. bulk ↑ ⇒ leg_length ↓ slightly) as an explicit construct, or is it enough to express it via `Scalar::Sum` inside parts?

### 3.2 Template: a tree of parts
```rust
pub struct SpeciesTemplate {
    pub id: SpeciesId,
    pub name: Name,
    pub genes: Vec<GeneDef>,
    pub root: PartNode,               // usually a Spine
    pub tone: ToneDesc,
    pub palette: PaletteDesc,
    pub locomotion: LocomotionDesc,   // gaits, speeds; legs are discovered from parts
    pub actions: Vec<ActionId>,       // which shared action defs this species can perform
    pub clip_set: ClipSetDesc,        // which clips to bake (§5.6)
}
pub struct PartNode { pub name: Name, pub desc: PartDesc, pub children: Vec<ChildPart> }
pub struct ChildPart {
    pub attach: AttachRef,            // where on the parent part
    pub offset: XformExpr,            // extra rotation/translation (genes allowed)
    pub symmetry: Symmetry,
    pub enabled: Option<Scalar>,      // gene-gated parts: >0.5 ⇒ present (horns on some individuals)
    pub node: PartNode,
}
pub enum Symmetry { None, MirrorX, Radial { count: u8 } }   // Radial: starfish, spider legs
pub struct AttachRef { pub point: Name /* "end", "segment", "jaw", ... */, pub t: Option<Scalar>, pub side: Side }
pub struct XformExpr { pub translate: [Scalar; 3], pub rotate_euler_deg: [Scalar; 3], pub scale: Scalar }
```

### 3.3 Part kinds
```rust
pub enum PartDesc {
    Spine(SpineDesc),      // the trunk: N segments along a curve, cross-section profile
    Neck(ChainDesc),
    Head(HeadDesc),        // skull blob + jaw bone + eye/ear/mouth attach points
    Limb(LimbDesc),        // 2–4 segments + optional hand/foot; registers an IK chain
    Digit(ChainDesc),      // fingers/toes; NO_CROWD by default
    Tail(ChainDesc),       // registers a spring chain
    Wing(WingDesc),        // see TQ-3
    Blob(BlobDesc),        // free primitive(s) on a bone: humps, crests, bellies
    Hard(HardPartDesc),    // claw, horn, beak, tooth, eye: non-SDF mesh (§4.5)
}
pub struct ChainDesc {
    pub segments: Scalar,             // rounded
    pub length: Scalar,
    pub radius: ProfileDesc,          // radius along the chain
    pub curl: Scalar,                 // rest curvature
    pub blend: Scalar,                // smooth-min radius to parent
}
pub struct ProfileDesc { pub curve: Curve1, pub scale: Scalar, pub cross_section: CrossSection }
pub enum CrossSection { Round, Ellipse { aspect: Scalar }, Flat { aspect: Scalar } }
pub struct SpineDesc { pub chain: ChainDesc, pub arch: Curve1 /* vertical bend */, pub belly: Scalar }
pub struct LimbDesc { pub chain: ChainDesc, pub stance: LimbStance, pub end: LimbEnd }
pub enum LimbStance { Plantigrade, Digitigrade, Unguligrade, Sprawl, Arm /* non-locomotor */ }
pub enum LimbEnd { None, Hand { palm: Scalar }, Foot { length: Scalar }, Hoof, Pad }
pub struct HeadDesc { pub skull: BlobDesc, pub snout: Scalar, pub jaw: Option<ChainDesc>, pub eyes: EyeDesc }
```
- **TQ-3** Wings: are they visual and folding only (a membrane is a flattened SDF sweep), or does flight need its own locomotion mode? This changes `LocomotionDesc`.

### 3.4 The `PartGen` trait and `PlanBuilder`
Each `PartDesc` variant implements `PartGen`. The template tree is walked recursively, and each part emits bones, primitives, sockets and rigs into the builder, plus named attach points for its children.
```rust
pub trait PartGen {
    fn emit(&self, b: &mut PlanBuilder, at: AttachPoint, cx: &mut EvalCx) -> Result<PartOutputs, PlanError>;
}
pub struct AttachPoint { pub bone: BoneId, pub local: Xform, pub side: Side, pub path: PartPath }
pub struct PartOutputs { pub attach: SmallVec<[(Name, AttachSlot); 4]> }
pub enum AttachSlot { Point(AttachPoint), Along(SmallVec<[AttachPoint; 8]>) /* t ∈ 0..1 over segments */ }

pub struct PlanBuilder { /* accumulates a BodyPlan */ }
impl PlanBuilder {
    pub fn add_bone(&mut self, parent: Option<BoneId>, name: Name, rest: Xform, length: f32, region: Region, flags: BoneFlags) -> BoneId;
    pub fn add_primitive(&mut self, p: Primitive);
    pub fn add_hard_part(&mut self, h: HardPart);
    pub fn add_socket(&mut self, s: SocketDef) -> SocketId;
    pub fn add_leg(&mut self, leg: LegRig);
    pub fn add_spring_chain(&mut self, c: SpringChain);
    pub fn set_look_at(&mut self, l: LookAtRig);
    pub fn add_joint_limit(&mut self, bone: BoneId, lim: JointLimit);
    pub fn finish(self) -> Result<BodyPlan, PlanError>;   // validates, computes inverse binds
}
pub fn build_plan(t: &SpeciesTemplate, g: &Genome) -> Result<BodyPlan, PlanError>;
```
- **T-13** Parts are an *enum* for serialization and dispatch, but each variant's logic lives in its own `impl PartGen`. Adding a new part kind is one variant plus one impl.
- **T-14** `PartPath` (e.g. `root/limb.front.l/digit.2`) is the `GenRng::fork` label, which delivers the stable randomness from T-4.

### 3.5 Body plan (output)
```rust
pub struct BodyPlan {
    pub skeleton: Skeleton,
    pub primitives: Vec<Primitive>,
    pub hard_parts: Vec<HardPart>,
    pub sockets: Vec<SocketDef>,
    pub joint_limits: Vec<(BoneId, JointLimit)>,
    pub rig: LocomotionRig,           // consumed by ornisculpt_anim
    pub palette: Palette,             // resolved colors (§4.6)
    pub tone: ResolvedTone,
}
pub struct Primitive {
    pub bone: BoneId,
    pub shape: PrimShape,
    pub local: Xform,                 // relative to bone
    pub blend: f32,                   // smooth-min k
    pub op: PrimOp,                   // Union | Subtract
    pub layer: PrimLayer,             // Base | Muscle | Landmark | Fat
    pub region: Region,
}
pub enum PrimShape {
    Sphere { r: f32 }, Ellipsoid { radii: Vec3 },
    Capsule { r0: f32, r1: f32, len: f32 },          // tapered capsule along +Y
    RoundCone { r0: f32, r1: f32, len: f32 },
    Sweep { spline: Curve3, radius: Curve1, cross: CrossSection },   // necks, tails, tentacles
}
pub struct JointLimit { pub swing_deg: Vec2, pub twist_deg: Vec2 }
pub struct HardPart { pub bone: BoneId, pub local: Xform, pub kind: HardKind, pub size: f32, pub region: Region }
pub enum HardKind { Claw { curve: f32 }, Horn { curl: f32, twist: f32 }, Beak { hook: f32 }, Tooth, Eye { iris: f32 }, Mesh(AssetRef) }
```

---

## 4. `ornisculpt_mesh`: body plan to skinned mesh

### 4.1 The `Field` trait
```rust
pub trait Field: Send + Sync {
    fn bounds(&self) -> Aabb;
    /// Signed distance (negative inside). The meshing hot path calls only this.
    fn distance(&self, p: Vec3A) -> f32;
    /// Batch version for SIMD and cache-friendly evaluation. Default loops over `distance`.
    fn distance_batch(&self, ps: &[Vec3A], out: &mut [f32]) { for (p, o) in ps.iter().zip(out) { *o = self.distance(*p) } }
    fn gradient(&self, p: Vec3A) -> Vec3A { /* central differences */ }
    /// Called once per *output vertex*, not per voxel: bone influences, region, layer.
    fn surface_info(&self, p: Vec3A) -> SurfaceInfo;
}
pub struct SurfaceInfo { pub influences: Influences, pub region: Region, pub layer: PrimLayer }
pub struct Influences { pub bones: [BoneId; 4], pub weights: [f32; 4] }   // top-4, normalized
```
- **T-15** Distance and influence are separate calls. Meshing needs about 100× more distance samples than vertices, so the expensive top-4 bookkeeping only happens at the vertices.
- Implementations:
  - `BodyField`: built from `BodyPlan.primitives`, with a BVH for culling, smooth-min and the tone layers.
  - Combinators `Offset<F>`, `Intersect<A,B>`, `Subtract<A,B>`, `RegionClip<F>` (keep the surface only where `surface_info.region ∈ mask`), `Displace<F, N>` (noise folds). Garments use the same combinators (§4.7).

### 4.2 The `Mesher` trait
```rust
pub trait Mesher {
    fn mesh<F: Field>(&self, field: &F, s: &MeshSettings) -> Result<RawMesh, MeshError>;
}
pub struct MeshSettings { pub voxel_size: f32, pub chunk_voxels: u32, pub max_triangles: u32 }
pub struct SurfaceNetsMesher;      // v1
// later: DualContourMesher, GpuSurfaceNets (lives in ornisculpt_bevy, since it needs wgpu)
```
- **T-16** `Mesher::mesh` is generic over `F` (static dispatch in the hot loop), so `Mesher` itself isn't object-safe. If a runtime-swappable mesher is needed, wrap it in an enum.

### 4.3 Mesh types
```rust
pub struct RawMesh { pub positions: Vec<Vec3>, pub normals: Vec<Vec3>, pub indices: Vec<u32> }

pub struct CreatureMesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub joints: Vec<[u16; 4]>,        // matches Bevy ATTRIBUTE_JOINT_INDEX (Uint16x4)
    pub weights: Vec<[f32; 4]>,       // matches ATTRIBUTE_JOINT_WEIGHT (Float32x4)
    pub attrs: Vec<PackedAttrs>,
    pub uvs: Option<Vec<Vec2>>,       // bake-time only (normal maps, decals)
    pub indices: Vec<u32>,
    pub submeshes: SmallVec<[Submesh; 4]>,   // body, hard parts, garments → material slots
}
pub struct Submesh { pub index_range: Range<u32>, pub material: MaterialSlot }
pub enum MaterialSlot { Skin, Hard, Eye, Garment(u8) }

/// Two u32s per vertex on the GPU.
#[repr(C)] pub struct PackedAttrs {
    pub a: u32,   // region:u8 | cavity:u8 | curvature:u8 | limb_t:u8
    pub b: u32,   // hide_mask:u8 | layer:u8 | primary_bone:u16   (bone-local pos is derived in shader from primary bone)
}
pub struct LodChain { pub levels: SmallVec<[LodLevel; 4]> }
pub struct LodLevel { pub mesh: CreatureMesh, pub screen_coverage: f32 }
```
- **T-17** Bone-local position is **not stored**. The shader reconstructs it from `primary_bone` and the bind pose, which saves 6–12 bytes per vertex.

### 4.4 Post-processing (free functions, not traits)
```rust
pub fn weld(m: &mut RawMesh, eps: f32);
pub fn taubin_smooth(m: &mut RawMesh, iters: u32, lambda: f32, mu: f32);
pub fn skin<F: Field>(m: RawMesh, field: &F, smooth_passes: u32) -> CreatureMesh;
pub fn compute_attrs(m: &mut CreatureMesh, field: &impl Field, plan: &BodyPlan);
pub fn build_lods(m: &CreatureMesh, spec: &[LodSpec], plan: &BodyPlan) -> LodChain;   // meshopt; folds NO_CROWD bones
pub fn unwrap_uvs(m: &mut CreatureMesh) -> Result<(), MeshError>;                      // xatlas, bake only
pub fn bake_normal_map<F: Field>(m: &CreatureMesh, detail: &F, res: u32) -> Image2D;
```
- **T-18** These are plain functions because they are fixed pipeline steps with no reason to swap them. A trait can be introduced later if one of them grows real alternatives.

### 4.5 Hard parts
```rust
pub trait HardPartGen { fn generate(&self, h: &HardPart, rng: &mut GenRng) -> CreatureMesh; }   // 100% weight to h.bone
```

### 4.6 Palette and tone
```rust
pub struct PaletteDesc { pub base: HueRange, pub secondary: HueRange, pub accent: HueRange, pub pattern: PatternDesc }
pub struct HueRange { pub h: Scalar, pub s: Scalar, pub v: Scalar }
pub enum PatternDesc { Solid, Countershade { sharpness: Scalar }, Stripes { freq: Scalar, warp: Scalar },
                       Spots { density: Scalar, size: Scalar }, Rosettes { .. }, Mottle { .. }, Scales { size: Scalar } }
pub struct Palette { pub base: [f32; 3], pub secondary: [f32; 3], pub accent: [f32; 3], pub pattern: PatternGpu }
#[repr(C)] pub struct PatternGpu { pub kind: u32, pub params: [f32; 3], pub region_mask: u32 }   // goes into instance data
pub struct ToneDesc { pub muscle: Scalar, pub fat: Scalar, pub folds: Scalar, pub landmark: Scalar, pub stamps: Vec<StampSet> }
```

### 4.7 Garments (types only for now)
```rust
pub enum GarmentDesc {
    Shell { cover: RegionMask, thickness: Scalar, extend: Scalar, hides: RegionMask, material: GarmentMaterial },
    Sweep { from: SocketKind, to: Option<SocketKind>, profile: ProfileDesc, .. },   // cloaks, belts, straps
    Authored { mesh: AssetRef, landmarks: Vec<(Name, Vec3)>, hides: RegionMask },   // RBF-fitted
}
pub struct Outfit { pub garments: SmallVec<[GarmentDesc; 4]> }
pub fn build_garment(g: &GarmentDesc, body: &BodyField, plan: &BodyPlan, base: &CreatureMesh) -> CreatureMesh;
```

---

## 5. `ornisculpt_anim`

### 5.1 Pose types
```rust
pub struct Pose { pub local: Vec<Xform> }               // one per bone, parent-relative
pub struct ModelPose { pub model: Vec<Affine3A> }
pub fn forward_kinematics(sk: &Skeleton, p: &Pose, root: Affine3A, out: &mut ModelPose);
pub fn skinning_matrices(sk: &Skeleton, mp: &ModelPose, out: &mut [Affine3A]);   // model * inverse_bind
```

### 5.2 Rig descriptions (from the body plan, shared via `Arc`)
```rust
pub struct LocomotionRig {
    pub legs: SmallVec<[LegRig; 8]>,
    pub spine: SmallVec<[BoneId; 16]>,
    pub springs: SmallVec<[SpringChain; 8]>,
    pub look_at: Option<LookAtRig>,
    pub gaits: SmallVec<[GaitDef; 4]>,
    pub root_height: f32,
}
pub struct LegRig { pub chain: SmallVec<[BoneId; 4]>, pub foot: BoneId, pub pole: Vec3, pub rest_foot: Vec3, pub phase: f32, pub stance: LimbStance }
pub struct GaitDef { pub name: Name, pub speed: Range<f32>, pub duty: f32, pub phase_override: Option<SmallVec<[f32; 8]>>, pub stride: f32, pub lift: f32 }
pub struct SpringChain { pub bones: SmallVec<[BoneId; 12]>, pub stiffness: f32, pub damping: f32, pub gravity: f32 }
pub struct LookAtRig { pub bones: SmallVec<[(BoneId, f32 /*share*/); 4]>, pub limit_deg: f32 }
```

### 5.3 Environment and control boundaries
```rust
/// Implemented by avian3d in-game and by `FlatGround` in the baker.
pub trait GroundQuery: Sync {
    fn ground(&self, origin: Vec3, max_drop: f32) -> Option<GroundHit>;
}
pub struct GroundHit { pub point: Vec3, pub normal: Vec3 }

/// What gameplay/AI writes; the only input animation reads.
#[derive(Clone, Default)]
pub struct ControlInput {
    pub velocity: Vec3,                 // desired, world
    pub facing: Quat,
    pub look_target: Option<Vec3>,
    pub action: Option<ActionRequest>,  // attack, emote...
    pub ik_overrides: SmallVec<[IkOverride; 2]>,   // "put hand.r here" (grab, lever, tool)
}
pub struct IkOverride { pub effector: Effector, pub target: Xform, pub weight: f32 }
pub enum Effector { Socket(SocketId), Bone(BoneId), Leg(u8) }
```
- **TQ-4** Is `ControlInput` the right granularity for gameplay? Alternatives are bone-level access (more power, easier to break) or higher-level intents ("go to X") pushed into the anim layer (less power).

### 5.4 IK
```rust
pub trait IkSolver {
    fn solve(&self, sk: &Skeleton, chain: &[BoneId], pose: &mut Pose, model: &mut ModelPose,
             target: Xform, pole: Option<Vec3>, limits: &JointLimits);
}
pub struct TwoBoneIk; pub struct Fabrik { pub iters: u8, pub tolerance: f32 }
```

### 5.5 Pose layers (the procedural animation stack)
```rust
pub trait PoseLayer {
    type State: Default + Send + Sync;
    fn update(&self, st: &mut Self::State, cx: &mut AnimCx, pose: &mut Pose);
}
pub struct AnimCx<'a> {
    pub dt: f32, pub root: Affine3A, pub skeleton: &'a Skeleton, pub rig: &'a LocomotionRig,
    pub input: &'a ControlInput, pub ground: &'a dyn GroundQuery, pub model: &'a mut ModelPose,
}
// Concrete layers (static dispatch, fixed order):
pub struct GaitLayer;     pub struct FootIkLayer;  pub struct BodyBalanceLayer;
pub struct SpringLayer;   pub struct LookAtLayer;  pub struct BreathLayer;   pub struct ActionLayer;

pub struct Animator { pub rig: Arc<LocomotionRig>, pub actions: Arc<ActionLibrary> }
pub struct AnimState { gait: GaitState, feet: FootIkState, balance: BalanceState, springs: SpringState,
                       look: LookState, breath: BreathState, action: ActionState }
impl Animator { pub fn update(&self, st: &mut AnimState, cx: &mut AnimCx, pose: &mut Pose); }
```
- **T-19** The layers are a **fixed, statically dispatched stack**, with the trait used for code organization. The state is one struct, so there are no per-creature heap allocations. The extension point is a later `extra: Vec<Box<dyn DynPoseLayer>>` if needed.
- **T-20** `GroundQuery` is `&dyn` because it's called a handful of times per creature per frame, not per bone, so virtual dispatch costs nothing that matters here.

### 5.6 Actions: retargetable procedural motions
Attacks and emotes can't be authored per skeleton, since every creature is different. Instead they're authored on **effectors in normalized body space**:
```rust
pub struct ActionDef {
    pub name: Name, pub duration: f32, pub requires: SmallVec<[SocketKind; 2]>,   // "needs a Grip"
    pub tracks: SmallVec<[ActionTrack; 4]>,
}
pub struct ActionTrack {
    pub effector: EffectorRef,     // e.g. Grip.r, Head, Pelvis
    pub space: ActionSpace,        // relative to shoulder/chest/root, scaled by limb length
    pub path: Curve3, pub rotation: Option<CurveQuat>, pub weight: Curve1,
}
pub enum EffectorRef { Socket(SocketKind, Side), Head, Pelvis, Chest, Leg(Side, u8) }
pub enum ActionSpace { LimbRoot, Chest, Root }
pub struct ActionLibrary { pub actions: Vec<ActionDef> }
```

### 5.7 Baking clips
```rust
pub struct ClipSetDesc { pub clips: Vec<ClipSpec>, pub fps: f32 }
pub struct ClipSpec { pub name: Name, pub kind: ClipKind, pub cycles: u8 }
pub enum ClipKind { Idle, Locomotion { speed: f32 }, Turn { rate_deg_s: f32 }, Action(ActionId), Hold(SocketKind) }

pub struct BakedClip { pub name: Name, pub first_frame: u32, pub frames: u32, pub fps: f32, pub looping: bool,
                       pub root_motion: Vec<Xform> }
/// Local-space TRS per bone per frame (NOT skinning matrices). See T-21.
pub struct BoneAnimTexture { pub bones: u16, pub frames: u32, pub texels: Vec<[f32; 4]> /* 2 texels/bone: quat, translation+uniform scale */ }
pub struct ClipBank { pub clips: Vec<BakedClip>, pub texture: BoneAnimTexture }
pub fn bake_clips(plan: &BodyPlan, animator: &Animator, set: &ClipSetDesc) -> ClipBank;
```
- **T-21 (changes plan D-13/D-15):** the BAT stores **local** bone poses, and the crowd pipeline runs a small **compute pass** per frame. It does FK, applies per-individual bone scale, multiplies by the inverse bind, and writes skinning matrices to a storage buffer.
  - Cost: 4000 × 64 bones × 48 B ≈ 12 MB of buffer and about 256k bone ops on the GPU, which is trivial.
  - Gain: per-individual proportions are *correct* through the hierarchy, not approximated, and clip blending happens on local TRS, which blends properly.

---

## 6. `ornisculpt_gen`: pipeline and assets

### 6.1 Identity
```rust
pub const GENERATOR_VERSION: u32 = 1;   // bump when output for the same input changes
#[derive(Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreatureKey(pub u128);       // xxh3-128 of (GENERATOR_VERSION, template hash, genome-if-mesh-affecting, outfit, quality)
pub struct Individual { pub species: SpeciesId, pub seed: u64, pub overrides: SmallVec<[(Name, f32); 8]>, pub outfit: Option<Outfit> }
pub fn resolve_genome(t: &SpeciesTemplate, ind: &Individual) -> Genome;
```

### 6.2 Settings
```rust
pub enum Quality { Preview, Crowd, Hero, Bake }
pub struct GenSettings {
    pub quality: Quality,
    pub mesh: MeshSettings,
    pub lods: SmallVec<[LodSpec; 4]>,
    pub normal_map: Option<u32>,        // resolution; None at runtime by default
    pub bake_clips: bool,
}
pub struct LodSpec { pub ratio: f32, pub error: f32, pub screen_coverage: f32, pub fold_no_crowd: bool }
```

### 6.3 Assets: species vs individual
Instancing only works when individuals share a mesh, so the two levels are explicit:
```rust
/// Shared by every member of a species. Required for crowds.
pub struct SpeciesAsset {
    pub template: Arc<SpeciesTemplate>,
    pub variants: SmallVec<[VariantMesh; 8]>,   // meshes at a few mesh-affecting gene points (lean, bulky, young, old)
    pub clips: ClipBank,                         // shared by all variants (same skeleton topology)
    pub rig: Arc<LocomotionRig>,
}
pub struct VariantMesh { pub genes: Vec<f32>, pub plan: Arc<BodyPlan>, pub lods: LodChain, pub normal_map: Option<Image2D> }

/// Only for heroes or near creatures whose mesh-affecting genes are far from every variant.
pub struct IndividualAsset { pub key: CreatureKey, pub plan: Arc<BodyPlan>, pub lods: LodChain, pub normal_map: Option<Image2D> }

/// What a spawned creature needs at runtime regardless of tier.
pub struct CreatureInstanceDesc {
    pub species: SpeciesId, pub variant: u8,
    pub bone_scale: Vec<f32>,                    // per-bone, derived from genome vs variant genes
    pub palette: Palette,
    pub unique: Option<CreatureKey>,             // Some ⇒ uses IndividualAsset at hero tier
}
```
- **T-22** Crowd individuals = nearest **variant mesh** + per-bone scale + palette. Body shape, color and proportions all vary while instancing is kept.
- **T-23 (required by T-22):** all variants in a species must share **skeleton topology**: the same bones in the same order. Gene-gated parts (`ChildPart.enabled`) therefore always emit their bones and only toggle primitives and hard parts. Bones that end up unused get zero weight.
- **TQ-5** How many variants per species: fixed (e.g. 8), or authored per species?

### 6.4 Entry points
```rust
pub fn generate_species(t: &SpeciesTemplate, s: &GenSettings, actions: &ActionLibrary) -> Result<SpeciesAsset, GenError>;
pub fn generate_individual(t: &SpeciesTemplate, ind: &Individual, s: &GenSettings) -> Result<IndividualAsset, GenError>;
pub fn instance_desc(sp: &SpeciesAsset, ind: &Individual) -> CreatureInstanceDesc;   // cheap: no meshing
pub enum GenError { Plan(PlanError), Mesh(MeshError), BoneBudget { got: usize, max: usize }, .. }
```

### 6.5 Storage
```rust
pub trait CreatureStore: Send + Sync {
    fn load_species(&self, id: SpeciesId, version: u32) -> Option<SpeciesAsset>;
    fn store_species(&self, a: &SpeciesAsset) -> io::Result<()>;
    fn load_individual(&self, k: CreatureKey) -> Option<IndividualAsset>;
    fn store_individual(&self, a: &IndividualAsset) -> io::Result<()>;
}
pub struct DiskStore { root: PathBuf }    // postcard + zstd, files named by key
pub struct MemoryStore { .. }             // LRU, sized in bytes
pub struct LayeredStore<A, B>(A, B);      // memory → disk
```
- **TQ-6** Is `postcard` + `zstd` fine for the cache, or do you want `rkyv` zero-copy from the start? `rkyv` loads faster but its types are more invasive.

---

## 7. Equipment (shared types; behaviour comes later)
```rust
pub struct ToolDef {
    pub id: ToolId, pub name: Name, pub mesh: AssetRef, pub nominal_size: f32,
    pub accepts: SocketKind,
    pub grips: SmallVec<[Grip; 2]>,           // two-handed tools have 2
    pub actions: SmallVec<[ActionId; 4]>,     // what you can do with it
}
pub struct Grip { pub local: Xform, pub hand: Side, pub curl: f32 }
pub struct Equipped { pub slots: SmallVec<[(SocketId, ToolId); 4]> }
```

---

## 8. `ornisculpt_bevy`: ECS surface

### 8.1 Assets and loaders
```rust
impl Asset for SpeciesTemplateAsset /* wraps SpeciesTemplate, RON loader, hot-reload */;
impl Asset for SpeciesAssetHandle    /* .species  binary loader */;
impl Asset for IndividualAssetHandle /* .creature binary loader */;
impl Asset for ActionLibraryAsset, ToolDefAsset;
```

### 8.2 Components
```rust
#[derive(Component)] pub struct Creature { pub species: SpeciesId, pub desc: Arc<CreatureInstanceDesc> }   // required components ↓
#[derive(Component, Default)] pub struct CreatureControl(pub ControlInput);
#[derive(Component)] pub enum AnimTier { Hero, Near, Far, Impostor }
#[derive(Component)] pub struct TierHysteresis { pub since: f32, pub pending: Option<AnimTier> }

// Tier A only:
#[derive(Component)] pub struct HeroRig { pub animator: Animator, pub state: AnimState, pub pose: Pose, pub joints: Vec<Entity> }
// Tiers B/C:
#[derive(Component)] pub struct CrowdAnim { pub clip: [ClipId; 2], pub time: [f32; 2], pub blend: f32, pub update_bucket: u8 }

#[derive(Component)] pub struct GenerationPending { pub task: Task<Result<IndividualAsset, GenError>> }
#[derive(Component)] pub struct Placeholder;
#[derive(Component)] pub struct EquippedTools(pub Equipped);
```
- **T-24** `Creature` uses Bevy **required components** to pull in `CreatureControl`, `AnimTier` and `Transform`. Spawning is `commands.spawn((Creature::new(..), Transform::..))`.

### 8.3 Resources
```rust
#[derive(Resource)] pub struct CreatureLibrary { species: HashMap<SpeciesId, Handle<..>>, tools: .., actions: .. }
#[derive(Resource)] pub struct LodConfig { pub tiers: [TierRule; 4], pub max_hero: u32, pub promotions_per_frame: u32 }
#[derive(Resource)] pub struct GenBudget { pub max_in_flight: u32, pub max_completions_per_frame: u32 }
#[derive(Resource)] pub struct Store(pub Arc<dyn CreatureStore>);
pub struct AvianGround<'w, 's> /* SystemParam implementing GroundQuery */;
```

### 8.4 Render-world types (crowd pipeline)
```rust
#[repr(C)] #[derive(Pod, Zeroable)]
pub struct CrowdInstanceGpu {
    pub transform: [[f32; 4]; 3],          // affine 3×4
    pub clip_frame: [u32; 2], pub clip_t: [f32; 2], pub blend: f32,
    pub bone_scale_offset: u32,            // into per-instance bone-scale storage buffer
    pub skin_offset: u32,                  // into skinning-matrix output buffer
    pub palette: [u32; 3],                 // packed rgb8 ×3
    pub pattern: PatternGpu,
}
pub struct CrowdBatches;    // (species, variant, lod) → instance range; one draw each, plus one shadow draw each
pub struct CrowdSkinningPipeline;   // compute: BAT + bone scale → skin matrices (T-21)
pub struct CrowdDrawPipeline;       // vertex: skin from storage buffer; fragment: creature material
```

### 8.5 Plugin and system sets
```rust
pub struct OrnisculptPlugin { pub store_root: PathBuf, pub lod: LodConfig, pub gen: GenBudget }
#[derive(SystemSet)] pub enum OrnisculptSet { Generation, Control, Tiering, HeroAnimate, CrowdAnimate, SyncJoints }
// Order: Generation → Control (user systems write CreatureControl) → Tiering → {HeroAnimate, CrowdAnimate} → SyncJoints → TransformPropagate
```

---

## 9. Open questions (summary)
| Tag | Question |
|---|---|
| TQ-1 | Fixed `Region` enum, or designer-defined regions? |
| TQ-2 | Explicit gene correlation construct? |
| TQ-3 | Wings: visual/folding only, or flight locomotion? |
| TQ-4 | Is `ControlInput` the right gameplay-facing granularity? |
| TQ-5 | Variants per species: fixed count or authored? |
| TQ-6 | Cache serialization: postcard+zstd or rkyv? |
| — | Plan questions still open: Q-2 (runtime normal maps), Q-3 (hardware/latency), Q-6 (fur/feathers), Q-7 (camera), Q-8 (growth/aging, which would make `bone_scale` animated, not static) |
