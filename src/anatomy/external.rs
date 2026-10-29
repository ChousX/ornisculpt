//! Authored mesh data used by [`PartKind::Sculpt`](super::PartKind::Sculpt) (blended in as an SDF)
//! and [`HardKind::Mesh`](super::HardKind::Mesh) (attached rigidly).
//!
//! Anatomy stays a pure function: the meshes are resolved before building and passed in as
//! [`PartAssets`]; their content hashes go into the body cache key.
//!
//! Authoring convention (glTF's own): Y up, +Z forward, origin at the attach point. With
//! `Orient::Model` a part keeps that orientation on the creature; mirrored copies are reflected.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_64;

use crate::num::Idx;

/// How an authored part is oriented where it attaches.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Orient {
    /// Keep the authored orientation (Y up, +Z forward) regardless of the slot's direction.
    #[default]
    Model,
    /// Align the authored +Y with the slot's outward direction (+Z toward the slot's forward).
    Slot,
}

/// Triangle mesh extracted from an asset.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TriMesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
    /// Content hash (positions + indices), part of body cache keys.
    pub hash: u64,
}

impl TriMesh {
    #[must_use]
    pub fn new(positions: Vec<Vec3>, normals: Vec<Vec3>, indices: Vec<u32>) -> Self {
        let mut bytes = Vec::with_capacity(positions.len() * 12 + indices.len() * 4);
        for p in &positions {
            bytes.extend(p.to_array().map(f32::to_bits).iter().flat_map(|b| b.to_le_bytes()));
        }
        for i in &indices {
            bytes.extend(i.to_le_bytes());
        }
        Self { positions, normals, indices, hash: xxh3_64(&bytes) }
    }

    /// Volume enclosed by the mesh; negative if its triangles wind inward.
    #[must_use]
    pub fn signed_volume(&self) -> f32 {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter_map(|t| {
                let [a, b, c] = t.map(|i| self.positions.get(i.idx()).copied());
                Some(a?.dot(b?.cross(c?)) / 6.0)
            })
            .sum()
    }

    #[must_use]
    pub fn bounds(&self) -> (Vec3, Vec3) {
        self.positions
            .iter()
            .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)))
    }
}

/// A signed distance field sampled on a regular grid (values at cell centres).
#[derive(Clone, Debug, PartialEq)]
pub struct SdfGrid {
    /// Centre of the first cell.
    pub origin: Vec3,
    pub cell: Vec3,
    pub dims: UVec3,
    pub values: Vec<f32>,
}

impl SdfGrid {
    /// Converts a watertight, consistently wound mesh (inward or outward). `resolution` = cells
    /// along the longest axis; 2 cells of padding.
    #[must_use]
    pub fn from_mesh(mesh: &TriMesh, resolution: u16) -> Self {
        let (lo, hi) = mesh.bounds();
        let size = (hi - lo).max(Vec3::splat(1e-4));
        let cell = size.max_element() / f32::from(resolution.max(4));
        let pad = Vec3::splat(cell * 2.0);
        let (lo, hi) = (lo - pad, hi + pad);
        let dims = ((hi - lo) / cell).ceil().as_uvec3().max(UVec3::splat(2));
        let origin = lo + Vec3::splat(cell * 0.5);
        let mut queries = Vec::with_capacity((dims.x * dims.y * dims.z).idx());
        for z in 0..dims.z {
            for y in 0..dims.y {
                for x in 0..dims.x {
                    queries.push((origin + UVec3::new(x, y, z).as_vec3() * cell).to_array());
                }
            }
        }
        let vertices: Vec<[f32; 3]> = mesh.positions.iter().map(Vec3::to_array).collect();
        // The sign method needs outward winding; accept either by checking the signed volume.
        let mut indices = mesh.indices.clone();
        if mesh.signed_volume() < 0.0 {
            indices.as_chunks_mut::<3>().0.iter_mut().for_each(|t| t.swap(1, 2));
        }
        let values = mesh_to_sdf::generate_sdf(
            &vertices,
            mesh_to_sdf::Topology::TriangleList(Some(&indices)),
            &queries,
            // Raycast signs flip on symmetry planes (rays graze shared edges); pseudo-normal signs
            // are exact for watertight, consistently wound meshes.
            mesh_to_sdf::AccelerationMethod::Bvh(mesh_to_sdf::SignMethod::Normal),
        );
        let mut grid = Self { origin, cell: Vec3::splat(cell), dims, values };
        grid.fill_voids();
        grid
    }

    /// Makes enclosed pockets solid: a mouth cavity, the cups behind modelled eyelids, the inside
    /// of a hollow mesh. Outside space is what the padding reaches (a flood fill through positive
    /// cells from the grid's faces); any other positive cell is a void and goes inside, as far in
    /// as it was from the pocket's wall.
    fn fill_voids(&mut self) {
        let (nx, ny, nz) = (self.dims.x.idx(), self.dims.y.idx(), self.dims.z.idx());
        let at = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
        let mut outside = vec![false; self.values.len()];
        let mut stack: Vec<(usize, usize, usize)> = Vec::new();
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    if (x == 0 || y == 0 || z == 0 || x + 1 == nx || y + 1 == ny || z + 1 == nz) && self.values.get(at(x, y, z)).is_some_and(|v| *v > 0.0) {
                        stack.push((x, y, z));
                    }
                }
            }
        }
        while let Some((x, y, z)) = stack.pop() {
            let i = at(x, y, z);
            if outside.get(i).copied().unwrap_or(true) {
                continue;
            }
            if let Some(o) = outside.get_mut(i) {
                *o = true;
            }
            let near = [
                (x.wrapping_sub(1), y, z),
                (x + 1, y, z),
                (x, y.wrapping_sub(1), z),
                (x, y + 1, z),
                (x, y, z.wrapping_sub(1)),
                (x, y, z + 1),
            ];
            for (a, b, c) in near {
                if a < nx && b < ny && c < nz {
                    let j = at(a, b, c);
                    if !outside.get(j).copied().unwrap_or(true) && self.values.get(j).is_some_and(|v| *v > 0.0) {
                        stack.push((a, b, c));
                    }
                }
            }
        }
        let floor = self.cell.min_element() * 0.5;
        for (v, out) in self.values.iter_mut().zip(&outside) {
            if *v > 0.0 && !*out {
                *v = -v.max(floor);
            }
        }
    }

    #[must_use]
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let half = self.cell * 0.5;
        (self.origin - half, self.origin + (self.dims.as_vec3() - Vec3::ONE) * self.cell + half)
    }

    /// The lowest point of the surface, as `height` measures it (a grid point's height after
    /// the part's transform), with `scale` the transform's scale: the lowest cell inside the
    /// surface, less its distance to it. Within about a cell, where the bounding box can be far
    /// off (padding, and a tilted box's corner dips well below the mesh). `None` if no cell is
    /// inside.
    #[must_use]
    pub fn lowest(&self, height: impl Fn(Vec3) -> f32, scale: f32) -> Option<f32> {
        let mut low: Option<f32> = None;
        for z in 0..self.dims.z {
            for y in 0..self.dims.y {
                for x in 0..self.dims.x {
                    let v = self.at(x, y, z);
                    if v > 0.0 {
                        continue;
                    }
                    let h = v.mul_add(scale, height(self.origin + UVec3::new(x, y, z).as_vec3() * self.cell));
                    low = Some(low.map_or(h, |l| l.min(h)));
                }
            }
        }
        low
    }

    fn at(&self, x: u32, y: u32, z: u32) -> f32 {
        self.values.get((x + self.dims.x * (y + self.dims.y * z)).idx()).copied().unwrap_or_default()
    }

    /// Trilinear sample; outside the grid adds the distance to the grid box (stays conservative).
    #[must_use]
    pub fn sample(&self, p: Vec3) -> f32 {
        let max = (self.dims - UVec3::ONE).as_vec3();
        let at = (p - self.origin) / self.cell;
        let clamped = at.clamp(Vec3::ZERO, max);
        let outside = ((at - clamped) * self.cell).length();
        let base = clamped.floor().as_uvec3().min(self.dims - UVec3::splat(2));
        let frac = clamped - base.as_vec3();
        let lerp = |lo: f32, hi: f32, t: f32| (hi - lo).mul_add(t, lo);
        let corner = |dx, dy, dz| self.at(base.x + dx, base.y + dy, base.z + dz);
        let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), frac.x);
        let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), frac.x);
        let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), frac.x);
        let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), frac.x);
        lerp(lerp(x00, x10, frac.y), lerp(x01, x11, frac.y), frac.z) + outside
    }
}

/// Authored meshes available to a build, by asset path. SDF grids are converted on first use and
/// shared (the conversion is deterministic, so sharing never changes results).
#[derive(Clone, Debug, Default)]
pub struct PartAssets {
    pub meshes: HashMap<String, Arc<TriMesh>>,
    sdfs: SdfCache,
}

/// Converted SDFs by (mesh hash, resolution). Each entry is filled once: builds that ask for one
/// while it is being converted wait for that conversion instead of starting their own.
type SdfCache = Arc<Mutex<HashMap<(u64, u16), Arc<OnceLock<Arc<SdfGrid>>>>>>;

impl PartAssets {
    pub fn insert(&mut self, path: impl Into<String>, mesh: Arc<TriMesh>) {
        self.meshes.insert(path.into(), mesh);
    }

    #[must_use]
    pub fn mesh(&self, path: &str) -> Option<Arc<TriMesh>> {
        self.meshes.get(path).cloned()
    }

    /// The SDF of `path` at `resolution`, converting it if needed.
    #[must_use]
    pub fn sdf(&self, path: &str, resolution: u16) -> Option<Arc<SdfGrid>> {
        let mesh = self.meshes.get(path)?;
        let entry = self.sdfs.lock().ok()?.entry((mesh.hash, resolution)).or_default().clone();
        Some(entry.get_or_init(|| Arc::new(SdfGrid::from_mesh(mesh, resolution))).clone())
    }

    /// A copy restricted to `paths`, sharing the SDF cache.
    #[must_use]
    pub fn subset<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Self {
        let meshes = paths.into_iter().filter_map(|p| self.meshes.get(p).map(|m| (p.to_owned(), m.clone()))).collect();
        Self { meshes, sdfs: self.sdfs.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::num::ToF32;

    /// Closed unit cube centred at the origin.
    fn cube() -> TriMesh {
        let p: Vec<Vec3> = (0u8..8)
            .map(|i| Vec3::new(f32::from(i & 1) - 0.5, f32::from((i >> 1) & 1) - 0.5, f32::from((i >> 2) & 1) - 0.5))
            .collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let indices = quads.iter().flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]]).collect();
        TriMesh::new(p, vec![], indices)
    }

    #[test]
    fn inward_wound_sphere_is_negative_inside() {
        // A 2k-triangle UV sphere wound inward (clockwise seen from outside).
        let (rings, sides) = (24u32, 48u32);
        let mut points = vec![Vec3::Y];
        for i in 1..rings {
            let phi = std::f32::consts::PI * i.to_f32() / rings.to_f32();
            for j in 0..sides {
                let theta = std::f32::consts::TAU * j.to_f32() / sides.to_f32();
                points.push(Vec3::new(phi.sin() * theta.sin(), phi.cos(), phi.sin() * theta.cos()));
            }
        }
        points.push(Vec3::NEG_Y);
        let ring = |i: u32, j: u32| 1 + (i - 1) * sides + j % sides;
        let mut idx = vec![];
        for j in 0..sides { idx.extend([0, ring(1, j + 1), ring(1, j)]); }
        for i in 1..rings - 1 { for j in 0..sides { let (v00, v01, v10, v11) = (ring(i, j), ring(i, j + 1), ring(i + 1, j), ring(i + 1, j + 1)); idx.extend([v00, v01, v10, v01, v11, v10]); } }
        let south = u32::try_from(points.len()).unwrap() - 1;
        for j in 0..sides { idx.extend([south, ring(rings - 1, j), ring(rings - 1, j + 1)]); }
        let mesh = TriMesh::new(points, vec![], idx);
        let t = std::time::Instant::now();
        let g = SdfGrid::from_mesh(&mesh, 40);
        eprintln!("{} cells in {:?}, centre {}", g.values.len(), t.elapsed(), g.sample(Vec3::ZERO));
        assert!(g.sample(Vec3::ZERO) < -0.9, "inward-wound mesh must still be negative inside");
    }

    #[test]
    fn cube_sdf_signs_and_surface() {
        let g = SdfGrid::from_mesh(&cube(), 24);
        assert!(g.sample(Vec3::ZERO) < -0.4);
        assert!(g.sample(Vec3::new(2.0, 0.0, 0.0)) > 1.0);
        assert!(g.sample(Vec3::new(0.5, 0.0, 0.0)).abs() < 0.05);
    }

    /// Builds that ask for the same SDF at once share one conversion.
    #[test]
    fn concurrent_requests_share_one_conversion() {
        let mut assets = PartAssets::default();
        assets.insert("cube", Arc::new(cube()));
        let start = std::sync::Barrier::new(4);
        let grids: Vec<Arc<SdfGrid>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        start.wait();
                        assets.sdf("cube", 48).unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(grids.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])), "every caller gets the one converted grid");
    }

    /// Timing (ignored): four builds asking for the sculpted head's SDF at once.
    #[test]
    #[ignore = "timing"]
    fn concurrent_head_sdf_timing() {
        let plan: crate::anatomy::BodyPlan = ron::from_str(include_str!("../../assets/mobs/man.body.ron")).unwrap();
        let assets = crate::anatomy::test_assets(&plan);
        let path = assets.meshes.keys().next().unwrap().clone();
        let start = std::sync::Barrier::new(4);
        let t = std::time::Instant::now();
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    start.wait();
                    assets.sdf(&path, 128).unwrap();
                });
            }
        });
        eprintln!("4 concurrent head SDFs: {:?}", t.elapsed());
    }
}
