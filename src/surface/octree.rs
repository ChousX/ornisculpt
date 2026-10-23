//! Adaptive meshing: octree dual contouring (Ju, Losasso, Schaefer & Warren, "Dual Contouring of
//! Hermite Data", SIGGRAPH 2002) with surface-nets vertex placement (the mean of a cell's edge
//! crossings, projected onto the surface; bodies are smooth, so no sharp-feature QEF).
//!
//! Cells refine where the body is thin: a cell splits while it is larger than the nearby
//! primitives' smallest radius over [`MeshSettings::cells_per_radius`], down to `voxel_size /
//! 2^detail_levels`. Fingers get small cells, the torso keeps the base size. Dual contouring
//! on an octree joins cells of different sizes without cracks: each sign-changing edge emits one
//! quad between the (up to four) cells around it, taking the sign from the smallest.
//!
//! The face recursion and its tables follow the standard formulation (cell/face/edge procs) as
//! in Ju et al. and its widely used reference implementations.

use std::collections::HashMap;

use bevy::math::Vec3A;
use bevy::prelude::*;
use bevy::shape::Aabb3d;
use smallvec::SmallVec;

use super::{BodyField, ChunkView, Field, MeshSettings, Mesher, RawMesh, SurfaceError};
use crate::num::{Clamp, Saturate, ToF32};

/// Octree dual contouring; see the module docs.
#[derive(Default, Clone, Copy)]
pub struct AdaptiveContour;

/// Child (and corner) `i` sits at `(i >> 2 & 1, i >> 1 & 1, i & 1)`.
fn child(i: usize) -> IVec3 {
    IVec3::new(i32::from(i & 4 != 0), i32::from(i & 2 != 0), i32::from(i & 1 != 0))
}

/// Cell edges as corner pairs: 0–3 along x, 4–7 along y, 8–11 along z.
const EDGEVMAP: [[usize; 2]; 12] = [[0, 4], [1, 5], [2, 6], [3, 7], [0, 2], [1, 3], [4, 6], [5, 7], [0, 1], [2, 3], [4, 5], [6, 7]];
const CELL_PROC_FACE_MASK: [[usize; 3]; 12] =
    [[0, 4, 0], [1, 5, 0], [2, 6, 0], [3, 7, 0], [0, 2, 1], [4, 6, 1], [1, 3, 1], [5, 7, 1], [0, 1, 2], [2, 3, 2], [4, 5, 2], [6, 7, 2]];
const CELL_PROC_EDGE_MASK: [[usize; 5]; 6] = [[0, 1, 2, 3, 0], [4, 5, 6, 7, 0], [0, 4, 1, 5, 1], [2, 6, 3, 7, 1], [0, 2, 4, 6, 2], [1, 3, 5, 7, 2]];
const FACE_PROC_FACE_MASK: [[[usize; 3]; 4]; 3] = [
    [[4, 0, 0], [5, 1, 0], [6, 2, 0], [7, 3, 0]],
    [[2, 0, 1], [6, 4, 1], [3, 1, 1], [7, 5, 1]],
    [[1, 0, 2], [3, 2, 2], [5, 4, 2], [7, 6, 2]],
];
const FACE_PROC_EDGE_MASK: [[[usize; 6]; 4]; 3] = [
    [[1, 4, 0, 5, 1, 1], [1, 6, 2, 7, 3, 1], [0, 4, 6, 0, 2, 2], [0, 5, 7, 1, 3, 2]],
    [[0, 2, 3, 0, 1, 0], [0, 6, 7, 4, 5, 0], [1, 2, 0, 6, 4, 2], [1, 3, 1, 7, 5, 2]],
    [[1, 1, 0, 3, 2, 0], [1, 5, 4, 7, 6, 0], [0, 1, 5, 0, 4, 1], [0, 3, 7, 2, 6, 1]],
];
const EDGE_PROC_EDGE_MASK: [[[usize; 5]; 2]; 3] =
    [[[3, 2, 1, 0, 0], [7, 6, 5, 4, 0]], [[5, 1, 4, 0, 1], [7, 3, 6, 2, 1]], [[6, 4, 2, 0, 2], [7, 5, 3, 1, 2]]];
const PROCESS_EDGE_MASK: [[usize; 4]; 3] = [[3, 2, 1, 0], [7, 5, 6, 4], [11, 10, 9, 8]];
/// Which of a face's two nodes each of an edge's four cells comes from.
const FACE_PROC_ORDERS: [[usize; 4]; 2] = [[0, 0, 1, 1], [0, 1, 0, 1]];

enum Node {
    Internal(Box<[Option<Self>; 8]>),
    Leaf(Leaf),
    /// A subtree being built on another thread (index into the job list).
    Pending(usize),
}

struct Leaf {
    /// Bit `i` set: corner `i` is inside.
    corners: u8,
    /// 0 = finest.
    level: u8,
    /// One vertex (position, normal) per sheet of surface through the cell: more than one only
    /// in a finest cell that couldn't split further (see [`edge_sheets`]).
    verts: SmallVec<[(Vec3, Vec3); 1]>,
    /// Per cell edge, its sheet's vertex (`u8::MAX`: the edge doesn't cross).
    sheet: [u8; 12],
    /// Mesh index of the first vertex.
    index: u32,
}

impl Node {
    const fn internal(&self) -> bool {
        matches!(self, Self::Internal(_))
    }

    fn child(&self, i: usize) -> Option<&Self> {
        match self {
            Self::Internal(c) => c.get(i).and_then(Option::as_ref),
            _ => Some(self),
        }
    }
}

/// Sizes in lattice units of the finest cell.
struct Grid {
    origin: Vec3A,
    fine: f32,
    /// Levels above the finest at which cells have the base size.
    base_level: u8,
    per_radius: f32,
}

impl Grid {
    fn point(&self, c: IVec3) -> Vec3A {
        self.origin + c.as_vec3a() * self.fine
    }

    /// Edge length of a cell at `level`.
    fn size(&self, level: u8) -> f32 {
        self.fine * (1u32 << level).to_f32()
    }

    fn region(&self, cell: IVec3, level: u8, pad: f32) -> Aabb3d {
        let lo = self.point(cell);
        let size = self.size(level);
        Aabb3d { min: lo - Vec3A::splat(pad), max: lo + Vec3A::splat(size + pad) }
    }

    /// Whether the surface may pass through a cell. An exact distance field would allow the
    /// half-diagonal (0.87 × size); ours overstates by up to ~20% where limbs switch between
    /// smooth and hard unions, and a cell dropped wrongly leaves a hole, so the bound is looser.
    fn near_surface(&self, view: &ChunkView, cell: IVec3, level: u8) -> bool {
        let size = self.size(level);
        let centre = self.point(cell) + Vec3A::splat(size * 0.5);
        view.distance(centre).abs() <= size.mul_add(1.2, self.fine)
    }
}

/// One thread's octree builder: shares corner samples between neighbouring cells.
struct Builder<'a> {
    grid: &'a Grid,
    corners: HashMap<IVec3, f32>,
}

impl Builder<'_> {
    fn corner(&mut self, view: &ChunkView, c: IVec3) -> f32 {
        if let Some(d) = self.corners.get(&c) {
            return *d;
        }
        let d = view.distance(self.grid.point(c));
        self.corners.insert(c, d);
        d
    }

    /// The field at a cell's eight corners.
    fn corners(&mut self, view: &ChunkView, cell: IVec3, level: u8) -> [f32; 8] {
        let span = 1i32 << level;
        std::array::from_fn(|i| self.corner(view, cell + child(i) * span))
    }

    fn build(&mut self, view: &ChunkView, cell: IVec3, level: u8) -> Option<Node> {
        let g = self.grid;
        if !g.near_surface(view, cell, level) {
            return None;
        }
        let size = g.size(level);
        let split = level > 0 && {
            let centre = g.point(cell) + Vec3A::splat(size * 0.5);
            level > g.base_level
                || size > (local_radius(view, centre, g.size(g.base_level)) / g.per_radius).max(g.fine) * 1.001
                || size > g.fine * 1.001 && curved(view, centre, size)
        };
        // A cell whose corners imply more than one sheet of surface (the gap between thumb and
        // palm) would pinch them into one vertex: split it while it can be.
        let split = split || level > 0 && (ambiguous(inside_mask(&self.corners(view, cell, level))) || !self.consistent(view, cell, level));
        if !split {
            return self.leaf(view, cell, level).map(Node::Leaf);
        }
        let narrowed = (level >= 3).then(|| view.narrow(g.region(cell, level, g.fine)));
        let view = narrowed.as_ref().unwrap_or(view);
        let half = 1i32 << (level - 1);
        let children: [Option<Node>; 8] = std::array::from_fn(|i| self.build(view, cell + child(i) * half, level - 1));
        children.iter().any(Option::is_some).then(|| Node::Internal(Box::new(children)))
    }

    /// Whether the finer lattice samples on a cell's edges and faces (which finer neighbours see)
    /// agree with the cell's corners on topology: as many sign changes along every edge, and no
    /// surface poking through a face whose four corners agree. A cell that disagrees would
    /// contour differently from its neighbours and leave a crack.
    fn consistent(&mut self, view: &ChunkView, cell: IVec3, level: u8) -> bool {
        let span = 1i32 << level;
        let inside = self.corners(view, cell, level).map(|d| d < 0.0);
        for [a, b] in EDGEVMAP {
            let (Some(&in_a), Some(&in_b)) = (inside.get(a), inside.get(b)) else { continue };
            let dir = child(b) - child(a);
            let mut changes = 0;
            let mut prev = in_a;
            for k in 1..=span {
                let s = if k == span { in_b } else { self.corner(view, cell + child(a) * span + dir * k) < 0.0 };
                changes += u32::from(s != prev);
                prev = s;
            }
            if changes != u32::from(in_a != in_b) {
                return false;
            }
        }
        for axis in 0..3 {
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            for side in [0, 1] {
                let mut corners = inside.iter().enumerate().filter(|(i, _)| child(*i)[axis] == side).map(|(_, c)| *c);
                let Some(first) = corners.next() else { continue };
                if corners.any(|c| c != first) {
                    continue;
                }
                for a in 1..span {
                    for b in 1..span {
                        let mut o = IVec3::ZERO;
                        o[axis] = side * span;
                        o[u] = a;
                        o[v] = b;
                        if (self.corner(view, cell + o) < 0.0) != first {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    fn leaf(&mut self, view: &ChunkView, cell: IVec3, level: u8) -> Option<Leaf> {
        let g = self.grid;
        let span = 1i32 << level;
        let d = self.corners(view, cell, level);
        let corners = inside_mask(&d);
        if corners == 0 || corners == 0xff {
            return None;
        }
        // Surface-nets placement, one vertex per sheet: the mean of its edge crossings...
        let at = |i: usize| g.point(cell + child(i) * span);
        let (sheet, count) = edge_sheets(corners);
        let mut sums = [(Vec3A::ZERO, 0.0f32); 4];
        for ([a, b], s) in EDGEVMAP.into_iter().zip(sheet) {
            let (Some(&da), Some(&db), Some(slot)) = (d.get(a), d.get(b), sums.get_mut(usize::from(s))) else { continue };
            slot.0 += at(a).lerp(at(b), da / (da - db));
            slot.1 += 1.0;
        }
        // ...pulled onto the surface, staying near the cell so faces don't fold.
        let size = g.size(level);
        let (lo, hi) = (at(0) - Vec3A::splat(size * 0.25), at(7) + Vec3A::splat(size * 0.25));
        let verts = sums
            .iter()
            .take(usize::from(count))
            .map(|&(sum, n)| {
                let mut p = sum / n.max(1.0);
                for _ in 0..2 {
                    // `Field::gradient` is a raw central difference over ±1 mm: scale it to a true gradient.
                    let grad = view.gradient(p) / 2e-3;
                    let len2 = grad.length_squared();
                    if len2 > 1e-12 {
                        p = (p - grad * (view.distance(p) / len2)).clamp(lo, hi);
                    }
                }
                (p.into(), view.gradient(p).normalize_or(Vec3A::Y).into())
            })
            .collect();
        Some(Leaf { corners, level, verts, sheet, index: 0 })
    }
}

impl Mesher for AdaptiveContour {
    fn mesh(&self, field: &BodyField, settings: &MeshSettings) -> Result<RawMesh, SurfaceError> {
        let base = settings.voxel_size.max(1e-4);
        let detail = settings.detail_levels.min(4);
        let fine = base / (1u32 << detail).to_f32();
        let b = field.bounds();
        let extent = 4.0f32.mul_add(base, Vec3::from(b.max - b.min).max_element());
        let root_level = detail + (extent / base).log2().ceil().max(0.0).sat_u8();
        let grid = Grid { origin: b.min - Vec3A::splat(2.0 * base), fine, base_level: detail, per_radius: settings.cells_per_radius.max(0.5) };

        // Upper levels serially, down to subtrees that build in parallel.
        let job_level = root_level.saturating_sub(3).max(detail);
        let mut jobs: Vec<IVec3> = Vec::new();
        let whole = field.view(field.bounds());
        let mut root = top(&grid, &whole, IVec3::ZERO, root_level, job_level, &mut jobs);
        let mut results: Vec<Option<Node>> = super::par_jobs(&jobs, || (), |(), cell| {
            // A fresh corner cache per job: corners on a job's border are sampled through its own
            // view, so the mesh doesn't depend on which jobs share a thread.
            let mut builder = Builder { grid: &grid, corners: HashMap::new() };
            let view = field.view(grid.region(*cell, job_level, grid.fine));
            builder.build(&view, *cell, job_level)
        })
        .map_err(|_| SurfaceError::WorkerPanicked)?;
        if let Some(r) = root.as_mut() {
            fill(r, &mut results);
        }
        let mut root = root.and_then(prune);

        let mut mesh = RawMesh::default();
        if let Some(r) = root.as_mut() {
            number(r, &mut mesh);
            cell_proc(r, &mut mesh.indices);
        }
        mesh.checked(settings.max_triangles)
    }
}

/// The top of the tree, with subtrees at `job_level` left as jobs.
fn top(grid: &Grid, view: &ChunkView, cell: IVec3, level: u8, job_level: u8, jobs: &mut Vec<IVec3>) -> Option<Node> {
    if !grid.near_surface(view, cell, level) {
        return None;
    }
    if level <= job_level {
        jobs.push(cell);
        return Some(Node::Pending(jobs.len() - 1));
    }
    let narrowed = view.narrow(grid.region(cell, level, grid.fine));
    let half = 1i32 << (level - 1);
    let children: [Option<Node>; 8] = std::array::from_fn(|i| top(grid, &narrowed, cell + child(i) * half, level - 1, job_level, jobs));
    children.iter().any(Option::is_some).then(|| Node::Internal(Box::new(children)))
}

fn fill(node: &mut Node, results: &mut [Option<Node>]) {
    if let Node::Internal(children) = node {
        for slot in children.iter_mut() {
            if let Some(Node::Pending(i)) = slot {
                *slot = results.get_mut(*i).and_then(Option::take);
            } else if let Some(c) = slot {
                fill(c, results);
            }
        }
    }
}

/// Drops internal nodes left without children once the jobs are in.
fn prune(node: Node) -> Option<Node> {
    match node {
        Node::Internal(children) => {
            let children: [Option<Node>; 8] = (*children).map(|c| c.and_then(prune));
            children.iter().any(Option::is_some).then(|| Node::Internal(Box::new(children)))
        }
        Node::Pending(_) => None,
        leaf @ Node::Leaf(_) => Some(leaf),
    }
}

/// Gives every leaf's vertex its index in `mesh`.
fn number(node: &mut Node, mesh: &mut RawMesh) {
    match node {
        Node::Internal(children) => children.iter_mut().flatten().for_each(|c| number(c, mesh)),
        Node::Leaf(l) => {
            l.index = mesh.positions.len().clamp_u32();
            for &(pos, normal) in &l.verts {
                mesh.positions.push(pos);
                mesh.normals.push(normal);
            }
        }
        Node::Pending(_) => {}
    }
}

fn cell_proc(node: &Node, out: &mut Vec<u32>) {
    let Node::Internal(children) = node else { return };
    for c in children.iter().flatten() {
        cell_proc(c, out);
    }
    let at = |i: usize| children.get(i).and_then(Option::as_ref);
    for [a, b, dir] in CELL_PROC_FACE_MASK {
        if let (Some(a), Some(b)) = (at(a), at(b)) {
            face_proc([a, b], dir, out);
        }
    }
    for [a, b, c, d, dir] in CELL_PROC_EDGE_MASK {
        if let (Some(a), Some(b), Some(c), Some(d)) = (at(a), at(b), at(c), at(d)) {
            edge_proc([a, b, c, d], dir, out);
        }
    }
}

fn face_proc(nodes: [&Node; 2], dir: usize, out: &mut Vec<u32>) {
    if !nodes[0].internal() && !nodes[1].internal() {
        return;
    }
    for &[c0, c1, d] in FACE_PROC_FACE_MASK.get(dir).into_iter().flatten() {
        if let (Some(a), Some(b)) = (nodes[0].child(c0), nodes[1].child(c1)) {
            face_proc([a, b], d, out);
        }
    }
    for &[o, c0, c1, c2, c3, d] in FACE_PROC_EDGE_MASK.get(dir).into_iter().flatten() {
        let Some(order) = FACE_PROC_ORDERS.get(o) else { continue };
        let cs = [c0, c1, c2, c3];
        let picked: [Option<&Node>; 4] =
            std::array::from_fn(|j| order.get(j).zip(cs.get(j)).and_then(|(&side, &c)| nodes.get(side)?.child(c)));
        if let [Some(a), Some(b), Some(c), Some(e)] = picked {
            edge_proc([a, b, c, e], d, out);
        }
    }
}

fn edge_proc(nodes: [&Node; 4], dir: usize, out: &mut Vec<u32>) {
    if nodes.iter().all(|n| !n.internal()) {
        process_edge(nodes, dir, out);
        return;
    }
    for &[c0, c1, c2, c3, d] in EDGE_PROC_EDGE_MASK.get(dir).into_iter().flatten() {
        let cs = [c0, c1, c2, c3];
        let picked: [Option<&Node>; 4] = std::array::from_fn(|j| nodes.get(j).zip(cs.get(j)).and_then(|(n, &c)| n.child(c)));
        if let [Some(a), Some(b), Some(c), Some(e)] = picked {
            edge_proc([a, b, c, e], d, out);
        }
    }
}

/// Four leaves around one edge: a quad if the smallest leaf's copy of the edge changes sign.
fn process_edge(nodes: [&Node; 4], dir: usize, out: &mut Vec<u32>) {
    let Some(edges) = PROCESS_EDGE_MASK.get(dir) else { return };
    let mut min_level = u8::MAX;
    let mut sign_change = false;
    let mut flip = false;
    let mut idx = [0u32; 4];
    for ((n, &edge), slot) in nodes.iter().zip(edges).zip(&mut idx) {
        let Node::Leaf(l) = n else { return };
        let Some(&[c1, c2]) = EDGEVMAP.get(edge) else { return };
        let (m1, m2) = (l.corners >> c1 & 1 == 1, l.corners >> c2 & 1 == 1);
        if l.level < min_level {
            min_level = l.level;
            flip = m1;
            sign_change = m1 != m2;
        }
        // The vertex of the sheet crossing this edge (a coarser cell whose own copy of the edge
        // doesn't cross has one sheet).
        *slot = l.index + u32::from(l.sheet.get(edge).copied().filter(|s| *s != u8::MAX).unwrap_or(0));
    }
    if !sign_change {
        return;
    }
    let [i0, i1, i2, i3] = idx;
    let tris = if flip { [[i0, i3, i1], [i0, i2, i3]] } else { [[i0, i1, i3], [i0, i3, i2]] };
    for [a, b, c] in tris {
        if a != b && b != c && a != c {
            out.extend([a, b, c]);
        }
    }
}

/// Whether the surface bends sharply within a cell of `size` centred on `p`: normals half a cell
/// either side of the nearest surface point differ by more than ~16° (a radius of curvature
/// under ~3.5 cells). A nose, lip or knuckle is
/// a small feature on a thick body, which [`local_radius`] alone would mesh at the base size.
fn curved(view: &ChunkView, p: Vec3A, size: f32) -> bool {
    const MIN_COS: f32 = 0.96;
    let normal = |q: Vec3A| view.gradient(q).normalize_or(Vec3A::Y);
    let dist = view.distance(p);
    if dist.abs() > size {
        return false;
    }
    let surface = p - normal(p) * dist;
    let n = normal(surface);
    let (tangent, bitangent) = Vec3::from(n).any_orthonormal_pair();
    let half = size * 0.5;
    [tangent, -tangent, bitangent, -bitangent].into_iter().any(|o| n.dot(normal(surface + Vec3A::from(o) * half)) < MIN_COS)
}

/// How thick the body is near `p`: the deepest point reached marching inward from the nearest
/// surface (the inscribed-ball radius), up to `limit` × 4. A finger is thin; a bump on a torso
/// is thick underneath, however small the primitive that made it.
fn local_radius(view: &ChunkView, p: Vec3A, limit: f32) -> f32 {
    let grad = |q: Vec3A| (view.gradient(q) / 2e-3).normalize_or(Vec3A::Y);
    let n = grad(p);
    let surface = p - n * view.distance(p);
    let n = grad(surface);
    let mut deepest = 0.0f32;
    for k in [0.25, 0.5, 1.0, 2.0, 4.0] {
        let d = -view.distance(surface - n * (limit * k));
        deepest = deepest.max(d);
        if d < deepest * 0.5 {
            break;
        }
    }
    deepest
}

/// Bit `i` set where corner `i` is inside.
fn inside_mask(d: &[f32; 8]) -> u8 {
    (0u8..).zip(d).fold(0, |m, (i, v)| if *v < 0.0 { m | 1 << i } else { m })
}

/// Labels the corners on one side of the surface (`inside` or not) by connected group along the
/// cube's edges (`u8::MAX` for the other side's corners), and counts the groups.
fn corner_groups(mask: u8, inside: bool) -> ([u8; 8], u8) {
    let member = |i: usize| (mask >> i & 1 == 1) == inside;
    let mut label = [u8::MAX; 8];
    let mut count = 0u8;
    for start in 0..8 {
        if !member(start) || label.get(start).is_some_and(|l| *l != u8::MAX) {
            continue;
        }
        let mut stack = vec![start];
        if let Some(l) = label.get_mut(start) {
            *l = count;
        }
        while let Some(c) = stack.pop() {
            for [a, b] in EDGEVMAP {
                let other = if a == c { b } else if b == c { a } else { continue };
                if let Some(l) = label.get_mut(other).filter(|l| **l == u8::MAX && member(other)) {
                    *l = count;
                    stack.push(other);
                }
            }
        }
        count += 1;
    }
    (label, count)
}

/// Whether a corner pattern has more than one connected group of inside corners, or of outside
/// corners, along the cube's edges (one vertex can't represent it).
fn ambiguous(mask: u8) -> bool {
    mask != 0 && mask != 0xff && (corner_groups(mask, true).1 > 1 || corner_groups(mask, false).1 > 1)
}

/// Which sheet of surface each of a cell's edges crosses: the group of its corner on whichever
/// side is split into several (the inside, if both are), so two fingers in one finest cell get a
/// vertex each instead of being pinched into one. `u8::MAX` for edges that don't cross.
fn edge_sheets(mask: u8) -> ([u8; 12], u8) {
    let (inside, n_in) = corner_groups(mask, true);
    let (outside, n_out) = corner_groups(mask, false);
    let (labels, count, side) = if n_in > 1 || n_out <= 1 { (inside, n_in.max(1), true) } else { (outside, n_out, false) };
    let mut sheets = [u8::MAX; 12];
    for (sheet, [a, b]) in sheets.iter_mut().zip(EDGEVMAP) {
        let (ia, ib) = (mask >> a & 1 == 1, mask >> b & 1 == 1);
        if ia == ib {
            continue;
        }
        let corner = if ia == side { a } else { b };
        *sheet = labels.get(corner).copied().filter(|l| *l != u8::MAX).unwrap_or(0).min(count - 1);
    }
    (sheets, count)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::anatomy::{LodLevel, build_src};
    use crate::num::Idx;
    use crate::surface::{BodyFieldOptions, SurfaceNets};

    fn field(src: &str) -> BodyField {
        let a = build_src(src);
        BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: true })
    }

    /// (edges not shared by exactly two triangles, directed edges used twice, signed volume).
    fn check(m: &RawMesh) -> (usize, usize, f32) {
        let mut undirected: HashMap<(u32, u32), u32> = HashMap::new();
        let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
        let mut volume = 0.0;
        for t in m.indices.chunks(3) {
            let p = [m.positions[t[0].idx()], m.positions[t[1].idx()], m.positions[t[2].idx()]];
            volume += p[0].dot(p[1].cross(p[2])) / 6.0;
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *undirected.entry((a.min(b), a.max(b))).or_default() += 1;
                *directed.entry((a, b)).or_default() += 1;
            }
        }
        (undirected.values().filter(|n| **n != 2).count(), directed.values().filter(|n| **n > 1).count(), volume)
    }

    #[test]
    fn adaptive_mesh_is_closed_and_refines_thin_parts() {
        let f = field(include_str!("../../assets/mobs/biped.body.ron"));
        let uniform = MeshSettings { voxel_size: 0.008, ..default() };
        let adaptive = MeshSettings { detail_levels: 2, ..uniform };
        let t = std::time::Instant::now();
        let m = AdaptiveContour.mesh(&f, &adaptive).unwrap();
        let took = t.elapsed().as_secs_f32();
        let nets = SurfaceNets.mesh(&f, &uniform).unwrap();
        let (open, twice, volume) = check(&m);
        let (_, _, nets_volume) = check(&nets);
        println!(
            "adaptive: {} verts {} tris in {took:.2}s; open edges {open}, doubled directed edges {twice}, volume {volume:.4} (surface nets {nets_volume:.4}, {} tris)",
            m.positions.len(),
            m.triangle_count(),
            nets.triangle_count()
        );
        assert_eq!(volume.signum(), nets_volume.signum(), "same winding as surface nets");
        assert!((volume / nets_volume - 1.0).abs() < 0.05, "same body: {volume} vs {nets_volume}");
        let edges = m.triangle_count() * 3 / 2;
        // A uniform grid leaves a handful too: finest cells with two sheets of surface.
        assert!(open <= 20, "{open} of {edges} edges are open or non-manifold");
        // Nor a couple of edges three faces meet at: one vertex per cell can't keep every
        // near-tangent crossing manifold (a known limit of dual contouring); which cells it hits
        // depends on where the body falls on the grid.
        assert!(twice <= 4, "{twice} directed edges used twice (flipped faces)");
    }
}
