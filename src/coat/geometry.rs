//! Coat geometry built at generation time from a body LOD mesh: fur shells and feather cards.
//! Both reuse the body's skinning (same joint space as the LOD they are built from).

use bevy::prelude::*;

use super::{Coat, Feathers};
use crate::anatomy::{Anatomy, BoneId, LodLevel, RegionId, RegionKind, RegionMask};
use crate::random::{RngExt, fork};
use crate::surface::{BodyMeshData, MaterialSlot, MobVertexData, Submesh, simplify};

/// Fraction of the hero skin's triangles used as the source for fur shells.
const SHELL_SOURCE_RATIO: f32 = 0.2;

/// Extra coat geometry for one LOD level.
#[derive(Clone, Debug, Default)]
pub struct CoatMeshes {
    pub shells: Option<BodyMeshData>,
    pub feathers: Option<BodyMeshData>,
}

/// Builds the coat geometry for one LOD level. Shells and feather cards exist only at hero detail;
/// crowd tiers shade fur and feather regions directly on the skin (alpha-tested layers defeat
/// early depth rejection, which is unaffordable across thousands of mobs).
pub fn build_coat_meshes(coat: &Coat, anatomy: &Anatomy, body: &BodyMeshData, level: LodLevel) -> CoatMeshes {
    let masks = coat.coverage(anatomy);
    let detail = match level {
        LodLevel::Hero => 1.0,
        LodLevel::Near | LodLevel::Far => 0.0,
    };
    let shells = coat.fur().map_or(0, |f| (f.shells as f32 * detail).round() as u32);
    let feathers = coat.feathers();
    // Fur hides fine surface detail, so shells use a simplified copy of the skin: every shell
    // multiplies its triangle count and they are alpha-tested.
    let shell_source = (shells > 0).then(|| simplify(body, SHELL_SOURCE_RATIO, 0.01));
    CoatMeshes {
        shells: shell_source.and_then(|s| fur_shells(&s, masks.fur, shells)),
        feathers: feathers
            .filter(|_| detail > 0.0)
            .and_then(|f| feather_cards(body, anatomy, level, masks.feathers, &f, f.density * detail)),
    }
}

fn region_of(body: &BodyMeshData, v: u32) -> RegionId {
    MobVertexData::unpack(body.mob_data[v as usize]).region
}

/// Skin triangles whose vertices are all in `mask`.
fn covered_triangles<'a>(body: &'a BodyMeshData, mask: RegionMask) -> impl Iterator<Item = [u32; 3]> + 'a {
    let skin = body.submeshes.iter().filter(|s| s.slot == MaterialSlot::Skin).flat_map(|s| s.indices.clone());
    let skin: Vec<u32> = skin.collect();
    skin.chunks_exact(3)
        .map(|t| [body.indices[t[0] as usize], body.indices[t[1] as usize], body.indices[t[2] as usize]])
        .filter(move |t| t.iter().all(|v| mask.contains(region_of(body, *v))))
        .collect::<Vec<_>>()
        .into_iter()
}

/// `shells` offset copies of the covered skin; `uvs.x` is the shell height (0..1].
pub fn fur_shells(body: &BodyMeshData, mask: RegionMask, shells: u32) -> Option<BodyMeshData> {
    let tris: Vec<[u32; 3]> = covered_triangles(body, mask).collect();
    if tris.is_empty() {
        return None;
    }
    // Compact the covered vertices once.
    let mut remap = vec![u32::MAX; body.positions.len()];
    let mut used = Vec::new();
    for v in tris.iter().flatten() {
        if remap[*v as usize] == u32::MAX {
            remap[*v as usize] = used.len() as u32;
            used.push(*v);
        }
    }
    let n = used.len() as u32;
    let mut out = BodyMeshData::default();
    for k in 1..=shells {
        let h = k as f32 / shells as f32;
        for &v in &used {
            let i = v as usize;
            out.positions.push(body.positions[i]);
            out.normals.push(body.normals[i]);
            out.joints.push(body.joints[i]);
            out.weights.push(body.weights[i]);
            out.mob_data.push(body.mob_data[i]);
            out.uvs.push([h, 0.0]);
            out.coverage.push(body.coverage.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
            out.flow.push(body.flow.get(i).copied().unwrap_or([0.0, 0.0, 1.0, 0.0]));
        }
        let base = (k - 1) * n;
        out.indices.extend(tris.iter().flat_map(|t| t.map(|v| base + remap[v as usize])));
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len() as u32, slot: MaterialSlot::Skin });
    Some(out)
}

/// Smoothing passes over the coverage and flow (softens covering borders over a few triangles).
const ANNOTATE_SMOOTHING: usize = 3;

/// Bakes coat inputs into a body LOD mesh: per-vertex coverage weights (scales, fur, feathers)
/// and the surface flow (bone axis, tangent to the skin, signed by region). Both are smoothed
/// across the skin so covering borders and bone boundaries blend instead of stepping.
pub fn annotate(body: &mut BodyMeshData, coat: &Coat, anatomy: &Anatomy, level: LodLevel) {
    let n = body.positions.len();
    body.coverage.clear();
    body.flow.clear();
    if coat.coverings.is_empty() {
        return;
    }
    let masks = coat.coverage(anatomy);
    let model = anatomy.skeleton.model_rest();
    let crowd = &anatomy.skeleton.crowd;
    let mut skin = vec![false; n];
    for s in body.submeshes.iter().filter(|s| s.slot == MaterialSlot::Skin) {
        for &v in &body.indices[s.indices.start as usize..s.indices.end as usize] {
            skin[v as usize] = true;
        }
    }
    let mut cover = vec![Vec3::ZERO; n];
    let mut flow = vec![Vec3::Z; n];
    for i in (0..n).filter(|i| skin[*i]) {
        let region = region_of(body, i as u32);
        cover[i] = Vec3::new(
            f32::from(u8::from(masks.scales.contains(region))),
            f32::from(u8::from(masks.fur.contains(region))),
            f32::from(u8::from(masks.feathers.contains(region))),
        );
        let j = body.joints[i][0];
        let bone = if level == LodLevel::Hero { BoneId(j as u8) } else { crowd.bones[j as usize] };
        let axis = model.get(bone.index()).map_or(Vec3::Z, |m| Vec3::from(m.matrix3.y_axis).normalize_or(Vec3::Z));
        let kind = anatomy.regions.get(region).map_or(RegionKind::Mass, |r| r.kind);
        flow[i] = if flows_backward(kind) { -axis } else { axis };
    }
    // Skin adjacency from the triangles.
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); n];
    for s in body.submeshes.iter().filter(|s| s.slot == MaterialSlot::Skin) {
        for t in body.indices[s.indices.start as usize..s.indices.end as usize].chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if !neighbours[a as usize].contains(&b) {
                    neighbours[a as usize].push(b);
                    neighbours[b as usize].push(a);
                }
            }
        }
    }
    for _ in 0..ANNOTATE_SMOOTHING {
        let (c0, f0) = (cover.clone(), flow.clone());
        for i in (0..n).filter(|i| skin[*i] && !neighbours[*i].is_empty()) {
            let k = neighbours[i].len() as f32;
            cover[i] = (c0[i] + neighbours[i].iter().map(|j| c0[*j as usize]).sum::<Vec3>() / k) * 0.5;
            flow[i] = f0[i] + neighbours[i].iter().map(|j| f0[*j as usize]).sum::<Vec3>() / k;
        }
    }
    for i in 0..n {
        let nrm = body.normals[i];
        let f = flow[i] - nrm * flow[i].dot(nrm);
        flow[i] = f.normalize_or(nrm.any_orthonormal_vector());
    }
    body.coverage = cover.iter().map(|c| c.to_array()).collect();
    // Distance along the flow (flow.w): a field whose gradient follows the flow, so feather rows
    // stay evenly spaced across bones. Crowd tiers only; hero skin uses shells and cards.
    let mut along: Vec<f32> = (0..n).map(|i| body.positions[i].dot(flow[i])).collect();
    if level != LodLevel::Hero {
        for _ in 0..FLOW_ITERATIONS {
            for i in (0..n).filter(|i| skin[*i] && !neighbours[*i].is_empty()) {
                let p = body.positions[i];
                let sum: f32 = neighbours[i]
                    .iter()
                    .map(|j| {
                        let j = *j as usize;
                        along[j] + (p - body.positions[j]).dot((flow[i] + flow[j]) * 0.5)
                    })
                    .sum();
                along[i] = sum / neighbours[i].len() as f32;
            }
        }
    }
    body.flow = flow.iter().zip(along).map(|(f, u)| f.extend(u).to_array()).collect();
}

/// Gauss-Seidel sweeps integrating the flow into the along-flow coordinate.
const FLOW_ITERATIONS: usize = 150;

/// Which way feathers lie on a region: backward over the body, toward the tip on limbs and tails.
fn flows_backward(kind: RegionKind) -> bool {
    matches!(
        kind,
        RegionKind::TorsoFront
            | RegionKind::TorsoMid
            | RegionKind::TorsoRear
            | RegionKind::Belly
            | RegionKind::Neck
            | RegionKind::Skull
            | RegionKind::Face
            | RegionKind::Snout
            | RegionKind::Jaw
            | RegionKind::Mass
    )
}

/// Feather cards scattered over the covered skin (`density` per m²), deterministic per body.
pub fn feather_cards(
    body: &BodyMeshData,
    anatomy: &Anatomy,
    level: LodLevel,
    mask: RegionMask,
    f: &Feathers,
    density: f32,
) -> Option<BodyMeshData> {
    let model = anatomy.skeleton.model_rest();
    let crowd = &anatomy.skeleton.crowd;
    let full_bone = |j: u16| -> BoneId {
        if level == LodLevel::Hero { BoneId(j as u8) } else { crowd.bones[j as usize] }
    };
    let mut rng = fork(0, "feathers");
    let lift = f.lift_deg.to_radians();
    let mut out = BodyMeshData::default();
    for [a, b, c] in covered_triangles(body, mask) {
        let (pa, pb, pc) = (body.positions[a as usize], body.positions[b as usize], body.positions[c as usize]);
        let area = (pb - pa).cross(pc - pa).length() * 0.5;
        let expected = area * density;
        let count = expected.floor() as u32 + u32::from(rng.random::<f32>() < expected.fract());
        for _ in 0..count {
            let (mut u, mut v) = (rng.random::<f32>(), rng.random::<f32>());
            if u + v > 1.0 {
                (u, v) = (1.0 - u, 1.0 - v);
            }
            let p = pa + (pb - pa) * u + (pc - pa) * v;
            let n = (body.normals[a as usize] * (1.0 - u - v) + body.normals[b as usize] * u + body.normals[c as usize] * v)
                .normalize_or(Vec3::Y);
            let region = region_of(body, a);
            let kind = anatomy.regions.get(region).map_or(RegionKind::Mass, |r| r.kind);
            let bone = full_bone(body.joints[a as usize][0]);
            let axis = Vec3::from(model[bone.index()].matrix3.y_axis).normalize_or(Vec3::Z);
            let flow = if flows_backward(kind) { -axis } else { axis };
            let along = (flow - n * flow.dot(n)).normalize_or(n.any_orthonormal_vector());
            let side = n.cross(along).normalize_or(Vec3::X);
            let length = f.length * (0.8 + 0.4 * rng.random::<f32>());
            let half = f.width * 0.5;
            let root = p - n * 0.002;
            let tip = root + (along * lift.cos() + n * lift.sin()) * length;
            let base = out.positions.len() as u32;
            for (pos, uv) in [
                (root - side * half, [0.0, 0.0]),
                (root + side * half, [1.0, 0.0]),
                (tip - side * half * 0.8, [0.0, 1.0]),
                (tip + side * half * 0.8, [1.0, 1.0]),
            ] {
                out.positions.push(pos);
                out.normals.push(n);
                out.joints.push(body.joints[a as usize]);
                out.weights.push(body.weights[a as usize]);
                out.mob_data.push(body.mob_data[a as usize]);
                out.uvs.push(uv);
                out.coverage.push([0.0, 0.0, 1.0]);
                out.flow.push(along.extend(0.0).to_array());
            }
            out.indices.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
    }
    if out.indices.is_empty() {
        return None;
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len() as u32, slot: MaterialSlot::Skin });
    Some(out)
}
