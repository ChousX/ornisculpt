//! Authored mesh data used by [`PartKind::Sculpt`](super::PartKind::Sculpt) (blended in as an SDF)
//! and [`HardKind::Mesh`](super::HardKind::Mesh) (attached rigidly).
//!
//! Anatomy stays a pure function: the meshes are resolved before building and passed in as
//! [`PartAssets`]; their content hashes go into the body cache key.
//!
//! Authoring convention (glTF's own): Y up, +Z forward, origin at the attach point. With
//! `Orient::Model` a part keeps that orientation on the creature; mirrored copies are reflected.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_64;

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
    pub fn new(positions: Vec<Vec3>, normals: Vec<Vec3>, indices: Vec<u32>) -> Self {
        let mut bytes = Vec::with_capacity(positions.len() * 12 + indices.len() * 4);
        positions.iter().for_each(|p| bytes.extend(p.to_array().map(f32::to_bits).iter().flat_map(|b| b.to_le_bytes())));
        indices.iter().for_each(|i| bytes.extend(i.to_le_bytes()));
        Self { positions, normals, indices, hash: xxh3_64(&bytes) }
    }

    /// Volume enclosed by the mesh; negative if its triangles wind inward.
    pub fn signed_volume(&self) -> f32 {
        self.indices
            .chunks_exact(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2].map(|k| self.positions[t[k] as usize]);
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

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
    pub fn from_mesh(mesh: &TriMesh, resolution: u16) -> Self {
        let (lo, hi) = mesh.bounds();
        let size = (hi - lo).max(Vec3::splat(1e-4));
        let cell = size.max_element() / resolution.max(4) as f32;
        let pad = Vec3::splat(cell * 2.0);
        let (lo, hi) = (lo - pad, hi + pad);
        let dims = ((hi - lo) / cell).ceil().as_uvec3().max(UVec3::splat(2));
        let origin = lo + Vec3::splat(cell * 0.5);
        let mut queries = Vec::with_capacity((dims.x * dims.y * dims.z) as usize);
        for z in 0..dims.z {
            for y in 0..dims.y {
                for x in 0..dims.x {
                    queries.push((origin + UVec3::new(x, y, z).as_vec3() * cell).to_array());
                }
            }
        }
        let vertices: Vec<[f32; 3]> = mesh.positions.iter().map(|p| p.to_array()).collect();
        // The sign method needs outward winding; accept either by checking the signed volume.
        let mut indices = mesh.indices.clone();
        if mesh.signed_volume() < 0.0 {
            indices.chunks_exact_mut(3).for_each(|t| t.swap(1, 2));
        }
        let values = mesh_to_sdf::generate_sdf(
            &vertices,
            mesh_to_sdf::Topology::TriangleList(Some(&indices)),
            &queries,
            // Raycast signs flip on symmetry planes (rays graze shared edges); pseudo-normal signs
            // are exact for watertight, consistently wound meshes.
            mesh_to_sdf::AccelerationMethod::Bvh(mesh_to_sdf::SignMethod::Normal),
        );
        Self { origin, cell: Vec3::splat(cell), dims, values }
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        let half = self.cell * 0.5;
        (self.origin - half, self.origin + (self.dims.as_vec3() - Vec3::ONE) * self.cell + half)
    }

    fn at(&self, x: u32, y: u32, z: u32) -> f32 {
        self.values[(x + self.dims.x * (y + self.dims.y * z)) as usize]
    }

    /// Trilinear sample; outside the grid adds the distance to the grid box (stays conservative).
    pub fn sample(&self, p: Vec3) -> f32 {
        let max = (self.dims - UVec3::ONE).as_vec3();
        let g = (p - self.origin) / self.cell;
        let gc = g.clamp(Vec3::ZERO, max);
        let outside = ((g - gc) * self.cell).length();
        let i = gc.floor().as_uvec3().min(self.dims - UVec3::splat(2));
        let f = gc - i.as_vec3();
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c = |dx, dy, dz| self.at(i.x + dx, i.y + dy, i.z + dz);
        let x00 = lerp(c(0, 0, 0), c(1, 0, 0), f.x);
        let x10 = lerp(c(0, 1, 0), c(1, 1, 0), f.x);
        let x01 = lerp(c(0, 0, 1), c(1, 0, 1), f.x);
        let x11 = lerp(c(0, 1, 1), c(1, 1, 1), f.x);
        lerp(lerp(x00, x10, f.y), lerp(x01, x11, f.y), f.z) + outside
    }
}

/// Authored meshes available to a build, by asset path. SDF grids are converted on first use and
/// shared (the conversion is deterministic, so sharing never changes results).
#[derive(Clone, Debug, Default)]
pub struct PartAssets {
    pub meshes: HashMap<String, Arc<TriMesh>>,
    sdfs: Arc<Mutex<HashMap<(u64, u16), Arc<SdfGrid>>>>,
}

impl PartAssets {
    pub fn insert(&mut self, path: impl Into<String>, mesh: Arc<TriMesh>) {
        self.meshes.insert(path.into(), mesh);
    }

    pub fn mesh(&self, path: &str) -> Option<Arc<TriMesh>> {
        self.meshes.get(path).cloned()
    }

    /// The SDF of `path` at `resolution`, converting it if needed.
    pub fn sdf(&self, path: &str, resolution: u16) -> Option<Arc<SdfGrid>> {
        let mesh = self.meshes.get(path)?;
        let key = (mesh.hash, resolution);
        if let Some(g) = self.sdfs.lock().ok()?.get(&key) {
            return Some(g.clone());
        }
        let grid = Arc::new(SdfGrid::from_mesh(mesh, resolution));
        self.sdfs.lock().ok()?.insert(key, grid.clone());
        Some(grid)
    }

    /// A copy restricted to `paths`, sharing the SDF cache.
    pub fn subset<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> Self {
        let meshes = paths.into_iter().filter_map(|p| self.meshes.get(p).map(|m| (p.to_owned(), m.clone()))).collect();
        Self { meshes, sdfs: self.sdfs.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed unit cube centred at the origin.
    fn cube() -> TriMesh {
        let p: Vec<Vec3> = (0..8)
            .map(|i| Vec3::new((i & 1) as f32 - 0.5, ((i >> 1) & 1) as f32 - 0.5, ((i >> 2) & 1) as f32 - 0.5))
            .collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let indices = quads.iter().flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]]).collect();
        TriMesh::new(p, vec![], indices)
    }

    #[test]
    fn inward_wound_sphere_is_negative_inside() {
        // A 2k-triangle UV sphere wound inward (clockwise seen from outside).
        let (rings, sides) = (24u32, 48u32);
        let mut p = vec![Vec3::Y];
        for i in 1..rings {
            let phi = std::f32::consts::PI * i as f32 / rings as f32;
            for j in 0..sides {
                let a = std::f32::consts::TAU * j as f32 / sides as f32;
                p.push(Vec3::new(phi.sin() * a.sin(), phi.cos(), phi.sin() * a.cos()));
            }
        }
        p.push(Vec3::NEG_Y);
        let ring = |i: u32, j: u32| 1 + (i - 1) * sides + j % sides;
        let mut idx = vec![];
        for j in 0..sides { idx.extend([0, ring(1, j + 1), ring(1, j)]); }
        for i in 1..rings - 1 { for j in 0..sides { let (a, b, c, d) = (ring(i, j), ring(i, j + 1), ring(i + 1, j), ring(i + 1, j + 1)); idx.extend([a, b, c, b, d, c]); } }
        let s = p.len() as u32 - 1;
        for j in 0..sides { idx.extend([s, ring(rings - 1, j), ring(rings - 1, j + 1)]); }
        let mesh = TriMesh::new(p, vec![], idx);
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
}
