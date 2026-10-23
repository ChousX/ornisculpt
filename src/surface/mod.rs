//! Surface: turns an [`Anatomy`](crate::anatomy::Anatomy) into skinned meshes.
//!
//! Data flow per LOD level:
//! `Anatomy` → [`BodyField`] (SDF, parts culled for the level) → [`Mesher`] → [`RawMesh`]
//! → [`weld`] / [`smooth`] → [`skin_and_attrs`] → [`simplify`] → [`BodyMeshData`] → Bevy `Mesh`.
//! Hard parts (claws, horns, eyes) are generated separately by a [`HardPartGen`] and merged.

mod binding;
mod body;
mod combinators;
mod cor;
mod field;
mod hard;
mod lod;
mod mesh_data;
mod mesher;
mod muscle;
mod octree;
mod preview;
mod preview_material;
mod process;
pub mod sdf;

pub use binding::{BindSettings, VoxelBinding};
pub use cor::centres_of_rotation;
#[cfg(feature = "soft-tissue")]
pub(crate) use body::primitive_parts;
pub use body::{BodyField, BodyFieldOptions, ChunkView};
pub use combinators::{Displace, Intersect, RegionClip};
pub use field::{Field, Influences, SurfaceInfo};
pub use hard::{HardPartGen, ProceduralHardParts};
pub use lod::{LodSpec, SurfaceSettings, mesh_all_lods, mesh_lod};
pub use mesh_data::{
    ATTRIBUTE_COR, ATTRIBUTE_CROWD_JOINT_INDEX, ATTRIBUTE_CROWD_JOINT_WEIGHT, ATTRIBUTE_MOB_DATA, ATTRIBUTE_MUSCLE, ATTRIBUTE_MUSCLE_JOINT,
    ATTRIBUTE_SOFT, BodyMeshData, MaterialSlot, MobVertexData, MuscleFlex,
    NOT_CLOTH, SoftVertex, Submesh, Vertex, VertexChannel,
};
pub use mesher::{MeshSettings, Mesher, RawMesh, SurfaceNets};
pub use muscle::muscle_flex;
pub use octree::AdaptiveContour;
pub use preview::{BRICK_VOXELS, Brick, PRIM_WORDS, PreviewField, PreviewSheets};
pub use preview_material::{
    SdfPreview, SdfPreviewAssets, SdfPreviewBake, SdfPreviewExt, SdfPreviewMaterial, SdfPreviewPlugin, SdfPreviewUniform, SlotBrick,
};
pub use process::{merge, remap_to_crowd, simplify, skin_and_attrs, smooth, weld};

use bevy::prelude::*;
use thiserror::Error;

/// Registers surface settings.
pub struct SurfacePlugin;

impl Plugin for SurfacePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SurfaceSettings>().register_type::<SurfaceSettings>();
    }
}

/// Threads each parallel meshing stage may use; 0 = one per core.
static WORKER_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Caps the threads each parallel stage (meshing, centres of rotation) spawns; 0 = one per core.
///
/// Process-wide. The mob plugin sets it from `GenBudget` so several bodies generating at once
/// leave cores for the main and render threads.
pub fn set_worker_threads(threads: usize) {
    WORKER_THREADS.store(threads, std::sync::atomic::Ordering::Relaxed);
}

/// Threads a parallel stage may use now (always 1 with the `web` feature: browsers have no threads).
#[must_use]
pub fn worker_threads() -> usize {
    if cfg!(feature = "web") {
        return 1;
    }
    match WORKER_THREADS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(4, std::num::NonZero::get),
        n => n,
    }
}

/// Maps `f` over `items` on up to [`worker_threads`] threads, in contiguous chunks; the results keep
/// `items`' order, so they don't depend on the thread count. Small inputs stay on this thread.
/// A worker's panic resumes here.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    const MIN_PER_THREAD: usize = 512;
    let threads = worker_threads().min(items.len() / MIN_PER_THREAD).max(1);
    if threads == 1 {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(threads);
    scope(|s| {
        let f = &f;
        let handles: Vec<_> = items.chunks(chunk).map(|c| s.spawn(move || c.iter().map(f).collect::<Vec<R>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
    })
}

/// Scoped workers for the parallel stages: [`std::thread::scope`], or with the `web` feature (no
/// threads in the browser) [`sequential::scope`], where every spawned closure runs at once on the
/// calling thread. Each stage merges its workers' output in job order, so the result is the same.
#[cfg(not(feature = "web"))]
pub(crate) use std::thread::scope;
#[cfg(feature = "web")]
pub(crate) use sequential::scope;

/// The shape of [`std::thread::scope`] with every worker run at once on the calling thread, so a
/// stage written for scoped threads runs unchanged where there are none. A worker's panic
/// surfaces from `spawn` instead of `join`; nothing here catches it either way.
#[cfg(feature = "web")]
mod sequential {
    use std::marker::PhantomData;

    pub struct Scope<'scope, 'env: 'scope> {
        scope: PhantomData<&'scope mut &'scope ()>,
        env: PhantomData<&'env mut &'env ()>,
    }

    /// A finished worker's result.
    pub struct JoinHandle<T>(T);

    pub fn scope<'env, F, T>(f: F) -> T
    where
        F: for<'scope> FnOnce(&'scope Scope<'scope, 'env>) -> T,
    {
        f(&Scope { scope: PhantomData, env: PhantomData })
    }

    impl<'scope> Scope<'scope, '_> {
        /// Runs `f` now; the handle holds its result.
        #[allow(clippy::unused_self, reason = "the shape of `Scope::spawn`")]
        pub fn spawn<F, T>(&'scope self, f: F) -> JoinHandle<T>
        where
            F: FnOnce() -> T + Send + 'scope,
            T: Send + 'scope,
        {
            JoinHandle(f())
        }
    }

    impl<T> JoinHandle<T> {
        #[allow(clippy::unnecessary_wraps, reason = "the shape of `ScopedJoinHandle::join`")]
        pub fn join(self) -> std::thread::Result<T> {
            Ok(self.0)
        }
    }
}

#[derive(Debug, Error)]
pub enum SurfaceError {
    #[error("field produced no surface (empty or fully inside/outside)")]
    Empty,
    #[error("mesh exceeds {max} triangles ({got}); raise voxel size")]
    TooManyTriangles { got: usize, max: usize },
    #[error("no parts present at this LOD level")]
    NoPartsAtLevel,
    #[error("a meshing worker thread panicked")]
    WorkerPanicked,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};

    /// The hero mesh and its centres of rotation are the same whatever the worker thread count
    /// (the app caps it while several bodies generate; coat geometry is placed on the vertices).
    #[test]
    fn meshing_is_independent_of_thread_count() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/bird.body.ron")).unwrap();
        let anatomy = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
        let settings = SurfaceSettings::default();
        let mesh = |threads: usize| {
            set_worker_threads(threads);
            let m = mesh_lod(&AdaptiveContour, &ProceduralHardParts, &anatomy, LodLevel::Hero, settings.get(LodLevel::Hero), 1).unwrap();
            let cor = centres_of_rotation(&m);
            set_worker_threads(0);
            (m, cor)
        };
        let ((all, all_cor), (two, two_cor)) = (mesh(0), mesh(2));
        assert_eq!(all.positions, two.positions, "vertices");
        assert_eq!(all.indices, two.indices, "triangles");
        assert_eq!(all_cor, two_cor, "centres of rotation");
    }

    #[test]
    fn quadruped_meshes_at_far_level() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/quadruped.body.ron")).unwrap();
        let anatomy = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
        let settings = SurfaceSettings::default();
        let data = mesh_lod(&SurfaceNets, &ProceduralHardParts, &anatomy, LodLevel::Far, settings.get(LodLevel::Far), 1)
            .expect("mesh");
        assert!(data.triangle_count() > 100, "{} triangles", data.triangle_count());
        assert_eq!(data.positions.len(), data.weights.len());
        assert!(data.weights.iter().all(|w| (w.iter().sum::<f32>() - 1.0).abs() < 1e-3));
        let crowd = u16::try_from(anatomy.skeleton.crowd.bones.len()).unwrap();
        assert!(data.joints.iter().flatten().all(|&j| j < crowd), "joints must index the crowd skeleton");
        assert!(data.indices.iter().all(|&i| usize::try_from(i).unwrap() < data.positions.len()));
        // Grounded body: lowest vertex near y = 0.
        let min_y = data.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        assert!(min_y.abs() < 0.06, "lowest vertex at {min_y}");
    }
}
