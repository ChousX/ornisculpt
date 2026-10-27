//! Coat geometry built at generation time from a body LOD mesh: fur shells and feather cards.
//! Both reuse the body's skinning (same joint space as the LOD they are built from).

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::{Coat, Feathers};
use crate::anatomy::{Anatomy, BoneId, LodLevel, RegionId, RegionKind, RegionMask};
use crate::num::{Clamp, Idx, Saturate, ToF32};
use crate::random::{RngExt, fork};
use crate::surface::{BodyMeshData, MaterialSlot, MobVertexData, Submesh, Vertex, VertexChannel, simplify};

/// Fraction of the hero skin's triangles used as the source for fur shells.
const SHELL_SOURCE_RATIO: f32 = 0.2;

/// Extra coat geometry for one LOD level.
#[derive(Clone, Debug, Default)]
pub struct CoatMeshes {
    pub shells: Option<BodyMeshData>,
    pub feathers: Option<BodyMeshData>,
}

/// Builds the coat geometry for one LOD level.
///
/// Shells and feather cards exist only at hero detail; crowd tiers shade fur and feather regions
/// directly on the skin (alpha-tested layers defeat early depth rejection, which is unaffordable
/// across thousands of mobs).
#[must_use]
pub fn build_coat_meshes(coat: &Coat, anatomy: &Anatomy, body: &BodyMeshData, level: LodLevel) -> CoatMeshes {
    let masks = coat.coverage(anatomy);
    let detail = match level {
        LodLevel::Hero => 1.0,
        LodLevel::Near | LodLevel::Far => 0.0,
    };
    let shells = coat.fur().map_or(0, |f| (f32::from(f.shells) * detail).round().sat_u32());
    let feathers = coat.feathers();
    // Fur hides fine surface detail, so shells use a simplified copy of the skin: every shell
    // multiplies its triangle count and they are alpha-tested.
    let shell_source = (shells > 0).then(|| simplify(body, SHELL_SOURCE_RATIO, 0.01));
    let lengths = coat.fur_lengths(anatomy);
    CoatMeshes {
        shells: shell_source.and_then(|s| fur_shells(&s, masks.fur, shells, &lengths)),
        feathers: feathers
            .filter(|_| detail > 0.0)
            .and_then(|f| feather_cards(body, anatomy, level, masks.feathers, &f, f.density * detail)),
    }
}

fn region_of(body: &BodyMeshData, v: usize) -> Option<RegionId> {
    body.mob_data.get(v).map(|d| MobVertexData::unpack(*d).region)
}

/// The full-skeleton bone of joint `j` at `level` (crowd levels index the crowd skeleton).
fn full_bone(anatomy: &Anatomy, level: LodLevel, j: u16) -> Option<BoneId> {
    if level == LodLevel::Hero {
        Some(BoneId(j.clamp_u8()))
    } else {
        anatomy.skeleton.crowd.bones.get(j.idx()).copied()
    }
}

/// Skin triangles whose vertices are all complete and in `mask`.
fn covered_triangles(body: &BodyMeshData, mask: RegionMask) -> impl Iterator<Item = [u32; 3]> + '_ {
    body.slot_triangles(MaterialSlot::Skin).copied().filter(move |t| {
        t.iter().all(|v| body.vertex(v.idx()).is_some() && region_of(body, v.idx()).is_some_and(|r| mask.contains(r)))
    })
}

/// `shells` offset copies of the covered skin; `uvs` = (shell height (0..1], the fur's length at
/// the vertex in metres, by its region from `lengths`).
#[must_use]
fn fur_shells(body: &BodyMeshData, mask: RegionMask, shells: u32, lengths: &[f32; RegionId::MAX]) -> Option<BodyMeshData> {
    let tris: Vec<[u32; 3]> = covered_triangles(body, mask).collect();
    if tris.is_empty() {
        return None;
    }
    // Compact the covered vertices once.
    let mut remap = vec![u32::MAX; body.positions.len()];
    let mut used = Vec::new();
    for &v in tris.iter().flatten() {
        if let Some(r) = remap.get_mut(v.idx())
            && *r == u32::MAX
        {
            *r = used.len().clamp_u32();
            used.push(v);
        }
    }
    let n = used.len().clamp_u32();
    let mut out = BodyMeshData::default();
    for k in 1..=shells {
        let h = k.to_f32() / shells.to_f32();
        // Covered triangles only reference complete vertices.
        for (i, v) in used.iter().filter_map(|&v| Some((v.idx(), body.vertex(v.idx())?))) {
            out.push_vertex(v);
            // No coverage on shells or cards: a skin mesh with coverage puts coat weights in UV0
            // in place of these coordinates (see `BodyMeshData::build_mesh`).
            let length = region_of(body, i).and_then(|r| lengths.get(usize::from(r.0))).copied().unwrap_or(0.0);
            out.uvs.push([h, length]);
            out.flow.push(body.flow.get(i).copied().unwrap_or([0.0, 0.0, 1.0, 0.0]));
            out.push_from(body, i, &[VertexChannel::Paint, VertexChannel::Cor, VertexChannel::Muscle, VertexChannel::Soft]);
        }
        let base = (k - 1) * n;
        out.indices.extend(tris.iter().flat_map(|t| t.map(|v| base + remap.get(v.idx()).copied().unwrap_or(0))));
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len().clamp_u32(), slot: MaterialSlot::Skin });
    Some(out)
}

/// Smoothing passes over the coverage and flow (softens covering borders over a few triangles).
const ANNOTATE_SMOOTHING: usize = 3;

/// Bakes coat inputs into a body LOD mesh.
///
/// These are per-vertex coverage weights (scales, fur, feathers) and the surface flow (bone axis,
/// tangent to the skin, signed by region). Both are smoothed across the skin so covering borders
/// and bone boundaries blend instead of stepping.
pub fn annotate(body: &mut BodyMeshData, coat: &Coat, anatomy: &Anatomy, level: LodLevel) {
    super::markings::paint(body, coat, anatomy, level);
    let n = body.positions.len();
    body.coverage.clear();
    // Non-skin layers (wing sheets) bring their own flow; keep it.
    let prior_flow = std::mem::take(&mut body.flow);
    if coat.coverings.is_empty() {
        body.flow = prior_flow;
        return;
    }
    let masks = coat.coverage(anatomy);
    let model = anatomy.skeleton.rest_model();
    let skin = body.skin_vertex_mask();
    let vertex_coat = |i: usize| -> Option<(Vec3, Vec3)> {
        let region = region_of(body, i)?;
        let cover = Vec3::new(
            f32::from(u8::from(masks.scales.contains(region))),
            f32::from(u8::from(masks.fur.contains(region))),
            f32::from(u8::from(masks.feathers.contains(region))),
        );
        Some((cover, flow_axis(anatomy, &model, level, region, body.joints.get(i)?[0])))
    };
    let (mut cover, mut flow): (Vec<Vec3>, Vec<Vec3>) = skin
        .iter()
        .enumerate()
        .map(|(i, &s)| s.then(|| vertex_coat(i)).flatten().unwrap_or((Vec3::ZERO, Vec3::Z)))
        .unzip();
    // Skin adjacency from the triangles.
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &[a, b, c] in body.slot_triangles(MaterialSlot::Skin) {
        for (a, b) in [(a, b), (b, c), (c, a)] {
            if b.idx() >= n || neighbours.get(a.idx()).is_none_or(|ns| ns.contains(&b)) {
                continue;
            }
            if let Some(ns) = neighbours.get_mut(a.idx()) {
                ns.push(b);
            }
            if let Some(ns) = neighbours.get_mut(b.idx()) {
                ns.push(a);
            }
        }
    }
    for _ in 0..ANNOTATE_SMOOTHING {
        let (c0, f0) = (cover.clone(), flow.clone());
        for (((c, f), ns), _) in cover.iter_mut().zip(&mut flow).zip(&neighbours).zip(&skin).filter(|(_, s)| **s) {
            if ns.is_empty() {
                continue;
            }
            let k = ns.len().to_f32();
            let mean = |of: &[Vec3]| ns.iter().filter_map(|j| of.get(j.idx())).sum::<Vec3>() / k;
            *c = (*c + mean(&c0)) * 0.5;
            *f += mean(&f0);
        }
    }
    for (f, &nrm) in flow.iter_mut().zip(&body.normals) {
        let t = *f - nrm * f.dot(nrm);
        *f = t.normalize_or(nrm.any_orthonormal_vector());
    }
    body.coverage = cover.iter().map(Vec3::to_array).collect();
    // Distance along the flow (flow.w): a field whose gradient follows the flow, so feather rows
    // stay evenly spaced across bones. Crowd tiers only; hero skin uses shells and cards.
    let mut along: Vec<f32> = body.positions.iter().zip(&flow).map(|(p, f)| p.dot(*f)).collect();
    if level != LodLevel::Hero {
        for _ in 0..FLOW_ITERATIONS {
            for (i, ns) in neighbours.iter().enumerate() {
                let (Some(true), Some(&p), Some(&fi)) = (skin.get(i).copied(), body.positions.get(i), flow.get(i)) else {
                    continue;
                };
                if ns.is_empty() {
                    continue;
                }
                let sum: f32 = ns
                    .iter()
                    .filter_map(|j| {
                        let j = j.idx();
                        Some(along.get(j)? + (p - *body.positions.get(j)?).dot((fi + *flow.get(j)?) * 0.5))
                    })
                    .sum();
                if let Some(a) = along.get_mut(i) {
                    *a = sum / ns.len().to_f32();
                }
            }
        }
    }
    body.flow = flow
        .iter()
        .zip(along)
        .zip(&skin)
        .enumerate()
        .map(|(i, ((f, u), &s))| {
            let own = f.extend(u).to_array();
            if s { own } else { prior_flow.get(i).copied().unwrap_or(own) }
        })
        .collect();
}

/// Gauss-Seidel sweeps integrating the flow into the along-flow coordinate.
const FLOW_ITERATIONS: usize = 150;

/// The way the coat lies at a vertex of `region` whose first joint is `joint`: along its bone,
/// backward or toward the tip as [`flows_backward`] says.
fn flow_axis(anatomy: &Anatomy, model: &[Affine3A], level: LodLevel, region: RegionId, joint: u16) -> Vec3 {
    let axis = full_bone(anatomy, level, joint)
        .and_then(|b| model.get(b.index()))
        .map_or(Vec3::Z, |m| Vec3::from(m.matrix3.y_axis).normalize_or(Vec3::Z));
    let kind = anatomy.regions.get(region).map_or(RegionKind::Mass, |r| r.kind);
    if flows_backward(kind) { -axis } else { axis }
}

/// Which way feathers lie on a region: backward over the body, toward the tip on limbs and tails.
const fn flows_backward(kind: RegionKind) -> bool {
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
#[must_use]
fn feather_cards(
    body: &BodyMeshData,
    anatomy: &Anatomy,
    level: LodLevel,
    mask: RegionMask,
    feathers: &Feathers,
    density: f32,
) -> Option<BodyMeshData> {
    let model = anatomy.skeleton.rest_model();
    let mut rng = fork(0, "feathers");
    let lift = feathers.lift_deg.to_radians();
    let mut out = BodyMeshData::default();
    for [a, b, c] in covered_triangles(body, mask) {
        let (Some(va), Some(vb), Some(vc)) = (body.vertex(a.idx()), body.vertex(b.idx()), body.vertex(c.idx())) else {
            continue;
        };
        let (pa, pb, pc) = (va.position, vb.position, vc.position);
        let flow = flow_axis(anatomy, &model, level, MobVertexData::unpack(va.mob_data).region, va.joints[0]);
        let area = (pb - pa).cross(pc - pa).length() * 0.5;
        let expected = area * density;
        let count = expected.floor().sat_u32() + u32::from(rng.random::<f32>() < expected.fract());
        for _ in 0..count {
            let (mut u, mut v) = (rng.random::<f32>(), rng.random::<f32>());
            if u + v > 1.0 {
                (u, v) = (1.0 - u, 1.0 - v);
            }
            let p = pa + (pb - pa) * u + (pc - pa) * v;
            let n = (va.normal * (1.0 - u - v) + vb.normal * u + vc.normal * v).normalize_or(Vec3::Y);
            let along = (flow - n * flow.dot(n)).normalize_or(n.any_orthonormal_vector());
            let side = n.cross(along).normalize_or(Vec3::X);
            let length = feathers.length * 0.4f32.mul_add(rng.random::<f32>(), 0.8);
            let half = feathers.width * 0.5;
            let root = p - n * 0.002;
            let tip = root + (along * lift.cos() + n * lift.sin()) * length;
            let base = out.positions.len().clamp_u32();
            for (position, uv) in [
                (root - side * half, [0.0, 0.0]),
                (root + side * half, [1.0, 0.0]),
                (tip - side * half * 0.8, [0.0, 1.0]),
                (tip + side * half * 0.8, [1.0, 1.0]),
            ] {
                out.push_vertex(Vertex { position, normal: n, ..va });
                out.uvs.push(uv);
                out.flow.push(along.extend(0.0).to_array());
                // A card turns about its root vertex's centre (it shares that vertex's weights),
                // and rides its root vertex's muscle and soft tissue.
                out.push_from(body, a.idx(), &[VertexChannel::Paint, VertexChannel::Cor, VertexChannel::Muscle, VertexChannel::Soft]);
            }
            out.indices.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
    }
    if out.indices.is_empty() {
        return None;
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len().clamp_u32(), slot: MaterialSlot::Skin });
    Some(out)
}
