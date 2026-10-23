use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::mesh_data::vertex_normals;
use super::{
    BindSettings, BodyField, BodyFieldOptions, BodyMeshData, VoxelBinding, Field, MaterialSlot, MeshSettings, Mesher, MobVertexData,
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
pub fn mesh_lod<M: Mesher>(
    mesher: &M,
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
    let sheets = raw.sheets.clone();
    let mut data = skin_and_attrs(raw, &field, anatomy);
    // Weights by distance through the body (in the meshing pose, before the unbind uses them).
    let bind = BindSettings { voxel: (mesh_settings.voxel_size * 1.5).max(0.012), ..BindSettings::default() };
    VoxelBinding::new(&field, anatomy, &bind).apply(&mut data, &sheets, &field);
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
            if let Some(m) = super::hard::hard_part_mesh(part, anatomy, level) {
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
pub fn mesh_all_lods<M: Mesher>(
    mesher: &M,
    anatomy: &Anatomy,
    settings: &SurfaceSettings,
    seed: u64,
) -> Result<[BodyMeshData; 3], SurfaceError> {
    // Near and far on their own threads while hero (most of the work) runs on this one (one after
    // another on the web).
    let level = |l: LodLevel| mesh_lod(mesher, anatomy, l, settings.get(l), seed);
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
    use crate::anatomy::build_src;
    use crate::surface::{AdaptiveContour};

    /// Where `mesh_lod`'s time goes, per level and stage.
    #[test]
    #[ignore = "timing probe"]
    fn mesh_lod_stage_times() {
        for body in ["biped", "horse"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let anatomy = build_src(&text);
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
                let sheets = raw.sheets.clone();
                let mut data = skin_and_attrs(raw, &field, &anatomy);
                lap("skin_attrs", &mut t);
                let bind = BindSettings { voxel: (ms.voxel_size * 1.5).max(0.012), ..BindSettings::default() };
                let binding = VoxelBinding::new(&field, &anatomy, &bind);
                lap("bind_grid", &mut t);
                binding.apply(&mut data, &sheets, &field);
                lap("bind_apply", &mut t);
                field.unbind(&mut data);
                lap("unbind", &mut t);
                if spec.simplify_ratio < 1.0 {
                    data = simplify(&data, spec.simplify_ratio, spec.max_error);
                }
                lap("simplify", &mut t);
                let full = std::time::Instant::now();
                let _ = mesh_lod(&AdaptiveContour, &anatomy, level, spec, 1).unwrap();
                println!("| whole mesh_lod {:.0}", full.elapsed().as_secs_f32() * 1000.0);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::as_conversions, clippy::cast_precision_loss, clippy::many_single_char_names, reason = "throwaway diagnostic probe")]
mod limb_seams {
    use std::collections::HashMap;

    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::AdaptiveContour;

    /// Hero-mesh triangles whose vertices are bound mostly to two different limbs, per body.
    #[test]
    #[ignore = "probe"]
    fn cross_limb_triangles() {
        for body in ["biped", "curvy", "thick", "quadruped", "horse", "hexapod", "bat", "dragon", "wyvern", "dragonfly", "beetle"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let limb_of = |part: u16| -> Option<u16> {
                let p = a.parts.get(part.idx())?;
                if p.role.is_some() {
                    return Some(part);
                }
                p.parent.filter(|q| a.parts.get(q.idx()).is_some_and(|q| q.role.is_some()))
            };
            let spec = SurfaceSettings::default().levels[0];
            let field = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: spec.geometric_folds, spread: true });
            let t = std::time::Instant::now();
            let mut ms = spec.mesh;
            ms.voxel_size *= (Vec3::from(field.bounds().max - field.bounds().min).max_element() / REFERENCE_EXTENT).max(1.0);
            let raw = AdaptiveContour.mesh(&field, &ms).unwrap();
            let took = t.elapsed().as_secs_f32();
            let mut by_sheet: HashMap<u16, usize> = HashMap::new();
            for s in &raw.sheets {
                if *s != crate::surface::Sheet::WHOLE_ID {
                    *by_sheet.entry(*s).or_default() += 1;
                }
            }
            println!("{body}: mesher {took:.2}s, {} verts, on sheets {by_sheet:?}", raw.positions.len());
            let m = mesh_lod(&AdaptiveContour, &a, LodLevel::Hero, &spec, 1).unwrap();
            let limb: Vec<Option<u16>> = m.joints.iter().map(|j| a.skeleton.bones.get(usize::from(j[0])).and_then(|b| limb_of(b.part))).collect();
            let mut pairs: HashMap<(u16, u16), usize> = HashMap::new();
            for t in m.indices.chunks(3) {
                let ls: Vec<u16> = t.iter().filter_map(|&i| limb[i.idx()]).collect();
                for x in &ls {
                    for y in &ls {
                        if x < y {
                            *pairs.entry((*x, *y)).or_default() += 1;
                        }
                    }
                }
            }
            let mut pairs: Vec<_> = pairs.into_iter().map(|((x, y), n)| (format!("{}-{}", a.parts[x.idx()].path.0, a.parts[y.idx()].path.0), n)).collect();
            pairs.sort();
            println!("{body}: {} tris; cross-limb {:?}", m.indices.len() / 3, pairs);
        }
    }
}

#[cfg(test)]
#[allow(clippy::as_conversions, clippy::cast_precision_loss, clippy::many_single_char_names, reason = "throwaway diagnostic probe")]
mod rest_contact_probe {
    use std::collections::HashMap;

    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{AdaptiveContour, Sheet};

    /// Meshed at rest (arms hanging against the hips): sheet vertices, and mesh edges joining a
    /// vertex on a detached limb's surface to one on another limb or the body.
    #[test]
    #[ignore = "probe"]
    fn rest_pose_contacts() {
        for body in ["biped", "curvy", "thick", "horse"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let spec = SurfaceSettings::default().levels[0];
            let field = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: false });
            let mut ms = spec.mesh;
            ms.voxel_size *= (Vec3::from(field.bounds().max - field.bounds().min).max_element() / REFERENCE_EXTENT).max(1.0);
            let t = std::time::Instant::now();
            let mut raw = AdaptiveContour.mesh(&field, &ms).unwrap();
            let took = t.elapsed().as_secs_f32();
            weld(&mut raw, ms.voxel_size * 1e-3);
            let mut by_sheet: HashMap<u16, usize> = HashMap::new();
            for s in &raw.sheets {
                *by_sheet.entry(*s).or_default() += 1;
            }
            // Which limb's own surface each vertex is on, by the nearest primitive, where that limb is detached.
            // (On the limb's own surface rather than the body's: the nearer of the two.)
            let limb_at = |p: Vec3| {
                let p = bevy::math::Vec3A::from(p);
                field.part_at(p).and_then(|part| {
                    let limb = a.parts.get(usize::from(part)).and_then(|q| if q.role.is_some() { Some(part) } else { q.parent.filter(|r| a.parts.get(usize::from(*r)).is_some_and(|r| r.role.is_some())) })?;
                    (field.detached(limb, p) && field.sheet(Sheet::Limb(limb)).distance(p).abs() < field.sheet(Sheet::Body).distance(p).abs()).then_some(limb)
                })
            };
            let owner: Vec<Option<u16>> = raw.positions.iter().zip(&raw.sheets).map(|(p, s)| match Sheet::from_id(*s) { Sheet::Limb(l) => Some(l), Sheet::Body => None, Sheet::Whole => limb_at(*p) }).collect();
            let mut kinds: HashMap<&str, usize> = HashMap::new();
            let mut wheres: HashMap<String, usize> = HashMap::new();
            let mut dumped = 0;
            let mut joins = 0;
            let mut seen = std::collections::HashSet::new();
            for t in raw.indices.chunks(3) {
                for k in 0..3 {
                    let (x, y) = (t[k] as usize, t[(k + 1) % 3] as usize);
                    if !seen.insert((x.min(y), x.max(y))) { continue; }
                    let (ox, oy) = (owner[x], owner[y]);
                    // A detached limb's vertex joined to another limb's or to a body-sheet vertex.
                    let sx = Sheet::from_id(raw.sheets[x]);
                    let sy = Sheet::from_id(raw.sheets[y]);
                    if ox.is_some() && oy.is_some() && ox != oy || (ox.is_some() && sy == Sheet::Body) || (oy.is_some() && sx == Sheet::Body) {
                        joins += 1;
                        let kind = |s: Sheet| match s { Sheet::Whole => "whole", Sheet::Body => "body", Sheet::Limb(_) => "limb" };
                        let (ka, kb) = (kind(sx).min(kind(sy)), kind(sx).max(kind(sy)));
                        let key: &str = Box::leak(format!("{ka}-{kb}{}", if ox.is_some() && oy.is_some() { " 2 limbs" } else { "" }).into_boxed_str());
                        *kinds.entry(key).or_default() += 1;
                        // Where: the limb, and how far past where it attaches (0 = at the zone's edge).
                        let (v, limb) = if ox.is_some() { (x, ox) } else { (y, oy) };
                        if let Some(limb) = limb {
                            let p = bevy::math::Vec3A::from(raw.positions[v]);
                            let past = (0..40).map(|k| k as f32 * 0.005).find(|&r| {
                                [bevy::math::Vec3A::X, bevy::math::Vec3A::NEG_X, bevy::math::Vec3A::Y, bevy::math::Vec3A::NEG_Y, bevy::math::Vec3A::Z, bevy::math::Vec3A::NEG_Z].iter().any(|d| !field.detached(limb, p + *d * r))
                            });
                            let key = format!("{} past {:?}", a.parts[usize::from(limb)].path.0, past.map(|r| (r * 100.0).round() / 100.0));
                            if past.is_some_and(|r| r > 0.05) && dumped < 6 {
                                dumped += 1;
                                let o = if v == x { y } else { x };
                                let q = bevy::math::Vec3A::from(raw.positions[o]);
                                let ev = |p: bevy::math::Vec3A| (field.distance(p), field.sheet(Sheet::Body).distance(p), field.sheet(Sheet::Limb(limb)).distance(p));
                                println!("    whole {:?} (whole/body/limb {:?}) — body-sheet {:?} ({:?})", raw.positions[v], ev(p), raw.positions[o], ev(q));
                            }
                            *wheres.entry(key).or_default() += 1;
                        }
                    }
                }
            }
            println!("{body} at rest: mesher {took:.2}s, {} verts, sheets {by_sheet:?}, detached-limb joins {joins} {kinds:?}", raw.positions.len());
            let mut w: Vec<_> = wheres.into_iter().collect();
            w.sort();
            println!("  {w:?}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::as_conversions, clippy::cast_precision_loss, clippy::many_single_char_names, reason = "throwaway diagnostic probe")]
mod flip_probe {
    use std::collections::HashMap;

    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::AdaptiveContour;

    /// Hero mesh triangles facing against their vertices' normals, by the sheets of their corners.
    #[test]
    #[ignore = "probe"]
    fn flipped_triangles() {
        for body in ["horse", "curvy", "dragon", "biped"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let spec = SurfaceSettings::default().levels[0];
            let field = BodyField::new(&a, &BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: spec.geometric_folds, spread: true });
            let mut ms = spec.mesh;
            ms.voxel_size *= (Vec3::from(field.bounds().max - field.bounds().min).max_element() / REFERENCE_EXTENT).max(1.0);
            let mut raw = AdaptiveContour.mesh(&field, &ms).unwrap();
            weld(&mut raw, ms.voxel_size * 1e-3);
            let mut kinds: HashMap<String, usize> = HashMap::new();
            let mut shown = 0;
            for t in raw.indices.chunks(3) {
                let [i, j, k] = [t[0] as usize, t[1] as usize, t[2] as usize];
                let (p0, p1, p2) = (raw.positions[i], raw.positions[j], raw.positions[k]);
                let n = (p1 - p0).cross(p2 - p0);
                let vn = raw.normals[i] + raw.normals[j] + raw.normals[k];
                if n.dot(vn) < 0.0 {
                    let mut ss = [raw.sheets[i], raw.sheets[j], raw.sheets[k]];
                    ss.sort_unstable();
                    *kinds.entry(format!("{ss:?}")).or_default() += 1;
                    if shown < 8 && ss.iter().any(|s| *s != crate::surface::Sheet::WHOLE_ID) {
                        shown += 1;
                        println!("   flip at {p0:?} sheets {ss:?}");
                    }
                }
            }
            let mut k: Vec<_> = kinds.into_iter().collect();
            k.sort();
            println!("{body}: {} tris, flipped {k:?}", raw.indices.len() / 3);
        }
    }
}

#[cfg(test)]
#[allow(clippy::as_conversions, clippy::cast_precision_loss, clippy::many_single_char_names, reason = "throwaway diagnostic probe")]
mod pose_probe {
    use std::collections::HashMap;

    use bevy::math::{Affine3A, Vec3A};

    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{AdaptiveContour, Sheet};

    /// The hero mesh skinned into a pose with the legs swung at the hip (front and hind opposite
    /// ways), against the field built in that pose: triangles standing out of the posed surface
    /// or stretched, by the sheets of their corners.
    #[test]
    #[ignore = "probe"]
    fn posed_defects() {
        for body in ["horse", "curvy", "thick", "biped", "dragon"] {
            let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let plan: BodyPlan = ron::from_str(&text).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let spec = SurfaceSettings::default().levels[0];
            let opts = BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: true };
            let field = BodyField::new(&a, &opts);
            let mut ms = spec.mesh;
            ms.voxel_size *= (Vec3::from(field.bounds().max - field.bounds().min).max_element() / REFERENCE_EXTENT).max(1.0);
            let mut raw = AdaptiveContour.mesh(&field, &ms).unwrap();
            weld(&mut raw, ms.voxel_size * 1e-3);
            smooth(&mut raw, 2);
            let sheets = raw.sheets.clone();
            let mut data = skin_and_attrs(raw, &field, &a);
            let bind = BindSettings { voxel: (ms.voxel_size * 1.5).max(0.012), ..BindSettings::default() };
            VoxelBinding::new(&field, &a, &bind).apply(&mut data, &sheets, &field);
            field.unbind(&mut data);
            for swing in [30.0f32] {
                let mut local: Vec<Transform> = a.skeleton.bones.iter().map(|b| b.rest).collect();
                for (i, leg) in a.legs.iter().enumerate() {
                    let hip = leg.chain[0].index();
                    let s = if i % 2 == 0 { swing } else { -swing };
                    local[hip].rotation = (Quat::from_rotation_x(s.to_radians()) * local[hip].rotation).normalize();
                }
                let rest = a.skeleton.model_rest();
                let posed = a.skeleton.model_from(&local);
                let skin: Vec<Affine3A> = posed.iter().zip(&rest).map(|(p, r)| *p * r.inverse()).collect();
                let pos: Vec<Vec3A> = data.positions.iter().zip(&data.joints).zip(&data.weights).map(|((p, j), w)| {
                    let mut m = bevy::math::Mat3A::ZERO;
                    let mut t = Vec3A::ZERO;
                    for (&j, &w) in j.iter().zip(w) {
                        m += skin[j as usize].matrix3 * w;
                        t += skin[j as usize].translation * w;
                    }
                    Affine3A { matrix3: m, translation: t }.transform_point3a(Vec3A::from(*p))
                }).collect();
                let pf = BodyField::with_pose(&a, &opts, &local);
                let mut out: HashMap<String, usize> = HashMap::new();
                let mut stretched: HashMap<String, usize> = HashMap::new();
                let mut shown = 0;
                for t in data.indices.chunks(3) {
                    let [i, j, k] = [t[0] as usize, t[1] as usize, t[2] as usize];
                    let kind = |v: usize| match Sheet::from_id(sheets.get(v).copied().unwrap_or(Sheet::WHOLE_ID)) { Sheet::Whole => "w", Sheet::Body => "b", Sheet::Limb(_) => "l" };
                    let mut ks = [kind(i), kind(j), kind(k)];
                    ks.sort_unstable();
                    let key = ks.concat();
                    let c = (pos[i] + pos[j] + pos[k]) / 3.0;
                    let d = pf.distance(c);
                    if d > 0.006 {
                        *out.entry(key.clone()).or_default() += 1;
                        if shown < 6 && key != "www" {
                            shown += 1;
                            let bone = |v: usize| format!("{}:{:.2}", a.skeleton.bones[data.joints[v][0] as usize].name, data.weights[v][0]);
                            println!("    out {d:.3} at {c:?} ({key}) bones {} {} {}", bone(i), bone(j), bone(k));
                        }
                    }
                    let len = |x: &[Vec3A]| (x[0].distance(x[1])).max(x[1].distance(x[2])).max(x[2].distance(x[0]));
                    let before = len(&[Vec3A::from(data.positions[i]), Vec3A::from(data.positions[j]), Vec3A::from(data.positions[k])]);
                    let after = len(&[pos[i], pos[j], pos[k]]);
                    if after > 0.015 && after > 3.0 * before {
                        if key != "www" && *stretched.get(&key).unwrap_or(&0) < 3 {
                            let bone = |v: usize| format!("{}:{:.2}/{}:{:.2}", a.skeleton.bones[data.joints[v][0] as usize].name, data.weights[v][0], a.skeleton.bones[data.joints[v][1] as usize].name, data.weights[v][1]);
                            println!("    stretched {before:.4} -> {after:.4} at {c:?} ({key}) {} | {} | {}", bone(i), bone(j), bone(k));
                        }
                        *stretched.entry(key).or_default() += 1;
                    }
                }
                let mut o: Vec<_> = out.into_iter().collect();
                o.sort();
                let mut st: Vec<_> = stretched.into_iter().collect();
                st.sort();
                println!("{body} swing {swing}: {} tris; out of the surface {o:?}; stretched {st:?}", data.indices.len() / 3);
            }
        }
    }
}
