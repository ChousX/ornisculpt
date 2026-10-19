use std::collections::HashMap;
use std::sync::Arc;

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use xxhash_rust::xxh3::xxh3_128;

use super::{LiveBody, Mob, MobSeed};
use crate::anatomy::{
    Anatomy, BodyPlan, Form, LodLevel, PartAssets, PartLodRules, TriMesh, apply_form, build_anatomy, referenced_assets,
};
use crate::coat::{Coat, CoatExt, CoatLayer, CoatMeshes, CoatUniform, MobCoat, MobMaterial, annotate, build_coat_meshes};
use crate::motion::{BakedMotion, Gait, bake_motion};
use crate::num::Clamp;
use super::CrowdSkin;
use bevy::pbr::ExtendedMaterial;
use bevy::render::storage::ShaderBuffer;
use crate::clothing::{MobOutfit, Outfit, fit_outfit, garment_lods};
use crate::surface::{AdaptiveContour, BodyMeshData, centres_of_rotation, MaterialSlot, ProceduralHardParts, SurfaceSettings, merge, mesh_all_lods, muscle_flex};

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
    for ev in plan_events.read() {
        if let AssetEvent::Modified { id } = ev {
            for (entity, mob, _) in &mobs {
                if mob.body.id() == *id {
                    commands.entity(entity).remove::<LiveBody>().insert(GenRequest);
                }
            }
        }
    }
    for ev in coat_events.read() {
        if let AssetEvent::Modified { id } = ev {
            for (entity, coat) in &coated {
                if coat.0.id() == *id {
                    commands.entity(entity).insert(GenRequest);
                }
            }
        }
    }
    for ev in outfit_events.read() {
        if let AssetEvent::Modified { id } = ev {
            for (entity, outfit) in &dressed {
                if outfit.0.id() == *id {
                    commands.entity(entity).insert(GenRequest);
                }
            }
        }
    }
    let mut stale = Vec::new();
    for ev in mesh_events.read() {
        if let AssetEvent::Modified { id } = ev {
            stale.extend(parts.handles.iter().filter(|(_, h)| h.id() == *id).map(|(p, _)| p.clone()));
        }
    }
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
    pub crowd_bindposes: Handle<SkinnedMeshInverseBindposes>,
    /// Bounding radius around the mob origin, for tiering.
    pub radius: f32,
    /// Baked crowd-tier motion for the default gait of this body's leg count.
    pub motion: Arc<BakedMotion>,
    /// Fur shells / feather cards per LOD (indexed like `lods`).
    pub coat_meshes: [CoatMeshHandles; 3],
    /// Materials for this body + coat: `[hero, crowd]`.
    pub materials: [SlotMaterials; 2],
}

/// Coat geometry meshes for one LOD level.
#[derive(Clone, Debug, Default)]
pub struct CoatMeshHandles {
    pub shells: Option<Handle<Mesh>>,
    pub feathers: Option<Handle<Mesh>>,
}

/// One material per surface kind, for one skinning path (hero or crowd).
#[derive(Clone, Debug)]
pub struct SlotMaterials {
    pub skin: Handle<MobMaterial>,
    pub hard: Handle<MobMaterial>,
    pub eye: Handle<MobMaterial>,
    pub shell: Handle<MobMaterial>,
    pub feather: Handle<MobMaterial>,
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

/// Builds the ten materials of a body + coat.
fn body_materials(
    coat: &Coat,
    outfit: &Outfit,
    anatomy: &Anatomy,
    bones: &Handle<ShaderBuffer>,
    materials: &mut Assets<MobMaterial>,
) -> [SlotMaterials; 2] {
    let skin_uniform = CoatUniform { cloth: outfit.palette(), ..coat.uniform(anatomy) };
    let hard = Coat::plain([0.85, 0.8, 0.68], 0.4);
    // Iris (base), white (secondary), pupil (accent); eyes without white show only the iris.
    let eye = Coat { secondary: [0.86, 0.84, 0.8], accent: [0.01, 0.01, 0.01], ..Coat::plain([0.13, 0.075, 0.04], 0.1) };
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
                extension: CoatExt { bones: bones.clone(), coat: uniform, crowd, layer },
            })
        };
        SlotMaterials {
            skin: mat(skin_uniform, coat.roughness, CoatLayer::Skin),
            hard: mat(hard.uniform(anatomy), hard.roughness, CoatLayer::Hard),
            eye: mat(eye.uniform(anatomy), eye.roughness, CoatLayer::Hard),
            shell: mat(skin_uniform, 0.9, CoatLayer::Shell),
            feather: mat(skin_uniform, 0.6, CoatLayer::Feather),
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

/// Finished bodies plus in-flight work, deduplicated by key.
#[derive(Resource, Default)]
pub struct GenCache {
    pub bodies: HashMap<BodyKey, Handle<GeneratedBody>>,
    in_flight: HashMap<BodyKey, Task<Result<GenOutput, String>>>,
}

impl GenCache {
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

/// Generation throughput limits.
#[derive(Resource, Clone, Debug)]
pub struct GenBudget {
    pub max_in_flight: usize,
    pub max_completions_per_frame: usize,
}

impl Default for GenBudget {
    fn default() -> Self {
        Self { max_in_flight: 4, max_completions_per_frame: 2 }
    }
}

pub(super) struct GenOutput {
    anatomy: Anatomy,
    lods: [BodyMeshData; 3],
    motion: BakedMotion,
    coat: Coat,
    coat_meshes: [CoatMeshes; 3],
    outfit: Outfit,
}

pub(super) fn request_on_add(add: On<Add<Mob>>, mut commands: Commands) {
    commands.entity(add.entity).insert(GenRequest);
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
    mobs: Query<(Entity, &Mob, &Form, &MobSeed, Option<&LiveBody>, Option<&MobCoat>, Option<&MobOutfit>), With<GenRequest>>,
) {
    for (entity, mob, form, seed, live, mob_coat, mob_outfit) in &mobs {
        let outfit = match mob_outfit {
            Some(o) => match outfits.get(&o.0) {
                Some(o) => o.clone(),
                None => continue,
            },
            None => Outfit::default(),
        };
        let coat = match mob_coat {
            Some(c) => match coats.get(&c.0) {
                Some(c) => c.clone(),
                None => continue,
            },
            None => Coat::default(),
        };
        let live = if let Some(l) = live { l.0.clone() } else {
            let Some(plan) = plans.get(&mob.body) else { continue };
            match apply_form(plan, form) {
                Ok(l) => {
                    commands.entity(entity).insert(LiveBody(l.clone()));
                    l
                }
                Err(e) => {
                    error!("mob {entity}: {e}");
                    commands.entity(entity).remove::<GenRequest>();
                    continue;
                }
            }
        };
        let mut paths = referenced_assets(&live);
        paths.extend(outfit.referenced_meshes());
        let Some(assets) = part_assets.resolve(&paths, &server, &meshes) else { continue };
        let key = body_key(&live, &settings, seed.0, &assets, &coat, &outfit);
        if !cache.bodies.contains_key(&key) && !cache.in_flight.contains_key(&key) {
            if cache.in_flight.len() >= budget.max_in_flight {
                continue;
            }
            let (settings, rules, seed) = (settings.clone(), *lod_rules, seed.0);
            let task = AsyncComputeTaskPool::get().spawn(async move {
                let anatomy = build_anatomy(&live, &rules, &assets).map_err(|e| e.to_string())?;
                let (anatomy, cloth) = if outfit.garments.is_empty() { (anatomy, None) } else { fit_outfit(anatomy, &outfit, &assets) };
                let mut lods = mesh_all_lods(&AdaptiveContour, &ProceduralHardParts, &anatomy, &settings, seed)
                    .map_err(|e| e.to_string())?;
                if let Some(cloth) = &cloth {
                    for (lod, garments) in lods.iter_mut().zip(garment_lods(cloth, &anatomy, &settings)) {
                        merge(lod, &garments);
                    }
                }
                for (lod, level) in lods.iter_mut().zip(LodLevel::ALL) {
                    annotate(lod, &coat, &anatomy, level);
                }
                // Centres of rotation on the final vertices and weights (coat meshes copy them).
                for lod in &mut lods {
                    lod.cor = centres_of_rotation(lod);
                }
                // Muscle flex on the final skin (before the coat copies it onto shells and cards).
                for (lod, level) in lods.iter_mut().zip(LodLevel::ALL) {
                    lod.muscle = muscle_flex(lod, &anatomy, level);
                }
                let motion = bake_motion(&anatomy, &Gait::for_legs(anatomy.legs.len()));
                let coat_meshes = LodLevel::ALL.map(|level| build_coat_meshes(&coat, &anatomy, level.pick(&lods), level));
                Ok(GenOutput { anatomy, lods, motion, coat, coat_meshes, outfit })
            });
            cache.in_flight.insert(key, task);
        }
        commands.entity(entity).remove::<GenRequest>().insert(GenPending(key));
    }
}

/// Polls in-flight tasks and turns finished outputs into [`GeneratedBody`] assets.
pub(super) fn finish_generation(
    mut cache: ResMut<GenCache>,
    budget: Res<GenBudget>,
    mut bodies: ResMut<Assets<GeneratedBody>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    mut materials: ResMut<Assets<MobMaterial>>,
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
        let out = match result {
            Ok(o) => o,
            Err(e) => {
                error!("generation failed: {e}");
                continue;
            }
        };
        let skeleton = &out.anatomy.skeleton;
        let full = bindposes.add(SkinnedMeshInverseBindposes::from(skeleton.inverse_bind.clone()));
        let crowd = bindposes.add(SkinnedMeshInverseBindposes::from(
            skeleton.crowd.bones.iter().map(|b| skeleton.inverse_bind.get(b.index()).copied().unwrap_or(Mat4::IDENTITY)).collect::<Vec<_>>(),
        ));
        let radius = LodLevel::Far.pick(&out.lods).positions.iter().map(|p| p.length()).fold(0.1, f32::max);
        // Hero meshes are Bevy-skinned; coarser levels are crowd-skinned.
        let mut level = 0;
        let lods = out.lods.each_ref().map(|data| {
            let split = if level == 0 { data.to_bevy_meshes() } else { data.to_crowd_meshes() };
            level += 1;
            LodMeshes(split.into_iter().map(|(slot, mesh)| (slot, meshes.add(mesh))).collect())
        });
        let mut level = 0;
        let coat_meshes = out.coat_meshes.each_ref().map(|m| {
            let crowd = level != 0;
            level += 1;
            let mut add = |d: &Option<BodyMeshData>| {
                d.as_ref().map(|d| meshes.add(if crowd { d.to_crowd_mesh() } else { d.to_bevy_mesh() }))
            };
            CoatMeshHandles { shells: add(&m.shells), feathers: add(&m.feathers) }
        });
        let body_materials = body_materials(&out.coat, &out.outfit, &out.anatomy, &crowd_skin.bones, &mut materials);
        let handle = bodies.add(GeneratedBody {
            key,
            anatomy: Arc::new(out.anatomy),
            lods,
            full_bindposes: full,
            crowd_bindposes: crowd,
            radius,
            motion: Arc::new(out.motion),
            coat_meshes,
            materials: body_materials,
        });
        cache.bodies.insert(key, handle);
    }
}

/// Resolves a pending mob's body once its key is ready (used by `attach_body`).
pub(super) fn ready_body(cache: &GenCache, pending: &GenPending) -> Option<Handle<GeneratedBody>> {
    cache.bodies.get(&pending.0).cloned()
}

/// Failed keys are neither cached nor in flight; mobs waiting on them give up.
pub(super) fn is_lost(cache: &GenCache, pending: &GenPending) -> bool {
    !cache.bodies.contains_key(&pending.0) && !cache.in_flight.contains_key(&pending.0)
}
