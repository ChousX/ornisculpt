use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{
    BindSettings, BodyField, BodyFieldOptions, BodyMeshData, VoxelBinding, Field, HardPartGen, MaterialSlot, MeshSettings, Mesher, MobVertexData,
    Submesh, SurfaceError, merge,
    remap_to_crowd, simplify, skin_and_attrs, smooth, weld,
};
use crate::anatomy::{Anatomy, BoneId, LodLevel, PrimLayer, Sheet};

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
        let spec = |voxel_size, detail_levels, simplify_ratio, geometric_folds, hard_parts| LodSpec {
            mesh: MeshSettings { voxel_size, detail_levels, ..default() },
            simplify_ratio,
            max_error: 0.01,
            geometric_folds,
            hard_parts,
        };
        Self {
            levels: [
                // Geometric creases are off: at this voxel size they mesh as scratches. Fine
                // wrinkles belong in shading; keep geometry for large folds only.
                // Detail levels: cells halve (to 2 mm close up) where the body is thin, so fingers and
                // ears keep their shape without meshing the whole body finer.
                spec(0.008, 2, 0.5, false, true),  // Hero
                spec(0.02, 1, 0.22, false, true),  // Near
                spec(0.05, 0, 0.35, false, false), // Far
            ],
        }
    }
}

impl SurfaceSettings {
    pub fn get(&self, level: LodLevel) -> &LodSpec {
        &self.levels[level as usize]
    }
}

/// Body extent (largest bounding-box side, metres) the [`LodSpec`] voxel sizes are tuned for.
pub const REFERENCE_EXTENT: f32 = 1.8;

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
    let opts = BodyFieldOptions { level, seed, geometric_folds: spec.geometric_folds, spread: true };
    let field = BodyField::new(anatomy, &opts);
    if field.primitive_count() == 0 {
        return Err(SurfaceError::NoPartsAtLevel);
    }
    // Voxel sizes are tuned for human-sized bodies; larger bodies scale them up so detail (and
    // triangle count) stays proportional to the body rather than to the metre.
    let extent = Vec3::from(field.bounds().max - field.bounds().min).max_element();
    let mut mesh_settings = spec.mesh;
    mesh_settings.voxel_size *= (extent / REFERENCE_EXTENT).max(1.0);
    let mut raw = mesher.mesh(&field, &mesh_settings)?;
    weld(&mut raw, mesh_settings.voxel_size * 1e-3);
    smooth(&mut raw, 2);
    let mut data = skin_and_attrs(raw, &field, anatomy);
    // Weights by distance through the body (in the meshing pose, before the unbind uses them).
    let bind = BindSettings { voxel: (mesh_settings.voxel_size * 1.5).max(0.012), ..BindSettings::default() };
    VoxelBinding::new(&field, anatomy, &bind).apply(&mut data);
    field.unbind(&mut data);
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
    for sheet in anatomy.sheets.iter().filter(|s| s.lod >= level && anatomy.parts[s.part as usize].lod >= level) {
        merge(&mut data, &sheet_mesh(sheet));
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

/// A wing sheet as mesh data: double-sided in the shader, so one layer of triangles.
/// Vertex colour w (via `cloth`) carries the sheet kind; UV0 the sheet coordinates; the tangent
/// the shading direction (xyz) and thickness (w).
fn sheet_mesh(sheet: &Sheet) -> BodyMeshData {
    let n = sheet.positions.len();
    let mut normals = vec![Vec3::ZERO; n];
    for t in sheet.indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize);
        let nrm = (sheet.positions[b] - sheet.positions[a]).cross(sheet.positions[c] - sheet.positions[a]);
        for v in [a, b, c] {
            normals[v] += nrm;
        }
    }
    let normals = normals.into_iter().map(|v| v.normalize_or(Vec3::Y)).collect();
    let mob_data = sheet
        .joints
        .iter()
        .map(|j| {
            MobVertexData {
                region: sheet.region,
                cavity: 0,
                curvature: 128,
                limb_t: 0,
                primary_bone: BoneId(j[0] as u8),
                layer: PrimLayer::Base,
            }
            .pack()
        })
        .collect();
    BodyMeshData {
        positions: sheet.positions.clone(),
        normals,
        joints: sheet.joints.clone(),
        weights: sheet.weights.clone(),
        mob_data,
        uvs: sheet.uvs.clone(),
        coverage: Vec::new(),
        flow: sheet.dirs.iter().zip(&sheet.thickness).map(|(d, t)| d.extend(*t).to_array()).collect(),
        cloth: vec![sheet.kind as u8; n],
        cor: Vec::new(),
        indices: sheet.indices.clone(),
        submeshes: vec![Submesh { indices: 0..sheet.indices.len() as u32, slot: MaterialSlot::Wing }],
    }
}

#[cfg(test)]
mod hero_timing {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::ProceduralHardParts;

    #[test]
    #[ignore = "timing probe"]
    fn hero_mesh_cost_by_voxel() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        for v in [0.008] {
            let mut spec = SurfaceSettings::default().levels[0].clone();
            spec.mesh.voxel_size = v;
            let t = std::time::Instant::now();
            let m = mesh_lod(&crate::surface::AdaptiveContour, &ProceduralHardParts, &a, LodLevel::Hero, &spec, 1).unwrap();
            println!("voxel {v}: {:.2}s, {} tris", t.elapsed().as_secs_f32(), m.indices.len() / 3);
        }
    }
}
