//! Crowd rendering: near/far mobs have no joint entities.
//!
//! Main world: every generated body's baked clips are appended once to a clip atlas (and, per
//! body and motion graph, the idles and actions the graph plays: `crate::motion`'s crowd module);
//! every crowd mob has a slot and a small [`CrowdInstance`] (the idle pose and up to
//! [`CROWD_LAYERS`] layers of frame pairs, each with a weight and a mask row), which is uploaded
//! only when it changes. Render world ([`super::crowd_gpu`]): a compute pass blends the atlas
//! frames into a GPU-only bone buffer; crowd meshes use the coat material (`crate::coat`), whose
//! vertex shader skins from that buffer at the slot in the mesh's `MeshTag`. Mobs sharing a body
//! share mesh and material, so Bevy batches them into instanced draws.

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
use crate::num::{Clamp, Idx, Saturate, ToF32, usize_to_u64};

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

/// Layers a [`CrowdInstance`] stacks on the idle pose.
pub const CROWD_LAYERS: usize = 3;
/// A layer's `mask` for "every bone" (no mask row).
pub const MASK_ALL: u32 = 0x7FFF_FFFF;
/// Set in a layer's `mask`: the layer adds its frame's difference from the idle pose instead of
/// replacing the pose under it.
pub const LAYER_ADDITIVE: u32 = 0x8000_0000;
/// Matrices a per-bone row (a mask, say) takes in the atlas: one float per crowd slot entry,
/// padded to whole matrices.
pub const ROW_MATRICES: usize = CROWD_SLOT.div_ceil(16);

/// One layer of a [`CrowdInstance`] (matches `CrowdLayer` in WESL): a pair of atlas frames mixed
/// by `t`, laid over the pose below by `weight` × the mask row's weight for the bone.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct CrowdLayer {
    /// Atlas matrix index of the frames' bone 0.
    pub frame_a: u32,
    pub frame_b: u32,
    /// Atlas index of the mask row (one weight per crowd slot entry), [`MASK_ALL`] for every
    /// bone; [`LAYER_ADDITIVE`] marks an additive layer.
    pub mask: u32,
    /// `(t, weight)` as two unorm16: frame B's share of the pair, and the layer's weight.
    pub blend: u32,
}

impl CrowdLayer {
    /// A layer at `weight` over frames `a` and `b` mixed by `t`.
    #[must_use]
    pub fn new(a: u32, b: u32, t: f32, weight: f32, mask: u32) -> Self {
        Self { frame_a: a, frame_b: b, mask, blend: pack_unorm2(t, weight) }
    }

    /// Whether this layer does nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.blend >> 16 == 0
    }

    /// `(t, weight)` unpacked.
    #[must_use]
    pub fn blend(&self) -> (f32, f32) {
        unpack_unorm2(self.blend)
    }
}

/// Two 0..1 values as unorm16 in one word (`x` low), as WGSL's `unpack2x16unorm` reads them.
#[must_use]
pub fn pack_unorm2(x: f32, y: f32) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round().sat_u32();
    q(x) | (q(y) << 16)
}

#[must_use]
pub fn unpack_unorm2(v: u32) -> (f32, f32) {
    ((v & 0xFFFF).to_f32() / 65535.0, (v >> 16).to_f32() / 65535.0)
}

/// Per-slot blend instructions for the compute pass (matches `CrowdInstance` in WESL).
///
/// The pose starts as the body's idle and each layer in turn mixes its frame pair in (or adds
/// its difference from the idle) by its weight and mask.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct CrowdInstance {
    /// Atlas matrix index of the idle pose (bone 0).
    pub idle: u32,
    /// Crowd slot entries to write (bones, then soft regions); 0 disables the slot.
    pub bones: u32,
    pub pad: [u32; 2],
    pub layers: [CrowdLayer; CROWD_LAYERS],
}

impl CrowdInstance {
    /// Standing in the idle pose.
    #[must_use]
    pub fn standing(layout: &ClipLayout) -> Self {
        Self { idle: layout.base, bones: layout.bones, ..default() }
    }

    /// Adds `layer` in the first free slot; a full instance drops it (the caller ranks layers).
    pub fn push(&mut self, layer: CrowdLayer) -> bool {
        self.layers.iter_mut().find(|l| l.is_empty()).is_some_and(|slot| {
            *slot = layer;
            true
        })
    }
}

/// The last instance uploaded for a mob (to skip unchanged slots).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct CrowdUploaded(pub CrowdInstance);

/// Where a body's baked gait clips live in the atlas.
#[derive(Clone, Copy, Debug)]
pub struct ClipLayout {
    /// Atlas index of the idle pose; clip `m`, frame `f` is at `base + bones·(1 + m·F + f)`.
    pub base: u32,
    pub bones: u32,
    pub modes: u32
}

impl ClipLayout {
    /// The frame pair of gait `mode` at `phase` as a whole-body layer, weighted by `moving`
    /// (0 stands in the idle pose).
    #[must_use]
    pub fn gait_layer(&self, motion: &BakedMotion, mode: usize, phase: f32, moving: f32) -> Option<CrowdLayer> {
        let frames = BAKE_FRAMES.clamp_u32();
        let frame = |m: u32, f: u32| self.base + self.bones * (1 + m * frames + f);
        let clip = motion.clips.get(mode)?;
        let mode = u32::try_from(mode).ok().filter(|m| *m < self.modes)?;
        if moving <= 0.0 {
            return None;
        }
        let at = (phase - clip.start_phase).rem_euclid(1.0) * frames.to_f32();
        let (f0, t) = (at.floor().sat_u32() % frames, at.fract());
        let f1 = (f0 + 1) % frames;
        Some(CrowdLayer::new(frame(mode, f0), frame(mode, f1), t, moving, MASK_ALL))
    }

    /// Blend instructions for gait `mode` at `phase`, blended toward idle by `idle_weight`.
    #[must_use]
    pub fn instance(&self, motion: &BakedMotion, mode: usize, phase: f32, idle_weight: f32) -> CrowdInstance {
        let mut instance = CrowdInstance::standing(self);
        if let Some(layer) = self.gait_layer(motion, mode, phase, 1.0 - idle_weight.clamp(0.0, 1.0)) {
            instance.push(layer);
        }
        instance
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
    /// Everything appended to the atlas, by base index, for CPU-side sampling (held tools).
    chunks: Vec<(u32, Arc<Vec<[f32; 16]>>)>,
}

impl FromWorld for CrowdSkin {
    fn from_world(world: &mut World) -> Self {
        let capacity = 64;
        let bones = world.resource_mut::<Assets<ShaderBuffer>>().add(bone_buffer(capacity));
        Self { bones, bone_capacity: capacity, free: Vec::new(), next: 0, atlas: default(), atlas_len: 0, chunks: Vec::new() }
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

    /// Appends `data` to the clip atlas (queued for upload) and returns its first matrix's index.
    pub fn append(&mut self, data: Arc<Vec<[f32; 16]>>, uploads: &mut CrowdUploads) -> u32 {
        let base = self.atlas_len;
        self.atlas_len += data.len().clamp_u32();
        self.chunks.push((base, data.clone()));
        uploads.clips.push((base, data));
        base
    }

    /// Atlas matrix `index`, from the CPU copy.
    #[must_use]
    pub fn matrix(&self, index: u32) -> Option<&[f32; 16]> {
        let at = self.chunks.partition_point(|(base, _)| *base <= index).checked_sub(1)?;
        let (base, data) = self.chunks.get(at)?;
        data.get((index - base).idx())
    }

    /// Entry `i` of a per-bone row at `row` (mask rows and the like).
    #[must_use]
    pub fn row_value(&self, row: u32, i: u32) -> Option<f32> {
        self.matrix(row + i / 16)?.get((i % 16).idx()).copied()
    }

    /// The skin matrix the compute pass writes for crowd slot entry `bone` of `instance`: the
    /// CPU mirror of `crowd_blend.wesl`, for held tools in crowd tiers.
    #[must_use]
    pub fn blend_bone(&self, instance: &CrowdInstance, bone: u32) -> Option<Mat4> {
        blend_with(instance, bone, |index| self.matrix(index).copied())
    }
}

/// [`CrowdInstance`]'s blend for slot entry `bone`, over the atlas as `matrix` reads it (the
/// CPU mirror of `crowd_blend.wesl`).
pub fn blend_with(instance: &CrowdInstance, bone: u32, matrix: impl Fn(u32) -> Option<[f32; 16]>) -> Option<Mat4> {
    if bone >= instance.bones {
        return None;
    }
    let at = |index: u32| matrix(index + bone).map(|m| Mat4::from_cols_array(&m));
    let row = |row: u32| matrix(row + bone / 16).and_then(|m| m.get((bone % 16).idx()).copied());
    let idle = at(instance.idle)?;
    let mut m = idle;
    for layer in &instance.layers {
        let (t, mut w) = layer.blend();
        if w <= 0.0 {
            continue;
        }
        let mask = layer.mask & MASK_ALL;
        if mask != MASK_ALL {
            w *= row(mask)?;
        }
        if w <= 0.0 {
            continue;
        }
        let f = at(layer.frame_a)? * (1.0 - t) + at(layer.frame_b)? * t;
        if layer.mask & LAYER_ADDITIVE != 0 {
            m += (f - idle) * w;
        } else {
            m = m * (1.0 - w) + f * w;
        }
    }
    Some(m)
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
        let base = skin.append(Arc::new(data), &mut uploads);
        skin.atlas.insert(*id, ClipLayout { base, bones, modes: m.clips.len().clamp_u32() });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_layout_matches_the_shader() {
        assert_eq!(size_of::<CrowdLayer>(), 16);
        assert_eq!(size_of::<CrowdInstance>(), 16 + CROWD_LAYERS * 16);
        let (t, w) = unpack_unorm2(pack_unorm2(0.25, 0.8));
        assert!((t - 0.25).abs() < 1e-4 && (w - 0.8).abs() < 1e-4);
        assert!(CrowdLayer::new(0, 0, 0.5, 0.0, MASK_ALL).is_empty());
        assert!(!CrowdLayer::new(0, 0, 0.5, 1e-3, MASK_ALL).is_empty());
    }

    /// Two layers over an idle: the atlas holds the idle (index 0), a frame (1) and a mask row
    /// (2..) with bone 0 masked out. Bone 1 takes the override and the additive delta; bone 0
    /// only what the unmasked layer gives it.
    #[test]
    fn blend_mirrors_the_compute_pass() {
        let bones = 2;
        let mat = |x: f32| Mat4::from_translation(Vec3::X * x).to_cols_array();
        let idle = [mat(0.0), mat(0.0)];
        let frame = [mat(1.0), mat(1.0)];
        let mut row = [0.0f32; 16];
        row[1] = 1.0;
        let atlas: Vec<[f32; 16]> = idle.iter().chain(&frame).copied().chain([row]).collect();
        let lookup = |i: u32| atlas.get(i.idx()).copied();
        let mut inst = CrowdInstance { idle: 0, bones, ..default() };
        assert!(inst.push(CrowdLayer::new(2, 2, 0.0, 0.5, 4)));
        assert!(inst.push(CrowdLayer::new(2, 2, 0.0, 0.5, MASK_ALL | LAYER_ADDITIVE)));
        let x = |b: u32| blend_with(&inst, b, lookup).map(|m| m.w_axis.x);
        assert!((x(1).unwrap() - 1.0).abs() < 1e-3, "masked override to 0.5, then +0.5 of the delta: {:?}", x(1));
        assert!((x(0).unwrap() - 0.5).abs() < 1e-3, "only the additive layer reaches bone 0");
        assert!(x(2).is_none(), "past the slot's bones");
        assert!(inst.push(CrowdLayer::new(2, 2, 0.0, 1.0, MASK_ALL)));
        assert!(!inst.push(CrowdLayer::new(2, 2, 0.0, 1.0, MASK_ALL)), "three layers at most");
    }
}
