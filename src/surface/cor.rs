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
//! bones share their candidate triangles too, so each such group gathers them into one compact
//! array and scores all its members against it (cache-friendly; 2–2.6× faster than walking the
//! buckets once per weight vector) — once per work item of up to `GROUP_CHUNK` members, not once
//! per group. The gather also strips each triangle down to the group's bone pairs it actually
//! weights, so the member loop visits only the pairs that score.

use std::collections::HashMap;

use bevy::prelude::*;

use super::BodyMeshData;

use crate::anatomy::LodLevel;
use crate::num::{Clamp, Idx, Saturate, ToF32};

/// The paper's default width of the weight similarity.
pub const SIGMA: f32 = 0.1;

/// Samples in [`exp_table`] over `[0, EXP_LAST]`.
const EXP_STEPS: usize = 2048;
/// Where [`exp_table`] stops. Past it `exp_neg` is zero: the term is under 1e-7 of the largest,
/// and the 3.8 % of calls that land there carry a billionth of the total weight (measured on the
/// biped's hero mesh).
const EXP_LAST: f32 = 16.0;

/// `e^-x` sampled at `i * EXP_LAST / EXP_STEPS`, with one entry past the end for the interpolation.
fn exp_table() -> Vec<f32> {
    (0..=EXP_STEPS + 1).map(|i| (-(i.to_f32() * (EXP_LAST / EXP_STEPS.to_f32()))).exp()).collect()
}

/// `e^-x` for finite `x >= 0`, to about 8e-6 relative, by interpolating [`exp_table`]; zero past
/// `EXP_LAST`. `f32::exp` is a `libm` call per bone pair per triangle, half of this stage's time;
/// the similarity weights don't need its last bits.
#[inline]
#[allow(clippy::suboptimal_flops)] // plain multiplies, for the reason given at `group_centres`
fn exp_neg(table: &[f32], x: f32) -> f32 {
    let t = x * (EXP_STEPS.to_f32() / EXP_LAST);
    let i = t.sat_usize();
    let (Some(&lo), Some(&hi)) = (table.get(i), table.get(i + 1)) else { return 0.0 };
    lo + (t - i.to_f32()) * (hi - lo)
}

type Weights = smallvec::SmallVec<[(u16, f32); 8]>;

/// A candidate triangle's weights on one group's bones, quantized: the key that decides which
/// candidates collapse together in [`group_centres`].
type QWeights = smallvec::SmallVec<[u16; 8]>;

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
    level: LodLevel,
    bones: &[u16],
    members: &[u32],
    weights: &[Weights],
    uniques: &[usize],
    tris: &[Tri],
    buckets: &HashMap<(u16, u16), Vec<u32>>,
    exp: &[f32],
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
    // Per-candidate: the group's bones' weights, the centre and the area, plus the quantized
    // weights that decide which candidates collapse together (below).
    let mut pws: Vec<smallvec::SmallVec<[f32; 8]>> = Vec::with_capacity(candidates.len());
    let mut geom: Vec<(Vec3, f32)> = Vec::with_capacity(candidates.len());
    let mut order: Vec<(QWeights, u32)> = Vec::with_capacity(candidates.len());
    let steps = tri_weight_steps(level);
    for (i, tri) in candidates.iter().filter_map(|&t| tris.get(t.idx())).enumerate() {
        let pw: smallvec::SmallVec<[f32; 8]> = bones.iter().map(|&b| get(&tri.weights, b)).collect();
        order.push((pw.iter().map(|w| (w * steps).round().sat_u16()).collect(), i.clamp_u32()));
        geom.push((tri.centre, tri.area));
        pws.push(pw);
    }
    // Collapse candidates that weight the group's bones alike. The similarity depends on the
    // triangle only through those weights, so two candidates with the same weights score the same
    // against *every* member, and summing `area` and `area × centre` over them lets one scoring
    // stand for all of them. Quantizing the weights first makes the collapse much stronger for a
    // bounded error (see `TRI_WEIGHT_STEPS`), and is the same trick the vertex side already uses
    // to share one centre per weight vector.
    //
    // Sorted by the whole quantized vector rather than by a hash of it: a hash collision here
    // would silently merge candidates that weight the bones differently, and the gather is a
    // fraction of a percent of the scoring it feeds, so there is nothing to buy by risking it.
    order.sort_unstable();
    // Per collapsed bucket: (Σ area × centre, Σ area), and only the group's bone pairs the bucket
    // actually weights, with the weights that represent it. Which pairs those are, like `pj`, `pk`
    // and their product, is a property of the bucket and the pair, not of the member being
    // scored: computing it here costs one pass over the candidates instead of one per member.
    let mut centres: Vec<(Vec3, f32)> = Vec::with_capacity(order.len());
    // (index into `pairs`, pj, pk), grouped by bucket; `pair_ends[t]` ends bucket `t`.
    let mut tri_pairs: Vec<(u8, f32, f32)> = Vec::with_capacity(order.len() * 2);
    let mut pair_ends: Vec<u32> = Vec::with_capacity(order.len());
    let mut rep: smallvec::SmallVec<[f32; 8]> = smallvec::SmallVec::new();
    let mut run = 0usize;
    while run < order.len() {
        let Some((key, _)) = order.get(run) else { break };
        let end = order.iter().skip(run).position(|(k, _)| k != key).map_or(order.len(), |n| run + n);
        // The bucket's weights are the area-weighted mean of its members' exact weights, not the
        // grid point they quantized to: it is the same cost here and keeps the first moment of
        // what was collapsed, so the error is smaller than the step alone would give.
        let (mut acc_centre, mut acc_area) = (Vec3::ZERO, 0.0f32);
        rep.clear();
        rep.resize(n, 0.0);
        for &(_, t) in order.get(run..end).unwrap_or_default() {
            let (Some(&(centre, area)), Some(pw)) = (geom.get(t.idx()), pws.get(t.idx())) else { continue };
            acc_centre += centre * area;
            acc_area += area;
            for (r, &w) in rep.iter_mut().zip(pw) {
                *r += w * area;
            }
        }
        if acc_area > 0.0 {
            for r in &mut rep {
                *r /= acc_area;
            }
        }
        for (i, &(j, k)) in pairs.iter().enumerate() {
            let (Some(&pj), Some(&pk)) = (rep.get(j), rep.get(k)) else { continue };
            // A bucket off either bone of the pair adds nothing.
            if pj * pk != 0.0 {
                tri_pairs.push((i.clamp_u8(), pj, pk));
            }
        }
        pair_ends.push(tri_pairs.len().clamp_u32());
        centres.push((acc_centre, acc_area));
        run = end;
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
        let mut start = 0usize;
        for (&end, &(centre, area)) in pair_ends.iter().zip(&centres) {
            let (from, upto) = (start, end.idx());
            start = upto;
            let Some(shared) = tri_pairs.get(from..upto) else { continue };
            let mut sim = 0.0f32;
            for &(i, pj, pk) in shared {
                let Some(&(vj, vk, vjk)) = vp.get(i.idx()) else { continue };
                let e = pk * vj - pj * vk;
                sim += pj * pk * vjk * exp_neg(exp, e * e * inv_s2);
            }
            // `centre` is already Σ area × centre over the bucket and `area` its Σ area, so this
            // is the same sum the per-triangle loop made, with the shared factor taken out.
            let contribution = 2.0 * sim;
            num += centre * contribution;
            den += area * contribution;
        }
        out.push((u, (den > 1e-12).then(|| num / den)));
    }
}

/// Total member×candidate visits the scoring loop makes, before and after the collapse: the
/// quantity the plan's probe predicted (79.5 M → 51.2 M at 1/256 on the biped's hero mesh).
///
/// Counted by repeating the grouping that [`centres_of_rotation`] does, not by instrumenting it,
/// so it costs a normal run nothing. A **proxy for cost, not cost** — see the plan's rule.
#[cfg(test)]
fn collapse_counts(mesh: &BodyMeshData) -> (usize, usize) {
    let weights: Vec<Weights> = mesh.joints.iter().zip(&mesh.weights).map(|(j, w)| sparse(*j, *w)).collect();
    let tri_weights: Vec<Weights> = mesh
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|t| {
            let [a, b, c] = t.map(Idx::idx);
            let mut sum = Weights::new();
            for &(bone, w) in weights.get(a)?.iter().chain(weights.get(b)?).chain(weights.get(c)?) {
                match sum.iter_mut().find(|(j, _)| *j == bone) {
                    Some(e) => e.1 += w / 3.0,
                    None => sum.push((bone, w / 3.0)),
                }
            }
            Some(sum)
        })
        .collect();
    let mut buckets: HashMap<(u16, u16), Vec<u32>> = HashMap::new();
    for (i, w) in (0u32..).zip(&tri_weights) {
        for pair in bone_pairs(w) {
            buckets.entry(pair).or_default().push(i);
        }
    }
    let key = |w: &Weights| {
        let mut q: smallvec::SmallVec<[(u16, u16); 4]> =
            w.iter().map(|(b, x)| (*b, (x * WEIGHT_STEPS).round().sat_u16())).filter(|e| e.1 > 0).collect();
        q.sort_unstable();
        q
    };
    let mut seen: HashMap<smallvec::SmallVec<[(u16, u16); 4]>, usize> = HashMap::new();
    let mut uniques: Vec<usize> = Vec::new();
    for (i, w) in weights.iter().enumerate() {
        let next = uniques.len();
        seen.entry(key(w)).or_insert_with(|| {
            uniques.push(i);
            next
        });
    }
    let mut by_bones: HashMap<smallvec::SmallVec<[u16; 8]>, usize> = HashMap::new();
    for &i in &uniques {
        let Some(w) = weights.get(i).filter(|w| w.len() >= 2) else { continue };
        let mut bones: smallvec::SmallVec<[u16; 8]> = w.iter().map(|(b, _)| *b).collect();
        bones.sort_unstable();
        *by_bones.entry(bones).or_default() += 1;
    }
    // `collapse_counts` reports the hero mesh's ratio, so it reads the hero step.
    let steps = tri_weight_steps(LodLevel::Hero);
    let (mut before, mut after) = (0usize, 0usize);
    for (bones, members) in &by_bones {
        let n = bones.len();
        let mut candidates: Vec<u32> = Vec::new();
        for j in 0..n {
            for k in j + 1..n {
                if let (Some(&a), Some(&b)) = (bones.get(j), bones.get(k))
                    && let Some(list) = buckets.get(&(a.min(b), a.max(b)))
                {
                    candidates.extend_from_slice(list);
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        let mut keys: Vec<QWeights> = candidates
            .iter()
            .filter_map(|&t| tri_weights.get(t.idx()))
            .map(|w| bones.iter().map(|&b| (get(w, b) * steps).round().sat_u16()).collect())
            .collect();
        keys.sort_unstable();
        let distinct = keys.windows(2).filter(|w| w[0] != w[1]).count() + usize::from(!keys.is_empty());
        // Each group's candidates are walked once per member, in chunks of `GROUP_CHUNK`.
        before += members * candidates.len();
        after += members * distinct;
    }
    (before, after)
}

/// Every unordered pair of bones in `w`, smaller first.
fn bone_pairs(w: &Weights) -> impl Iterator<Item = (u16, u16)> + '_ {
    w.iter().enumerate().flat_map(move |(x, &(a, _))| w.iter().skip(x + 1).map(move |&(b, _)| (a.min(b), a.max(b))))
}

/// Steps per unit weight when grouping vertices that share a centre.
const WEIGHT_STEPS: f32 = 256.0;

/// Steps per unit weight when collapsing *candidate triangles* that score alike (see
/// [`group_centres`]), **per LOD level** in [`LodLevel::ALL`] order.
///
/// A different quantity from [`WEIGHT_STEPS`], quantized for a different reason, so it gets its own
/// constant: the vertex side decides which vertices share a centre, this decides which triangles
/// share a scoring.
///
/// **Per level, because the gate says the constraint is not the same at every tier.** Measured at
/// a uniform step, the image gate fails on two unrelated kinds of shot: hero close-ups
/// (`ox_hereford.eye`) and `crowd4000`, which renders at the near and far tiers where meshes are
/// simplified hardest and a fixed step is proportionally largest against fewest vertices. The cost,
/// meanwhile, is almost all at hero, which has ten times the vertices of near. So the step that
/// buys the time and the step the crowd can afford are not the same number.
///
/// The step is **not** monotone in the gate verdict — `crowd4000` measured 1.974 % of pixels at
/// 1/32, **3.084 %** at 1/64 and 1.167 % at 1/128, and 1/64 failed more shots than the coarser
/// 1/32. Quantisation is a lattice, so whether a step aligns with the actual distribution of
/// triangle weights is not ordered by coarseness, and the adaptive contour tips ties either way.
/// **Do not look for a boundary; test the steps you intend to use.**
const TRI_WEIGHT_STEPS: [f32; 3] = [256.0, 256.0, 256.0];

/// The step actually used at `level`. Constant in a normal build; the probes sweep it.
#[cfg(not(test))]
#[inline]
const fn tri_weight_steps(level: LodLevel) -> f32 {
    *level.pick(&TRI_WEIGHT_STEPS)
}

/// Under test the step can be swept, uniformly across levels. An atomic rather than a
/// `thread_local`, because [`group_centres`] runs on [`super::par_jobs`] workers and a
/// thread-local would leave them on the default while the probe thought it had changed the step.
#[cfg(test)]
static TRI_STEPS_OVERRIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(test)]
#[inline]
fn tri_weight_steps(level: LodLevel) -> f32 {
    let bits = TRI_STEPS_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed);
    if bits == 0 { *level.pick(&TRI_WEIGHT_STEPS) } else { f32::from_bits(bits) }
}

/// Runs `f` with the candidate-collapse step forced to `steps` at every level (tests only; not
/// re-entrant).
#[cfg(test)]
fn with_tri_steps<R>(steps: f32, f: impl FnOnce() -> R) -> R {
    use std::sync::atomic::Ordering;
    let before = TRI_STEPS_OVERRIDE.swap(steps.to_bits(), Ordering::Relaxed);
    let out = f();
    TRI_STEPS_OVERRIDE.store(before, Ordering::Relaxed);
    out
}

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
    centres_of_rotation_at(mesh, LodLevel::Hero)
}

/// [`centres_of_rotation`] for a mesh at `level`, which selects the candidate-collapse step (see
/// [`TRI_WEIGHT_STEPS`]).
#[must_use]
pub fn centres_of_rotation_at(mesh: &BodyMeshData, level: LodLevel) -> Vec<Vec3> {
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
    let exp = exp_table();
    let per_item = super::par_jobs(&items, || (), |(), &(bones, members)| {
        let mut out = Vec::new();
        group_centres(level, bones, members, &weights, &uniques, &tris, &buckets, &exp, &mut out);
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
        let table = exp_table();
        let mut worst = 0.0f32;
        for i in 0..=1600 {
            let x = f32::from(i16::try_from(i).unwrap()) * 0.01;
            let (got, want) = (exp_neg(&table, x), (-x).exp());
            worst = worst.max((got - want).abs() / want.max(1e-30));
        }
        assert!(worst < 1e-5, "largest relative error {worst}");
        assert_eq!(exp_neg(&table, 0.0), 1.0);
        assert_eq!(exp_neg(&table, 100.0), 0.0);
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

    /// Chooses [`TRI_WEIGHT_STEPS`]: for each candidate step, how far the centres move from the
    /// paper's formula (the budget `grouped_centres_match_brute_force` polices, 5 mm) and how much
    /// of the scoring work the collapse actually removes.
    ///
    /// Both numbers, side by side, because the second is a *proxy* for cost and the plan's rule is
    /// to count the work and not a proxy for it: the visit count is here to be compared against
    /// the plan's prediction, not to stand in for a measured delta.
    #[test]
    #[ignore = "step-choice probe"]
    fn collapse_ratio_and_error_by_step() {
        let anatomy = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let settings = SurfaceSettings::default();
        let mesh = mesh_lod(&AdaptiveContour, &anatomy, LodLevel::Near, settings.get(LodLevel::Near), 1).unwrap();
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
        // The paper's formula per vertex, once: the reference every step is measured against.
        let reference: Vec<Vec3> = weights
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let (mut num, mut den) = (Vec3::ZERO, 0.0f32);
                for (centre, area, p) in &tris {
                    let c = similarity(w, p) * area;
                    num += *centre * c;
                    den += c;
                }
                if w.len() >= 2 && den > 1e-12 { num / den } else { mesh.positions[i] }
            })
            .collect();
        // The error is measured on this mesh because brute force is only feasible here. The
        // collapse ratio is also measured on the biped's *hero* mesh, which is what the plan's
        // prediction was made on and is ten times the size: how much collapses depends on how
        // many candidates a group has, so a small mesh understates it badly.
        let hero = {
            let a = build_src(include_str!("../../assets/mobs/biped.body.ron"));
            mesh_lod(&AdaptiveContour, &a, LodLevel::Hero, settings.get(LodLevel::Hero), 1).unwrap()
        };
        println!("error mesh: {} vertices, {} triangles", weights.len(), tris.len());
        println!("ratio mesh: biped hero, {} vertices, {} triangles", hero.positions.len(), hero.indices.len() / 3);
        for steps in [1024.0f32, 256.0, 128.0, 64.0, 32.0, 16.0, 8.0] {
            let (cor, before, after, hbefore, hafter) = with_tri_steps(steps, || {
                let c = centres_of_rotation(&mesh);
                let (b, a) = collapse_counts(&mesh);
                let (hb, ha) = collapse_counts(&hero);
                (c, b, a, hb, ha)
            });
            let worst = reference.iter().zip(&cor).map(|(r, g)| r.distance(*g)).fold(0.0f32, f32::max);
            let mean = reference.iter().zip(&cor).map(|(r, g)| r.distance(*g)).sum::<f32>() / cor.len().to_f32();
            println!(
                "1/{steps:<6.0} worst {worst_mm:>8.4} mm  mean {mean_mm:>8.4} mm | visits quad {before} -> {after} ({qr:.2}x) | biped hero {hbefore} -> {hafter} ({hr:.2}x)",
                worst_mm = worst * 1000.0,
                mean_mm = mean * 1000.0,
                qr = before.to_f32() / after.max(1).to_f32(),
                hr = hbefore.to_f32() / hafter.max(1).to_f32(),
            );
        }
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

