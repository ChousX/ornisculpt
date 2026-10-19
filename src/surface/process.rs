//! Fixed post-meshing steps. Plain functions: no reason to swap any of them yet.

use std::collections::HashMap;

use bevy::math::Vec3A;
use bevy::prelude::*;

use super::{BodyMeshData, Field, MaterialSlot, MobVertexData, NOT_CLOTH, RawMesh, Submesh};
use crate::anatomy::Anatomy;

/// Merges vertices closer than `epsilon` (chunk seams), rebuilding indices and dropping
/// triangles that became degenerate.
pub fn weld(mesh: &mut RawMesh, epsilon: f32) {
    let inv = 1.0 / epsilon.max(1e-7);
    let mut map: HashMap<[i64; 3], u32> = HashMap::with_capacity(mesh.positions.len());
    let mut remap = Vec::with_capacity(mesh.positions.len());
    let mut positions = Vec::with_capacity(mesh.positions.len());
    let mut normals = Vec::with_capacity(mesh.positions.len());
    for (i, p) in mesh.positions.iter().enumerate() {
        let key = [(p.x * inv).round() as i64, (p.y * inv).round() as i64, (p.z * inv).round() as i64];
        let idx = *map.entry(key).or_insert_with(|| {
            positions.push(*p);
            normals.push(mesh.normals.get(i).copied().unwrap_or(Vec3::Y));
            positions.len() as u32 - 1
        });
        remap.push(idx);
    }
    let mut indices = Vec::with_capacity(mesh.indices.len());
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [remap[tri[0] as usize], remap[tri[1] as usize], remap[tri[2] as usize]];
        if a != b && b != c && a != c {
            indices.extend([a, b, c]);
        }
    }
    *mesh = RawMesh { positions, normals, indices };
}

/// Vertex neighbour lists from triangle edges.
fn neighbours(vertex_count: usize, indices: &[u32]) -> Vec<Vec<u32>> {
    let mut n: Vec<Vec<u32>> = vec![Vec::new(); vertex_count];
    for tri in indices.chunks_exact(3) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            if !n[a as usize].contains(&b) {
                n[a as usize].push(b);
            }
            if !n[b as usize].contains(&a) {
                n[b as usize].push(a);
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
            for (i, ns) in nb.iter().enumerate() {
                if ns.is_empty() {
                    continue;
                }
                let avg = ns.iter().map(|&j| mesh.positions[j as usize]).sum::<Vec3>() / ns.len() as f32;
                next[i] = mesh.positions[i] + (avg - mesh.positions[i]) * factor;
            }
            std::mem::swap(&mut mesh.positions, &mut next);
        }
    }
}

/// Skin weights, normals and per-vertex data from the field (one `surface_info` per vertex).
/// Cavity is a one-sample occlusion probe along the normal.
pub fn skin_and_attrs<F: Field>(mesh: RawMesh, field: &F, anatomy: &Anatomy) -> BodyMeshData {
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
        indices: mesh.indices,
        submeshes: Vec::new(),
    };
    for p in &out.positions {
        let pa = Vec3A::from(*p);
        let normal = field.gradient(pa).normalize_or(Vec3A::Y);
        let info = field.surface_info(pa);
        let occluded = ((PROBE - field.distance(pa + normal * PROBE)) / PROBE).clamp(0.0, 1.0);
        let bone = info.influences.primary();
        let def = anatomy.skeleton.get(bone);
        let range = &anatomy.parts[def.part as usize].bones;
        let span = (range.end - range.start).max(1) as f32;
        let limb_t = ((bone.0 - range.start) as f32 + 0.5) / span;
        out.normals.push(normal.into());
        out.joints.push(info.influences.bones.map(|b| b.0 as u16));
        out.weights.push(info.influences.weights);
        out.mob_data.push(
            MobVertexData {
                region: info.region,
                cavity: (occluded * 255.0) as u8,
                curvature: 128,
                limb_t: (limb_t * 255.0) as u8,
                primary_bone: bone,
                layer: info.layer,
            }
            .pack(),
        );
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len() as u32, slot: MaterialSlot::Skin });
    out
}

/// meshoptimizer simplification to `target_ratio` of the triangles, within `max_error`
/// (relative to mesh extents), then vertex-cache ordering and compaction. Applies to single-submesh
/// data (call before merging hard parts).
pub fn simplify(data: &BodyMeshData, target_ratio: f32, max_error: f32) -> BodyMeshData {
    let stride = std::mem::size_of::<Vec3>();
    let bytes = meshopt::typed_to_bytes(&data.positions);
    let Ok(adapter) = meshopt::VertexDataAdapter::new(bytes, stride, 0) else {
        return data.clone();
    };
    let target = ((data.indices.len() as f32 * target_ratio) as usize / 3) * 3;
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
fn compact(data: &BodyMeshData, indices: &[u32]) -> BodyMeshData {
    let mut remap = vec![u32::MAX; data.positions.len()];
    let mut out = BodyMeshData::default();
    for &i in indices {
        let slot = &mut remap[i as usize];
        if *slot == u32::MAX {
            *slot = out.positions.len() as u32;
            let i = i as usize;
            out.positions.push(data.positions[i]);
            out.normals.push(data.normals[i]);
            out.joints.push(data.joints[i]);
            out.weights.push(data.weights[i]);
            out.mob_data.push(data.mob_data[i]);
            if let Some(c) = data.coverage.get(i) {
                out.coverage.push(*c);
            }
            if let Some(f) = data.flow.get(i) {
                out.flow.push(*f);
            }
            if let Some(c) = data.cloth.get(i) {
                out.cloth.push(*c);
            }
        }
        out.indices.push(*slot);
    }
    let slot = data.submeshes.first().map_or(MaterialSlot::Skin, |s| s.slot);
    out.submeshes.push(Submesh { indices: 0..out.indices.len() as u32, slot });
    out
}

/// Rewrites joint indices from the full skeleton to the crowd skeleton
/// (`anatomy.skeleton.crowd.map`), merging weights of folded bones.
pub fn remap_to_crowd(data: &mut BodyMeshData, anatomy: &Anatomy) {
    let map = &anatomy.skeleton.crowd.map;
    for (joints, weights) in data.joints.iter_mut().zip(&mut data.weights) {
        let mut merged: [(u16, f32); 4] = [(0, 0.0); 4];
        let mut n = 0;
        for k in 0..4 {
            if weights[k] <= 0.0 {
                continue;
            }
            let j = map[joints[k] as usize] as u16;
            match merged[..n].iter_mut().find(|(mj, _)| *mj == j) {
                Some(m) => m.1 += weights[k],
                None => {
                    merged[n] = (j, weights[k]);
                    n += 1;
                }
            }
        }
        if n == 0 {
            merged[0] = (map[joints[0] as usize] as u16, 1.0);
        }
        *joints = merged.map(|m| m.0);
        *weights = merged.map(|m| m.1);
    }
    for d in &mut data.mob_data {
        let mut v = MobVertexData::unpack(*d);
        v.primary_bone = crate::anatomy::BoneId(map[v.primary_bone.index()]);
        *d = v.pack();
    }
}

/// Appends `other` (e.g. hard parts) into `base`, offsetting indices and submesh ranges.
pub fn merge(base: &mut BodyMeshData, other: &BodyMeshData) {
    let vbase = base.positions.len() as u32;
    let ibase = base.indices.len() as u32;
    base.positions.extend_from_slice(&other.positions);
    base.normals.extend_from_slice(&other.normals);
    base.joints.extend_from_slice(&other.joints);
    base.weights.extend_from_slice(&other.weights);
    base.mob_data.extend_from_slice(&other.mob_data);
    // Optional per-vertex channels: pad whichever side lacks them.
    pad_extend(&mut base.uvs, &other.uvs, vbase as usize, other.positions.len(), [0.0; 2]);
    pad_extend(&mut base.coverage, &other.coverage, vbase as usize, other.positions.len(), [0.0; 3]);
    pad_extend(&mut base.flow, &other.flow, vbase as usize, other.positions.len(), [0.0, 0.0, 1.0, 0.0]);
    // Centres of rotation: a vertex without one rotates about itself (exactly linear blending).
    if !base.cor.is_empty() || !other.cor.is_empty() {
        if base.cor.len() != vbase as usize {
            base.cor = base.positions[..vbase as usize].to_vec();
        }
        if other.cor.len() == other.positions.len() {
            base.cor.extend_from_slice(&other.cor);
        } else {
            base.cor.extend_from_slice(&other.positions);
        }
    }
    if !base.cloth.is_empty() || !other.cloth.is_empty() {
        base.cloth.resize(vbase as usize, NOT_CLOTH);
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
