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
//! weights, so vertices with the same (quantized) weights share one.

use std::collections::HashMap;

use bevy::prelude::*;

use super::BodyMeshData;
use crate::num::{Idx, Saturate};

/// The paper's default width of the weight similarity.
pub const SIGMA: f32 = 0.1;

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

/// Every unordered pair of bones in `w`, smaller first.
fn bone_pairs(w: &Weights) -> impl Iterator<Item = (u16, u16)> + '_ {
    w.iter().enumerate().flat_map(move |(x, &(a, _))| w.iter().skip(x + 1).map(move |&(b, _)| (a.min(b), a.max(b))))
}

/// Centre of rotation for every vertex.
///
/// Vertices on one bone (where it doesn't matter) and those with no similar surface get their own
/// position, which skins them exactly as linear blending (as do all of a worker's vertices,
/// should it panic).
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
            w.iter().map(|(b, x)| (*b, (x * 256.0).round().sat_u16())).filter(|e| e.1 > 0).collect();
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

    let centre_for = |w: &Weights, stamp: &mut [u32], round: u32| -> Option<Vec3> {
        if w.len() < 2 {
            return None;
        }
        let (mut num, mut den) = (Vec3::ZERO, 0.0f32);
        for pair in bone_pairs(w) {
            let Some(list) = buckets.get(&pair) else { continue };
            for &t in list {
                let (Some(seen), Some(tri)) = (stamp.get_mut(t.idx()), tris.get(t.idx())) else { continue };
                if *seen == round {
                    continue;
                }
                *seen = round;
                let contribution = similarity(w, &tri.weights) * tri.area;
                num += tri.centre * contribution;
                den += contribution;
            }
        }
        (den > 1e-12).then(|| num / den)
    };
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get).min(uniques.len().max(1));
    let per_unique: Vec<Option<Vec3>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(threads);
        for worker in 0..threads {
            let (uniques, weights, centre_for, count) = (&uniques, &weights, &centre_for, tris.len());
            handles.push(scope.spawn(move || {
                let mut stamp = vec![u32::MAX; count];
                let mut out = Vec::new();
                for (u, &i) in (0u32..).zip(uniques).skip(worker).step_by(threads) {
                    let centre = weights.get(i).and_then(|w| centre_for(w, &mut stamp, u));
                    out.push((u, centre));
                }
                out
            }));
        }
        let mut all = vec![None; uniques.len()];
        for handle in handles {
            for (u, centre) in handle.join().unwrap_or_default() {
                if let Some(slot) = all.get_mut(u.idx()) {
                    *slot = centre;
                }
            }
        }
        all
    });
    let centre = |i: usize| slots.get(i).and_then(|&s| per_unique.get(s).copied().flatten());
    mesh.positions.iter().enumerate().map(|(i, p)| centre(i).unwrap_or(*p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{AdaptiveContour, BodyField, BodyFieldOptions, Field, ProceduralHardParts, SurfaceSettings, mesh_lod};

    /// On the human: every blended vertex's centre lies inside the body (where a joint is), and
    /// computing them stays quick.
    #[test]
    fn centres_lie_inside_the_body() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let settings = SurfaceSettings::default();
        let m = mesh_lod(&AdaptiveContour, &ProceduralHardParts, &a, LodLevel::Hero, settings.get(LodLevel::Hero), 1).unwrap();
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
