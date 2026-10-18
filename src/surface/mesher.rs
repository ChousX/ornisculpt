use bevy::math::Vec3A;
use bevy::prelude::*;
use bevy::shape::Aabb3d;
use fast_surface_nets::ndshape::{RuntimeShape, Shape};
use fast_surface_nets::{SurfaceNetsBuffer, surface_nets};
use serde::{Deserialize, Serialize};

use super::{BodyField, Field, SurfaceError};

/// Turns a field into a triangle mesh.
///
/// Takes a [`BodyField`] rather than any [`Field`](super::Field) so it can cull per chunk with
/// [`BodyField::view`]; garments get their own entry point when they exist.
pub trait Mesher: Send + Sync {
    fn mesh(&self, field: &BodyField, settings: &MeshSettings) -> Result<RawMesh, SurfaceError>;
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct MeshSettings {
    /// Grid spacing in metres.
    pub voxel_size: f32,
    /// Chunk edge length in voxels (excluding the 1-voxel overlap Surface Nets needs).
    pub chunk_voxels: u32,
    /// Hard cap; exceeding it is an error rather than a hitch.
    pub max_triangles: u32,
    /// Adaptive meshers: how many times cells may halve below `voxel_size` where the body is thin.
    pub detail_levels: u8,
    /// Adaptive meshers: cells across the radius of the thinnest nearby part.
    pub cells_per_radius: f32,
}

impl Default for MeshSettings {
    fn default() -> Self {
        Self { voxel_size: 0.01, chunk_voxels: 30, max_triangles: 200_000, detail_levels: 0, cells_per_radius: 2.0 }
    }
}

/// Unskinned triangle soup straight out of the mesher (model space, rest pose).
#[derive(Clone, Debug, Default)]
pub struct RawMesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
}

impl RawMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Naive Surface Nets via the `fast-surface-nets` crate, over sparse chunks: chunks whose
/// [`ChunkView`](super::ChunkView) is empty are never sampled. Chunks are meshed in parallel.
#[derive(Default, Clone, Copy)]
pub struct SurfaceNets;

impl Mesher for SurfaceNets {
    fn mesh(&self, field: &BodyField, settings: &MeshSettings) -> Result<RawMesh, SurfaceError> {
        let v = settings.voxel_size.max(1e-4);
        let c = settings.chunk_voxels.max(4);
        let b = field.bounds();
        let origin = Vec3A::from(b.min) - Vec3A::splat(2.0 * v);
        let samples = ((Vec3A::from(b.max) - origin) / v).ceil() + Vec3A::splat(3.0);
        let chunks = (samples / c as f32).ceil().as_uvec3().max(UVec3::ONE);

        let jobs: Vec<UVec3> = (0..chunks.z)
            .flat_map(|z| (0..chunks.y).flat_map(move |y| (0..chunks.x).map(move |x| UVec3::new(x, y, z))))
            .collect();
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(jobs.len().max(1));
        let parts: Vec<RawMesh> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let jobs = &jobs;
                    scope.spawn(move || {
                        let mut out = RawMesh::default();
                        let mut chunk = ChunkScratch::new(c);
                        for job in jobs.iter().skip(t).step_by(threads) {
                            chunk.mesh(field, origin, v, *job * c, &mut out);
                        }
                        out
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().expect("meshing thread panicked")).collect()
        });

        let mut mesh = RawMesh::default();
        for part in parts {
            let base = mesh.positions.len() as u32;
            mesh.positions.extend(part.positions);
            mesh.normals.extend(part.normals);
            mesh.indices.extend(part.indices.iter().map(|i| i + base));
        }
        if mesh.indices.is_empty() {
            return Err(SurfaceError::Empty);
        }
        if mesh.triangle_count() > settings.max_triangles as usize {
            return Err(SurfaceError::TooManyTriangles {
                got: mesh.triangle_count(),
                max: settings.max_triangles as usize,
            });
        }
        Ok(mesh)
    }
}

/// Per-thread buffers for meshing one chunk: `c` voxels plus a 1-sample overlap on each
/// positive side, so neighbouring chunks stitch (Surface Nets skips positive-boundary faces).
struct ChunkScratch {
    shape: RuntimeShape<u32, 3>,
    points: Vec<Vec3A>,
    sdf: Vec<f32>,
    buffer: SurfaceNetsBuffer,
    edge: u32,
}

impl ChunkScratch {
    fn new(c: u32) -> Self {
        let edge = c + 2;
        let shape = RuntimeShape::<u32, 3>::new([edge; 3]);
        let n = shape.size() as usize;
        Self { shape, points: Vec::with_capacity(n), sdf: vec![0.0; n], buffer: default(), edge }
    }

    fn mesh(&mut self, field: &BodyField, origin: Vec3A, v: f32, start: UVec3, out: &mut RawMesh) {
        let lo = origin + start.as_vec3a() * v;
        let hi = lo + Vec3A::splat((self.edge - 1) as f32 * v);
        let view = field.view(Aabb3d { min: lo, max: hi });
        if view.is_empty() {
            return;
        }
        self.points.clear();
        for i in 0..self.shape.size() {
            let [x, y, z] = self.shape.delinearize(i);
            self.points.push(origin + (start + UVec3::new(x, y, z)).as_vec3a() * v);
        }
        view.distance_batch(&self.points, &mut self.sdf);
        let (inside, outside) = self.sdf.iter().fold((false, false), |(i, o), d| (i || *d < 0.0, o || *d >= 0.0));
        if !(inside && outside) {
            return;
        }
        surface_nets(&self.sdf, &self.shape, [0; 3], [self.edge - 1; 3], &mut self.buffer);
        let base = out.positions.len() as u32;
        let offset = origin + start.as_vec3a() * v;
        out.positions.extend(
            self.buffer.positions.iter().map(|p| Vec3::from(offset + Vec3A::from_array(*p) * v)),
        );
        out.normals.extend(self.buffer.normals.iter().map(|n| Vec3::from_array(*n).normalize_or_zero()));
        out.indices.extend(self.buffer.indices.iter().map(|i| i + base));
    }
}
