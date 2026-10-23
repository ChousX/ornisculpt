//! Crowd rendering: near/far mobs have no joint entities.
//!
//! Main world: every generated body's baked clips are appended once to a clip atlas; every crowd
//! mob has a slot and a small [`CrowdInstance`] (which frames to blend, with which weights), which
//! is uploaded only when it changes. Render world ([`super::crowd_gpu`]): a compute pass blends the
//! atlas frames into a GPU-only bone buffer; crowd meshes use the coat material (`crate::coat`), whose vertex
//! shader skins from that buffer at the slot in the mesh's `MeshTag`. Mobs sharing a body share
//! mesh and material, so Bevy batches them into instanced draws.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::BufferUsages;
use bevy::shader::ShaderDefVal;
use bevy::render::storage::{ShaderBuffer, ShaderBufferData};
use bytemuck::{Pod, Zeroable};

use super::GeneratedBody;
use crate::anatomy::CROWD_SLOT;
use crate::motion::{BAKE_FRAMES, BakedMotion};
use crate::num::{Clamp, Saturate, ToF32, usize_to_u64};

/// Bytes per bone matrix (column-major `mat4x4<f32>`).
pub const BONE_BYTES: u64 = 64;
/// Bytes per crowd slot in the bone buffer.
pub const SLOT_BYTES: u64 = BONE_BYTES * usize_to_u64(CROWD_SLOT);

/// Shader defines for the crowd bone buffer's slot stride (`CROWD_SLOT`), wider with soft tissue.
pub fn crowd_slot_defs() -> Vec<ShaderDefVal> {
    if cfg!(feature = "soft-tissue") { vec!["SOFT_TISSUE".into()] } else { vec![] }
}

/// Which slot of the crowd bone buffer a mob uses (also its meshes' `MeshTag`).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrowdSlot(pub u32);

/// Per-slot blend instructions for the compute pass (matches `CrowdInstance` in WESL).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct CrowdInstance {
    /// Atlas matrix index of frame A, frame B and the idle pose (bone 0 of each).
    pub frame_a: u32,
    pub frame_b: u32,
    pub idle: u32,
    /// Crowd bones to write; 0 disables the slot.
    pub bones: u32,
    pub weight_a: f32,
    pub weight_b: f32,
    pub weight_idle: f32,
    /// Padding to 16 bytes.
    pub pad: u32,
}

/// The last instance uploaded for a mob (to skip unchanged slots).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CrowdUploaded(pub CrowdInstance);

/// Where a body's baked clips live in the atlas.
#[derive(Clone, Copy, Debug)]
pub struct ClipLayout {
    /// Atlas index of the idle pose; clip `m`, frame `f` is at `base + bones·(1 + m·F + f)`.
    pub base: u32,
    pub bones: u32,
    pub modes: u32,
}

impl ClipLayout {
    /// Blend instructions for gait `mode` at `phase`, blended toward idle by `idle_weight`.
    #[must_use]
    pub fn instance(&self, motion: &BakedMotion, mode: usize, phase: f32, idle_weight: f32) -> CrowdInstance {
        let frames = BAKE_FRAMES.clamp_u32();
        let frame = |m: u32, f: u32| self.base + self.bones * (1 + m * frames + f);
        let idle_only = CrowdInstance {
            frame_a: self.base,
            frame_b: self.base,
            idle: self.base,
            bones: self.bones,
            weight_idle: 1.0,
            ..default()
        };
        let Some(clip) = motion.clips.get(mode) else { return idle_only };
        let Some(mode) = u32::try_from(mode).ok().filter(|m| *m < self.modes) else { return idle_only };
        if idle_weight >= 1.0 {
            return idle_only;
        }
        let at = (phase - clip.start_phase).rem_euclid(1.0) * frames.to_f32();
        let (f0, t) = (at.floor().sat_u32() % frames, at.fract());
        let f1 = (f0 + 1) % frames;
        let moving = 1.0 - idle_weight;
        CrowdInstance {
            frame_a: frame(mode, f0),
            frame_b: frame(mode, f1),
            idle: self.base,
            bones: self.bones,
            weight_a: (1.0 - t) * moving,
            weight_b: t * moving,
            weight_idle: idle_weight,
            pad: 0,
        }
    }
}

/// Main-world crowd state: slot allocator, clip atlas and the GPU-only bone buffer.
#[derive(Resource)]
pub struct CrowdSkin {
    pub bones: Handle<ShaderBuffer>,
    bone_capacity: u32,
    free: Vec<u32>,
    next: u32,
    atlas: HashMap<AssetId<GeneratedBody>, ClipLayout>,
    atlas_len: u32,
}

impl FromWorld for CrowdSkin {
    fn from_world(world: &mut World) -> Self {
        let capacity = 64;
        let bones = world.resource_mut::<Assets<ShaderBuffer>>().add(bone_buffer(capacity));
        Self { bones, bone_capacity: capacity, free: Vec::new(), next: 0, atlas: default(), atlas_len: 0 }
    }
}

/// GPU-only storage for `slots` slots of skin matrices (written by the compute pass).
fn bone_buffer(slots: u32) -> ShaderBuffer {
    ShaderBuffer {
        data: ShaderBufferData::Uninitialized(u64::from(slots) * SLOT_BYTES),
        label: "ornisculpt crowd bones".into(),
        buffer_usage: BufferUsages::STORAGE,
        asset_usage: RenderAssetUsages::RENDER_WORLD,
        copy_on_resize: false,
    }
}

impl CrowdSkin {
    pub fn alloc(&mut self) -> u32 {
        self.free.pop().unwrap_or_else(|| {
            self.next += 1;
            self.next - 1
        })
    }

    pub fn release(&mut self, slot: u32) {
        self.free.push(slot);
    }

    #[must_use]
    pub fn layout(&self, body: AssetId<GeneratedBody>) -> Option<ClipLayout> {
        self.atlas.get(&body).copied()
    }
}

/// Uploads collected this frame; extracted to the render world, then cleared next frame.
#[derive(Resource, Clone, Default)]
pub struct CrowdUploads {
    /// `(slot, instance)` for every slot whose instance changed.
    pub instances: Vec<(u32, CrowdInstance)>,
    /// `(atlas index, matrices)` appended to the clip atlas.
    pub clips: Vec<(u32, Arc<Vec<[f32; 16]>>)>,
    /// Slots in use (the compute pass covers `0..slots`).
    pub slots: u32,
    /// The bone buffer the compute pass writes.
    pub bones: AssetId<ShaderBuffer>,
}

pub(super) fn clear_uploads(mut uploads: ResMut<CrowdUploads>) {
    uploads.instances.clear();
    uploads.clips.clear();
}

/// Appends newly generated bodies' baked clips to the atlas.
pub(super) fn register_clips(
    mut events: MessageReader<AssetEvent<GeneratedBody>>,
    bodies: Res<Assets<GeneratedBody>>,
    mut skin: ResMut<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
) {
    for ev in events.read() {
        let AssetEvent::Added { id } = ev else { continue };
        let Some(body) = bodies.get(*id) else { continue };
        let m = &body.motion;
        let bones = m.idle.len().clamp_u32();
        let mut data: Vec<[f32; 16]> = Vec::with_capacity(m.idle.len() * (1 + m.clips.len() * BAKE_FRAMES));
        data.extend_from_slice(&m.idle);
        for clip in &m.clips {
            for frame in &clip.frames {
                data.extend_from_slice(frame);
            }
        }
        let base = skin.atlas_len;
        skin.atlas_len += data.len().clamp_u32();
        skin.atlas.insert(*id, ClipLayout { base, bones, modes: m.clips.len().clamp_u32() });
        uploads.clips.push((base, Arc::new(data)));
    }
}

/// Grows the bone buffer to cover every allocated slot and publishes the slot count.
pub(super) fn sync_bone_capacity(
    mut skin: ResMut<CrowdSkin>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut uploads: ResMut<CrowdUploads>,
) {
    if skin.next > skin.bone_capacity {
        skin.bone_capacity = skin.next.next_power_of_two();
        if let Some(mut b) = buffers.get_mut(&skin.bones) {
            *b = bone_buffer(skin.bone_capacity);
        }
    }
    uploads.slots = skin.next;
    uploads.bones = skin.bones.id();
}

/// Frees a slot and disables it on the GPU.
pub(super) fn release_slot(
    ev: On<Remove<CrowdSlot>>,
    slots: Query<&CrowdSlot>,
    mut skin: ResMut<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
) {
    if let Ok(slot) = slots.get(ev.entity) {
        skin.release(slot.0);
        uploads.instances.push((slot.0, CrowdInstance::default()));
    }
}
