//! Surface: turns an [`Anatomy`](crate::anatomy::Anatomy) into skinned meshes.
//!
//! Data flow per LOD level:
//! `Anatomy` → [`BodyField`] (SDF, parts culled for the level) → [`Mesher`] → [`RawMesh`]
//! → [`weld`] / [`smooth`] → [`skin_and_attrs`] → [`simplify`] → [`BodyMeshData`] → Bevy `Mesh`.
//! Hard parts (claws, horns, eyes) are generated separately by a [`HardPartGen`] and merged.

// Stub phase: bodies are `todo!()`, so parameters are intentionally unused.
#![allow(unused_variables, dead_code)]

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
mod process;
pub mod sdf;

pub use binding::{BindSettings, VoxelBinding};
pub use cor::centres_of_rotation;
pub use body::{BodyField, BodyFieldOptions, ChunkView};
pub use combinators::{Displace, Intersect, Offset, RegionClip, Subtract};
pub use field::{Field, Influences, SurfaceInfo};
pub use hard::{HardPartGen, ProceduralHardParts};
pub use lod::{LodSpec, SurfaceSettings, mesh_all_lods, mesh_lod};
pub use mesh_data::{
    ATTRIBUTE_COR, ATTRIBUTE_CROWD_JOINT_INDEX, ATTRIBUTE_CROWD_JOINT_WEIGHT, ATTRIBUTE_MOB_DATA, ATTRIBUTE_MUSCLE, ATTRIBUTE_MUSCLE_JOINT,
    BodyMeshData, MaterialSlot, MobVertexData, MuscleFlex,
    NOT_CLOTH, Submesh, Vertex,
};
pub use mesher::{MeshSettings, Mesher, RawMesh, SurfaceNets};
pub use muscle::muscle_flex;
pub use octree::AdaptiveContour;
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
