use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::mesh_data::vertex_normals;
use super::{
    BindSettings, BodyField, BodyFieldOptions, BodyMeshData, VoxelBinding, Field, HardPartGen, MaterialSlot, MeshSettings, Mesher, MobVertexData,
    Submesh, SurfaceError, merge,
    remap_to_crowd, simplify, skin_and_attrs, smooth, weld,
};
use crate::anatomy::{Anatomy, BoneId, LodLevel, PrimLayer, Sheet};
use crate::num::{Clamp, Idx};

/// How one LOD level is produced: re-meshed from the field (without parts culled at this level),
/// then trimmed by simplification.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct LodSpec {
    pub mesh: MeshSettings,
    /// Fraction of triangles kept by `simplify` after re-meshing.
    pub simplify_ratio: f32,
    /// Simplification error limit as a fraction of the mesh extent (meshopt's relative error):
    /// 0.01 lets a 1.8 m body drift ~2 cm, which erases faces and knuckles at close range.
    pub max_error: f32,
    pub geometric_folds: bool,
    pub hard_parts: bool,
    /// Centres of rotation and muscle flex for this level's mesh: skinning that holds up at bent
    /// joints, muscles that bulge. Off, the level skins linearly and its muscles are still; it
    /// comes out two to three times sooner (a preview).
    pub deformation: bool,
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
        let spec = |voxel_size, detail_levels, simplify_ratio, max_error, geometric_folds, hard_parts| LodSpec {
            mesh: MeshSettings { voxel_size, detail_levels, ..default() },
            simplify_ratio,
            max_error,
            geometric_folds,
            hard_parts,
            deformation: true,
        };
        Self {
            levels: [
                // Geometric creases are off: at this voxel size they mesh as scratches. Fine
                // wrinkles belong in shading; keep geometry for large folds only.
                // Detail levels: cells halve (to 2 mm close up) where the body is thin, so fingers and
                // ears keep their shape without meshing the whole body finer.
                spec(0.008, 2, 0.5, 0.0015, false, true), // Hero
                spec(0.02, 1, 0.22, 0.01, false, true),   // Near
                spec(0.05, 0, 0.35, 0.01, false, false),  // Far
            ],
        }
    }
}

impl SurfaceSettings {
    #[must_use]
    pub const fn get(&self, level: LodLevel) -> &LodSpec {
        let [hero, near, far] = &self.levels;
        match level {
            LodLevel::Hero => hero,
            LodLevel::Near => near,
            LodLevel::Far => far,
        }
    }
}

/// Body extent (largest bounding-box side, metres) the [`LodSpec`] voxel sizes are tuned for.
pub const REFERENCE_EXTENT: f32 = 1.8;

/// Full pipeline for one level: field → mesh → weld → smooth → skin → simplify → hard parts →
/// (crowd remap for `Near`/`Far`).
///
/// # Errors
///
/// When no part is present at `level`, or the mesher fails.
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
        for (part, owner) in anatomy.hard_parts.iter().zip(owner) {
            if anatomy.parts.get(owner.idx()).is_none_or(|p| p.lod < level) {
                continue;
            }
            if let Some(m) = hard.generate(part, anatomy, level) {
                merge(&mut data, &m);
            }
        }
    }
    for sheet in anatomy.sheets.iter().filter(|s| s.lod >= level && anatomy.parts.get(s.part.idx()).is_some_and(|p| p.lod >= level)) {
        merge(&mut data, &sheet_mesh(sheet));
    }
    if level != LodLevel::Hero {
        remap_to_crowd(&mut data, anatomy);
    }
    Ok(data)
}

/// Owning part of every hard part (from the parts' hard-part ranges).
fn hard_part_owners(anatomy: &Anatomy) -> Vec<u16> {
    super::body::range_owners(anatomy, anatomy.hard_parts.len(), |p| p.hard_parts.start.idx()..p.hard_parts.end.idx())
}

/// All three levels, built in parallel (near and far beside hero, which is most of the work).
///
/// # Errors
///
/// When any level fails (see [`mesh_lod`]).
pub fn mesh_all_lods<M: Mesher, H: HardPartGen>(
    mesher: &M,
    hard: &H,
    anatomy: &Anatomy,
    settings: &SurfaceSettings,
    seed: u64,
) -> Result<[BodyMeshData; 3], SurfaceError> {
    // Near and far on their own threads while hero (most of the work) runs on this one (one after
    // another on the web).
    let level = |l: LodLevel| mesh_lod(mesher, hard, anatomy, l, settings.get(l), seed);
    let (hero, near, far) = super::scope(|scope| {
        let near = scope.spawn(|| level(LodLevel::Near));
        let far = scope.spawn(|| level(LodLevel::Far));
        let hero = level(LodLevel::Hero);
        let near = near.join().unwrap_or(Err(SurfaceError::WorkerPanicked));
        let far = far.join().unwrap_or(Err(SurfaceError::WorkerPanicked));
        (hero, near, far)
    });
    Ok([hero?, near?, far?])
}

/// A wing sheet as mesh data: double-sided in the shader, so one layer of triangles.
/// Vertex colour w (via `cloth`) carries the sheet kind; UV0 the sheet coordinates; the tangent
/// the shading direction (xyz) and thickness (w).
fn sheet_mesh(sheet: &Sheet) -> BodyMeshData {
    let n = sheet.positions.len();
    let normals = vertex_normals(&sheet.positions, &sheet.indices);
    let mob_data = sheet
        .joints
        .iter()
        .map(|j| {
            MobVertexData {
                region: sheet.region,
                cavity: 0,
                curvature: 128,
                limb_t: 0,
                primary_bone: BoneId(j[0].clamp_u8()),
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
        cloth: vec![u8::from(sheet.kind); n],
        cor: Vec::new(),
        soft: Vec::new(),
        muscle: Vec::new(),
        indices: sheet.indices.clone(),
        submeshes: vec![Submesh { indices: 0..sheet.indices.len().clamp_u32(), slot: MaterialSlot::Wing }],
    }
}

#[cfg(test)]
mod stage_probe {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{AdaptiveContour, ProceduralHardParts};

    /// Where `mesh_lod`'s time goes, per level and stage.
    #[test]
    #[ignore = "timing probe"]
    fn mesh_lod_stage_times() {
        for body in ["biped", "horse"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let anatomy = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let settings = SurfaceSettings::default();
            for level in LodLevel::ALL {
                let spec = settings.get(level);
                let mut t = std::time::Instant::now();
                let lap = |name: &str, t: &mut std::time::Instant| {
                    print!("{name} {:.0}  ", t.elapsed().as_secs_f32() * 1000.0);
                    *t = std::time::Instant::now();
                };
                print!("{body} {level:?} ms: ");
                let opts = BodyFieldOptions { level, seed: 1, geometric_folds: spec.geometric_folds, spread: true };
                let field = BodyField::new(&anatomy, &opts);
                let extent = Vec3::from(field.bounds().max - field.bounds().min).max_element();
                let mut ms = spec.mesh;
                ms.voxel_size *= (extent / REFERENCE_EXTENT).max(1.0);
                lap("field", &mut t);
                let mut raw = AdaptiveContour.mesh(&field, &ms).unwrap();
                lap("mesher", &mut t);
                weld(&mut raw, ms.voxel_size * 1e-3);
                smooth(&mut raw, 2);
                lap("weld+smooth", &mut t);
                let mut data = skin_and_attrs(raw, &field, &anatomy);
                lap("skin_attrs", &mut t);
                let bind = BindSettings { voxel: (ms.voxel_size * 1.5).max(0.012), ..BindSettings::default() };
                let binding = VoxelBinding::new(&field, &anatomy, &bind);
                lap("bind_grid", &mut t);
                binding.apply(&mut data);
                lap("bind_apply", &mut t);
                field.unbind(&mut data);
                lap("unbind", &mut t);
                if spec.simplify_ratio < 1.0 {
                    data = simplify(&data, spec.simplify_ratio, spec.max_error);
                }
                lap("simplify", &mut t);
                let full = std::time::Instant::now();
                let _ = mesh_lod(&AdaptiveContour, &ProceduralHardParts, &anatomy, level, spec, 1).unwrap();
                println!("| whole mesh_lod {:.0}", full.elapsed().as_secs_f32() * 1000.0);
            }
        }
    }
}
