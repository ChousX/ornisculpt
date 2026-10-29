use std::collections::HashMap;
use std::sync::Arc;

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use xxhash_rust::xxh3::xxh3_128;

use super::{BodyFailed, LiveBody, Mob, MobSeed};
use crate::anatomy::{
    Anatomy, BodyPlan, Form, LodLevel, PartAssets, PartLodRules, TriMesh, apply_form, build_anatomy, referenced_assets,
};
use crate::coat::{Coat, CoatExt, CoatLayer, CoatMeshes, CoatUniform, MobCoat, MobMaterial, annotate, build_coat_meshes};
use crate::motion::{BakedMotion, Gait, bake_motion};
use crate::num::Clamp;
use super::CrowdSkin;
use bevy::pbr::ExtendedMaterial;
use bevy::render::storage::ShaderBuffer;
use crate::clothing::{GarmentCover, MobOutfit, Outfit, cover_skin, fit_outfit, garment_lods};
use crate::surface::{AdaptiveContour, BodyMeshData, ContactSet, centres_of_rotation, MaterialSlot, SurfaceSettings, merge, mesh_all_lods_keeping, muscle_flex};

/// Bump when generation output changes for the same input (invalidates cached bodies).
pub const GENERATOR_VERSION: u32 = 1;

/// Content hash identifying a generated body. Equal keys share one [`GeneratedBody`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BodyKey(pub u128);

/// Hash of everything generation depends on, including the content of referenced authored meshes.
///
/// The seed only counts when some level meshes seed-dependent geometry (creases), so identical
/// bodies normally share meshes.
#[must_use]
pub fn body_key(live: &BodyPlan, settings: &SurfaceSettings, seed: u64, assets: &PartAssets, coat: &Coat, outfit: &Outfit) -> BodyKey {
    let uses_seed = LodLevel::ALL.iter().any(|l| settings.get(*l).geometric_folds);
    let mut mesh_hashes: Vec<(&String, u64)> = assets.meshes.iter().map(|(p, m)| (p, m.hash)).collect();
    mesh_hashes.sort();
    let text = format!(
        "{GENERATOR_VERSION}|{}|{}|{}|{mesh_hashes:?}|{}|{}",
        ron::to_string(live).unwrap_or_default(),
        ron::to_string(settings).unwrap_or_default(),
        if uses_seed { seed } else { 0 },
        ron::to_string(coat).unwrap_or_default(),
        ron::to_string(outfit).unwrap_or_default(),
    );
    BodyKey(xxh3_128(text.as_bytes()))
}

/// Authored meshes referenced by body plans, loaded through the asset server and extracted once.
#[derive(Resource, Default)]
pub struct PartAssetCache {
    handles: HashMap<String, Handle<Mesh>>,
    pub assets: PartAssets,
}

impl PartAssetCache {
    /// Ensures every path is loaded and extracted. Returns the subset, or `None` while loading.
    fn resolve(&mut self, paths: &[String], server: &AssetServer, meshes: &Assets<Mesh>) -> Option<PartAssets> {
        let mut ready = true;
        for path in paths {
            if self.assets.meshes.contains_key(path) {
                continue;
            }
            let handle = self.handles.entry(path.clone()).or_insert_with(|| server.load(path.clone()));
            match meshes.get(&*handle).map(extract_trimesh) {
                Some(Some(tri)) => self.assets.insert(path.clone(), Arc::new(tri)),
                Some(None) => {
                    error!("{path}: not an indexed triangle mesh with positions");
                    ready = false;
                }
                None => ready = false,
            }
        }
        ready.then(|| self.assets.subset(paths.iter().map(String::as_str)))
    }
}

/// Triangle data from a Bevy mesh (must keep its main-world copy, as glTF meshes do by default).
fn extract_trimesh(mesh: &Mesh) -> Option<TriMesh> {
    use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
    if mesh.primitive_topology() != PrimitiveTopology::TriangleList {
        return None;
    }
    let Some(VertexAttributeValues::Float32x3(pos)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { return None };
    let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
        Some(VertexAttributeValues::Float32x3(n)) => n.iter().map(|v| Vec3::from_array(*v)).collect(),
        _ => Vec::new(),
    };
    let indices: Vec<u32> = match mesh.indices() {
        Some(Indices::U32(i)) => i.clone(),
        Some(Indices::U16(i)) => i.iter().map(|&x| u32::from(x)).collect(),
        None => (0..pos.len().clamp_u32()).collect(),
    };
    Some(TriMesh::new(pos.iter().map(|v| Vec3::from_array(*v)).collect(), normals, indices))
}

/// Hot reload: regenerate mobs whose body plan or authored meshes changed on disk.
#[allow(clippy::too_many_arguments)]
pub(super) fn reload_changed_assets(
    mut commands: Commands,
    mut plan_events: MessageReader<AssetEvent<BodyPlan>>,
    mut mesh_events: MessageReader<AssetEvent<Mesh>>,
    mut coat_events: MessageReader<AssetEvent<Coat>>,
    mut outfit_events: MessageReader<AssetEvent<Outfit>>,
    coated: Query<(Entity, &MobCoat)>,
    dressed: Query<(Entity, &MobOutfit)>,
    outfits: Res<Assets<Outfit>>,
    mut parts: ResMut<PartAssetCache>,
    mobs: Query<(Entity, &Mob, Option<&LiveBody>)>,
) {
    let plans = modified(&mut plan_events);
    for (entity, mob, _) in &mobs {
        if plans.contains(&mob.body.id()) {
            commands.entity(entity).remove::<LiveBody>().insert(GenRequest);
        }
    }
    let coats = modified(&mut coat_events);
    for (entity, coat) in &coated {
        if coats.contains(&coat.0.id()) {
            commands.entity(entity).insert(GenRequest);
        }
    }
    let outfit_ids = modified(&mut outfit_events);
    for (entity, outfit) in &dressed {
        if outfit_ids.contains(&outfit.0.id()) {
            commands.entity(entity).insert(GenRequest);
        }
    }
    let meshes = modified(&mut mesh_events);
    let stale: Vec<String> = parts.handles.iter().filter(|(_, h)| meshes.contains(&h.id())).map(|(p, _)| p.clone()).collect();
    for path in stale {
        parts.assets.meshes.remove(&path);
        for (entity, _, live) in &mobs {
            if live.is_some_and(|l| referenced_assets(&l.0).contains(&path)) {
                commands.entity(entity).insert(GenRequest);
            }
        }
        for (entity, outfit) in &dressed {
            if outfits.get(&outfit.0).is_some_and(|o| o.referenced_meshes().contains(&path)) {
                commands.entity(entity).insert(GenRequest);
            }
        }
    }
}

/// Every asset id reported modified since the last read, in order.
fn modified<A: Asset>(events: &mut MessageReader<AssetEvent<A>>) -> Vec<AssetId<A>> {
    events.read().filter_map(|ev| if let AssetEvent::Modified { id } = ev { Some(*id) } else { None }).collect()
}

/// Meshes for one LOD level, one per material slot.
#[derive(Clone, Debug, Default)]
pub struct LodMeshes(pub Vec<(MaterialSlot, Handle<Mesh>)>);

/// A generated, upload-ready body shared by every mob with the same [`BodyKey`].
#[derive(Asset, TypePath, Debug)]
pub struct GeneratedBody {
    pub key: BodyKey,
    pub anatomy: Arc<Anatomy>,
    /// Indexed by `LodLevel as usize`. `Hero` uses the full skeleton, coarser levels the crowd one.
    pub lods: [LodMeshes; 3],
    pub full_bindposes: Handle<SkinnedMeshInverseBindposes>,
    /// Bounding radius around the mob origin, for tiering.
    pub radius: f32,
    /// Baked crowd-tier motion for the default gait of this body's leg count.
    pub motion: Arc<BakedMotion>,
    /// Fur shells / feather cards per LOD (indexed like `lods`).
    pub coat_meshes: [CoatMeshHandles; 3],
    /// Materials for this body + coat: `[hero, crowd]`.
    pub materials: [SlotMaterials; 2],
    /// Soft-tissue regions; their springs drive the hero soft joints (crowds play them baked).
    #[cfg(feature = "soft-tissue")]
    pub soft: Arc<crate::soft::SoftTissue>,
}

/// Coat geometry meshes for one LOD level.
#[derive(Clone, Debug, Default)]
pub struct CoatMeshHandles {
    pub shells: Option<Handle<Mesh>>,
    pub feathers: Option<Handle<Mesh>>,
    pub hair: Option<Handle<Mesh>>,
}

/// One material per surface kind, for one skinning path (hero or crowd).
#[derive(Clone, Debug)]
pub struct SlotMaterials {
    pub skin: Handle<MobMaterial>,
    pub hard: Handle<MobMaterial>,
    pub eye: Handle<MobMaterial>,
    pub shell: Handle<MobMaterial>,
    pub feather: Handle<MobMaterial>,
    pub hair: Handle<MobMaterial>,
    pub cloth: Handle<MobMaterial>,
    pub wing: Handle<MobMaterial>,
}

impl SlotMaterials {
    #[must_use]
    pub fn get(&self, slot: MaterialSlot) -> Handle<MobMaterial> {
        match slot {
            MaterialSlot::Skin => self.skin.clone(),
            MaterialSlot::Hard => self.hard.clone(),
            MaterialSlot::Eye => self.eye.clone(),
            MaterialSlot::Cloth => self.cloth.clone(),
            MaterialSlot::Wing => self.wing.clone(),
        }
    }
}

impl GeneratedBody {
    /// Materials for a tier (`Hero` = Bevy skinning, coarser = crowd skinning).
    #[must_use]
    pub fn materials_for(&self, level: LodLevel) -> &SlotMaterials {
        let [hero, crowd] = &self.materials;
        if level == LodLevel::Hero { hero } else { crowd }
    }
}

/// Builds the materials of a body + coat (one per surface kind, hero and crowd).
fn body_materials(
    coat: &Coat,
    outfit: &Outfit,
    anatomy: &Anatomy,
    [bones, contacts]: [&Handle<ShaderBuffer>; 2],
    materials: &mut Assets<MobMaterial>,
) -> [SlotMaterials; 2] {
    let skin_uniform = CoatUniform { cloth: outfit.palette(), ..coat.uniform(anatomy) };
    let hard = Coat::plain(coat.hard, 0.4);
    // Iris (base), white (secondary), pupil (accent); eyes without white show only the iris.
    let eye = Coat { secondary: [0.82, 0.79, 0.76], accent: [0.01, 0.01, 0.01], ..Coat::plain(coat.iris, 0.1) };
    [false, true].map(|crowd| {
        let mut mat = |uniform: CoatUniform, roughness: f32, layer: CoatLayer| {
            // Thin wing surfaces let light through (a flipped diffuse lobe; the shader varies it).
            let diffuse_transmission = if layer == CoatLayer::Wing && !crowd { 0.5 } else { 0.0 };
            materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    perceptual_roughness: roughness,
                    cull_mode: None,
                    double_sided: true,
                    diffuse_transmission,
                    ..default()
                },
                extension: CoatExt { bones: bones.clone(), contacts: contacts.clone(), coat: uniform, crowd, layer },
            })
        };
        SlotMaterials {
            skin: mat(skin_uniform, coat.roughness, CoatLayer::Skin),
            hard: mat(hard.uniform(anatomy), hard.roughness, CoatLayer::Hard),
            eye: mat(eye.uniform(anatomy), eye.roughness, CoatLayer::Hard),
            shell: mat(skin_uniform, 0.9, CoatLayer::Shell),
            feather: mat(skin_uniform, 0.6, CoatLayer::Feather),
            hair: mat(skin_uniform, 0.5, CoatLayer::Hair),
            cloth: mat(skin_uniform, 0.85, CoatLayer::Cloth),
            wing: mat(skin_uniform, 0.6, CoatLayer::Wing),
        }
    })
}

/// Marks a mob that needs (re)generation.
#[derive(Component, Default)]
pub struct GenRequest;

/// Mob is waiting on the in-flight generation for this key.
#[derive(Component)]
pub(super) struct GenPending(pub BodyKey);

/// A generation task's result: the body and its meshes, or why it failed.
type GenResult = Result<(GenOutput, BodyMeshes), String>;

/// Finished bodies plus in-flight work, deduplicated by key.
///
/// `bodies` holds every body generated so far; an app that generates many bodies it no longer
/// shows (an editor) can drop entries it has no use for.
#[derive(Resource, Default)]
pub struct GenCache {
    pub bodies: HashMap<BodyKey, Handle<GeneratedBody>>,
    in_flight: HashMap<BodyKey, Task<GenResult>>,
    /// Why recent keys failed, for the mobs waiting on them ([`BodyFailed`]).
    failed: HashMap<BodyKey, String>,
}

/// Failure messages kept; past this the oldest are forgotten (they are read within a frame).
const FAILED_KEPT: usize = 32;

impl GenCache {
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Whether `key` is generated or generating.
    fn known(&self, key: &BodyKey) -> bool {
        self.bodies.contains_key(key) || self.in_flight.contains_key(key)
    }

    /// A pending mob's body, once its key is ready (used by `attach_body`).
    pub(super) fn ready(&self, pending: &GenPending) -> Option<Handle<GeneratedBody>> {
        self.bodies.get(&pending.0).cloned()
    }

    /// Failed keys are neither cached nor in flight; mobs waiting on them give up. Returns the
    /// failure's message (a generic one if it has been forgotten).
    pub(super) fn lost_error(&self, pending: &GenPending) -> Option<String> {
        (!self.known(&pending.0)).then(|| self.failed.get(&pending.0).cloned().unwrap_or_else(|| "generation failed".into()))
    }
}

/// The asset `handle` points at once loaded, or the default asset without a handle.
fn loaded_or_default<A: Asset + Clone + Default>(handle: Option<&Handle<A>>, assets: &Assets<A>) -> Option<A> {
    handle.map_or_else(|| Some(A::default()), |h| assets.get(h).cloned())
}

/// Generation throughput limits.
#[derive(Resource, Clone, Debug)]
pub struct GenBudget {
    pub max_in_flight: usize,
    pub max_completions_per_frame: usize,
    /// Cores left to the main and render threads while bodies generate: the rest are shared by
    /// the bodies generating at the time, for their parallel meshing stages ([`crate::surface::set_worker_threads`]).
    pub reserved_threads: usize,
}

impl Default for GenBudget {
    fn default() -> Self {
        Self { max_in_flight: 4, max_completions_per_frame: 2, reserved_threads: 2 }
    }
}

/// Everything one body's generation depends on.
pub struct GenInput {
    pub live: BodyPlan,
    pub rules: PartLodRules,
    pub assets: PartAssets,
    pub settings: SurfaceSettings,
    pub seed: u64,
    pub coat: Coat,
    pub outfit: Outfit,
}

/// A generated body before it becomes GPU assets.
pub struct GenOutput {
    pub anatomy: Anatomy,
    pub lods: [BodyMeshData; 3],
    pub motion: BakedMotion,
    pub coat: Coat,
    pub coat_meshes: [CoatMeshes; 3],
    pub outfit: Outfit,
    /// Proxies the hero skin is pushed out of where limbs meet (`deform.wesl`).
    pub contacts: ContactSet,
    #[cfg(feature = "soft-tissue")]
    pub soft: crate::soft::SoftTissue,
}

/// Wall time of each generation stage, in first-run order (a stage run twice is summed).
#[derive(Clone, Debug, Default)]
pub struct StageTimes(pub Vec<(&'static str, std::time::Duration)>);

impl StageTimes {
    /// Runs `f` as stage `name`: a trace span (for Tracy) plus a timing entry.
    fn stage<T>(&mut self, name: &'static str, f: impl FnOnce() -> T) -> T {
        let _span = info_span!("generate", stage = name).entered();
        // Bevy's `Instant` also works in the browser, where `std`'s panics.
        let t = bevy::platform::time::Instant::now();
        let out = f();
        let took = t.elapsed();
        match self.0.iter_mut().find(|(n, _)| *n == name) {
            Some((_, d)) => *d += took,
            None => self.0.push((name, took)),
        }
        out
    }
}

/// Generates one body: anatomy, outfit, meshes for every LOD, coat geometry and baked motion.
/// The app runs it on the async compute pool; benchmarks call it directly.
///
/// # Errors
/// When the anatomy or a level's mesh can't be built.
pub fn generate_body(input: GenInput, times: &mut StageTimes) -> Result<GenOutput, String> {
    let GenInput { live, rules, assets, settings, seed, coat, outfit } = input;
    let anatomy = times.stage("anatomy", || build_anatomy(&live, &rules, &assets)).map_err(|e| e.to_string())?;
    let (anatomy, cloth) =
        if outfit.garments.is_empty() { (anatomy, None) } else { times.stage("fit_outfit", || fit_outfit(anatomy, &outfit, &assets)) };
    // Give the simplifier the coat's markings, baked on the full mesh, so the triangles it keeps
    // go where the colour changes (a Hereford's white face and socks, a zebra's stripes) rather
    // than wherever the shape alone allows: crowd levels simplify hard, and even the hero's half
    // leaves triangles several centimetres across on a flat flank.
    let keep = |dense: &BodyMeshData, _level: LodLevel| {
        (!coat.markings.is_empty()).then(|| {
            let mut painted = dense.clone();
            // Bones are still the full skeleton's here (crowd levels remap them after).
            crate::coat::paint(&mut painted, &coat, &anatomy, LodLevel::Hero);
            painted.paint
        })
    };
    let mut lods = times
        .stage("mesh", || mesh_all_lods_keeping(&AdaptiveContour, &anatomy, &settings, seed, Some(&keep)))
        .map_err(|e| e.to_string())?;
    if let Some(cloth) = &cloth {
        times.stage("garments", || {
            for (lod, garments) in lods.iter_mut().zip(garment_lods(cloth, &anatomy, &settings)) {
                merge(lod, &garments);
            }
        });
    }
    times.stage("annotate", || {
        for (lod, level) in lods.iter_mut().zip(LodLevel::ALL) {
            annotate(lod, &coat, &anatomy, level);
        }
    });
    // Centres of rotation on the final vertices and weights (coat meshes copy them). Levels
    // without `deformation` keep neither (empty = linear skinning, still muscles).
    times.stage("centres_of_rotation", || {
        for (lod, _) in lods.iter_mut().zip(LodLevel::ALL).filter(|(_, l)| settings.get(*l).deformation) {
            lod.cor = centres_of_rotation(lod);
        }
    });
    // Muscle flex on the final skin (before the coat copies it onto shells and cards).
    times.stage("muscle_flex", || {
        for (lod, level) in lods.iter_mut().zip(LodLevel::ALL).filter(|(_, l)| settings.get(*l).deformation) {
            lod.muscle = muscle_flex(lod, &anatomy, level);
        }
    });
    // Soft tissue: regions from the stamps, each skin vertex's share of them (before the coat
    // copies it onto shells and cards).
    #[cfg(feature = "soft-tissue")]
    let soft = times.stage("soft_tissue", || {
        let soft = crate::soft::SoftTissue::of(&anatomy);
        for (lod, level) in lods.iter_mut().zip(LodLevel::ALL).filter(|(_, l)| settings.get(*l).deformation) {
            lod.soft = crate::soft::soft_weights(lod, &anatomy, &soft, level);
        }
        soft
    });
    // Cloth takes the flex and jiggle of the skin under it, then the skin it hides goes (after
    // the stages above, before the coat copies the skin onto shells and cards).
    if cloth.is_some() {
        times.stage("cover_skin", || {
            let design = GarmentCover::new(&anatomy, &outfit);
            for (lod, level) in lods.iter_mut().zip(LodLevel::ALL) {
                cover_skin(lod, settings.get(level).mesh.voxel_size, &design);
            }
        });
    }
    let motion = times.stage("bake_motion", || bake_motion(&anatomy, &Gait::for_legs(anatomy.legs.len())));
    let coat_meshes = times.stage("coat_meshes", || LodLevel::ALL.map(|level| build_coat_meshes(&coat, &anatomy, level.pick(&lods), level)));
    let contacts = times.stage("contacts", || ContactSet::new(&anatomy));
    Ok(GenOutput {
        anatomy,
        lods,
        motion,
        coat,
        coat_meshes,
        outfit,
        contacts,
        #[cfg(feature = "soft-tissue")]
        soft,
    })
}

pub(super) fn request_on_add(add: On<Add<Mob>>, mut commands: Commands) {
    commands.entity(add.entity).insert(GenRequest);
}

/// The key of a requested mob that is waiting for a free generation slot (see
/// `start_generation`).
#[derive(Component)]
pub(super) struct WaitingKey(BodyKey);

/// A new request may come with a changed body, coat or outfit: forget the waiting key.
pub(super) fn forget_waiting_key(ev: On<Insert<GenRequest>>, mut commands: Commands, waiting: Query<(), With<WaitingKey>>) {
    if waiting.contains(ev.entity) {
        commands.entity(ev.entity).try_remove::<WaitingKey>();
    }
}

/// Changing what a mob wears regenerates its body (the outfit is baked in).
pub(super) fn request_on_outfit(ev: On<Insert<MobOutfit>>, mut commands: Commands, mobs: Query<(), With<Mob>>) {
    if mobs.contains(ev.entity) {
        commands.entity(ev.entity).try_insert(GenRequest);
    }
}

pub(super) fn request_on_undress(ev: On<Remove<MobOutfit>>, mut commands: Commands, mobs: Query<(), With<Mob>>) {
    if mobs.contains(ev.entity) {
        commands.entity(ev.entity).try_insert(GenRequest);
    }
}

/// For each requested mob whose body asset is loaded: resolve its live body, then either reuse a
/// cached/in-flight body or start a task (within budget).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn start_generation(
    mut commands: Commands,
    mut cache: ResMut<GenCache>,
    budget: Res<GenBudget>,
    plans: Res<Assets<BodyPlan>>,
    settings: Res<SurfaceSettings>,
    lod_rules: Res<PartLodRules>,
    server: Res<AssetServer>,
    meshes: Res<Assets<Mesh>>,
    mut part_assets: ResMut<PartAssetCache>,
    coats: Res<Assets<Coat>>,
    outfits: Res<Assets<Outfit>>,
    mobs: Query<
        (Entity, &Mob, &Form, &MobSeed, Option<&LiveBody>, Option<&MobCoat>, Option<&MobOutfit>, Option<&WaitingKey>),
        With<GenRequest>,
    >,
) {
    for (entity, mob, form, seed, live, mob_coat, mob_outfit, waiting) in &mobs {
        // Waiting on the budget with its key known: only a slot opening (or its key starting for
        // another mob) changes anything, so skip re-resolving and re-hashing the whole body.
        if let Some(&WaitingKey(key)) = waiting {
            if cache.known(&key) {
                commands.entity(entity).remove::<(GenRequest, WaitingKey)>().insert(GenPending(key));
                continue;
            }
            if cache.in_flight.len() >= budget.max_in_flight {
                continue;
            }
        }
        let Some(outfit) = loaded_or_default(mob_outfit.map(|o| &o.0), &outfits) else { continue };
        let Some(coat) = loaded_or_default(mob_coat.map(|c| &c.0), &coats) else { continue };
        let live = if let Some(l) = live { l.0.clone() } else {
            let Some(plan) = plans.get(&mob.body) else { continue };
            match apply_form(plan, form) {
                Ok(l) => {
                    commands.entity(entity).insert(LiveBody(l.clone()));
                    l
                }
                Err(e) => {
                    error!("mob {entity}: {e}");
                    let error = e.to_string();
                    commands.entity(entity).remove::<GenRequest>().trigger(move |entity| BodyFailed { entity, error });
                    continue;
                }
            }
        };
        let mut paths = referenced_assets(&live);
        paths.extend(outfit.referenced_meshes());
        let Some(assets) = part_assets.resolve(&paths, &server, &meshes) else { continue };
        let key = body_key(&live, &settings, seed.0, &assets, &coat, &outfit);
        if !cache.known(&key) {
            if cache.in_flight.len() >= budget.max_in_flight {
                commands.entity(entity).insert(WaitingKey(key));
                continue;
            }
            let (settings, rules, seed) = (settings.clone(), *lod_rules, seed.0);
            let input = GenInput { live, rules, assets, settings, seed, coat, outfit };
            // Meshes are built in the task too: for a furry body that is several MB of vertex data.
            let task = AsyncComputeTaskPool::get().spawn(async move {
                generate_body(input, &mut StageTimes::default()).map(|out| {
                    let meshes = BodyMeshes::new(&out);
                    (out, meshes)
                })
            });
            cache.in_flight.insert(key, task);
        }
        commands.entity(entity).remove::<(GenRequest, WaitingKey)>().insert(GenPending(key));
    }
}

/// Polls in-flight tasks and turns finished outputs into [`GeneratedBody`] assets.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_generation(
    mut cache: ResMut<GenCache>,
    budget: Res<GenBudget>,
    mut bodies: ResMut<Assets<GeneratedBody>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut materials: ResMut<Assets<MobMaterial>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    crowd_skin: Res<CrowdSkin>,
) {
    let mut done = Vec::new();
    for (key, task) in &mut cache.in_flight {
        if done.len() >= budget.max_completions_per_frame {
            break;
        }
        if let Some(result) = check_ready(task) {
            done.push((*key, result));
        }
    }
    for (key, result) in done {
        cache.in_flight.remove(&key);
        let (out, ready) = match result {
            Ok((o, m)) => (o, m),
            Err(e) => {
                error!("generation failed: {e}");
                if cache.failed.len() >= FAILED_KEPT {
                    cache.failed.clear();
                }
                cache.failed.insert(key, e);
                continue;
            }
        };
        let skeleton = &out.anatomy.skeleton;
        // Hero soft joints come after the bones and bind like their bones.
        #[allow(unused_mut)]
        let mut full_bind = skeleton.inverse_bind.clone();
        #[cfg(feature = "soft-tissue")]
        full_bind.extend(out.soft.hero.iter().map(|r| skeleton.inverse_bind.get(r.bone.index()).copied().unwrap_or(Mat4::IDENTITY)));
        let full = bindposes.add(SkinnedMeshInverseBindposes::from(full_bind));
        let radius = LodLevel::Far.pick(&out.lods).positions.iter().map(|p| p.length()).fold(0.1, f32::max);
        let lods = ready.lods.map(|split| LodMeshes(split.into_iter().map(|(slot, mesh)| (slot, meshes.add(mesh))).collect()));
        let coat_meshes = ready.coats.map(|[shells, feathers, hair]| CoatMeshHandles {
            shells: shells.map(|m| meshes.add(m)),
            feathers: feathers.map(|m| meshes.add(m)),
            hair: hair.map(|m| meshes.add(m)),
        });
        let contacts = buffers.add(ShaderBuffer::from(out.contacts.gpu()));
        let body_materials = body_materials(&out.coat, &out.outfit, &out.anatomy, [&crowd_skin.bones, &contacts], &mut materials);
        let handle = bodies.add(GeneratedBody {
            key,
            anatomy: Arc::new(out.anatomy),
            lods,
            full_bindposes: full,
            radius,
            motion: Arc::new(out.motion),
            coat_meshes,
            materials: body_materials,
            #[cfg(feature = "soft-tissue")]
            soft: Arc::new(out.soft),
        });
        cache.bodies.insert(key, handle);
    }
}

/// Splits the cores not reserved for the main and render threads between the bodies generating
/// now (each parallel stage reads the cap as it starts, so a lone body gets them all). Only as
/// many bodies run at once as the async compute pool has threads; the rest of `in_flight` waits.
pub(super) fn apply_worker_budget(budget: Res<GenBudget>, cache: Res<GenCache>, mut last: Local<usize>) {
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let running = cache.in_flight.len().min(AsyncComputeTaskPool::get().thread_num()).max(1);
    let threads = (cores.saturating_sub(budget.reserved_threads) / running).max(1);
    if threads != *last {
        *last = threads;
        crate::surface::set_worker_threads(threads);
    }
}

/// A generated body's Bevy meshes, built off the main thread.
pub(super) struct BodyMeshes {
    /// Per level, one mesh per material slot.
    lods: [Vec<(MaterialSlot, Mesh)>; 3],
    /// Per level, fur shells, feather cards and hair cards.
    coats: [[Option<Mesh>; 3]; 3],
}

impl BodyMeshes {
    /// Hero meshes are Bevy-skinned; coarser levels are crowd-skinned.
    fn new(out: &GenOutput) -> Self {
        let crowd = |level: LodLevel| level != LodLevel::Hero;
        let lods = LodLevel::ALL.map(|level| {
            let data = level.pick(&out.lods);
            if crowd(level) { data.to_crowd_meshes() } else { data.to_bevy_meshes() }
        });
        let coats = LodLevel::ALL.map(|level| {
            let m = level.pick(&out.coat_meshes);
            [&m.shells, &m.feathers, &m.hair].map(|d| d.as_ref().map(|d| if crowd(level) { d.to_crowd_mesh() } else { d.to_bevy_mesh() }))
        });
        Self { lods, coats }
    }
}
