//! Signed distances from a watertight mesh to a grid of cells, in two passes. First every
//! triangle measures the cells within a cell of it exactly. Then the rest of the grid fills by
//! jump flooding: cells adopt the nearest triangles their neighbours found and measure
//! themselves against them, so each cell tests a handful of triangles instead of searching a
//! tree. Signs come from the angle-weighted pseudo-normal of the nearest feature (Bærentzen &
//! Aanæs 2005), exact for a watertight, consistently wound mesh.
//!
//! A bounding-volume query per cell spends most of its time on box distances high in the tree,
//! and all the more the farther the cell is from the surface, which most of a solid's cells are:
//! the sculpted head's 1.75 M cells took 2.7 s on sixteen Zen 2 threads that way (87 % of it in
//! the traversal), against 0.53 s here (band 10 ms, flood 450 ms, signs the rest). The two
//! agree to under a millimetre at all but 88 cells, and at those, and at the 109 whose signs
//! differ, an exhaustive search sides with this one every time: the tree's sign was the nearest
//! face's own normal, wrong beside concave edges.

// Plain multiplies and adds throughout: `mul_add` on x86 without the FMA target feature is a
// `libm` call per use (see `surface::cor`), and this is the hot loop of a head's conversion.
#![allow(clippy::suboptimal_flops)]

use std::collections::HashMap;

use bevy::prelude::*;

use crate::num::{Clamp, Idx, ToF32};
use crate::surface::{par_map, scope, worker_threads};

/// Cells past a triangle's bounds, each way, that it measures exactly.
const BAND: u32 = 1;

/// Which part of a triangle a point is nearest.
#[derive(Clone, Copy)]
enum Feature {
    A,
    B,
    C,
    Ab,
    Bc,
    Ca,
    Face,
}

/// The nearest point of a triangle to a query, and how far it is (squared).
struct Hit {
    d2: f32,
    q: Vec3,
    feature: Feature,
}

/// A triangle with the pseudo-normals of its features.
struct Tri {
    a: Vec3,
    b: Vec3,
    c: Vec3,
    face: Vec3,
    at_a: Vec3,
    at_b: Vec3,
    at_c: Vec3,
    on_ab: Vec3,
    on_bc: Vec3,
    on_ca: Vec3,
}

impl Tri {
    /// Ericson's closest point on a triangle (Real-Time Collision Detection §5.1.5), naming
    /// the region the point fell in.
    #[allow(clippy::many_single_char_names, clippy::similar_names)]
    fn closest(&self, p: Vec3) -> Hit {
        let hit = |q: Vec3, feature: Feature| Hit { d2: p.distance_squared(q), q, feature };
        let (a, b, c) = (self.a, self.b, self.c);
        let (ab, ac, ap) = (b - a, c - a, p - a);
        let (d1, d2) = (ab.dot(ap), ac.dot(ap));
        if d1 <= 0.0 && d2 <= 0.0 {
            return hit(a, Feature::A);
        }
        let bp = p - b;
        let (d3, d4) = (ab.dot(bp), ac.dot(bp));
        if d3 >= 0.0 && d4 <= d3 {
            return hit(b, Feature::B);
        }
        let vc = d1 * d4 - d3 * d2;
        if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            return hit(a + ab * (d1 / (d1 - d3)), Feature::Ab);
        }
        let cp = p - c;
        let (d5, d6) = (ab.dot(cp), ac.dot(cp));
        if d6 >= 0.0 && d5 <= d6 {
            return hit(c, Feature::C);
        }
        let vb = d5 * d2 - d1 * d6;
        if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            return hit(a + ac * (d2 / (d2 - d6)), Feature::Ca);
        }
        let va = d3 * d6 - d5 * d4;
        if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
            return hit(b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))), Feature::Bc);
        }
        let denom = 1.0 / (va + vb + vc);
        hit(a + ab * (vb * denom) + ac * (vc * denom), Feature::Face)
    }

    /// Pseudo-normal of a feature: the face's, an edge's (its two faces summed) or a vertex's
    /// (its faces weighted by their angle there).
    const fn normal(&self, feature: Feature) -> Vec3 {
        match feature {
            Feature::A => self.at_a,
            Feature::B => self.at_b,
            Feature::C => self.at_c,
            Feature::Ab => self.on_ab,
            Feature::Bc => self.on_bc,
            Feature::Ca => self.on_ca,
            Feature::Face => self.face,
        }
    }

    /// Signed distance of `p`: negative behind the nearest feature's pseudo-normal.
    fn signed(&self, p: Vec3) -> f32 {
        let hit = self.closest(p);
        let d = hit.d2.sqrt();
        if (p - hit.q).dot(self.normal(hit.feature)) < 0.0 { -d } else { d }
    }

    fn bounds(&self) -> (Vec3, Vec3) {
        (self.a.min(self.b).min(self.c), self.a.max(self.b).max(self.c))
    }

    /// Bounding sphere: centroid and the farthest corner from it.
    fn sphere(&self) -> Vec4 {
        let centre = (self.a + self.b + self.c) / 3.0;
        centre.extend([self.a, self.b, self.c].iter().map(|v| v.distance(centre)).fold(0.0, f32::max))
    }
}

/// Angle at `a` between `ab` and `ac`.
fn corner_angle(ab: Vec3, ac: Vec3) -> f32 {
    ab.normalize_or_zero().dot(ac.normalize_or_zero()).clamp(-1.0, 1.0).acos()
}

/// Triangles of an outward-wound mesh with their pseudo-normals, degenerate ones dropped.
fn triangles(positions: &[Vec3], indices: &[u32]) -> Vec<Tri> {
    let corners: Vec<([u32; 3], [Vec3; 3], Vec3)> = indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|&ids| {
            let [a, b, c] = ids.map(|i| positions.get(i.idx()).copied());
            let (a, b, c) = (a?, b?, c?);
            let n = (b - a).cross(c - a);
            (n.length_squared() > 1e-30).then(|| (ids, [a, b, c], n.normalize()))
        })
        .collect();
    let mut at_vertex: Vec<Vec3> = vec![Vec3::ZERO; positions.len()];
    let mut on_edge: HashMap<(u32, u32), Vec3> = HashMap::new();
    let edge = |i: u32, j: u32| (i.min(j), i.max(j));
    for &([ia, ib, ic], [a, b, c], n) in &corners {
        for (i, angle) in [(ia, corner_angle(b - a, c - a)), (ib, corner_angle(c - b, a - b)), (ic, corner_angle(a - c, b - c))] {
            if let Some(v) = at_vertex.get_mut(i.idx()) {
                *v += n * angle;
            }
        }
        for e in [edge(ia, ib), edge(ib, ic), edge(ic, ia)] {
            *on_edge.entry(e).or_default() += n;
        }
    }
    let at = |i: u32| at_vertex.get(i.idx()).copied().unwrap_or(Vec3::ZERO);
    let on = |i: u32, j: u32| on_edge.get(&edge(i, j)).copied().unwrap_or(Vec3::ZERO);
    corners
        .iter()
        .map(|&([ia, ib, ic], [a, b, c], face)| Tri {
            a,
            b,
            c,
            face,
            at_a: at(ia),
            at_b: at(ib),
            at_c: at(ic),
            on_ab: on(ia, ib),
            on_bc: on(ib, ic),
            on_ca: on(ic, ia),
        })
        .collect()
}

/// The grid's cells: their count, and the position and index of each.
struct Cells {
    origin: Vec3,
    cell: f32,
    nx: usize,
    ny: usize,
    nz: usize,
}

impl Cells {
    const fn len(&self) -> usize {
        self.nx * self.ny * self.nz
    }

    const fn index(&self, x: usize, y: usize, z: usize) -> usize {
        (z * self.ny + y) * self.nx + x
    }

    const fn coords(&self, i: usize) -> (usize, usize, usize) {
        (i % self.nx, (i / self.nx) % self.ny, i / (self.nx * self.ny))
    }

    fn position(&self, x: usize, y: usize, z: usize) -> Vec3 {
        self.origin + Vec3::new(x.to_f32(), y.to_f32(), z.to_f32()) * self.cell
    }

    /// Cell range (inclusive) covering `(lo, hi)` plus [`BAND`] cells each way.
    fn range(&self, lo: Vec3, hi: Vec3) -> (UVec3, UVec3) {
        let max = UVec3::new(self.nx.clamp_u32(), self.ny.clamp_u32(), self.nz.clamp_u32()) - UVec3::ONE;
        let lo = ((lo - self.origin) / self.cell).floor().max(Vec3::ZERO).as_uvec3().saturating_sub(UVec3::splat(BAND)).min(max);
        let hi = (((hi - self.origin) / self.cell).ceil().max(Vec3::ZERO).as_uvec3() + UVec3::splat(BAND)).min(max);
        (lo, hi)
    }
}

/// Nearest triangle so far of each cell, by squared distance; `u32::MAX` where none yet.
struct Nearest {
    d2: Vec<f32>,
    tri: Vec<u32>,
}

/// Pass one: every triangle measures the cells within [`BAND`] of its bounds (which may leave a
/// cell at the band's skin with a triangle that reached it but isn't its nearest; the caller
/// drops those). Workers own slabs of z, so no cell is written by two.
fn band(tris: &[Tri], cells: &Cells, nearest: &mut Nearest) {
    let ranges: Vec<(UVec3, UVec3)> = tris.iter().map(|t| { let (lo, hi) = t.bounds(); cells.range(lo, hi) }).collect();
    let slab_z = cells.nz.div_ceil(worker_threads().clamp(1, cells.nz));
    let slab = slab_z * cells.nx * cells.ny;
    scope(|s| {
        for (k, (d2, tri)) in nearest.d2.chunks_mut(slab).zip(nearest.tri.chunks_mut(slab)).enumerate() {
            let (z0, z1) = (k * slab_z, ((k + 1) * slab_z).min(cells.nz));
            let ranges = &ranges;
            s.spawn(move || {
                for (ti, (t, &(lo, hi))) in tris.iter().zip(ranges.iter()).enumerate() {
                    if hi.z.idx() < z0 || lo.z.idx() >= z1 {
                        continue;
                    }
                    for z in lo.z.idx().max(z0)..=hi.z.idx().min(z1 - 1) {
                        for y in lo.y.idx()..=hi.y.idx() {
                            for x in lo.x.idx()..=hi.x.idx() {
                                let h = t.closest(cells.position(x, y, z));
                                let i = cells.index(x, y, z) - z0 * cells.nx * cells.ny;
                                if let (Some(d), Some(best)) = (d2.get_mut(i), tri.get_mut(i))
                                    && h.d2 < *d
                                {
                                    *d = h.d2;
                                    *best = ti.clamp_u32();
                                }
                            }
                        }
                    }
                }
            });
        }
    });
}

/// One row of cells (fixed `y` and `z`) measured against the nearest triangles of the cells
/// `step` away in each of the 26 directions; a cell keeps its own triangle if none measures
/// closer, and one within `exact` (squared) is already right.
#[allow(clippy::too_many_arguments)]
fn flood_row(y: usize, z: usize, step: usize, exact: f32, tris: &[Tri], spheres: &[Vec4], cells: &Cells, nearest: &Nearest) -> Vec<(f32, u32)> {
    let span = |c: usize, n: usize| [c.checked_sub(step), Some(c), (c + step < n).then_some(c + step)];
    let rows: smallvec::SmallVec<[usize; 9]> = span(z, cells.nz).into_iter().flatten().flat_map(|cz| span(y, cells.ny).into_iter().flatten().map(move |cy| (cz * cells.ny + cy) * cells.nx)).collect();
    let row = (z * cells.ny + y) * cells.nx;
    let mut out = Vec::with_capacity(cells.nx);
    for x in 0..cells.nx {
        let mut best = (nearest.d2.get(row + x).copied().unwrap_or(f32::INFINITY), nearest.tri.get(row + x).copied().unwrap_or(u32::MAX));
        if best.0 <= exact {
            out.push(best);
            continue;
        }
        let p = cells.position(x, y, z);
        let mut reach = best.0.sqrt();
        for &base in &rows {
            for cx in span(x, cells.nx).into_iter().flatten() {
                let Some(&t) = nearest.tri.get(base + cx) else { continue };
                if t == u32::MAX || t == best.1 {
                    continue;
                }
                // Its bounding sphere must come nearer than the best so far (the spheres are
                // packed apart from the triangles so this test stays in cache).
                if let Some(sphere) = spheres.get(t.idx())
                    && p.distance_squared(sphere.truncate()) < (reach + sphere.w) * (reach + sphere.w)
                    && let Some(tri) = tris.get(t.idx())
                {
                    let d2 = tri.closest(p).d2;
                    if d2 < best.0 {
                        best = (d2, t);
                        reach = d2.sqrt();
                    }
                }
            }
        }
        out.push(best);
    }
    out
}

/// Pass two, jump flooding (Rong & Tan 2006) from the band: in passes whose jump halves from
/// half the grid's longest side down to one cell, every cell adopts the nearest triangle of any
/// of its 26 neighbours at that jump when it measures closer. Every cell is reached, and almost
/// every one ends on its true nearest triangle; the rest are off by a small fraction of a cell
/// (a second pass at one cell didn't change the head's worst error). Each pass is a parallel
/// map over rows reading the previous pass; the passes at jumps of 2 to 16 cost the most, and
/// their time is in gathering the neighbours, not in the triangle tests.
fn jump_flood(tris: &[Tri], cells: &Cells, exact: f32, nearest: &mut Nearest) {
    let rows: Vec<(usize, usize)> = (0..cells.nz).flat_map(|z| (0..cells.ny).map(move |y| (y, z))).collect();
    let longest = cells.nx.max(cells.ny).max(cells.nz);
    let first = (longest / 2).max(1).checked_next_power_of_two().map_or(1, |p| if p > longest / 2 { (p / 2).max(1) } else { p });
    let steps: Vec<usize> = std::iter::successors(Some(first), |&s| (s > 1).then_some(s / 2)).collect();
    let spheres: Vec<Vec4> = tris.iter().map(Tri::sphere).collect();
    for step in steps {
        let found = par_map(&rows, |&(y, z)| flood_row(y, z, step, exact, tris, &spheres, cells, nearest));
        for (i, (d2, t)) in found.into_iter().flatten().enumerate() {
            if let (Some(d), Some(best)) = (nearest.d2.get_mut(i), nearest.tri.get_mut(i)) {
                *d = d2;
                *best = t;
            }
        }
    }
}

/// Signed distance from the mesh at every cell of the grid whose first cell centre is `origin`,
/// in row-major x, y, z order. The mesh must wind outward. Cells no triangle reaches (an empty
/// mesh) are far outside.
#[must_use]
pub fn grid_distances(positions: &[Vec3], indices: &[u32], origin: Vec3, cell: f32, dims: UVec3) -> Vec<f32> {
    let cells = Cells { origin, cell, nx: dims.x.idx(), ny: dims.y.idx(), nz: dims.z.idx() };
    let tris = triangles(positions, indices);
    let mut nearest = Nearest { d2: vec![f32::INFINITY; cells.len()], tri: vec![u32::MAX; cells.len()] };
    if tris.is_empty() {
        return vec![cell * dims.max_element().to_f32(); cells.len()];
    }
    band(&tris, &cells, &mut nearest);
    // A cell is exact only if a triangle lies within BAND cells of it: any such triangle's
    // bounds reach it. One reached from farther may not be its nearest, and a wrong seed would
    // spread, so the band keeps only the exact cells, a shell two cells thick round the surface.
    let exact = (BAND.to_f32() * cell) * (BAND.to_f32() * cell);
    for (d, t) in nearest.d2.iter_mut().zip(&mut nearest.tri) {
        if *d > exact {
            *d = f32::INFINITY;
            *t = u32::MAX;
        }
    }
    jump_flood(&tris, &cells, exact, &mut nearest);
    let rows: Vec<usize> = (0..cells.ny * cells.nz).collect();
    let far = cell * dims.max_element().to_f32();
    par_map(&rows, |&row| {
        (row * cells.nx..(row + 1) * cells.nx)
            .map(|i| {
                let (x, y, z) = cells.coords(i);
                nearest.tri.get(i).and_then(|&t| tris.get(t.idx())).map_or(far, |t| t.signed(cells.position(x, y, z)))
            })
            .collect::<Vec<f32>>()
    })
    .into_iter()
    .flatten()
    .collect()
}

/// The nearest triangle over all of them, and the pseudo-normal sign from it.
#[cfg(test)]
fn brute_force(tris: &[Tri], p: Vec3) -> f32 {
    tris.iter().map(|t| (t.closest(p).d2, t)).min_by(|a, b| a.0.total_cmp(&b.0)).map_or(f32::INFINITY, |(_, t)| t.signed(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the sculpted head, every cell's flooded distance matches a search over every triangle,
    /// sign included, to a small fraction of a cell.
    #[test]
    fn flooded_distances_match_a_full_search_on_the_head() {
        let plan: crate::anatomy::BodyPlan = ron::from_str(include_str!("../../assets/mobs/man.body.ron")).unwrap();
        let assets = crate::anatomy::test_assets(&plan);
        let mesh = assets.meshes.values().next().unwrap().clone();
        let (lo, hi) = mesh.bounds();
        let cell = (hi - lo).max_element() / 64.0;
        let pad = Vec3::splat(cell * 2.0);
        let (lo, hi) = (lo - pad, hi + pad);
        let dims = ((hi - lo) / cell).ceil().as_uvec3();
        let origin = lo + Vec3::splat(cell * 0.5);
        let t = std::time::Instant::now();
        let values = grid_distances(&mesh.positions, &mesh.indices, origin, cell, dims);
        let took = t.elapsed();
        let tris = triangles(&mesh.positions, &mesh.indices);
        let cells = Cells { origin, cell, nx: dims.x.idx(), ny: dims.y.idx(), nz: dims.z.idx() };
        let (mut worst, mut flips, mut checked) = (0.0f32, 0, 0);
        for i in (0..cells.len()).step_by(331) {
            let (x, y, z) = cells.coords(i);
            let want = brute_force(&tris, cells.position(x, y, z));
            let got = values[i];
            checked += 1;
            flips += usize::from(want.is_sign_negative() != got.is_sign_negative() && want.abs() > 1e-6);
            worst = worst.max((got - want).abs());
        }
        println!("{} cells ({} tris) in {took:?}; {checked} checked, worst {:.3} mm, {flips} sign flips", cells.len(), tris.len(), worst * 1000.0);
        assert_eq!(flips, 0, "{flips} of {checked} cells disagree in sign");
        assert!(worst < cell * 0.05, "worst error {worst} m against a cell of {cell} m");
    }
}
