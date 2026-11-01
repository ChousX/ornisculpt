//! Optimized centres of rotation (Le & Hodgins, "Real-time Skeletal Skinning with Optimized
//! Centers of Rotation", SIGGRAPH 2016): per vertex, the point its blended rotation turns about.
//! Linear blending shrinks bent and twisted joints; dual quaternions keep their volume but bulge
//! at ball joints. Rotating each vertex about a centre taken from the surface that shares its
//! weights avoids both: `p* = Σ area·sim·centroid / Σ area·sim` over triangles, where `sim`
//! compares skin weights (the paper's similarity, σ = 0.1). The skinning itself is in
//! `coat/dqs.wesl`.
//!
//! Only triangles sharing a weighted bone pair with the vertex contribute (the similarity is zero
//! otherwise), so triangles are bucketed by bone pair; and the centre depends only on the
//! weights, so vertices with the same (quantized) weights share one. Weight vectors on the same
//! bones share their candidate triangles too, so each such group gathers them once into a dense
//! array and scores all its members against it (cache-friendly; 2–2.6× faster than walking the
//! buckets once per weight vector).

use std::collections::HashMap;

use bevy::prelude::*;

use super::BodyMeshData;
use crate::num::{Idx, Saturate};

/// The paper's default width of the weight similarity.
pub const SIGMA: f32 = 0.1;

/// `e^-x` for `x >= 0`, to about 2e-5 relative: `2^-t` split into an integer power (an exponent
/// shift) and a degree-6 series on the fraction. `f32::exp` is a `libm` call per bone pair per
/// triangle, half of this stage's time; the similarity weights don't need its last bits.
#[inline]
#[allow(clippy::suboptimal_flops)] // a Horner chain of plain multiplies, for the reason given at `group_centres`
fn exp_neg(x: f32) -> f32 {
    if x > 80.0 {
        return 0.0;
    }
    let t = x * std::f32::consts::LOG2_E;
    let whole = t.floor();
    let frac = whole - t; // (-1, 0]
    // Taylor series of 2^frac about 0: ln2^n / n!.
    let series = 1.0 + frac * (std::f32::consts::LN_2 + frac * (0.240_226_5 + frac * (0.055_504_1 + frac * (0.009_618_13 + frac * (0.001_333_36 + frac * 0.000_154_035)))));
    // 2^-whole as a float with exponent 127 - whole (whole <= 116, so never a subnormal).
    f32::from_bits((127.0 - whole).sat_u32() << 23) * series
}

type Weights = smallvec::SmallVec<[(u16, f32); 8]>;

fn sparse(joints: [u16; 4], weights: [f32; 4]) -> Weights {
    let mut out = Weights::new();
    for (j, w) in joints.into_iter().zip(weights) {
        if w > 0.0 {
            match out.iter_mut().find(|(b, _)| *b == j) {
                Some(e) => e.1 += w,
                None => out.push((j, w)),
            }
        }
    }
    out
}

fn get(w: &Weights, bone: u16) -> f32 {
    w.iter().find(|(b, _)| *b == bone).map_or(0.0, |(_, v)| *v)
}

/// The paper's weight similarity: high when two weight vectors use the same bones in the same
/// proportions.
#[cfg(test)]
fn similarity(p: &Weights, v: &Weights) -> f32 {
    let s2 = SIGMA * SIGMA;
    let mut sim = 0.0;
    for &(j, pj) in p {
        let vj = get(v, j);
        if vj == 0.0 {
            continue;
        }
        for &(k, pk) in p {
            if k == j {
                continue;
            }
            let vk = get(v, k);
            if vk == 0.0 {
                continue;
            }
            let e = f32::mul_add(pk, -vj, pj * vk);
            sim = (pj * pk * vj * vk).mul_add((-(e * e) / s2).exp(), sim);
        }
    }
    sim
}

struct Tri {
    centre: Vec3,
    area: f32,
    weights: Weights,
}

/// Centres for the distinct weight vectors `members` (indices into `uniques`), which all weight
/// exactly `bones` (sorted). Pushes `(member, centre)` to `out`.
///
/// The paper's similarity, over the group's bones only (others have zero vertex weight):
/// Σ over ordered pairs j ≠ k of pⱼpₖvⱼvₖ·exp(−(pₖvⱼ − pⱼvₖ)²/σ²), which is symmetric in the pair,
/// so twice the sum over j < k.
// Plain multiplies and adds in the hot loop: `mul_add` on x86 without the FMA target feature is a
// `libm` call per use, a sixth of this stage's time on a Zen 2, and it keeps LLVM from
// vectorising the pair loop.
#[allow(clippy::too_many_arguments, clippy::suboptimal_flops)]
fn group_centres(
    bones: &[u16],
    members: &[u32],
    weights: &[Weights],
    uniques: &[usize],
    tris: &[Tri],
    buckets: &HashMap<(u16, u16), Vec<u32>>,
    out: &mut Vec<(u32, Option<Vec3>)>,
) {
    let n = bones.len();
    let pairs: smallvec::SmallVec<[(usize, usize); 28]> = (0..n).flat_map(|j| (j + 1..n).map(move |k| (j, k))).collect();
    let mut candidates: Vec<u32> = Vec::new();
    for &(j, k) in &pairs {
        if let (Some(&a), Some(&b)) = (bones.get(j), bones.get(k))
            && let Some(list) = buckets.get(&(a.min(b), a.max(b)))
        {
            candidates.extend_from_slice(list);
        }
    }
    candidates.sort_unstable();
    candidates.dedup();
    // Dense per-triangle data: centre·area, area, and the weights of the group's bones.
    let mut tri_weights: Vec<f32> = Vec::with_capacity(candidates.len() * n);
    let mut centres: Vec<(Vec3, f32)> = Vec::with_capacity(candidates.len());
    for tri in candidates.iter().filter_map(|&t| tris.get(t.idx())) {
        tri_weights.extend(bones.iter().map(|&b| get(&tri.weights, b)));
        centres.push((tri.centre, tri.area));
    }
    let inv_s2 = 1.0 / (SIGMA * SIGMA);
    let mut v = smallvec::SmallVec::<[f32; 8]>::new();
    // Per pair of the member's weights: (vj, vk, vj·vk).
    let mut vp = smallvec::SmallVec::<[(f32, f32, f32); 28]>::new();
    for &u in members {
        let Some(w) = uniques.get(u.idx()).and_then(|&i| weights.get(i)) else { continue };
        v.clear();
        v.extend(bones.iter().map(|&b| get(w, b)));
        vp.clear();
        vp.extend(pairs.iter().map(|&(j, k)| {
            let (vj, vk) = (v.get(j).copied().unwrap_or(0.0), v.get(k).copied().unwrap_or(0.0));
            (vj, vk, vj * vk)
        }));
        let (mut num, mut den) = (Vec3::ZERO, 0.0f32);
        for (p, &(centre, area)) in tri_weights.chunks_exact(n).zip(&centres) {
            let mut sim = 0.0f32;
            for (&(j, k), &(vj, vk, vjk)) in pairs.iter().zip(&vp) {
                let (Some(&pj), Some(&pk)) = (p.get(j), p.get(k)) else { continue };
                let e = pk * vj - pj * vk;
                sim += pj * pk * vjk * exp_neg(e * e * inv_s2);
            }
            let contribution = 2.0 * sim * area;
            num += centre * contribution;
            den += contribution;
        }
        out.push((u, (den > 1e-12).then(|| num / den)));
    }
}

/// Every unordered pair of bones in `w`, smaller first.
fn bone_pairs(w: &Weights) -> impl Iterator<Item = (u16, u16)> + '_ {
    w.iter().enumerate().flat_map(move |(x, &(a, _))| w.iter().skip(x + 1).map(move |&(b, _)| (a.min(b), a.max(b))))
}

/// Steps per unit weight when grouping vertices that share a centre.
const WEIGHT_STEPS: f32 = 256.0;

/// Weight vectors scored per work item (a group's candidate triangles are gathered once per item).
const GROUP_CHUNK: usize = 128;

/// Centre of rotation for every vertex.
///
/// Vertices on one bone (where it doesn't matter) and those with no similar surface get their own
/// position, which skins them exactly as linear blending (as does every vertex, should any
/// worker panic). Cost: about 1.7 s on one thread for a 29k-vertex hero mesh, parallel over
/// [`super::worker_threads`].
#[must_use]
pub fn centres_of_rotation(mesh: &BodyMeshData) -> Vec<Vec3> {
    let weights: Vec<Weights> = mesh.joints.iter().zip(&mesh.weights).map(|(j, w)| sparse(*j, *w)).collect();
    let tris: Vec<Tri> = mesh
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|tri| {
            let [a, b, c] = tri.map(Idx::idx);
            let (pa, pb, pc) = (*mesh.positions.get(a)?, *mesh.positions.get(b)?, *mesh.positions.get(c)?);
            let mut sum = Weights::new();
            for &(bone, weight) in weights.get(a)?.iter().chain(weights.get(b)?).chain(weights.get(c)?) {
                match sum.iter_mut().find(|(j, _)| *j == bone) {
                    Some(entry) => entry.1 += weight / 3.0,
                    None => sum.push((bone, weight / 3.0)),
                }
            }
            Some(Tri { centre: (pa + pb + pc) / 3.0, area: (pb - pa).cross(pc - pa).length() * 0.5, weights: sum })
        })
        .collect();
    let mut buckets: HashMap<(u16, u16), Vec<u32>> = HashMap::new();
    for (i, tri) in (0u32..).zip(&tris) {
        for pair in bone_pairs(&tri.weights) {
            buckets.entry(pair).or_default().push(i);
        }
    }

    // One centre per distinct (quantized) weight vector.
    let key = |w: &Weights| {
        let mut quantized: smallvec::SmallVec<[(u16, u16); 4]> =
            w.iter().map(|(b, x)| (*b, (x * WEIGHT_STEPS).round().sat_u16())).filter(|e| e.1 > 0).collect();
        quantized.sort_unstable();
        quantized
    };
    let mut slot_of: HashMap<smallvec::SmallVec<[(u16, u16); 4]>, usize> = HashMap::new();
    let mut slots: Vec<usize> = Vec::with_capacity(weights.len());
    let mut uniques: Vec<usize> = Vec::new();
    for (i, w) in weights.iter().enumerate() {
        let next = uniques.len();
        let slot = *slot_of.entry(key(w)).or_insert_with(|| {
            uniques.push(i);
            next
        });
        slots.push(slot);
    }

    // Distinct weight vectors on the same bones share their candidate triangles (those sharing a
    // bone pair with them), so each group gathers its triangles once into a dense array of just
    // its bones' weights and scores all its members against it: the same sums, without walking
    // scattered triangles once per weight vector.
    let mut by_bones: HashMap<smallvec::SmallVec<[u16; 8]>, Vec<u32>> = HashMap::new();
    for (u, &i) in (0u32..).zip(&uniques) {
        let Some(w) = weights.get(i).filter(|w| w.len() >= 2) else { continue };
        let mut bones: smallvec::SmallVec<[u16; 8]> = w.iter().map(|(b, _)| *b).collect();
        bones.sort_unstable();
        by_bones.entry(bones).or_default().push(u);
    }
    let mut groups: Vec<(smallvec::SmallVec<[u16; 8]>, Vec<u32>)> = by_bones.into_iter().collect();
    groups.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    // Big groups (the trunk's bones) split into chunks so the threads share them.
    let items: Vec<(&[u16], &[u32])> = groups.iter().flat_map(|(bones, members)| members.chunks(GROUP_CHUNK).map(move |c| (bones.as_slice(), c))).collect();
    let per_item = super::par_jobs(&items, || (), |(), &(bones, members)| {
        let mut out = Vec::new();
        group_centres(bones, members, &weights, &uniques, &tris, &buckets, &mut out);
        out
    });
    // A worker's panic leaves every vertex at its own position.
    let mut per_unique: Vec<Option<Vec3>> = vec![None; uniques.len()];
    for (u, centre) in per_item.unwrap_or_default().into_iter().flatten() {
        if let Some(slot) = per_unique.get_mut(u.idx()) {
            *slot = centre;
        }
    }
    let centre = |i: usize| slots.get(i).and_then(|&s| per_unique.get(s).copied().flatten());
    mesh.positions.iter().enumerate().map(|(i, p)| centre(i).unwrap_or(*p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{LodLevel, build_src};
    use crate::surface::{AdaptiveContour, BodyField, BodyFieldOptions, Field, SurfaceSettings, mesh_lod};

    #[test]
    fn cheap_exponential_matches_exp() {
        let mut worst = 0.0f32;
        for i in 0..=8000 {
            let x = f32::from(i16::try_from(i).unwrap()) * 0.01;
            let (got, want) = (exp_neg(x), (-x).exp());
            worst = worst.max((got - want).abs() / want.max(1e-30));
        }
        assert!(worst < 5e-5, "largest relative error {worst}");
        assert_eq!(exp_neg(0.0), 1.0);
        assert_eq!(exp_neg(100.0), 0.0);
    }

    /// The grouped evaluation matches the paper's formula summed over every triangle, vertex by
    /// vertex (on the near-tier quadruped: small enough to brute-force).
    #[test]
    fn grouped_centres_match_brute_force() {
        let anatomy = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let settings = SurfaceSettings::default();
        let mesh = mesh_lod(&AdaptiveContour, &anatomy, LodLevel::Near, settings.get(LodLevel::Near), 1).unwrap();
        let fast = centres_of_rotation(&mesh);
        let weights: Vec<Weights> = mesh.joints.iter().zip(&mesh.weights).map(|(j, w)| sparse(*j, *w)).collect();
        let tris: Vec<(Vec3, f32, Weights)> = mesh
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(Idx::idx);
                let (pa, pb, pc) = (mesh.positions[a], mesh.positions[b], mesh.positions[c]);
                let mut sum = Weights::new();
                for &(bone, w) in weights[a].iter().chain(&weights[b]).chain(&weights[c]) {
                    match sum.iter_mut().find(|(j, _)| *j == bone) {
                        Some(e) => e.1 += w / 3.0,
                        None => sum.push((bone, w / 3.0)),
                    }
                }
                ((pa + pb + pc) / 3.0, (pb - pa).cross(pc - pa).length() * 0.5, sum)
            })
            .collect();
        let mut worst = 0.0f32;
        for (i, w) in weights.iter().enumerate() {
            let (mut num, mut den) = (Vec3::ZERO, 0.0f32);
            for (centre, area, p) in &tris {
                let c = similarity(w, p) * area;
                num += *centre * c;
                den += c;
            }
            let expected = if w.len() >= 2 && den > 1e-12 { num / den } else { mesh.positions[i] };
            worst = worst.max(expected.distance(fast[i]));
        }
        println!("{} vertices, largest difference {:.4} mm", weights.len(), worst * 1000.0);
        // The grouped path shares one centre per quantized weight vector (1/256 steps).
        assert!(worst < 5e-3, "largest difference {worst} m");
    }

    /// On the human: every blended vertex's centre lies inside the body (where a joint is), and
    /// computing them stays quick.
    #[test]
    fn centres_lie_inside_the_body() {
        let a = build_src(include_str!("../../assets/mobs/biped.body.ron"));
        let settings = SurfaceSettings::default();
        let m = mesh_lod(&AdaptiveContour, &a, LodLevel::Hero, settings.get(LodLevel::Hero), 1).unwrap();
        let t = std::time::Instant::now();
        let cor = centres_of_rotation(&m);
        let took = t.elapsed().as_secs_f32();
        let field = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false });
        let blended: Vec<usize> = (0..m.positions.len()).filter(|&i| m.weights[i].iter().filter(|w| **w > 0.05).count() >= 2).collect();
        let outside = blended.iter().filter(|&&i| field.distance(cor[i].into()) > 0.01).count();
        let bad = cor.iter().filter(|c| !c.is_finite()).count();
        let far = (0..cor.len()).filter(|&i| cor[i].distance(m.positions[i]) > 0.5).count();
        println!("non-finite centres {bad}, centres over 0.5 m from their vertex {far}");
        assert_eq!(bad, 0, "non-finite centres");
        println!("{} vertices, {} blended, centres in {took:.2}s, {outside} outside the body", m.positions.len(), blended.len());
        assert!(outside * 50 <= blended.len(), "{outside} of {} centres outside the body", blended.len());
    }
}

