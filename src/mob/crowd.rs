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

use std::ops::Range;
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::BufferUsages;
use bevy::shader::ShaderDefVal;
use bevy::render::storage::{ShaderBuffer, ShaderBufferData};
use bytemuck::{Pod, Zeroable};

use super::GeneratedBody;
use crate::anatomy::{Anatomy, CROWD_SLOT};
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
/// Metres a packed foot offset spans (unorm16 over ±half of it).
pub const FOOT_RANGE: f32 = 8.0;

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
/// its difference from the idle) by its weight and mask. Then the ground: the whole pose is
/// tilted about the mob's origin and raised by `lift`, and each leg's bones move up or down by
/// its foot's offset times how far down the leg they are (`legs` rows).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct CrowdInstance {
    /// Atlas matrix index of the idle pose (bone 0).
    pub idle: u32,
    /// Crowd slot entries to write (bones, then soft regions); 0 disables the slot.
    pub bones: u32,
    /// Atlas index of the body's leg rows (which leg each entry hangs from, then how far down
    /// it), [`MASK_ALL`] for none.
    pub legs: u32,
    /// The root raised, metres.
    pub lift: f32,
    /// The root's tilt (a quaternion, mob space).
    pub tilt: [f32; 4],
    /// Eight foot offsets (metres over [`FOOT_RANGE`]) as unorm16 pairs, in leg order.
    pub feet: [u32; 4],
    pub layers: [CrowdLayer; CROWD_LAYERS],
}

impl Default for CrowdInstance {
    fn default() -> Self {
        Self { idle: 0, bones: 0, legs: MASK_ALL, lift: 0.0, tilt: Quat::IDENTITY.to_array(), feet: [pack_unorm2(0.5, 0.5); 4], layers: [CrowdLayer::default(); CROWD_LAYERS] }
    }
}

impl CrowdInstance {
    /// Standing in the idle pose on flat ground.
    #[must_use]
    pub fn standing(layout: &ClipLayout) -> Self {
        Self { idle: layout.base, bones: layout.bones, legs: layout.legs, ..default() }
    }

    /// Sits the pose on the ground: raised by `lift`, tilted by `tilt`, each foot moved by
    /// `feet` (metres, leg order; at most eight).
    pub fn ground(&mut self, lift: f32, tilt: Quat, feet: &[f32]) {
        self.lift = lift;
        self.tilt = tilt.to_array();
        let at = |i: usize| feet.get(i).copied().unwrap_or(0.0) / FOOT_RANGE + 0.5;
        self.feet = std::array::from_fn(|k| pack_unorm2(at(2 * k), at(2 * k + 1)));
    }

    /// Foot `leg`'s offset, metres.
    #[must_use]
    pub fn foot(&self, leg: usize) -> f32 {
        let pair = self.feet.get(leg / 2).map_or((0.5, 0.5), |p| unpack_unorm2(*p));
        (if leg % 2 == 0 { pair.0 } else { pair.1 } - 0.5) * FOOT_RANGE
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
    pub modes: u32,
    /// Atlas index of the body's leg rows (see [`leg_rows`]).
    pub legs: u32,
}

/// Two per-bone rows for the ground fit, as atlas matrices.
///
/// Which leg each crowd slot entry hangs from (its index as a float, −1 for none), and how far
/// down that leg it sits (0 at the hip, 1 at the foot and below).
#[must_use]
pub fn leg_rows(anatomy: &Anatomy) -> Vec<[f32; 16]> {
    let skeleton = &anatomy.skeleton;
    let mut of_bone: Vec<(f32, f32)> = vec![(-1.0, 0.0); skeleton.len()];
    for (li, leg) in anatomy.legs.iter().enumerate() {
        let last = leg.chain.len().saturating_sub(1).max(1).to_f32();
        for (k, b) in leg.chain.iter().enumerate() {
            if let Some(slot) = of_bone.get_mut(b.index()) {
                *slot = (li.to_f32(), (k.to_f32() / last).min(1.0));
            }
        }
    }
    // Whatever grows from a leg bone (toes, a sole) goes with it.
    for i in 0..skeleton.len() {
        let inherited = skeleton.bones.get(i).and_then(|b| b.parent).and_then(|p| of_bone.get(p.index())).copied();
        if let (Some(slot), Some(parent)) = (of_bone.get_mut(i), inherited)
            && slot.0 < 0.0
            && parent.0 >= 0.0
        {
            *slot = parent;
        }
    }
    let of = |b: crate::anatomy::BoneId| of_bone.get(b.index()).copied().unwrap_or((-1.0, 0.0));
    #[cfg(feature = "soft-tissue")]
    let soft = crate::soft::SoftTissue::of(anatomy);
    let entries: Vec<(f32, f32)> = {
        let bones = skeleton.crowd.bones.iter().map(|b| of(*b));
        #[cfg(feature = "soft-tissue")]
        let bones = bones.chain(soft.crowd.iter().map(|r| of(r.bone)));
        bones.collect()
    };
    let entries = &entries;
    let row = |pick: fn(&(f32, f32)) -> f32| {
        (0..ROW_MATRICES).map(move |m| std::array::from_fn(|i| entries.get(m * 16 + i).map_or(-1.0, pick)))
    };
    let mut out: Vec<[f32; 16]> = row(|e| e.0).collect();
    out.extend(row(|e| e.1));
    out
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
    /// Holes in the atlas left by bodies and graph bakes that went, sorted by index and joined
    /// up where they touch. The next block that fits takes one instead of growing the atlas: a
    /// long session that generates bodies it later drops (an editor, or a game streaming
    /// creatures in and out) would otherwise grow the clip buffer for ever.
    free_clips: Vec<Range<u32>>,
    /// Everything appended to the atlas, by base index, for CPU-side sampling (held tools).
    chunks: Vec<(u32, Arc<Vec<[f32; 16]>>)>,
}

impl FromWorld for CrowdSkin {
    fn from_world(world: &mut World) -> Self {
        let capacity = 64;
        let bones = world.resource_mut::<Assets<ShaderBuffer>>().add(bone_buffer(capacity));
        Self::new(bones, capacity)
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
    fn new(bones: Handle<ShaderBuffer>, capacity: u32) -> Self {
        Self {
            bones,
            bone_capacity: capacity,
            free: Vec::new(),
            next: 0,
            atlas: default(),
            atlas_len: 0,
            free_clips: Vec::new(),
            chunks: Vec::new(),
        }
    }

    /// A skin with no GPU buffer behind it, for tests of the atlas allocator.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test() -> Self {
        Self::new(Handle::default(), 64)
    }

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

    /// Records where a body's clips are, for tests of the lookup.
    #[cfg(test)]
    pub(crate) fn set_layout(&mut self, body: AssetId<GeneratedBody>, layout: ClipLayout) {
        self.atlas.insert(body, layout);
    }

    /// Puts `data` in the clip atlas (queued for upload) and returns its first matrix's index:
    /// in the smallest hole that holds it ([`release_range`](Self::release_range)), else at the
    /// end. Give the range back with `release_range(base, data.len())` when nothing reads it.
    pub fn append(&mut self, data: Arc<Vec<[f32; 16]>>, uploads: &mut CrowdUploads) -> u32 {
        let base = self.take_range(data.len().clamp_u32());
        // Chunks stay sorted by index, since a block may land in a hole between two others.
        let at = self.chunks.partition_point(|(b, _)| *b < base);
        self.chunks.insert(at, (base, data.clone()));
        uploads.clips.push((base, data));
        base
    }

    /// `len` matrices from the smallest hole that holds them (so the big holes stay whole), else
    /// from the atlas's end.
    fn take_range(&mut self, len: u32) -> u32 {
        let best = self
            .free_clips
            .iter()
            .enumerate()
            .filter(|(_, hole)| hole.end - hole.start >= len)
            .min_by_key(|(_, hole)| hole.end - hole.start)
            .map(|(i, _)| i);
        let Some(i) = best else {
            let base = self.atlas_len;
            self.atlas_len += len;
            return base;
        };
        let Some(hole) = self.free_clips.get_mut(i) else { return self.atlas_len };
        let base = hole.start;
        hole.start += len;
        if hole.start >= hole.end {
            self.free_clips.remove(i);
        }
        base
    }

    /// Gives `len` matrices at `base` back to the atlas, for the next block to use. The matrices
    /// stay on the GPU until something is written over them; nothing reads them, because a block
    /// is only released once whatever referred to it has gone.
    pub fn release_range(&mut self, base: u32, len: u32) {
        if len == 0 {
            return;
        }
        let at = self.chunks.partition_point(|(b, _)| *b < base);
        if self.chunks.get(at).is_some_and(|(b, _)| *b == base) {
            self.chunks.remove(at);
        }
        let at = self.free_clips.partition_point(|hole| hole.start < base);
        self.free_clips.insert(at, base..base + len);
        // Join holes that touch, so a body's block can be reused by a bigger one later.
        let mut joined: Vec<Range<u32>> = Vec::with_capacity(self.free_clips.len());
        for hole in self.free_clips.drain(..) {
            match joined.last_mut() {
                Some(last) if last.end >= hole.start => last.end = last.end.max(hole.end),
                _ => joined.push(hole),
            }
        }
        self.free_clips = joined;
        // A hole at the end is not a hole: shorten the atlas instead, so the GPU buffer stops
        // growing in a session that keeps replacing bodies.
        if self.free_clips.last().is_some_and(|hole| hole.end >= self.atlas_len)
            && let Some(hole) = self.free_clips.pop()
        {
            self.atlas_len = hole.start;
        }
    }

    /// Frees the gait clips of a body that has gone (`register_clips`).
    fn release_clips(&mut self, body: AssetId<GeneratedBody>) {
        let Some(layout) = self.atlas.remove(&body) else { return };
        let at = self.chunks.partition_point(|(b, _)| *b < layout.base);
        let len = self.chunks.get(at).filter(|(b, _)| *b == layout.base).map_or(0, |(_, data)| data.len().clamp_u32());
        self.release_range(layout.base, len);
    }

    /// Matrices in the atlas, and the holes left in it by blocks that were released.
    #[must_use]
    pub fn atlas_usage(&self) -> (u32, u32) {
        (self.atlas_len, self.free_clips.iter().map(|hole| hole.end - hole.start).sum())
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
    // The ground.
    let tilt = Quat::from_array(instance.tilt);
    if instance.lift != 0.0 || tilt != Quat::IDENTITY {
        m = Mat4::from_quat(tilt) * m;
        m.w_axis.y += instance.lift;
    }
    if instance.legs != MASK_ALL {
        let leg = row(instance.legs)?;
        if leg >= 0.0 {
            let frac = row(instance.legs + ROW_MATRICES.clamp_u32())?;
            m.w_axis.y = instance.foot((leg + 0.5).sat_usize()).mul_add(frac, m.w_axis.y);
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

/// One baked motion as a block of the clip atlas: the idle pose, then every clip's frames, then
/// the body's leg rows (the ground fit).
pub struct ClipBlock {
    pub data: Arc<Vec<[f32; 16]>>,
    bones: u32,
    modes: u32,
    /// Offset of the leg rows within the block.
    legs: u32,
}

impl ClipBlock {
    /// The block for `motion` on `anatomy`, ready to [append](CrowdSkin::append).
    #[must_use]
    pub fn new(anatomy: &Anatomy, motion: &BakedMotion) -> Self {
        let bones = motion.idle.len().clamp_u32();
        let mut data: Vec<[f32; 16]> =
            Vec::with_capacity(motion.idle.len() * (1 + motion.clips.len() * BAKE_FRAMES) + 2 * ROW_MATRICES);
        data.extend_from_slice(&motion.idle);
        for clip in &motion.clips {
            for frame in &clip.frames {
                data.extend_from_slice(frame);
            }
        }
        let legs = data.len().clamp_u32();
        data.extend(leg_rows(anatomy));
        Self { data: Arc::new(data), bones, modes: motion.clips.len().clamp_u32(), legs }
    }

    /// Where the block's clips are, once it sits at `base`.
    #[must_use]
    pub const fn layout_at(&self, base: u32) -> ClipLayout {
        ClipLayout { base, bones: self.bones, modes: self.modes, legs: base + self.legs }
    }

    /// How many matrices the block takes.
    #[must_use]
    pub fn len(&self) -> u32 {
        self.data.len().clamp_u32()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Appends newly generated bodies' baked clips to the atlas, and frees the clips of bodies that
/// have gone (the asset is removed when its last handle drops, so by then no mob plays them).
pub(super) fn register_clips(
    mut events: MessageReader<AssetEvent<GeneratedBody>>,
    bodies: Res<Assets<GeneratedBody>>,
    mut skin: ResMut<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
) {
    for ev in events.read() {
        if let AssetEvent::Removed { id } = ev {
            skin.release_clips(*id);
            continue;
        }
        let AssetEvent::Added { id } = ev else { continue };
        let Some(body) = bodies.get(*id) else { continue };
        let block = ClipBlock::new(&body.anatomy, &body.motion);
        let base = skin.append(block.data.clone(), &mut uploads);
        skin.atlas.insert(*id, block.layout_at(base));
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

    fn skin() -> CrowdSkin {
        CrowdSkin::for_test()
    }

    /// `n` matrices, each holding `mark` so a reader can tell the blocks apart.
    fn block(n: usize, mark: f32) -> Arc<Vec<[f32; 16]>> {
        Arc::new(vec![[mark; 16]; n])
    }

    /// **Probe (roadmap Phase 2.5): how much of the clip atlas is holes over a session that
    /// replaces bodies?**
    ///
    /// Compaction would have to rewrite every `ClipLayout`, which is a real cost, so it is only
    /// worth paying if the fragmentation is real. This measures it before anything is built.
    ///
    /// Block sizes are the real ones, exactly as `register_clips` sizes them:
    /// `bones * (1 + modes * BAKE_FRAMES) + 2 * ROW_MATRICES`, with the crowd bone
    /// count of each of the bench's six plans, read from the plans themselves. The session keeps
    /// `live` bodies resident and replaces one at a time, which is the pattern the roadmap
    /// describes (an editor's previews, a game streaming creatures).
    ///
    /// `cargo test --release -- --ignored clip_atlas_fragmentation --nocapture`
    #[test]
    #[ignore = "fragmentation probe"]
    fn clip_atlas_fragmentation() {
        use rand::RngExt;
        let plans: [(&str, &str); 6] = [
            ("man", include_str!("../../assets/mobs/man.body.ron")),
            ("woman", include_str!("../../assets/mobs/woman.body.ron")),
            ("horse", include_str!("../../assets/mobs/horse.body.ron")),
            ("ox", include_str!("../../assets/mobs/ox.body.ron")),
            ("spider", include_str!("../../assets/mobs/spider.body.ron")),
            ("darner", include_str!("../../assets/mobs/darner.body.ron")),
        ];
        // Three gait modes, as `motion::library::standard` bakes for a crowd mob.
        let modes = 3u32;
        let sizes: Vec<(&str, u32)> = plans
            .iter()
            .map(|(name, src)| {
                let bones = crate::anatomy::build_src(src).skeleton.crowd.bones.len().clamp_u32();
                (*name, bones * (1 + modes * BAKE_FRAMES.clamp_u32()) + 2 * ROW_MATRICES.clamp_u32())
            })
            .collect();
        for (name, size) in &sizes {
            println!("plan {name}: block {size} matrices");
        }

        // Each scenario: (label, the block sizes a session draws from).
        let uniform: Vec<(&str, u32)> = vec![("one size", sizes.first().map_or(1, |(_, s)| *s))];
        let wide: Vec<(&str, u32)> = (1..=8).map(|k| ("wide", 300 * k)).collect();
        for (label, set) in [("real six plans", &sizes), ("one size only (control)", &uniform), ("wide spread (stress)", &wide)] {
            for live in [8usize, 24] {
                let mut skin = CrowdSkin::for_test();
                let mut uploads = CrowdUploads::default();
                let mut rng = rand_pcg::Pcg32::new(0x5f3d_1a77, 0xa02b_dbf7);
                let mut resident: Vec<(u32, u32)> = Vec::new();
                let pick = |rng: &mut rand_pcg::Pcg32| {
                    let i = usize::try_from(rng.random_range(0..set.len().clamp_u32())).unwrap_or(0);
                    set.get(i).map_or(1, |(_, s)| *s)
                };
                let mut worst = 0.0f32;
                print!("{label:24} live {live:3}:");
                for step in 1..=5000usize {
                    if resident.len() >= live {
                        let i = usize::try_from(rng.random_range(0..resident.len().clamp_u32())).unwrap_or(0);
                        if i < resident.len() {
                            let (base, len) = resident.swap_remove(i);
                            skin.release_range(base, len);
                        }
                    }
                    let len = pick(&mut rng);
                    let base = skin.append(Arc::new(vec![[0.0; 16]; len.idx()]), &mut uploads);
                    resident.push((base, len));
                    let (atlas, holes) = skin.atlas_usage();
                    let frac = if atlas == 0 { 0.0 } else { holes.to_f32() / atlas.to_f32() * 100.0 };
                    worst = worst.max(frac);
                    if matches!(step, 50 | 500 | 5000) {
                        print!("  n={step} {frac:.1}%");
                    }
                }
                let (atlas, holes) = skin.atlas_usage();
                let live_total: u32 = resident.iter().map(|(_, l)| *l).sum();
                println!("  | final atlas {atlas} holes {holes} live {live_total} worst-seen {worst:.1}%");
            }
        }
    }

    /// A body's clips are a block of the atlas, and a block whose body has gone must be reused
    /// rather than left as a hole: a session that keeps replacing bodies would otherwise grow
    /// the clip buffer without bound.
    #[test]
    fn a_released_block_is_used_again() {
        let (mut skin, mut uploads) = (skin(), CrowdUploads::default());
        let first = skin.append(block(10, 1.0), &mut uploads);
        let second = skin.append(block(4, 2.0), &mut uploads);
        let third = skin.append(block(6, 3.0), &mut uploads);
        assert_eq!((first, second, third), (0, 10, 14));
        assert_eq!(skin.atlas_usage(), (20, 0));

        // The middle block goes: the atlas keeps its length and holds a hole of 4.
        skin.release_range(second, 4);
        assert_eq!(skin.atlas_usage(), (20, 4));
        // Too big for the hole, so it goes on the end; one that fits takes the hole.
        assert_eq!(skin.append(block(5, 4.0), &mut uploads), 20);
        assert_eq!(skin.append(block(3, 5.0), &mut uploads), second);
        assert_eq!(skin.atlas_usage(), (25, 1), "a matrix of the hole is left over");
        // Chunks stay sorted by index, so the CPU-side read still finds the right block.
        assert_eq!(skin.matrix(second).map(|m| m[0]), Some(5.0));
        assert_eq!(skin.matrix(third).map(|m| m[0]), Some(3.0));
        assert_eq!(skin.matrix(second + 3), None, "the matrix left over belongs to nothing");
    }

    #[test]
    fn released_blocks_next_to_each_other_join_up() {
        let (mut skin, mut uploads) = (skin(), CrowdUploads::default());
        let a = skin.append(block(4, 1.0), &mut uploads);
        let b = skin.append(block(4, 2.0), &mut uploads);
        let c = skin.append(block(4, 3.0), &mut uploads);
        skin.append(block(4, 4.0), &mut uploads);
        // Freed out of order, with the join landing both ways round.
        skin.release_range(b, 4);
        skin.release_range(a, 4);
        skin.release_range(c, 4);
        assert_eq!(skin.atlas_usage(), (16, 12));
        assert_eq!(skin.append(block(12, 5.0), &mut uploads), a, "one hole of twelve, not three of four");
        assert_eq!(skin.atlas_usage(), (16, 0));
    }

    #[test]
    fn a_released_block_at_the_end_shortens_the_atlas() {
        let (mut skin, mut uploads) = (skin(), CrowdUploads::default());
        let a = skin.append(block(4, 1.0), &mut uploads);
        let b = skin.append(block(6, 2.0), &mut uploads);
        skin.release_range(b, 6);
        assert_eq!(skin.atlas_usage(), (4, 0), "the end is given up, not held as a hole");
        skin.release_range(a, 4);
        assert_eq!(skin.atlas_usage(), (0, 0), "an emptied atlas is empty");
        assert_eq!(skin.append(block(2, 3.0), &mut uploads), 0);
    }

    #[test]
    fn instance_layout_matches_the_shader() {
        assert_eq!(size_of::<CrowdLayer>(), 16);
        assert_eq!(size_of::<CrowdInstance>(), 48 + CROWD_LAYERS * 16);
        let mut inst = CrowdInstance::default();
        inst.ground(0.1, Quat::IDENTITY, &[0.25, -0.125, 1.0]);
        assert!((inst.foot(0) - 0.25).abs() < 1e-3 && (inst.foot(1) + 0.125).abs() < 1e-3 && (inst.foot(2) - 1.0).abs() < 1e-3);
        assert!(inst.foot(3).abs() < 1e-3 && inst.foot(7).abs() < 1e-3, "unset feet sit on the ground");
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

    /// The ground fit on the blend: bone 1 hangs from leg 0 at frac 0.5 (rows at 5 and 5 +
    /// `ROW_MATRICES`), so it takes half the foot's offset; the lift and a 90° roll reach both.
    #[test]
    fn ground_moves_the_blend() {
        let bones = 2;
        let mat = |x: f32| Mat4::from_translation(Vec3::X * x).to_cols_array();
        let mut legs = vec![[-1.0f32; 16]; 2 * ROW_MATRICES];
        let (leg_row, frac_row) = legs.split_at_mut(ROW_MATRICES);
        if let (Some(leg), Some(frac)) = (leg_row.first_mut(), frac_row.first_mut()) {
            leg[1] = 0.0;
            frac[1] = 0.5;
        }
        let atlas: Vec<[f32; 16]> = [mat(1.0), mat(1.0)].into_iter().chain(legs).collect();
        let lookup = |i: u32| atlas.get(i.idx()).copied();
        let mut inst = CrowdInstance { idle: 0, bones, legs: 2, ..default() };
        inst.ground(0.3, Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), &[0.2]);
        let p = |b: u32| blend_with(&inst, b, lookup).map(|m| m.w_axis.truncate());
        let b0 = p(0).unwrap();
        assert!((b0 - Vec3::new(0.0, 1.3, 0.0)).length() < 1e-4, "rolled up and lifted: {b0}");
        let b1 = p(1).unwrap();
        assert!((b1 - Vec3::new(0.0, 1.4, 0.0)).length() < 1e-3, "plus half the foot offset: {b1}");
    }
}
