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

use super::{BodyField, ChunkView, Field, MeshSettings, Mesher, RawMesh, SurfaceError};

/// Octree dual contouring; see the module docs.
#[derive(Default, Clone, Copy)]
pub struct AdaptiveContour;

/// Child (and corner) `i` sits at `(i >> 2 & 1, i >> 1 & 1, i & 1)`.
const CHILD: [IVec3; 8] = [
    IVec3::new(0, 0, 0),
    IVec3::new(0, 0, 1),
    IVec3::new(0, 1, 0),
    IVec3::new(0, 1, 1),
    IVec3::new(1, 0, 0),
    IVec3::new(1, 0, 1),
    IVec3::new(1, 1, 0),
    IVec3::new(1, 1, 1),
];
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

enum Node {
    Internal(Box<[Option<Node>; 8]>),
    Leaf(Leaf),
    /// A subtree being built on another thread (index into the job list).
    Pending(usize),
}

struct Leaf {
    /// Bit `i` set: corner `i` is inside.
    corners: u8,
    /// 0 = finest.
    level: u8,
    pos: Vec3,
    normal: Vec3,
    index: u32,
}

impl Node {
    fn internal(&self) -> bool {
        matches!(self, Node::Internal(_))
    }

    fn child(&self, i: usize) -> Option<&Node> {
        match self {
            Node::Internal(c) => c[i].as_ref(),
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

    fn region(&self, cell: IVec3, level: u8, pad: f32) -> Aabb3d {
        let lo = self.point(cell);
        let size = self.fine * (1u32 << level) as f32;
        Aabb3d { min: lo - Vec3A::splat(pad), max: lo + Vec3A::splat(size + pad) }
    }

    /// Whether the surface may pass through a cell. An exact distance field would allow the
    /// half-diagonal (0.87 × size); ours overstates by up to ~20% where limbs switch between
    /// smooth and hard unions, and a cell dropped wrongly leaves a hole, so the bound is looser.
    fn near_surface(&self, view: &ChunkView, cell: IVec3, level: u8) -> bool {
        let size = self.fine * (1u32 << level) as f32;
        let centre = self.point(cell) + Vec3A::splat(size * 0.5);
        view.distance(centre).abs() <= size * 1.2 + self.fine
    }
}

/// One thread's octree builder: shares corner samples between neighbouring cells.
struct Builder<'a> {
    field: &'a BodyField,
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

    fn build(&mut self, view: &ChunkView, cell: IVec3, level: u8) -> Option<Node> {
        let g = self.grid;
        if !g.near_surface(view, cell, level) {
            return None;
        }
        let size = g.fine * (1u32 << level) as f32;
        let split = level > 0 && {
            let centre = g.point(cell) + Vec3A::splat(size * 0.5);
            level > g.base_level || size > (local_radius(view, centre, g.fine * (1u32 << g.base_level) as f32) / g.per_radius).max(g.fine) * 1.001
        };
        // A cell whose corners imply more than one sheet of surface (the gap between thumb and
        // palm) would pinch them into one vertex: split it while it can be.
        let split = split || level > 0 && {
            let span = 1i32 << level;
            let mask = (0..8).fold(0u8, |m, i| if self.corner(view, cell + CHILD[i] * span) < 0.0 { m | 1 << i } else { m });
            ambiguous(mask) || !self.consistent(view, cell, level)
        };
        if !split {
            return self.leaf(view, cell, level).map(Node::Leaf);
        }
        let narrowed = (level >= 3).then(|| view.narrow(g.region(cell, level, g.fine)));
        let view = narrowed.as_ref().unwrap_or(view);
        let half = 1i32 << (level - 1);
        let children: [Option<Node>; 8] = std::array::from_fn(|i| self.build(view, cell + CHILD[i] * half, level - 1));
        children.iter().any(Option::is_some).then(|| Node::Internal(Box::new(children)))
    }

    /// Whether the finer lattice samples on a cell's edges and faces (which finer neighbours see)
    /// agree with the cell's corners on topology: as many sign changes along every edge, and no
    /// surface poking through a face whose four corners agree. A cell that disagrees would
    /// contour differently from its neighbours and leave a crack.
    fn consistent(&mut self, view: &ChunkView, cell: IVec3, level: u8) -> bool {
        let span = 1i32 << level;
        let inside: [bool; 8] = std::array::from_fn(|i| self.corner(view, cell + CHILD[i] * span) < 0.0);
        for [a, b] in EDGEVMAP {
            let dir = CHILD[b] - CHILD[a];
            let mut changes = 0;
            let mut prev = inside[a];
            for k in 1..=span {
                let s = if k == span { inside[b] } else { self.corner(view, cell + CHILD[a] * span + dir * k) < 0.0 };
                changes += (s != prev) as u32;
                prev = s;
            }
            if changes != (inside[a] != inside[b]) as u32 {
                return false;
            }
        }
        for axis in 0..3 {
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            for side in [0, 1] {
                let corners: Vec<bool> = (0..8).filter(|&i| CHILD[i][axis] == side).map(|i| inside[i]).collect();
                if corners.iter().any(|c| *c != corners[0]) {
                    continue;
                }
                for a in 1..span {
                    for b in 1..span {
                        let mut o = IVec3::ZERO;
                        o[axis] = side * span;
                        o[u] = a;
                        o[v] = b;
                        if (self.corner(view, cell + o) < 0.0) != corners[0] {
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
        let d: [f32; 8] = std::array::from_fn(|i| self.corner(view, cell + CHILD[i] * span));
        let corners = (0..8).fold(0u8, |m, i| if d[i] < 0.0 { m | 1 << i } else { m });
        if corners == 0 || corners == 0xff {
            return None;
        }
        // Surface-nets placement: the mean of the edge crossings...
        let at = |i: usize| g.point(cell + CHILD[i] * span);
        let (mut sum, mut n) = (Vec3A::ZERO, 0.0);
        for [a, b] in EDGEVMAP {
            if (d[a] < 0.0) != (d[b] < 0.0) {
                let t = d[a] / (d[a] - d[b]);
                sum += at(a).lerp(at(b), t);
                n += 1.0;
            }
        }
        let mut p = sum / n;
        // ...pulled onto the surface, staying near the cell so faces don't fold.
        let size = g.fine * span as f32;
        let (lo, hi) = (at(0) - Vec3A::splat(size * 0.25), at(7) + Vec3A::splat(size * 0.25));
        for _ in 0..2 {
            // `Field::gradient` is a raw central difference over ±1 mm: scale it to a true gradient.
            let grad = view.gradient(p) / 2e-3;
            let len2 = grad.length_squared();
            if len2 > 1e-12 {
                p = (p - grad * (view.distance(p) / len2)).clamp(lo, hi);
            }
        }
        let normal = view.gradient(p).normalize_or(Vec3A::Y);
        Some(Leaf { corners, level, pos: p.into(), normal: normal.into(), index: 0 })
    }
}

impl Mesher for AdaptiveContour {
    fn mesh(&self, field: &BodyField, settings: &MeshSettings) -> Result<RawMesh, SurfaceError> {
        let base = settings.voxel_size.max(1e-4);
        let detail = settings.detail_levels.min(4);
        let fine = base / (1u32 << detail) as f32;
        let b = field.bounds();
        let extent = Vec3::from(b.max - b.min).max_element() + 4.0 * base;
        let root_level = detail + (extent / base).log2().ceil().max(0.0) as u8;
        let grid = Grid { origin: b.min - Vec3A::splat(2.0 * base), fine, base_level: detail, per_radius: settings.cells_per_radius.max(0.5) };

        // Upper levels serially, down to subtrees that build in parallel.
        let job_level = root_level.saturating_sub(3).max(detail);
        let mut jobs: Vec<IVec3> = Vec::new();
        let whole = field.view(field.bounds());
        let mut root = top(&grid, &whole, IVec3::ZERO, root_level, job_level, &mut jobs);
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(jobs.len().max(1));
        let built: Vec<(usize, Option<Node>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let (jobs, grid) = (&jobs, &grid);
                    scope.spawn(move || {
                        let mut builder = Builder { field, grid, corners: HashMap::new() };
                        let mut out = Vec::new();
                        for (i, cell) in jobs.iter().enumerate().skip(t).step_by(threads) {
                            let view = field.view(grid.region(*cell, job_level, grid.fine));
                            out.push((i, builder.build(&view, *cell, job_level)));
                        }
                        out
                    })
                })
                .collect();
            handles.into_iter().flat_map(|h| h.join().expect("meshing thread panicked")).collect()
        });
        let mut results: Vec<Option<Node>> = (0..jobs.len()).map(|_| None).collect();
        for (i, node) in built {
            results[i] = node;
        }
        if let Some(r) = root.as_mut() {
            fill(r, &mut results);
        }
        let mut root = root.and_then(prune);

        let mut mesh = RawMesh::default();
        if let Some(r) = root.as_mut() {
            number(r, &mut mesh);
            cell_proc(r, &mut mesh.indices);
        }
        if mesh.indices.is_empty() {
            return Err(SurfaceError::Empty);
        }
        if mesh.triangle_count() > settings.max_triangles as usize {
            return Err(SurfaceError::TooManyTriangles { got: mesh.triangle_count(), max: settings.max_triangles as usize });
        }
        Ok(mesh)
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
    let children: [Option<Node>; 8] = std::array::from_fn(|i| top(grid, &narrowed, cell + CHILD[i] * half, level - 1, job_level, jobs));
    children.iter().any(Option::is_some).then(|| Node::Internal(Box::new(children)))
}

fn fill(node: &mut Node, results: &mut [Option<Node>]) {
    if let Node::Internal(children) = node {
        for slot in children.iter_mut() {
            if let Some(Node::Pending(i)) = slot {
                *slot = results[*i].take();
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
        leaf => Some(leaf),
    }
}

/// Gives every leaf's vertex its index in `mesh`.
fn number(node: &mut Node, mesh: &mut RawMesh) {
    match node {
        Node::Internal(children) => children.iter_mut().flatten().for_each(|c| number(c, mesh)),
        Node::Leaf(l) => {
            l.index = mesh.positions.len() as u32;
            mesh.positions.push(l.pos);
            mesh.normals.push(l.normal);
        }
        Node::Pending(_) => {}
    }
}

fn cell_proc(node: &Node, out: &mut Vec<u32>) {
    let Node::Internal(children) = node else { return };
    for c in children.iter().flatten() {
        cell_proc(c, out);
    }
    for [a, b, dir] in CELL_PROC_FACE_MASK {
        if let (Some(a), Some(b)) = (&children[a], &children[b]) {
            face_proc([a, b], dir, out);
        }
    }
    for [a, b, c, d, dir] in CELL_PROC_EDGE_MASK {
        if let (Some(a), Some(b), Some(c), Some(d)) = (&children[a], &children[b], &children[c], &children[d]) {
            edge_proc([a, b, c, d], dir, out);
        }
    }
}

fn face_proc(nodes: [&Node; 2], dir: usize, out: &mut Vec<u32>) {
    if !nodes[0].internal() && !nodes[1].internal() {
        return;
    }
    for [c0, c1, d] in FACE_PROC_FACE_MASK[dir] {
        if let (Some(a), Some(b)) = (nodes[0].child(c0), nodes[1].child(c1)) {
            face_proc([a, b], d, out);
        }
    }
    const ORDERS: [[usize; 4]; 2] = [[0, 0, 1, 1], [0, 1, 0, 1]];
    for [o, c0, c1, c2, c3, d] in FACE_PROC_EDGE_MASK[dir] {
        let order = ORDERS[o];
        let cs = [c0, c1, c2, c3];
        let picked: [Option<&Node>; 4] = std::array::from_fn(|j| nodes[order[j]].child(cs[j]));
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
    for [c0, c1, c2, c3, d] in EDGE_PROC_EDGE_MASK[dir] {
        let cs = [c0, c1, c2, c3];
        let picked: [Option<&Node>; 4] = std::array::from_fn(|j| nodes[j].child(cs[j]));
        if let [Some(a), Some(b), Some(c), Some(e)] = picked {
            edge_proc([a, b, c, e], d, out);
        }
    }
}

/// Four leaves around one edge: a quad if the smallest leaf's copy of the edge changes sign.
fn process_edge(nodes: [&Node; 4], dir: usize, out: &mut Vec<u32>) {
    let mut min_level = u8::MAX;
    let mut sign_change = false;
    let mut flip = false;
    let mut idx = [0u32; 4];
    for (i, n) in nodes.iter().enumerate() {
        let Node::Leaf(l) = n else { return };
        let [c1, c2] = EDGEVMAP[PROCESS_EDGE_MASK[dir][i]];
        let (m1, m2) = (l.corners >> c1 & 1 == 1, l.corners >> c2 & 1 == 1);
        if l.level < min_level {
            min_level = l.level;
            flip = m1;
            sign_change = m1 != m2;
        }
        idx[i] = l.index;
    }
    if !sign_change {
        return;
    }
    let tris = if flip { [[0, 3, 1], [0, 2, 3]] } else { [[0, 1, 3], [0, 3, 2]] };
    for t in tris {
        let (a, b, c) = (idx[t[0]], idx[t[1]], idx[t[2]]);
        if a != b && b != c && a != c {
            out.extend([a, b, c]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::anatomy::{BodyPlan, Form, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{BodyFieldOptions, SurfaceNets};

    fn field(src: &str) -> BodyField {
        let plan: BodyPlan = ron::from_str(src).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: true })
    }

    /// (edges not shared by exactly two triangles, directed edges used twice, signed volume).
    fn check(m: &RawMesh) -> (usize, usize, f32) {
        let mut undirected: HashMap<(u32, u32), u32> = HashMap::new();
        let mut directed: HashMap<(u32, u32), u32> = HashMap::new();
        let mut volume = 0.0;
        for t in m.indices.chunks(3) {
            let p = [m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]];
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
        assert_eq!(twice, 0, "flipped faces");
    }
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

/// Whether a corner pattern has more than one connected group of inside corners, or of outside
/// corners, along the cube's edges (one vertex can't represent it).
fn ambiguous(mask: u8) -> bool {
    let groups = |inside: bool| {
        let member = |i: usize| (mask >> i & 1 == 1) == inside;
        let mut seen = 0u8;
        let mut count = 0;
        for start in (0..8).filter(|&i| member(i)) {
            if seen >> start & 1 == 1 {
                continue;
            }
            count += 1;
            let mut stack = vec![start];
            seen |= 1 << start;
            while let Some(c) = stack.pop() {
                for [a, b] in EDGEVMAP {
                    let other = if a == c { b } else if b == c { a } else { continue };
                    if member(other) && seen >> other & 1 == 0 {
                        seen |= 1 << other;
                        stack.push(other);
                    }
                }
            }
        }
        count
    };
    mask != 0 && mask != 0xff && (groups(true) > 1 || groups(false) > 1)
}
