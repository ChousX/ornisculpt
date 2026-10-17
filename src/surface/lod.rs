use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    BodyField, BodyFieldOptions, BodyMeshData, HardPartGen, MeshSettings, Mesher, SurfaceError, merge,
    remap_to_crowd, simplify, skin_and_attrs, smooth, weld,
};
use crate::anatomy::{Anatomy, LodLevel};

/// How one LOD level is produced: re-meshed from the field (without parts culled at this level),
/// then trimmed by simplification.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct LodSpec {
    pub mesh: MeshSettings,
    /// Fraction of triangles kept by `simplify` after re-meshing.
    pub simplify_ratio: f32,
    pub max_error: f32,
    pub geometric_folds: bool,
    pub hard_parts: bool,
}

/// Per-level surface quality. Scaled by the quality profile later.
#[derive(Resource, Reflect, Serialize, Deserialize, Clone, Debug)]
#[reflect(Resource)]
pub struct SurfaceSettings {
    /// Indexed by `LodLevel as usize`.
    pub levels: [LodSpec; 3],
}

impl Default for SurfaceSettings {
    fn default() -> Self {
        let spec = |voxel_size, simplify_ratio, geometric_folds, hard_parts| LodSpec {
            mesh: MeshSettings { voxel_size, ..default() },
            simplify_ratio,
            max_error: 0.01,
            geometric_folds,
            hard_parts,
        };
        Self {
            levels: [
                // Geometric creases are off: at this voxel size they mesh as scratches. Fine
                // wrinkles belong in shading; keep geometry for large folds only.
                spec(0.008, 0.5, false, true),  // Hero
                spec(0.02, 0.22, false, true),  // Near
                spec(0.05, 0.35, false, false), // Far
            ],
        }
    }
}

impl SurfaceSettings {
    pub fn get(&self, level: LodLevel) -> &LodSpec {
        &self.levels[level as usize]
    }
}

/// Full pipeline for one level: field → mesh → weld → smooth → skin → simplify → hard parts →
/// (crowd remap for `Near`/`Far`).
pub fn mesh_lod<M: Mesher, H: HardPartGen>(
    mesher: &M,
    hard: &H,
    anatomy: &Anatomy,
    level: LodLevel,
    spec: &LodSpec,
    seed: u64,
) -> Result<BodyMeshData, SurfaceError> {
    let opts = BodyFieldOptions { level, seed, geometric_folds: spec.geometric_folds };
    let field = BodyField::new(anatomy, &opts);
    if field.primitive_count() == 0 {
        return Err(SurfaceError::NoPartsAtLevel);
    }
    let mut raw = mesher.mesh(&field, &spec.mesh)?;
    weld(&mut raw, spec.mesh.voxel_size * 1e-3);
    smooth(&mut raw, 2);
    let mut data = skin_and_attrs(raw, &field, anatomy);
    if spec.simplify_ratio < 1.0 {
        data = simplify(&data, spec.simplify_ratio, spec.max_error);
    }
    if spec.hard_parts {
        let owner = hard_part_owners(anatomy);
        for (i, part) in anatomy.hard_parts.iter().enumerate() {
            if anatomy.parts[owner[i] as usize].lod < level {
                continue;
            }
            if let Some(m) = hard.generate(part, anatomy, level) {
                merge(&mut data, &m);
            }
        }
    }
    if level != LodLevel::Hero {
        remap_to_crowd(&mut data, anatomy);
    }
    Ok(data)
}

/// Owning part of every hard part (from the parts' hard-part ranges).
fn hard_part_owners(anatomy: &Anatomy) -> Vec<u16> {
    let mut out = vec![0u16; anatomy.hard_parts.len()];
    for (pi, part) in anatomy.parts.iter().enumerate() {
        for i in part.hard_parts.clone() {
            out[i as usize] = pi as u16;
        }
    }
    out
}

/// All three levels; independent, so callers may run them in parallel instead.
pub fn mesh_all_lods<M: Mesher, H: HardPartGen>(
    mesher: &M,
    hard: &H,
    anatomy: &Anatomy,
    settings: &SurfaceSettings,
    seed: u64,
) -> Result<[BodyMeshData; 3], SurfaceError> {
    let [a, b, c] = LodLevel::ALL.map(|l| mesh_lod(mesher, hard, anatomy, l, settings.get(l), seed));
    Ok([a?, b?, c?])
}
