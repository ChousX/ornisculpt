//! Fixed post-meshing steps. Plain functions: no reason to swap any of them yet.

use std::collections::HashMap;

use bevy::math::Vec3A;
use bevy::prelude::*;

use super::{BodyField, BodyMeshData, Field, MaterialSlot, MobVertexData, MuscleFlex, NOT_CLOTH, RawMesh, Sheet, SoftVertex, Submesh, VertexChannel};
use crate::anatomy::{Anatomy, BoneId};
use crate::num::{Clamp, Idx, Saturate, ToF32};

/// Merges vertices closer than `epsilon` (chunk seams), rebuilding indices and dropping
/// triangles that became degenerate.
pub fn weld(mesh: &mut RawMesh, epsilon: f32) {
    let inv = 1.0 / epsilon.max(1e-7);
    // Vertices of different sheets stay apart, however close.
    let mut map: HashMap<([i64; 3], u16), u32> = HashMap::with_capacity(mesh.positions.len());
    let mut remap = Vec::with_capacity(mesh.positions.len());
    let mut positions = Vec::with_capacity(mesh.positions.len());
    let mut normals = Vec::with_capacity(mesh.positions.len());
    let mut sheets = Vec::with_capacity(mesh.sheets.len());
    for (i, p) in mesh.positions.iter().enumerate() {
        let sheet = mesh.sheets.get(i).copied();
        let key = ((*p * inv).round().to_array().map(Saturate::sat_i64), sheet.unwrap_or(Sheet::WHOLE_ID));
        let idx = *map.entry(key).or_insert_with(|| {
            positions.push(*p);
            normals.push(mesh.normals.get(i).copied().unwrap_or(Vec3::Y));
            sheets.extend(sheet);
            positions.len().saturating_sub(1).clamp_u32()
        });
        remap.push(idx);
    }
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for &[a, b, c] in mesh.indices.as_chunks::<3>().0 {
        let (Some(&a), Some(&b), Some(&c)) = (remap.get(a.idx()), remap.get(b.idx()), remap.get(c.idx())) else { continue };
        if a != b && b != c && a != c {
            indices.extend([a, b, c]);
        }
    }
    *mesh = RawMesh { positions, normals, indices, sheets };
}

/// Vertex neighbour lists from triangle edges.
fn neighbours(vertex_count: usize, indices: &[u32]) -> Vec<Vec<u32>> {
    let mut n: Vec<Vec<u32>> = vec![Vec::new(); vertex_count];
    for &[a, b, c] in indices.as_chunks::<3>().0 {
        for (from, to) in [(a, b), (b, c), (c, a), (b, a), (c, b), (a, c)] {
            if let Some(ns) = n.get_mut(from.idx())
                && !ns.contains(&to)
            {
                ns.push(to);
            }
        }
    }
    n
}

/// Taubin λ|μ smoothing: removes voxel stepping without shrinking volume.
pub fn smooth(mesh: &mut RawMesh, iterations: u32) {
    const LAMBDA: f32 = 0.5;
    const MU: f32 = -0.53;
    let nb = neighbours(mesh.positions.len(), &mesh.indices);
    let mut next = mesh.positions.clone();
    for _ in 0..iterations {
        for factor in [LAMBDA, MU] {
            for ((ns, out), &p) in nb.iter().zip(&mut next).zip(&mesh.positions) {
                if ns.is_empty() {
                    continue;
                }
                let avg = ns.iter().filter_map(|&j| mesh.positions.get(j.idx())).sum::<Vec3>() / ns.len().to_f32();
                *out = p + (avg - p) * factor;
            }
            std::mem::swap(&mut mesh.positions, &mut next);
        }
    }
}

/// Skin weights, normals and per-vertex data from the field (one `surface_info` per vertex).
/// Cavity is a one-sample occlusion probe along the normal.
///
/// A vertex on a sheet of its own (see [`RawMesh::sheets`]) is weighted by that sheet alone, so a
/// limb's surface hidden in the body binds to the limb, and the body's beneath it to the body.
#[must_use]
pub fn skin_and_attrs(mesh: RawMesh, field: &BodyField, anatomy: &Anatomy) -> BodyMeshData {
    const PROBE: f32 = 0.03;
    let n = mesh.positions.len();
    let mut out = BodyMeshData {
        positions: mesh.positions,
        normals: Vec::with_capacity(n),
        joints: Vec::with_capacity(n),
        weights: Vec::with_capacity(n),
        mob_data: Vec::with_capacity(n),
        uvs: Vec::new(),
        coverage: Vec::new(),
        flow: Vec::new(),
        cloth: Vec::new(),
        cor: Vec::new(),
        muscle: Vec::new(),
        soft: Vec::new(),
        indices: mesh.indices,
        submeshes: Vec::new(),
    };
    // Each vertex queries the field on its own: in parallel.
    let probe = |f: &dyn Field, pa: Vec3A| {
        let normal = f.gradient(pa).normalize_or(Vec3A::Y);
        let occluded = ((PROBE - f.distance(pa + normal * PROBE)) / PROBE).clamp(0.0, 1.0);
        (normal, f.surface_info(pa), occluded)
    };
    let at: Vec<(Vec3, u16)> = out.positions.iter().enumerate().map(|(i, p)| (*p, mesh.sheets.get(i).copied().unwrap_or(Sheet::WHOLE_ID))).collect();
    let attrs = super::par_map(&at, |&(p, sheet)| {
        let pa = Vec3A::from(p);
        let (normal, info, occluded) = match Sheet::from_id(sheet) {
            Sheet::Whole => probe(field, pa),
            sheet => probe(&field.sheet(sheet), pa),
        };
        let bone = info.influences.primary();
        // The bones of the part this bone belongs to (just this bone if that is unknown).
        let range = anatomy
            .skeleton
            .get(bone)
            .and_then(|def| anatomy.parts.get(def.part.idx()))
            .map_or_else(|| bone.0..bone.0.saturating_add(1), |part| part.bones.clone());
        let span = f32::from((range.end.saturating_sub(range.start)).max(1));
        let limb_t = (f32::from(bone.0.saturating_sub(range.start)) + 0.5) / span;
        let data = MobVertexData {
            region: info.region,
            cavity: (occluded * 255.0).sat_u8(),
            curvature: 128,
            limb_t: (limb_t * 255.0).sat_u8(),
            primary_bone: bone,
            layer: info.layer,
        };
        (Vec3::from(normal), info.influences.bones.map(|b| u16::from(b.0)), info.influences.weights, data.pack())
    });
    for (normal, joints, weights, data) in attrs {
        out.normals.push(normal);
        out.joints.push(joints);
        out.weights.push(weights);
        out.mob_data.push(data);
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len().clamp_u32(), slot: MaterialSlot::Skin });
    out
}

/// meshoptimizer simplification to `target_ratio` of the triangles, within `max_error`.
///
/// The error is relative to mesh extents; vertex-cache ordering and compaction follow. Applies to
/// single-submesh data (call before merging hard parts).
#[must_use]
pub fn simplify(data: &BodyMeshData, target_ratio: f32, max_error: f32) -> BodyMeshData {
    let stride = std::mem::size_of::<Vec3>();
    let bytes = meshopt::typed_to_bytes(&data.positions);
    let Ok(adapter) = meshopt::VertexDataAdapter::new(bytes, stride, 0) else {
        return data.clone();
    };
    let target = ((data.indices.len().to_f32() * target_ratio).sat_usize() / 3) * 3;
    let simplified = meshopt::simplify(
        &data.indices,
        &adapter,
        target.max(3),
        max_error,
        meshopt::SimplifyOptions::None,
        None,
    );
    let ordered = meshopt::optimize_vertex_cache(&simplified, data.positions.len());
    compact(data, &ordered)
}

/// Keeps only vertices referenced by `indices`, in first-use order (good for vertex fetch).
/// Triangles with a vertex out of range are dropped.
fn compact(data: &BodyMeshData, indices: &[u32]) -> BodyMeshData {
    use VertexChannel::{Cloth, Coverage, Flow, Muscle, Soft};
    let mut remap = vec![u32::MAX; data.positions.len()];
    let mut out = BodyMeshData::default();
    for tri in indices.as_chunks::<3>().0 {
        if !tri.iter().all(|i| data.vertex(i.idx()).is_some()) {
            continue;
        }
        for &i in tri {
            let i = i.idx();
            let (Some(slot), Some(v)) = (remap.get_mut(i), data.vertex(i)) else { continue };
            if *slot == u32::MAX {
                *slot = out.positions.len().clamp_u32();
                out.push_vertex(v);
                out.push_from(data, i, &[Coverage, Flow, Cloth, Muscle, Soft]);
            }
            out.indices.push(*slot);
        }
    }
    let slot = data.submeshes.first().map_or(MaterialSlot::Skin, |s| s.slot);
    out.submeshes.push(Submesh { indices: 0..out.indices.len().clamp_u32(), slot });
    out
}

/// Rewrites joint indices from the full skeleton to the crowd skeleton
/// (`anatomy.skeleton.crowd.map`), merging weights of folded bones.
pub fn remap_to_crowd(data: &mut BodyMeshData, anatomy: &Anatomy) {
    let map = &anatomy.skeleton.crowd.map;
    let crowd = |joint: u16| map.get(joint.idx()).copied().unwrap_or(0);
    for (joints, weights) in data.joints.iter_mut().zip(&mut data.weights) {
        let mut merged: [(u16, f32); 4] = [(0, 0.0); 4];
        let mut n = 0;
        for (&joint, &w) in joints.iter().zip(weights.iter()) {
            if w <= 0.0 {
                continue;
            }
            let j = u16::from(crowd(joint));
            if let Some(m) = merged.iter_mut().take(n).find(|(mj, _)| *mj == j) {
                m.1 += w;
            } else if let Some(slot) = merged.get_mut(n) {
                *slot = (j, w);
                n += 1;
            }
        }
        if n == 0 {
            merged[0] = (u16::from(crowd(joints[0])), 1.0);
        }
        *joints = merged.map(|m| m.0);
        *weights = merged.map(|m| m.1);
    }
    for d in &mut data.mob_data {
        let mut v = MobVertexData::unpack(*d);
        v.primary_bone = BoneId(crowd(u16::from(v.primary_bone.0)));
        *d = v.pack();
    }
}

/// Appends `other` (e.g. hard parts) into `base`, offsetting indices and submesh ranges.
pub fn merge(base: &mut BodyMeshData, other: &BodyMeshData) {
    let base_len = base.positions.len();
    let vbase = base_len.clamp_u32();
    let ibase = base.indices.len().clamp_u32();
    // Centres of rotation: a vertex without one rotates about itself (exactly linear blending).
    if (!base.cor.is_empty() || !other.cor.is_empty()) && base.cor.len() != base_len {
        base.cor.clone_from(&base.positions);
    }
    base.positions.extend_from_slice(&other.positions);
    base.normals.extend_from_slice(&other.normals);
    base.joints.extend_from_slice(&other.joints);
    base.weights.extend_from_slice(&other.weights);
    base.mob_data.extend_from_slice(&other.mob_data);
    // Optional per-vertex channels: pad whichever side lacks them.
    pad_extend(&mut base.uvs, &other.uvs, base_len, other.positions.len(), [0.0; 2]);
    pad_extend(&mut base.coverage, &other.coverage, base_len, other.positions.len(), [0.0; 3]);
    pad_extend(&mut base.flow, &other.flow, base_len, other.positions.len(), [0.0, 0.0, 1.0, 0.0]);
    pad_extend(&mut base.muscle, &other.muscle, base_len, other.positions.len(), MuscleFlex::default());
    pad_extend(&mut base.soft, &other.soft, base_len, other.positions.len(), SoftVertex::default());
    if !base.cor.is_empty() || !other.cor.is_empty() {
        if other.cor.len() == other.positions.len() {
            base.cor.extend_from_slice(&other.cor);
        } else {
            base.cor.extend_from_slice(&other.positions);
        }
    }
    if !base.cloth.is_empty() || !other.cloth.is_empty() {
        base.cloth.resize(base_len, NOT_CLOTH);
        if other.cloth.is_empty() {
            base.cloth.resize(base.positions.len(), NOT_CLOTH);
        } else {
            base.cloth.extend_from_slice(&other.cloth);
        }
    }
    base.indices.extend(other.indices.iter().map(|i| i + vbase));
    for s in &other.submeshes {
        let range = s.indices.start + ibase..s.indices.end + ibase;
        match base.submeshes.last_mut() {
            Some(last) if last.slot == s.slot && last.indices.end == range.start => last.indices.end = range.end,
            _ => base.submeshes.push(Submesh { indices: range, slot: s.slot }),
        }
    }
}

/// Appends `other` (for `other_len` vertices) to `base` (holding `base_len`), padding either side
/// with `fill` when it lacks the channel; leaves both empty if neither has it.
fn pad_extend<T: Copy>(base: &mut Vec<T>, other: &[T], base_len: usize, other_len: usize, fill: T) {
    if base.is_empty() && other.is_empty() {
        return;
    }
    base.resize(base_len, fill);
    if other.len() == other_len {
        base.extend_from_slice(other);
    } else {
        base.resize(base_len + other_len, fill);
    }
}

/// meshoptimizer (`meshopt`) takes its scratch memory from C++'s `operator new` and
/// `operator delete`, which nothing provides on wasm32-unknown-unknown: these are them, over Rust's
/// allocator. The size goes in a header in front of each block so `delete`, which only gets the
/// pointer, can rebuild the layout.
#[cfg(all(target_arch = "wasm32", feature = "web"))]
mod cxx_alloc {
    use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};

    /// Payload alignment, and the header size that keeps the payload aligned.
    const ALIGN: usize = 16;

    const fn layout(size: usize) -> Option<Layout> {
        match Layout::from_size_align(size.saturating_add(ALIGN), ALIGN) {
            Ok(l) => Some(l),
            Err(_) => None,
        }
    }

    /// `operator new(size_t)`.
    ///
    /// # Safety
    ///
    /// Called by C++ only; the block is freed with [`_ZdlPv`].
    #[allow(clippy::cast_ptr_alignment, reason = "the block is 16-aligned")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn _Znwm(size: usize) -> *mut u8 {
        let Some(layout) = layout(size) else { std::process::abort() };
        // SAFETY: the layout has a non-zero size.
        let base = unsafe { alloc(layout) };
        if base.is_null() {
            handle_alloc_error(layout);
        }
        // SAFETY: the block holds the header and `size` bytes after it.
        unsafe {
            base.cast::<usize>().write(size);
            base.add(ALIGN)
        }
    }

    /// `operator delete(void*)`.
    ///
    /// # Safety
    ///
    /// `ptr` is null or came from [`_Znwm`] and hasn't been freed.
    #[allow(clippy::cast_ptr_alignment, reason = "the block is 16-aligned")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn _ZdlPv(ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        // SAFETY: `ptr` was handed out by `_Znwm`, so the header precedes it and holds the size.
        unsafe {
            let base = ptr.sub(ALIGN);
            let size = base.cast::<usize>().read();
            if let Some(layout) = layout(size) {
                dealloc(base, layout);
            }
        }
    }
}
