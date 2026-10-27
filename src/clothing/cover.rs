//! Garments over the skin of one LOD mesh: cloth moves with the skin under it, and skin it hides
//! leaves the mesh.
//!
//! Muscle flex and soft-tissue jiggle are worked out for the skin. Cloth lying close over it takes
//! the skin's own (blended over the skin triangle under each cloth vertex), so the two move as one
//! and the skin never swells or swings through; cloth standing clear of the body (hanging, or
//! bridging a gap) fades to its bones alone. Skin under close-fitting cloth then can't be seen, so
//! it is dropped: it neither shows nor needs to move. A band of it stays at every hem and opening,
//! so the cloth's edge never shows a hole. So does skin a garment is meant to cover
//! ([`GarmentCover`]) where the fit left that garment's cloth under it: the cloth shows instead.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::f32::consts::TAU;

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use smallvec::SmallVec;

use super::{MAX_GARMENTS, Outfit};
use crate::anatomy::{Anatomy, BuiltPart, PartPath, mirror_point, path_matches, sample_axis};
use crate::num::{Clamp, Idx, ToF32, smoothstep};
use crate::surface::{BodyMeshData, MaterialSlot, MobVertexData, MuscleFlex, NOT_CLOTH, SoftVertex, Submesh};

/// Furthest (metres) close-fitting cloth stands off the skin: ease, cloth thickness, hems and the
/// fit's rise over bulges. Each level adds its voxel size: coarser skin is rounder, and coarser
/// cloth sits further out (see `garment_lods`).
const TIGHT: f32 = 0.02;
/// Past this (plus the voxel size) cloth is clear of the skin: it moves with its bones alone
/// (hangs, drapes, bridges a gap), and hides no skin.
const LOOSE: f32 = 0.05;
/// How far under the skin cloth may lie and still cover it (the fit's rounding).
const BURIED: f32 = 0.004;
/// How far under the skin (plus half the voxel size) cloth may lie and still cover skin its
/// garment is meant to cover (see [`GarmentCover`]): where the fit leaves cloth under a bulge it
/// doesn't rise over (a thick thigh's root, under a tunic), that skin goes and the cloth shows.
const DEEP: f32 = 0.04;
/// A limb leaves through a garment's open end, not its side, when its root points within 60° of
/// straight out of that end.
const END_EXIT: f32 = 0.5;
/// Skin is out of sight when cloth meets every sightline to it steeper than 1 in `SIGHTLINE` to
/// the skin (within about 63° of its normal): near a hem, sightlines slip under.
const SIGHTLINE: f32 = 2.0;
/// Sightlines tried around the normal.
const SIGHTLINES: u8 = 6;
/// Hidden skin stays this far (plus half the voxel size) along the skin from skin in sight: hems
/// slide over the skin as the body moves. Only whole triangles go, so coarser levels keep more.
const SLIDE: f32 = 0.01;
/// Barycentric slack, so that a ray through an edge or corner meets the triangles there.
const EDGE: f32 = 1e-5;
/// A triangle spanning more grid cells than this on an axis is broken (a collapsed vertex).
const MAX_SPAN: i32 = 16;

/// The body regions each garment is meant to cover, by its palette slot.
///
/// That is the skin of the parts it is worn on, and of limbs that leave through its open ends
/// (legs out of a tunic's hem, a neck out of its collar), but not of limbs that pass out through
/// its side (arms out of armholes).
#[derive(Clone, Debug, Default)]
pub struct GarmentCover {
    /// One bit per [`RegionId`](crate::anatomy::RegionId).
    regions: [u64; MAX_GARMENTS],
    /// Each garment on each part it is worn on: the regions it covers there, and the part's axis
    /// (stored model space), which the garment was fitted out from.
    pieces: Vec<(u64, Vec<Vec3>)>,
}

impl GarmentCover {
    #[must_use]
    pub fn new(anatomy: &Anatomy, outfit: &Outfit) -> Self {
        let mut regions = [0u64; MAX_GARMENTS];
        let mut pieces = Vec::new();
        for (index, garment) in outfit.garments.iter().enumerate() {
            // Garments past the last palette slot share it (as in `fit_outfit`).
            let Some(slot) = regions.get_mut(index.min(MAX_GARMENTS - 1)) else { continue };
            let pattern = PartPath(garment.on.clone());
            for (part_index, part) in (0u16..).zip(&anatomy.parts).filter(|(_, p)| !p.axis.is_empty() && path_matches(&pattern, &p.path)) {
                let bits = anatomy
                    .parts
                    .iter()
                    .filter(|c| c.parent == Some(part_index) && leaves_through_end(part, c, garment.span))
                    .fold(region_bits(part), |bits, child| bits | region_bits(child));
                *slot |= bits;
                let stored = |p: Vec3| if part.mirrored { mirror_point(p) } else { p };
                pieces.push((bits, part.axis.iter().map(|a| stored(a.centre())).collect()));
            }
        }
        Self { regions, pieces }
    }

    /// Whether the garment in palette slot `slot` is meant to cover a region among `bits`.
    fn covers(&self, slot: u8, bits: u64) -> bool {
        self.regions.get(usize::from(slot)).is_some_and(|m| m & bits != 0)
    }

    /// The nearest point to `p` on the axis of each part a garment covering a region among `bits`
    /// is worn on.
    fn axes_toward(&self, p: Vec3, bits: u64) -> impl Iterator<Item = Vec3> + '_ {
        self.pieces.iter().filter(move |(covered, _)| covered & bits != 0).filter_map(move |(_, axis)| {
            axis.array_windows::<2>()
                .map(|&[a, b]| a.lerp(b, ((p - a).dot(b - a) / (b - a).length_squared().max(1e-12)).clamp(0.0, 1.0)))
                .min_by(|q, r| p.distance_squared(*q).total_cmp(&p.distance_squared(*r)))
        })
    }
}

fn region_bits(part: &BuiltPart) -> u64 {
    part.regions.clone().filter(|&r| r < 64).fold(0, |bits, r| bits | 1 << r)
}

/// Whether `child`'s root points out through an end of `span` on `part` rather than its side.
fn leaves_through_end(part: &BuiltPart, child: &BuiltPart, span: (f32, f32)) -> bool {
    let (Some(root), Some(start), Some(end)) = (child.axis.first(), sample_axis(&part.axis, span.0), sample_axis(&part.axis, span.1)) else {
        return false;
    };
    let at = root.centre();
    let out = if at.distance(start.centre()) <= at.distance(end.centre()) { start.model.rotation * Vec3::NEG_Y } else { end.model.rotation * Vec3::Y };
    (root.model.rotation * Vec3::Y).dot(out) > END_EXIT
}

/// Moves the muscle flex and soft-tissue weights of `lod`'s skin onto the cloth over it, then
/// drops the skin no one can see.
///
/// That is skin close-fitting cloth hides, and skin over cloth of a garment meant to cover it
/// (`design`). `voxel` is the level's voxel size.
pub fn cover_skin(lod: &mut BodyMeshData, voxel: f32, design: &GarmentCover) {
    let n = lod.positions.len();
    if lod.cloth.len() != n || lod.cloth.iter().all(|&c| c == NOT_CLOTH) {
        return;
    }
    let (tight, loose) = (TIGHT + voxel, LOOSE + voxel);
    let skin: Vec<[u32; 3]> = lod.slot_triangles(MaterialSlot::Skin).copied().collect();
    let cloth: Vec<[u32; 3]> = lod.slot_triangles(MaterialSlot::Cloth).copied().collect();
    carry_motion(lod, &skin, &TriGrid::new(&lod.positions, &skin, tight), tight, loose);
    let cover = Cover {
        under: f32::mul_add(voxel, 0.5, BURIED),
        deep: f32::mul_add(voxel, 0.5, DEEP),
        over: loose,
        slide: f32::mul_add(voxel, 0.5, SLIDE),
    };
    let hidden = hidden_skin(lod, &skin, &cloth, &TriGrid::new(&lod.positions, &cloth, tight), &cover, design);
    drop_skin(lod, &hidden);
}

/// What counts as skin hidden under cloth (metres).
#[derive(Clone, Copy)]
struct Cover {
    /// Cloth this far under the skin still covers it.
    under: f32,
    /// Cloth of a garment meant to cover the skin covers it this far under.
    deep: f32,
    /// Furthest off the skin cloth still covers it.
    over: f32,
    /// Hidden skin stays this far from skin in sight (along the skin).
    slide: f32,
}

/// Each cloth vertex's flex and soft weights: the skin's at the nearest point under it, fading
/// out from `tight` to `loose` off the skin.
fn carry_motion(lod: &mut BodyMeshData, skin: &[[u32; 3]], grid: &TriGrid, tight: f32, loose: f32) {
    let n = lod.positions.len();
    let (muscle, soft) = (lod.muscle.len() == n, lod.soft.len() == n);
    if !muscle && !soft {
        return;
    }
    let pos = |i: u32| lod.positions.get(i.idx()).copied().unwrap_or_default();
    let mut carried: Vec<(usize, Option<MuscleFlex>, Option<SoftVertex>)> = Vec::new();
    for (i, (&p, _)) in lod.positions.iter().zip(&lod.cloth).enumerate().filter(|(_, (_, c))| **c != NOT_CLOTH) {
        let reach = Vec3::splat(loose);
        let mut best: Option<(f32, [u32; 3], [f32; 3])> = None;
        for t in grid.near(p - reach, p + reach).filter_map(|k| skin.get(k.idx())) {
            let corners = t.map(pos);
            let Some(bary) = closest_on_triangle(p, corners) else { continue };
            let d = p.distance_squared(at(corners, bary));
            if best.is_none_or(|(b, ..)| d < b) {
                best = Some((d, *t, bary));
            }
        }
        let Some((d, t, bary)) = best else { continue };
        let contact = 1.0 - smoothstep(tight, loose, d.sqrt());
        if contact <= 0.0 {
            continue;
        }
        let flex = muscle.then(|| blend_flex(t.map(|v| lod.muscle.get(v.idx()).copied().unwrap_or_default()), bary, contact));
        let jiggle = soft.then(|| blend_soft(t.map(|v| lod.soft.get(v.idx()).copied().unwrap_or_default()), bary, contact));
        carried.push((i, flex, jiggle));
    }
    for (i, flex, jiggle) in carried {
        if let (Some(f), Some(m)) = (flex, lod.muscle.get_mut(i)) {
            *m = f;
        }
        if let (Some(s), Some(v)) = (jiggle, lod.soft.get_mut(i)) {
            *v = s;
        }
    }
}

/// The flex of the strongest muscle under a point of a skin triangle (`bary` over its corners),
/// by `contact`. Corners over another muscle add nothing: where two meet, the skin fades its flex
/// to nothing at the switch, and so does the cloth.
fn blend_flex(corners: [MuscleFlex; 3], bary: [f32; 3], contact: f32) -> MuscleFlex {
    let Some((top, _)) = corners.iter().zip(bary).filter(|(m, _)| m.amplitude > 0.0).max_by(|a, b| (a.0.amplitude * a.1).total_cmp(&(b.0.amplitude * b.1)))
    else {
        return MuscleFlex::default();
    };
    let same = |m: &MuscleFlex| m.joint == top.joint && m.insertion.distance_squared(top.insertion) < 1e-10;
    let amplitude: f32 = corners.iter().zip(bary).filter(|(m, _)| same(m)).map(|(m, b)| m.amplitude * b).sum();
    MuscleFlex { amplitude: amplitude * contact, ..*top }
}

/// The two strongest soft regions under a point of a skin triangle (`bary` over its corners), by
/// `contact`. As on the skin, the third is taken off the other two, so a region dropping out of
/// the top two fades rather than pops.
fn blend_soft(corners: [SoftVertex; 3], bary: [f32; 3], contact: f32) -> SoftVertex {
    let mut acc: SmallVec<[(u32, f32); 6]> = SmallVec::new();
    for (s, b) in corners.iter().zip(bary) {
        for (&joint, w) in s.joints.iter().zip(s.weights).filter(|(_, w)| *w > 0.0) {
            match acc.iter_mut().find(|(j, _)| *j == joint) {
                Some(entry) => entry.1 = f32::mul_add(w, b, entry.1),
                None => acc.push((joint, w * b)),
            }
        }
    }
    acc.sort_by(|a, b| b.1.total_cmp(&a.1));
    let third = acc.get(2).map_or(0.0, |r| r.1);
    let mut out = SoftVertex::default();
    for ((joint, weight), (j, w)) in out.joints.iter_mut().zip(&mut out.weights).zip(acc).filter(|(_, (_, w))| *w > third) {
        *joint = j;
        *weight = (w - third) * contact;
    }
    out
}

/// Per vertex, whether it is skin out of sight under cloth (see [`Cover`]).
fn hidden_skin(lod: &BodyMeshData, skin: &[[u32; 3]], cloth: &[[u32; 3]], grid: &TriGrid, cover: &Cover, design: &GarmentCover) -> Vec<bool> {
    let count = lod.positions.len();
    let pos = |i: u32| lod.positions.get(i.idx()).copied().unwrap_or_default();
    let normal = |i: u32| lod.normals.get(i.idx()).copied().unwrap_or_default();
    let is_skin = lod.skin_vertex_mask();
    let Cover { under, deep, over, slide } = *cover;
    let region = |i: usize| lod.mob_data.get(i).map_or(0, |d| 1u64 << (MobVertexData::unpack(*d).region.0 & 63));
    let slot = |&[first, ..]: &[u32; 3]| lod.cloth.get(first.idx()).copied().unwrap_or(NOT_CLOTH);
    let meant = |bits: u64| design.regions.iter().any(|m| m & bits != 0);
    // A sightline's share along the normal and across it.
    let (steep, wide) = (1.0 / SIGHTLINE.hypot(1.0), SIGHTLINE / SIGHTLINE.hypot(1.0));
    // In sight unless the cloth first along the normal (from a little under the skin) faces the
    // same way, close enough, and cloth also meets every sightline around the normal. Cloth
    // facing back is another part's (the other thigh's trousers). Further under the skin, cloth
    // counts only if its garment is meant to cover that skin: an arm lying on a tunic stays.
    let in_sight: Vec<bool> = lod
        .positions
        .iter()
        .zip(&lod.normals)
        .zip(&is_skin)
        .enumerate()
        .map(|(i, ((&p, &nrm), &s))| {
            let dir = nrm.normalize_or_zero();
            if !s || dir == Vec3::ZERO {
                return true;
            }
            // Out of sight from `eye` (just under cloth) if cloth meets every sightline around `up`.
            let enclosed = |eye: Vec3, up: Vec3| {
                let (side, lift) = up.any_orthonormal_pair();
                (0..SIGHTLINES).all(|k| {
                    let turn = f32::from(k) * TAU / f32::from(SIGHTLINES);
                    let look = up * steep + (side * turn.cos() + lift * turn.sin()) * wide;
                    grid.cast(cloth, &pos, eye, look, (under + over) / steep).is_some()
                })
            };
            // Behind cloth `along` from just under the skin (negative: cloth under it) facing the
            // same way, from the skin or from just under that cloth.
            let behind = |(along, tri, bary): (f32, [u32; 3], [f32; 3])| {
                at(tri.map(normal), bary).dot(dir) > 0.0 && enclosed(p + dir * (along - under - 1e-4).min(0.0), dir)
            };
            let hidden = grid.cast(cloth, &pos, p - dir * under, dir, under + over).is_some_and(behind) || {
                // Over cloth of a garment meant to cover it: along the normal, or toward the axis
                // the garment was fitted out from (the top of a bulge faces away from cloth inside
                // it, a thick thigh's root under a tunic).
                let bits = region(i);
                let meant_cloth = |hit: &(f32, [u32; 3], [f32; 3])| design.covers(slot(&hit.1), bits);
                meant(bits)
                    && (grid.cast(cloth, &pos, p, -dir, deep).filter(meant_cloth).is_some_and(|(depth, tri, bary)| behind((under - depth, tri, bary)))
                        || design.axes_toward(p, bits).any(|q| {
                            let inward = (q - p).normalize_or_zero();
                            grid.cast(cloth, &pos, p, inward, deep.min(p.distance(q))).filter(meant_cloth).is_some_and(|(depth, tri, bary)| {
                                let up = at(tri.map(normal), bary).normalize_or_zero();
                                up.dot(inward) < 0.0 && enclosed(p + inward * (depth + 1e-4), up)
                            })
                        }))
            };
            !hidden
        })
        .collect();
    // Distance along the skin from skin in sight, out to `slide`.
    let mut next: Vec<SmallVec<[u32; 8]>> = vec![SmallVec::new(); count];
    for &[a, b, c] in skin {
        for (from, to) in [(a, b), (b, c), (c, a), (b, a), (c, b), (a, c)] {
            if let Some(out) = next.get_mut(from.idx()) {
                out.push(to);
            }
        }
    }
    let mut dist = vec![f32::INFINITY; count];
    let mut queue: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    for (vertex, (d, (&on_skin, &seen))) in (0u32..).zip(dist.iter_mut().zip(is_skin.iter().zip(&in_sight))) {
        if on_skin && seen {
            *d = 0.0;
            queue.push(Reverse((0, vertex)));
        }
    }
    // Distances are never negative, so their bits sort as they do.
    while let Some(Reverse((bits, here))) = queue.pop() {
        let so_far = f32::from_bits(bits);
        if so_far >= slide || dist.get(here.idx()).is_some_and(|&best| so_far > best) {
            continue;
        }
        for &there in next.get(here.idx()).into_iter().flatten() {
            let through = so_far + pos(here).distance(pos(there));
            if let Some(best) = dist.get_mut(there.idx())
                && through < *best
            {
                *best = through;
                queue.push(Reverse((through.to_bits(), there)));
            }
        }
    }
    is_skin.iter().zip(&in_sight).zip(&dist).map(|((&on_skin, &seen), &d)| on_skin && !seen && d >= slide).collect()
}

/// Drops the skin triangles whose corners are all `hidden`.
fn drop_skin(lod: &mut BodyMeshData, hidden: &[bool]) {
    let gone = |i: u32| hidden.get(i.idx()).copied().unwrap_or(false);
    let mut indices = Vec::with_capacity(lod.indices.len());
    let mut submeshes = Vec::with_capacity(lod.submeshes.len());
    for s in &lod.submeshes {
        let start = indices.len();
        for tri in lod.submesh_indices(s).as_chunks::<3>().0 {
            if !(s.slot == MaterialSlot::Skin && tri.iter().all(|&i| gone(i))) {
                indices.extend_from_slice(tri);
            }
        }
        if indices.len() > start {
            submeshes.push(Submesh { indices: start.clamp_u32()..indices.len().clamp_u32(), slot: s.slot });
        }
    }
    lod.indices = indices;
    lod.submeshes = submeshes;
}

/// `values` at the point `bary` of a triangle.
fn at(values: [Vec3; 3], bary: [f32; 3]) -> Vec3 {
    values.iter().zip(bary).map(|(value, share)| *value * share).sum()
}

/// Barycentric coordinates of the point of triangle `[a, b, c]` nearest `p` (Ericson, *Real-Time
/// Collision Detection* 5.1.5); `None` for a degenerate triangle.
fn closest_on_triangle(point: Vec3, [a, b, c]: [Vec3; 3]) -> Option<[f32; 3]> {
    let (ab, ac) = (b - a, c - a);
    if ab.cross(ac).length_squared() < 1e-14 {
        return None;
    }
    let ap = point - a;
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return Some([1.0, 0.0, 0.0]);
    }
    let bp = point - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return Some([0.0, 1.0, 0.0]);
    }
    let vc = d1.mul_add(d4, -(d3 * d2));
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let to_b = d1 / (d1 - d3);
        return Some([1.0 - to_b, to_b, 0.0]);
    }
    let cp = point - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return Some([0.0, 0.0, 1.0]);
    }
    let vb = d5.mul_add(d2, -(d1 * d6));
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let to_c = d2 / (d2 - d6);
        return Some([1.0 - to_c, 0.0, to_c]);
    }
    let va = d3.mul_add(d6, -(d5 * d4));
    if va <= 0.0 && d4 - d3 >= 0.0 && d5 - d6 >= 0.0 {
        let to_c = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return Some([0.0, 1.0 - to_c, to_c]);
    }
    let scale = 1.0 / (va + vb + vc);
    let (to_b, to_c) = (vb * scale, vc * scale);
    Some([1.0 - to_b - to_c, to_b, to_c])
}

/// Where the ray from `from` along `dir` crosses triangle `[a, b, c]` (either face): the distance
/// along it and the barycentric coordinates there (Möller–Trumbore).
fn ray_triangle(from: Vec3, dir: Vec3, [a, b, c]: [Vec3; 3]) -> Option<(f32, [f32; 3])> {
    let (e1, e2) = (b - a, c - a);
    let pv = dir.cross(e2);
    let det = e1.dot(pv);
    if det.abs() < 1e-12 {
        return None;
    }
    let tv = from - a;
    let to_b = tv.dot(pv) / det;
    let qv = tv.cross(e1);
    let to_c = dir.dot(qv) / det;
    let along = e2.dot(qv) / det;
    let inside = to_b >= -EDGE && to_c >= -EDGE && to_b + to_c <= 1.0 + EDGE;
    (inside && along >= 0.0).then_some((along, [1.0 - to_b - to_c, to_b, to_c]))
}

/// Triangles bucketed by the cells of a uniform grid their bounds overlap.
struct TriGrid {
    cell: f32,
    cells: HashMap<IVec3, Vec<u32>>,
}

impl TriGrid {
    fn new(positions: &[Vec3], triangles: &[[u32; 3]], cell: f32) -> Self {
        let mut cells: HashMap<IVec3, Vec<u32>> = HashMap::default();
        for (k, t) in (0u32..).zip(triangles) {
            let [a, b, c] = t.map(|i| positions.get(i.idx()).copied().unwrap_or_default());
            let (lo, hi) = (cell_of(a.min(b).min(c), cell), cell_of(a.max(b).max(c), cell));
            if (hi - lo).max_element() > MAX_SPAN {
                continue;
            }
            for key in cells_between(lo, hi) {
                cells.entry(key).or_default().push(k);
            }
        }
        Self { cell, cells }
    }

    /// The nearest of `triangles` (corners at `pos`) the ray from `from` along unit `dir` meets
    /// within `len`: the distance, the triangle and the barycentric coordinates there. Walks the
    /// cells the ray crosses, in order (Amanatides–Woo).
    fn cast(&self, triangles: &[[u32; 3]], pos: &impl Fn(u32) -> Vec3, from: Vec3, dir: Vec3, len: f32) -> Option<(f32, [u32; 3], [f32; 3])> {
        let mut key = cell_of(from, self.cell);
        let step = IVec3::new(sign(dir.x), sign(dir.y), sign(dir.z));
        let edge = |k: i32, s: i32| (k + i32::from(s > 0)).to_f32() * self.cell;
        let cross = |k: i32, s: i32, o: f32, d: f32| if s == 0 { f32::INFINITY } else { (edge(k, s) - o) / d };
        let mut exit = Vec3::new(cross(key.x, step.x, from.x, dir.x), cross(key.y, step.y, from.y, dir.y), cross(key.z, step.z, from.z, dir.z));
        let delta = Vec3::new(self.cell / dir.x.abs(), self.cell / dir.y.abs(), self.cell / dir.z.abs());
        let mut best: Option<(f32, [u32; 3], [f32; 3])> = None;
        loop {
            for t in self.cells.get(&key).into_iter().flatten().filter_map(|k| triangles.get(k.idx())) {
                if let Some((along, bary)) = ray_triangle(from, dir, t.map(pos))
                    && along <= len
                    && best.is_none_or(|(b, ..)| along < b)
                {
                    best = Some((along, *t, bary));
                }
            }
            let leave = exit.min_element();
            if leave > len || best.is_some_and(|(b, ..)| b <= leave) {
                return best;
            }
            // Into the next cell across the nearest wall.
            if exit.x <= exit.y && exit.x <= exit.z {
                key.x += step.x;
                exit.x += delta.x;
            } else if exit.y <= exit.z {
                key.y += step.y;
                exit.y += delta.y;
            } else {
                key.z += step.z;
                exit.z += delta.z;
            }
        }
    }

    /// Triangles in the cells overlapping `lo..hi` (one may come more than once).
    fn near(&self, lo: Vec3, hi: Vec3) -> impl Iterator<Item = u32> + '_ {
        cells_between(cell_of(lo, self.cell), cell_of(hi, self.cell)).flat_map(|key| self.cells.get(&key).into_iter().flatten().copied())
    }
}

/// -1, 0 or 1.
fn sign(x: f32) -> i32 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

fn cell_of(p: Vec3, cell: f32) -> IVec3 {
    (p / cell).floor().as_ivec3()
}

fn cells_between(lo: IVec3, hi: IVec3) -> impl Iterator<Item = IVec3> {
    (lo.x..=hi.x).flat_map(move |x| (lo.y..=hi.y).flat_map(move |y| (lo.z..=hi.z).map(move |z| IVec3::new(x, y, z))))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::anatomy::{BoneId, LodLevel, PartAssets, PrimLayer, RegionId, RegionKind, TriMesh, build_src};
    use crate::clothing::{Garment, fit_outfit, garment_lods};
    use crate::surface::{AdaptiveContour, SurfaceSettings, merge, mesh_lod, muscle_flex};

    /// A ring of 40 columns from `y = lo` to `y = hi` in 24 rows, `radius` out in wrap space.
    fn tube(lo: f32, hi: f32, radius: f32) -> TriMesh {
        let (rows, cols) = (24u32, 40u32);
        let positions = (0..=rows)
            .flat_map(|i| (0..cols).map(move |j| (i, j)))
            .map(|(i, j)| {
                let t = j.to_f32() / cols.to_f32() * TAU;
                Vec3::new(t.sin() * radius, f32::mul_add(hi - lo, i.to_f32() / rows.to_f32(), lo), -t.cos() * radius)
            })
            .collect();
        let v = |i: u32, j: u32| i * cols + j % cols;
        let indices = (0..rows).flat_map(|i| (0..cols).flat_map(move |j| [v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)])).collect();
        TriMesh::new(positions, Vec::new(), indices)
    }

    /// A square sheet of `cells` × `cells` quads, `size` across, at height `z`, facing `facing`
    /// (+1 = +Z).
    fn sheet(out: &mut BodyMeshData, size: f32, cells: u32, z: f32, facing: f32, slot: MaterialSlot) {
        let base = out.positions.len().clamp_u32();
        let start = out.indices.len().clamp_u32();
        for i in 0..=cells {
            for j in 0..=cells {
                let (x, y) = ((i.to_f32() / cells.to_f32() - 0.5) * size, (j.to_f32() / cells.to_f32() - 0.5) * size);
                out.positions.push(Vec3::new(x, y, z));
                out.normals.push(Vec3::Z * facing);
                out.joints.push([0; 4]);
                out.weights.push([1.0, 0.0, 0.0, 0.0]);
                let data = MobVertexData { region: RegionId(0), cavity: 0, curvature: 128, limb_t: 0, primary_bone: BoneId(0), layer: PrimLayer::Base };
                out.mob_data.push(data.pack());
                out.cloth.push(if slot == MaterialSlot::Cloth { 0 } else { NOT_CLOTH });
                // Skin flexes and jiggles more towards +x.
                let (amplitude, weight) = if slot == MaterialSlot::Skin { (0.01, 0.5 + x) } else { (0.0, 0.0) };
                out.muscle.push(MuscleFlex { insertion: Vec3::Y, amplitude, length: 0.1, joint: 3 });
                out.soft.push(SoftVertex { joints: [7, 0], weights: [weight, 0.0] });
            }
        }
        let v = |i: u32, j: u32| base + i * (cells + 1) + j;
        for i in 0..cells {
            for j in 0..cells {
                out.indices.extend([v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)]);
            }
        }
        out.submeshes.push(Submesh { indices: start..out.indices.len().clamp_u32(), slot });
    }

    /// Skin triangles left whose centre lies within `r` (square) of the middle, and beyond it.
    fn skin_left(lod: &BodyMeshData, r: f32) -> (usize, usize) {
        let pos = |i: u32| lod.positions.get(i.idx()).copied().unwrap_or_default();
        lod.slot_triangles(MaterialSlot::Skin).map(|t| at(t.map(pos), [1.0 / 3.0; 3])).fold((0, 0), |(inside, outside), c| {
            if c.x.abs().max(c.y.abs()) < r { (inside + 1, outside) } else { (inside, outside + 1) }
        })
    }

    #[test]
    fn cloth_carries_the_skin_motion_under_it() {
        let mut lod = BodyMeshData::default();
        sheet(&mut lod, 0.4, 40, 0.0, 1.0, MaterialSlot::Skin);
        let tight = lod.positions.len();
        sheet(&mut lod, 0.2, 10, 0.01, 1.0, MaterialSlot::Cloth);
        let loose = lod.positions.len();
        sheet(&mut lod, 0.2, 10, 0.09, 1.0, MaterialSlot::Cloth);
        cover_skin(&mut lod, 0.008, &GarmentCover::default());
        for (i, p) in lod.positions.iter().enumerate().skip(tight) {
            let (flex, soft) = (lod.muscle.get(i).copied().unwrap_or_default(), lod.soft.get(i).copied().unwrap_or_default());
            if i < loose {
                assert!((flex.amplitude - 0.01).abs() < 1e-4 && flex.joint == 3, "{p}: {flex:?}");
                assert!((soft.weights[0] - (0.5 + p.x)).abs() < 1e-3 && soft.joints[0] == 7, "{p}: {soft:?}");
            } else {
                assert!(flex.amplitude == 0.0 && soft.weights[0] == 0.0, "loose cloth at {p} moves with the skin");
            }
        }
    }

    #[test]
    fn skin_under_cloth_goes_but_not_at_the_hem() {
        let mut lod = BodyMeshData::default();
        sheet(&mut lod, 0.4, 40, 0.0, 1.0, MaterialSlot::Skin);
        sheet(&mut lod, 0.2, 10, 0.01, 1.0, MaterialSlot::Cloth);
        let before = skin_left(&lod, 0.1);
        cover_skin(&mut lod, 0.008, &GarmentCover::default());
        // Inside the margin from the hem the skin stays; well under the cloth it goes.
        let (band, outside) = skin_left(&lod, 0.1);
        assert_eq!(outside, before.1, "skin beyond the cloth stays");
        let (middle, _) = skin_left(&lod, 0.1 - SIGHTLINE.mul_add(0.01, SLIDE + 0.004) - 0.015);
        assert_eq!(middle, 0, "skin well under the cloth goes");
        assert!(band > 0 && band < before.0, "a band stays at the hem: {band} of {}", before.0);
    }

    #[test]
    fn cloth_facing_back_hides_nothing() {
        let mut lod = BodyMeshData::default();
        sheet(&mut lod, 0.4, 40, 0.0, 1.0, MaterialSlot::Skin);
        sheet(&mut lod, 0.2, 10, 0.01, -1.0, MaterialSlot::Cloth);
        let before = lod.slot_triangles(MaterialSlot::Skin).count();
        cover_skin(&mut lod, 0.008, &GarmentCover::default());
        assert_eq!(lod.slot_triangles(MaterialSlot::Skin).count(), before);
    }

    /// On a real body: a tunic hides the torso's skin (not the head's, arms' or legs') and flexes
    /// with the muscles under it.
    #[test]
    fn a_tunic_hides_the_torso_and_flexes_with_it() {
        let mut assets = PartAssets::default();
        assets.insert("tube", Arc::new(tube(0.0, 1.0, 1.0)));
        let tunic = Garment { name: "tunic".into(), on: "torso".into(), span: (0.0, 0.93), mesh: "tube".into(), ease: 0.008, ..default() };
        let outfit = Outfit { garments: vec![tunic] };
        let (anatomy, cloth) = fit_outfit(build_src(include_str!("../../assets/mobs/biped.body.ron")), &outfit, &assets);
        let settings = SurfaceSettings::default();
        let level = LodLevel::Near;
        let mut lod = mesh_lod(&AdaptiveContour, &anatomy, level, settings.get(level), 0).unwrap();
        let [_, near, _] = garment_lods(&cloth.unwrap(), &anatomy, &settings);
        merge(&mut lod, &near);
        lod.muscle = muscle_flex(&lod, &anatomy, level);
        let before: Vec<[u32; 3]> = lod.slot_triangles(MaterialSlot::Skin).copied().collect();
        cover_skin(&mut lod, settings.get(level).mesh.voxel_size, &GarmentCover::new(&anatomy, &outfit));
        let after: Vec<[u32; 3]> = lod.slot_triangles(MaterialSlot::Skin).copied().collect();
        let pos = |i: u32| lod.positions.get(i.idx()).copied().unwrap_or_default();
        let gone: Vec<Vec3> = before.iter().filter(|t| !after.contains(t)).map(|t| at(t.map(pos), [1.0 / 3.0; 3])).collect();
        let torso = anatomy.parts.iter().find(|p| p.path.0 == "torso").unwrap();
        let (low, high) = (sample_axis(&torso.axis, 0.0).unwrap().centre().y, sample_axis(&torso.axis, 0.93).unwrap().centre().y);
        // The torso's own skin under the tunic, away from its openings, goes.
        let torso_bones = torso.bones.clone();
        let on_torso = |t: &[u32; 3]| {
            let c = at(t.map(pos), [1.0 / 3.0; 3]);
            let bone = |i: u32| lod.mob_data.get(i.idx()).map_or(0, |d| MobVertexData::unpack(*d).primary_bone.0);
            c.y > low + 0.1 && c.y < high - 0.1 && t.iter().all(|&i| torso_bones.contains(&bone(i)))
        };
        let (torso_before, torso_after) = (before.iter().filter(|t| on_torso(t)).count(), after.iter().filter(|t| on_torso(t)).count());
        println!("{} of {} skin triangles hidden, {} of {torso_before} on the torso's middle", gone.len(), before.len(), torso_before - torso_after);
        assert!(torso_after * 2 < torso_before, "{torso_after} of {torso_before} skin triangles left on the torso's middle");
        for c in &gone {
            assert!(c.y > low - 0.1 && c.y < high, "skin hidden at {c}, outside the tunic ({low}..{high})");
        }
        let flexing = lod.muscle.iter().zip(&lod.cloth).filter(|(m, c)| **c != NOT_CLOTH && m.amplitude > 0.0).count();
        let cloth = lod.cloth.iter().filter(|c| **c != NOT_CLOTH).count();
        println!("{flexing} of {cloth} tunic vertices flex");
        assert!(flexing * 4 > cloth, "only {flexing} of {cloth} tunic vertices flex");
    }

    #[test]
    fn buried_cloth_hides_only_skin_it_is_meant_to_cover() {
        for (design, meant) in [(GarmentCover::default(), false), (GarmentCover { regions: [1, 0, 0, 0, 0, 0, 0, 0], pieces: Vec::new() }, true)] {
            let mut lod = BodyMeshData::default();
            sheet(&mut lod, 0.4, 40, 0.0, 1.0, MaterialSlot::Skin);
            // 2 cm under the skin: deeper than the fit's rounding.
            sheet(&mut lod, 0.2, 10, -0.02, 1.0, MaterialSlot::Cloth);
            let before = skin_left(&lod, 0.1);
            cover_skin(&mut lod, 0.008, &design);
            let after = skin_left(&lod, 0.1);
            assert_eq!(after.1, before.1, "skin beyond the cloth stays");
            if meant {
                assert_eq!(skin_left(&lod, 0.05).0, 0, "skin over the middle of buried cloth its garment covers goes");
            } else {
                assert_eq!(after.0, before.0, "skin over buried cloth no garment is meant to cover stays");
            }
        }
    }

    /// The thick body in the peasant outfit: where the fit leaves cloth under the thighs' roots,
    /// most of the skin there goes; the head, hands and feet keep all theirs.
    #[test]
    fn skin_over_buried_garments_goes_on_a_thick_body() {
        let outfit: Outfit = ron::from_str(include_str!("../../assets/outfits/peasant.outfit.ron")).unwrap();
        let mut assets = PartAssets::default();
        assets.insert("garments/garments.gltf#Mesh0/Primitive0", Arc::new(tube(0.0, 1.0, 1.0)));
        assets.insert("garments/garments.gltf#Mesh2/Primitive0", Arc::new(tube(0.0, 1.0, 1.005)));
        assets.insert("garments/garments.gltf#Mesh5/Primitive0", Arc::new(tube(-1.0, 1.0, 1.0)));
        let (anatomy, cloth) = fit_outfit(build_src(include_str!("../../assets/mobs/thick.body.ron")), &outfit, &assets);
        let settings = SurfaceSettings::default();
        let level = LodLevel::Hero;
        let voxel = settings.get(level).mesh.voxel_size;
        let mut lod = mesh_lod(&AdaptiveContour, &anatomy, level, settings.get(level), 0).unwrap();
        let [hero, _, _] = garment_lods(&cloth.unwrap(), &anatomy, &settings);
        merge(&mut lod, &hero);
        // Skin triangles with cloth facing the same way a little under them, past the fit's
        // rounding, of a garment meant to cover them (not an arm lying on a tunic).
        let design = GarmentCover::new(&anatomy, &outfit);
        let over_buried = |lod: &BodyMeshData| {
            let cloth: Vec<[u32; 3]> = lod.slot_triangles(MaterialSlot::Cloth).copied().collect();
            let grid = TriGrid::new(&lod.positions, &cloth, TIGHT + voxel);
            let pos = |i: u32| lod.positions.get(i.idx()).copied().unwrap_or_default();
            let normal = |i: u32| lod.normals.get(i.idx()).copied().unwrap_or_default();
            lod.slot_triangles(MaterialSlot::Skin)
                .filter(|t| {
                    let (c, n) = (at(t.map(pos), [1.0 / 3.0; 3]), at(t.map(normal), [1.0 / 3.0; 3]).normalize_or_zero());
                    let outside = grid.cast(&cloth, &pos, c, n, LOOSE + voxel).is_some();
                    let under = grid.cast(&cloth, &pos, c, -n, DEEP).filter(|(d, ..)| *d > f32::mul_add(voxel, 0.5, BURIED));
                    let region = lod.mob_data.get(t[0].idx()).map_or(0, |d| 1u64 << (MobVertexData::unpack(*d).region.0 & 63));
                    let slot = |tri: &[u32; 3]| lod.cloth.get(tri[0].idx()).copied().unwrap_or(NOT_CLOTH);
                    let meant = |&(_, tri, bary): &(f32, [u32; 3], [f32; 3])| design.covers(slot(&tri), region) && at(tri.map(normal), bary).dot(n) > 0.0;
                    // Or toward the axis the garment was fitted from, facing back along it.
                    let inward = design.axes_toward(c, region).any(|q| {
                        let toward = (q - c).normalize_or_zero();
                        grid.cast(&cloth, &pos, c, toward, DEEP.min(c.distance(q)))
                            .filter(|(d, ..)| *d > f32::mul_add(voxel, 0.5, BURIED))
                            .is_some_and(|(_, tri, bary)| design.covers(slot(&tri), region) && at(tri.map(normal), bary).dot(toward) < 0.0)
                    });
                    !outside && (under.as_ref().is_some_and(meant) || inward)
                })
                .count()
        };
        // Skin triangles of the head, hands and feet.
        let ends = |lod: &BodyMeshData| {
            let kind = |t: &[u32; 3]| {
                let region = lod.mob_data.get(t[0].idx()).map_or(RegionId(0), |d| MobVertexData::unpack(*d).region);
                anatomy.regions.regions.get(usize::from(region.0)).map(|r| r.kind)
            };
            let end = |t: &&[u32; 3]| matches!(kind(t), Some(RegionKind::Skull | RegionKind::Face | RegionKind::Brow | RegionKind::Eyelid | RegionKind::Lip | RegionKind::Jaw | RegionKind::LimbEnd | RegionKind::Digit));
            lod.slot_triangles(MaterialSlot::Skin).filter(end).count()
        };
        let ends_before = ends(&lod);
        let bare_before = over_buried(&lod);
        cover_skin(&mut lod, voxel, &design);
        let ends_after = ends(&lod);
        let bare_after = over_buried(&lod);
        println!("skin over buried cloth: {bare_before} triangles before, {bare_after} after; head, hands and feet {ends_before} -> {ends_after}");
        assert!(bare_before > 0, "the fit leaves no cloth under the thick body's skin");
        // Some stays: where a hanging belly faces down over cloth inside it, and near skin in sight.
        assert!(bare_after * 10 <= bare_before * 6, "{bare_after} of {bare_before} skin triangles over buried cloth left");
        assert_eq!(ends_after, ends_before, "head, hand or foot skin hidden");
    }
}
