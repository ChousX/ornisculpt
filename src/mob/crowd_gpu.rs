//! Render-world half of crowd skinning: uploads changed clip/instance data and runs the compute
//! pass that blends baked frames into the crowd bone buffer, once per frame before any camera.

use std::sync::Arc;

use bevy::asset::load_embedded_asset;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{storage_buffer_read_only_sized, storage_buffer_sized};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, Buffer, BufferDescriptor,
    BufferUsages, CachedComputePipelineId, ComputePassDescriptor, ComputePipelineDescriptor, PipelineCache,
    ShaderStages,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::storage::{GpuShaderBuffer, ShaderBuffer};
use bevy::render::Extract;

use super::crowd::{CrowdInstance, CrowdUploads, SLOT_BYTES};
use crate::anatomy::CROWD_SLOT;
use crate::num::{Clamp, Idx, usize_to_u64};

const WORKGROUP: u32 = 64;

/// A GPU storage buffer with a CPU mirror: writes go to the mirror and are flushed as one dirty
/// range; growing re-creates the buffer from the mirror.
#[derive(Default)]
struct MirroredBuffer {
    buffer: Option<Buffer>,
    mirror: Vec<u8>,
    dirty: Option<(usize, usize)>,
}

impl MirroredBuffer {
    fn write(&mut self, offset: usize, bytes: &[u8]) {
        let end = offset + bytes.len();
        if self.mirror.len() < end {
            self.mirror.resize(end, 0);
        }
        if let Some(dst) = self.mirror.get_mut(offset..end) {
            dst.copy_from_slice(bytes);
        }
        self.dirty = Some(self.dirty.map_or((offset, end), |(a, b)| (a.min(offset), b.max(end))));
    }

    fn flush(&mut self, label: &'static str, device: &RenderDevice, queue: &RenderQueue) {
        let needed = usize_to_u64(self.mirror.len().max(256));
        if self.buffer.as_ref().is_none_or(|b| b.size() < needed) {
            let size = needed.next_power_of_two();
            let buffer = device.create_buffer(&BufferDescriptor {
                label: Some(label),
                size,
                usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !self.mirror.is_empty() {
                queue.write_buffer(&buffer, 0, &self.mirror);
            }
            self.buffer = Some(buffer);
            self.dirty = None;
        }
        if let (Some(buffer), Some((a, b))) = (&self.buffer, self.dirty.take())
            && let Some(bytes) = self.mirror.get(a..b)
        {
            queue.write_buffer(buffer, usize_to_u64(a), bytes);
        }
    }
}

/// Render-world crowd state.
#[derive(Resource, Default)]
pub(super) struct CrowdGpu {
    clips: MirroredBuffer,
    instances: MirroredBuffer,
    pending_clips: Vec<(u32, Arc<Vec<[f32; 16]>>)>,
    pending_instances: Vec<(u32, CrowdInstance)>,
    slots: u32,
    bones: Option<AssetId<ShaderBuffer>>,
    bind_group: Option<BindGroup>,
    dispatch_slots: u32,
}

/// The blend compute pipeline.
#[derive(Resource)]
pub(super) struct CrowdBlendPipeline {
    layout: BindGroupLayoutDescriptor,
    id: CachedComputePipelineId,
}

impl FromWorld for CrowdBlendPipeline {
    fn from_world(world: &mut World) -> Self {
        let layout = BindGroupLayoutDescriptor::new(
            "ornisculpt crowd blend",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::COMPUTE,
                (
                    storage_buffer_read_only_sized(false, None),
                    storage_buffer_read_only_sized(false, None),
                    storage_buffer_sized(false, None),
                ),
            ),
        );
        let shader = load_embedded_asset!(world, "crowd_blend.wesl");
        let id = world.resource::<PipelineCache>().queue_compute_pipeline(ComputePipelineDescriptor {
            label: Some("ornisculpt crowd blend".into()),
            layout: vec![layout.clone()],
            immediate_size: 0,
            shader,
            // Slot stride (`CROWD_SLOT`): wider with soft tissue.
            shader_defs: if cfg!(feature = "soft-tissue") { vec!["SOFT_TISSUE".into()] } else { vec![] },
            entry_point: Some("main".into()),
            zero_initialize_workgroup_memory: false,
            constants: vec![],
        });
        Self { layout, id }
    }
}

pub(super) fn extract_crowd(uploads: Extract<Res<CrowdUploads>>, mut gpu: ResMut<CrowdGpu>) {
    gpu.pending_clips.extend(uploads.clips.iter().cloned());
    gpu.pending_instances.extend_from_slice(&uploads.instances);
    gpu.slots = uploads.slots;
    gpu.bones = Some(uploads.bones);
}

pub(super) fn prepare_crowd_buffers(mut gpu: ResMut<CrowdGpu>, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let gpu = &mut *gpu;
    for (index, matrices) in gpu.pending_clips.drain(..) {
        gpu.clips.write(index.idx() * 64, bytemuck::cast_slice(&matrices));
    }
    for (slot, instance) in gpu.pending_instances.drain(..) {
        gpu.instances.write(slot.idx() * size_of::<CrowdInstance>(), bytemuck::bytes_of(&instance));
    }
    if gpu.clips.mirror.is_empty() {
        return;
    }
    gpu.clips.flush("ornisculpt crowd clips", &device, &queue);
    gpu.instances.flush("ornisculpt crowd instances", &device, &queue);
}

pub(super) fn prepare_crowd_bind_group(
    mut gpu: ResMut<CrowdGpu>,
    pipeline: Res<CrowdBlendPipeline>,
    pipeline_cache: Res<PipelineCache>,
    shader_buffers: Res<RenderAssets<GpuShaderBuffer>>,
    device: Res<RenderDevice>,
) {
    let gpu = &mut *gpu;
    gpu.bind_group = None;
    let (Some(clips), Some(instances)) = (&gpu.clips.buffer, &gpu.instances.buffer) else { return };
    let Some(bones) = gpu.bones.and_then(|id| shader_buffers.get(id)) else { return };
    // Never dispatch past the bone buffer (it may lag a frame behind a resize).
    gpu.dispatch_slots = gpu.slots.min((bones.buffer.size() / SLOT_BYTES).clamp_u32());
    let layout = pipeline_cache.get_bind_group_layout(&pipeline.layout);
    let bind_group = device.create_bind_group(
        "ornisculpt crowd blend",
        &layout,
        &BindGroupEntries::sequential((clips.as_entire_binding(), instances.as_entire_binding(), bones.buffer.as_entire_binding())),
    );
    gpu.bind_group = Some(bind_group);
}

/// Blends baked frames into the bone buffer. Runs once per frame, before any camera renders.
pub(super) fn dispatch_crowd_blend(
    gpu: Res<CrowdGpu>,
    pipeline: Res<CrowdBlendPipeline>,
    pipeline_cache: Res<PipelineCache>,
    mut render_context: RenderContext,
) {
    let (Some(bind_group), Some(compute)) = (&gpu.bind_group, pipeline_cache.get_compute_pipeline(pipeline.id)) else {
        return;
    };
    if gpu.dispatch_slots == 0 {
        return;
    }
    let invocations = gpu.dispatch_slots * CROWD_SLOT.clamp_u32();
    let mut pass = render_context.command_encoder().begin_compute_pass(&ComputePassDescriptor {
        label: Some("ornisculpt crowd blend"),
        timestamp_writes: None,
    });
    pass.set_pipeline(compute);
    pass.set_bind_group(0, bind_group, &[]);
    pass.dispatch_workgroups(invocations.div_ceil(WORKGROUP), 1, 1);
}
