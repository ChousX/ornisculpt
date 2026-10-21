//! Drawing a [`PreviewField`]: its hull (the cells that may hold surface) ray-marches the field
//! from where each ray enters it (`sdf_preview.wesl`), through distance bricks that a compute pass
//! bakes from the field into a 3D texture atlas after every change (`sdf_bake.wesl`). Opt in with
//! [`SdfPreviewPlugin`]; the editor uses it to show edits on the next frame while the real mesh is
//! built.

use std::collections::HashMap;

use bevy::asset::{RenderAssetUsages, load_embedded_asset};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::ecs::system::SystemParam;
use bevy::image::ImageSampler;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MATERIAL_BIND_GROUP_INDEX, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline};
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::sync_component::SyncComponent;
use bevy::render::RenderApp;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{storage_buffer_read_only_sized, texture_storage_3d};
use bevy::render::render_resource::{
    AsBindGroup, BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedComputePipelineId,
    ComputePassDescriptor, ComputePipelineDescriptor, Extent3d, PipelineCache, RenderPipelineDescriptor, ShaderStages, ShaderType,
    SpecializedMeshPipelineError, StorageTextureAccess, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::storage::{GpuShaderBuffer, ShaderBuffer};
use bevy::render::sync_world::MainEntity;
use bevy::render::texture::GpuImage;
use bevy::shader::{ShaderDefVal, ShaderRef};

use super::{BRICK_VOXELS, PreviewField, PreviewSheets};

pub const SDF_PREVIEW_SHADER: &str = "embedded://ornisculpt/surface/sdf_preview.wesl";

/// The bake's workgroup size (`sdf_bake.wesl`).
const BAKE_WORKGROUP: u32 = 64;
/// Most workgroups along one dispatch axis (the bake wraps onto a second).
const MAX_GROUPS_X: u32 = 32_768;
/// Bricks per row and per layer of the atlas, along x and y (`BRICK_ROW` in `sdf_field.wesl`);
/// it grows a layer of `BRICK_ROW²` bricks at a time.
const BRICK_ROW: u32 = 16;
/// Lattice points along a brick's edge.
const BRICK_SAMPLES: u32 = BRICK_VOXELS + 1;
/// Atlas format: half floats, filterable, and writable from a compute pass everywhere.
const ATLAS_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

/// A body drawn straight from its field.
pub type SdfPreviewMaterial = ExtendedMaterial<StandardMaterial, SdfPreviewExt>;

/// Registers [`SdfPreviewMaterial`] and the brick bake, and keeps each [`SdfPreview`]'s
/// transforms current.
pub struct SdfPreviewPlugin;

impl Plugin for SdfPreviewPlugin {
    fn build(&self, app: &mut App) {
        bevy::shader::load_shader_library!(app, "sdf_field.wesl");
        bevy::asset::embedded_asset!(app, "sdf_preview.wesl");
        bevy::asset::embedded_asset!(app, "sdf_bake.wesl");
        app.add_plugins((MaterialPlugin::<SdfPreviewMaterial>::default(), ExtractComponentPlugin::<SdfPreview>::default()))
            .add_systems(PostUpdate, sync_transforms.after(TransformSystems::Propagate));
    }

    fn finish(&self, app: &mut App) {
        use bevy::render::renderer::{RenderGraph, RenderGraphSystems};
        use bevy::render::{GpuResourceAppExt, Render, RenderSystems};
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_gpu_resource::<BakePipeline>()
            .init_resource::<Bakes>()
            .add_systems(Render, prepare_bakes.in_set(RenderSystems::PrepareBindGroups))
            .add_systems(RenderGraph, dispatch_bakes.in_set(RenderGraphSystems::Begin));
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct SdfPreviewExt {
    /// The packed field ([`PreviewField::words`]).
    #[storage(100, read_only)]
    pub field: Handle<ShaderBuffer>,
    #[uniform(101)]
    pub params: SdfPreviewUniform,
    /// The baked distance bricks.
    #[texture(102, dimension = "3d")]
    #[sampler(103)]
    pub atlas: Handle<Image>,
}

#[derive(ShaderType, Reflect, Clone, Copy, Debug, PartialEq)]
pub struct SdfPreviewUniform {
    pub model_from_world: Mat4,
    pub world_from_model: Mat4,
    /// Tint of highlighted primitives: linear rgb, strength.
    pub highlight: Vec4,
}

impl Default for SdfPreviewUniform {
    fn default() -> Self {
        Self { model_from_world: Mat4::IDENTITY, world_from_model: Mat4::IDENTITY, highlight: Vec4::new(0.1, 0.75, 0.9, 0.45) }
    }
}

impl MaterialExtension for SdfPreviewExt {
    fn fragment_shader() -> ShaderRef {
        SDF_PREVIEW_SHADER.into()
    }

    // A stand-in for a moment: no depth prepass or shadow casting (both would need the ray march).
    fn enable_prepass() -> bool {
        false
    }

    fn enable_shadows() -> bool {
        false
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // `sdf_field.wesl` finds the packed words in the material's bind group.
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.push(ShaderDefVal::UInt("SDF_GROUP".into(), u32::try_from(MATERIAL_BIND_GROUP_INDEX).unwrap_or(3)));
        }
        Ok(())
    }
}

/// A ray-marched body, drawn with the field's hull: a child of the mob (its model space is the
/// mob's), or at the origin for a mob there. Fill it with [`SdfPreview::show`].
#[derive(Component, Clone, Debug)]
#[require(Visibility, Transform, NotShadowCaster, NoFrustumCulling)]
pub struct SdfPreview {
    pub material: Handle<SdfPreviewMaterial>,
    pub buffer: Handle<ShaderBuffer>,
    pub hull: Handle<Mesh>,
    pub atlas: Handle<Image>,
    /// The wing sheets, drawn as triangles by a child entity.
    pub sheets: Handle<Mesh>,
    /// Clay colour of the body (the sheets' vertex colours start from it).
    pub color: Color,
    /// Bricks the field in `buffer` has (the atlas may hold more).
    pub bricks: u32,
    /// Counts [`show`](Self::show)s: the render world bakes the bricks when it changes.
    pub generation: u64,
}

impl SyncComponent<RenderApp> for SdfPreview {
    type Target = Self;
}

impl ExtractComponent<RenderApp> for SdfPreview {
    type QueryData = &'static Self;
    type QueryFilter = ();
    type Out = Self;

    fn extract_component(item: &Self) -> Option<Self> {
        Some(item.clone())
    }
}

/// The assets a preview is made of.
#[derive(SystemParam)]
pub struct SdfPreviewAssets<'w> {
    pub meshes: ResMut<'w, Assets<Mesh>>,
    pub materials: ResMut<'w, Assets<SdfPreviewMaterial>>,
    pub buffers: ResMut<'w, Assets<ShaderBuffer>>,
    pub images: ResMut<'w, Assets<Image>>,
    pub standard: ResMut<'w, Assets<StandardMaterial>>,
}

/// A brick atlas `layers` layers of `BRICK_ROW²` bricks deep.
fn atlas(layers: u32) -> Image {
    let side = BRICK_ROW * BRICK_SAMPLES;
    let extent = Extent3d { width: side, height: side, depth_or_array_layers: layers.max(1) * BRICK_SAMPLES };
    let mut image = Image::new_uninit(extent, TextureDimension::D3, ATLAS_FORMAT, RenderAssetUsages::RENDER_WORLD);
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING;
    image.sampler = ImageSampler::linear();
    image
}

/// Bricks an atlas this deep holds.
const fn atlas_bricks(image: &Image) -> u32 {
    image.texture_descriptor.size.depth_or_array_layers / BRICK_SAMPLES * BRICK_ROW * BRICK_ROW
}

impl SdfPreview {
    /// The preview's components (spawn them as a child of the mob), clay coloured, with a child
    /// drawing the wing sheets.
    pub fn bundle(assets: &mut SdfPreviewAssets, color: Color) -> impl Bundle {
        let buffer = assets.buffers.add(ShaderBuffer::new(vec![[0.0f32; 4]; 64], RenderAssetUsages::RENDER_WORLD));
        let atlas = assets.images.add(atlas(1));
        let material = assets.materials.add(SdfPreviewMaterial {
            base: StandardMaterial { base_color: color, perceptual_roughness: 0.75, ..default() },
            extension: SdfPreviewExt { field: buffer.clone(), params: SdfPreviewUniform::default(), atlas: atlas.clone() },
        });
        let hull = assets.meshes.add(Cuboid::from_length(1e-3));
        let sheets = assets.meshes.add(sheets_mesh(&PreviewSheets::default(), color, Vec4::ZERO));
        // Colours come from the vertices; both faces lit (a sheet is one layer of triangles).
        let sheet_material =
            assets.standard.add(StandardMaterial { perceptual_roughness: 0.6, double_sided: true, cull_mode: None, ..default() });
        (
            Self { material: material.clone(), buffer, hull: hull.clone(), atlas, sheets: sheets.clone(), color, bricks: 0, generation: 0 },
            Mesh3d(hull),
            MeshMaterial3d(material),
            Visibility::Hidden,
            children![(Mesh3d(sheets), MeshMaterial3d(sheet_material))],
        )
    }

    /// Uploads `field` and its hull; the render world bakes the bricks before the next draw. The
    /// buffer keeps a power-of-two size and the atlas grows by layers, so most edits overwrite
    /// both in place rather than re-creating them (and rebinding the material).
    pub fn show(&mut self, field: &PreviewField, assets: &mut SdfPreviewAssets) {
        if let Some(mut buffer) = assets.buffers.get_mut(&self.buffer) {
            let mut words = field.words.clone();
            words.resize(field.words.len().next_power_of_two().max(64), [0.0; 4]);
            *buffer = ShaderBuffer::new(words, RenderAssetUsages::RENDER_WORLD);
        }
        if let Some(mut mesh) = assets.meshes.get_mut(&self.hull) {
            *mesh = hull_mesh(field);
        }
        let highlight = assets.materials.get(&self.material).map_or(Vec4::ZERO, |m| m.extension.params.highlight);
        if let Some(mut mesh) = assets.meshes.get_mut(&self.sheets) {
            *mesh = sheets_mesh(&field.sheets, self.color, highlight);
        }
        if assets.images.get(&self.atlas).is_some_and(|image| atlas_bricks(image) < field.bricks) {
            if let Some(mut image) = assets.images.get_mut(&self.atlas) {
                *image = atlas(field.bricks.div_ceil(BRICK_ROW * BRICK_ROW));
            }
            // The material binds the texture itself, not the asset: rebind it.
            if let Some(material) = assets.materials.get_mut(&self.material) {
                material.into_inner();
            }
        }
        self.bricks = field.bricks;
        self.generation += 1;
    }
}

/// The hull's quads as a mesh.
fn hull_mesh(field: &PreviewField) -> Mesh {
    let positions: Vec<[f32; 3]> = field.hull.iter().flat_map(|(q, _)| q.map(|v| v.to_array())).collect();
    let normals: Vec<[f32; 3]> = field.hull.iter().flat_map(|(_, n)| [n.to_array(); 4]).collect();
    let indices: Vec<u32> = (0..u32::try_from(field.hull.len()).unwrap_or(0)).flat_map(|q| [0, 1, 2, 0, 2, 3].map(|i| q * 4 + i)).collect();
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_indices(Indices::U32(indices))
}

/// The wing sheets as a mesh coloured per vertex: the clay, darker where the sheet is thin,
/// tinted like the body where highlighted (`highlight`: linear rgb, strength). Never empty (a
/// degenerate triangle stands in for no sheets).
fn sheets_mesh(sheets: &PreviewSheets, color: Color, highlight: Vec4) -> Mesh {
    let clay = color.to_linear().to_vec3();
    let colors: Vec<[f32; 4]> = sheets
        .shade
        .iter()
        .map(|[thickness, lit]| {
            let shaded = clay * thickness.clamp(0.0, 1.0).mul_add(0.3, 0.7);
            shaded.lerp(highlight.truncate(), highlight.w * lit).extend(1.0).to_array()
        })
        .collect();
    let (positions, normals, colors, indices) = if sheets.indices.is_empty() {
        (vec![[0.0; 3]; 3], vec![[0.0, 1.0, 0.0]; 3], vec![[0.0; 4]; 3], vec![0, 1, 2])
    } else {
        (
            sheets.positions.iter().map(Vec3::to_array).collect(),
            sheets.normals.iter().map(Vec3::to_array).collect(),
            colors,
            sheets.indices.clone(),
        )
    };
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD)
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
        .with_inserted_indices(Indices::U32(indices))
}

/// Keeps each preview's model ↔ world transforms (its parent's: the mob's) in its material.
fn sync_transforms(
    previews: Query<(&SdfPreview, Option<&ChildOf>)>,
    parents: Query<&GlobalTransform>,
    mut materials: ResMut<Assets<SdfPreviewMaterial>>,
) {
    for (preview, parent) in &previews {
        let world_from_model = parent.and_then(|p| parents.get(p.parent()).ok()).map_or(Mat4::IDENTITY, GlobalTransform::to_matrix);
        let Some(material) = materials.get(&preview.material) else { continue };
        if material.extension.params.world_from_model == world_from_model {
            continue;
        }
        if let Some(mut material) = materials.get_mut(&preview.material) {
            material.extension.params.world_from_model = world_from_model;
            material.extension.params.model_from_world = world_from_model.inverse();
        }
    }
}

/// The brick bake's compute pipeline.
#[derive(Resource)]
struct BakePipeline {
    layout: BindGroupLayoutDescriptor,
    id: CachedComputePipelineId,
}

impl FromWorld for BakePipeline {
    fn from_world(world: &mut World) -> Self {
        let layout = BindGroupLayoutDescriptor::new(
            "ornisculpt sdf bake",
            &BindGroupLayoutEntries::with_indices(
                ShaderStages::COMPUTE,
                (
                    (1, texture_storage_3d(ATLAS_FORMAT, StorageTextureAccess::WriteOnly)),
                    (100, storage_buffer_read_only_sized(false, None)),
                ),
            ),
        );
        let shader = load_embedded_asset!(world, "sdf_bake.wesl");
        let id = world.resource::<PipelineCache>().queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("ornisculpt sdf bake".into()),
            layout: vec![layout.clone()],
            immediate_size: 0,
            shader,
            shader_defs: vec![ShaderDefVal::UInt("SDF_GROUP".into(), 0)],
            entry_point: Some("main".into()),
            zero_initialize_workgroup_memory: false,
            constants: vec![],
        });
        Self { layout, id }
    }
}

/// Bakes to run this frame (with their brick counts), and the generation each preview was last
/// baked at.
#[derive(Resource, Default)]
struct Bakes {
    ready: Vec<(BindGroup, u32)>,
    baked: HashMap<MainEntity, u64>,
}

/// Bind groups for the previews whose field changed since their last bake (once their buffer,
/// atlas and the pipeline are ready on the GPU).
fn prepare_bakes(
    previews: Query<(&MainEntity, &SdfPreview)>,
    mut bakes: ResMut<Bakes>,
    pipeline: Res<BakePipeline>,
    pipeline_cache: Res<PipelineCache>,
    buffers: Res<RenderAssets<GpuShaderBuffer>>,
    images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
) {
    bakes.ready.clear();
    if pipeline_cache.get_compute_pipeline(pipeline.id).is_none() {
        return;
    }
    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    for (entity, preview) in &previews {
        if bakes.baked.get(entity) == Some(&preview.generation) || preview.bricks == 0 {
            continue;
        }
        let (Some(buffer), Some(image)) = (buffers.get(&preview.buffer), images.get(&preview.atlas)) else { continue };
        let layers = image.texture_descriptor.size.depth_or_array_layers / BRICK_SAMPLES;
        if layers * BRICK_ROW * BRICK_ROW < preview.bricks {
            // The grown atlas isn't on the GPU yet.
            continue;
        }
        let bind_group = device.create_bind_group(
            "ornisculpt sdf bake",
            &layout,
            &BindGroupEntries::with_indices(((1, &image.texture_view), (100, buffer.buffer.as_entire_binding()))),
        );
        bakes.ready.push((bind_group, preview.bricks));
        bakes.baked.insert(*entity, preview.generation);
    }
    let live: Vec<MainEntity> = previews.iter().map(|(e, _)| *e).collect();
    bakes.baked.retain(|e, _| live.contains(e));
}

/// Bakes the changed bricks, before any camera draws: one invocation per lattice point.
fn dispatch_bakes(bakes: Res<Bakes>, pipeline: Res<BakePipeline>, pipeline_cache: Res<PipelineCache>, mut render_context: RenderContext) {
    let Some(compute) = pipeline_cache.get_compute_pipeline(pipeline.id) else { return };
    if bakes.ready.is_empty() {
        return;
    }
    let mut pass = render_context.command_encoder().begin_compute_pass(&ComputePassDescriptor { label: Some("ornisculpt sdf bake"), timestamp_writes: None });
    pass.set_pipeline(compute);
    for (bind_group, bricks) in &bakes.ready {
        let groups = (bricks * BRICK_SAMPLES.pow(3)).div_ceil(BAKE_WORKGROUP);
        pass.set_bind_group(0, bind_group, &[]);
        pass.dispatch_workgroups(groups.min(MAX_GROUPS_X), groups.div_ceil(MAX_GROUPS_X), 1);
    }
}
