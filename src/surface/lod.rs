use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::mesh_data::vertex_normals;
use super::{
    BindSettings, BodyField, BodyFieldOptions, BodyMeshData, VoxelBinding, Field, MaterialSlot, MeshSettings, Mesher, MobVertexData, RawMesh,
    Submesh, SurfaceError, merge,
    remap_to_crowd, simplify, skin_and_attrs, smooth, weld,
};
use crate::anatomy::{Anatomy, BoneId, LodLevel, PrimLayer, Sheet};
use crate::num::{Clamp, Idx, ToF32};

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
                spec(0.008, 2, 0.5, 0.0012, false, true), // Hero
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

/// Meshes `field`, coarsening `settings.voxel_size` (up to [`BUDGET_RETRIES`] times) while the mesh
/// is over its triangle budget: a heavy body (an ox, a dragon) meshes a little coarser instead of
/// failing. The triangle count goes as one over the voxel size squared.
fn mesh_within_budget<M: Mesher>(mesher: &M, field: &BodyField, settings: &mut MeshSettings) -> Result<RawMesh, SurfaceError> {
    let mut tries = 0;
    loop {
        match mesher.mesh(field, settings) {
            Err(SurfaceError::TooManyTriangles { got, max }) if tries < BUDGET_RETRIES => {
                settings.voxel_size *= ((got.to_f32() / max.max(1).to_f32()).sqrt() * 1.05).max(1.1);
                tries += 1;
            }
            result => return result,
        }
    }
}

/// How many times [`mesh_within_budget`] coarsens a mesh over budget before giving up.
const BUDGET_RETRIES: u32 = 3;

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
    let mut raw = mesh_within_budget(mesher, &field, &mut mesh_settings)?;
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
                shade: 0,
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
        paint: Vec::new(),
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

/// Hero meshes of whole mobs, checked for what meshing limbs as sheets of their own is for: limbs
/// at rest against the body stay apart from it, and part from it cleanly when they move.
#[cfg(test)]
pub mod sheet_tests {
    use std::collections::HashSet;

    use bevy::math::{Affine3A, Mat3A, Vec3A};

    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartLodRules, apply_form, build_anatomy, test_assets};
    use crate::surface::{AdaptiveContour, RawMesh, Sheet};

    /// The anatomy of `assets/mobs/<body>.body.ron`.
    pub fn anatomy(body: &str) -> Anatomy {
        let text = std::fs::read_to_string(format!("{}/assets/mobs/{body}.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let plan: BodyPlan = ron::from_str(&text).unwrap();
        build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &test_assets(&plan)).unwrap()
    }

    /// The hero mesh of `field`, welded, at the voxel size `mesh_lod` would use.
    fn hero_raw(field: &BodyField) -> (RawMesh, MeshSettings) {
        let mut ms = SurfaceSettings::default().levels[0].mesh;
        ms.voxel_size *= (Vec3::from(field.bounds().max - field.bounds().min).max_element() / REFERENCE_EXTENT).max(1.0);
        let mut raw = AdaptiveContour.mesh(field, &ms).unwrap();
        weld(&mut raw, ms.voxel_size * 1e-3);
        (raw, ms)
    }

    /// The hero mesh as `mesh_lod` builds it, skinned and back at rest, before simplifying (no
    /// creases); with each vertex's [`Sheet`] id.
    pub fn hero_bound(a: &Anatomy) -> (BodyMeshData, Vec<u16>) {
        let field = BodyField::new(a, &BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: true });
        let (mut raw, ms) = hero_raw(&field);
        smooth(&mut raw, 2);
        let sheets = raw.sheets.clone();
        let mut data = skin_and_attrs(raw, &field, a);
        let bind = BindSettings { voxel: (ms.voxel_size * 1.5).max(0.012), ..BindSettings::default() };
        VoxelBinding::new(&field, a, &bind).apply(&mut data, &sheets, &field);
        field.unbind(&mut data);
        (data, sheets)
    }

    /// The rest pose (local bone transforms) with each leg swung `degrees` forward or back at the
    /// hip, alternate legs opposite ways.
    pub fn legs_swung(a: &Anatomy, degrees: f32) -> Vec<Transform> {
        let mut local: Vec<Transform> = a.skeleton.bones.iter().map(|b| b.rest).collect();
        for (i, leg) in a.legs.iter().enumerate() {
            let s = if i % 2 == 0 { degrees } else { -degrees };
            if let Some(hip) = local.get_mut(leg.chain[0].index()) {
                hip.rotation = (Quat::from_rotation_x(s.to_radians()) * hip.rotation).normalize();
            }
        }
        local
    }

    /// The idle pose (wings folded against the body) with the legs swung `degrees` as
    /// [`legs_swung`] swings them and each tail swung `degrees` to one side at its root, about the
    /// vertical: a tail swept against a hind leg, wings folded over the back and flanks.
    pub fn wings_folded_tail_swung(a: &Anatomy, degrees: f32) -> Vec<Transform> {
        let mut local = a.idle_pose();
        for (i, leg) in a.legs.iter().enumerate() {
            let s = if i % 2 == 0 { degrees } else { -degrees };
            if let Some(hip) = local.get_mut(leg.chain[0].index()) {
                hip.rotation = (Quat::from_rotation_x(s.to_radians()) * hip.rotation).normalize();
            }
        }
        let model = a.skeleton.model_from(&local);
        let wings: Vec<u16> = a.wings.iter().map(|w| w.part).collect();
        let tails = (0u16..).zip(&a.parts).filter(|(i, p)| p.apart && p.role.is_none() && !wings.contains(i)).map(|(_, p)| p);
        for tail in tails {
            let root = usize::from(tail.bones.start);
            let parent = a.skeleton.bones.get(root).and_then(|b| b.parent).and_then(|p| model.get(p.index()));
            let parent = parent.map_or(Quat::IDENTITY, |m| Quat::from_mat3a(&m.matrix3).normalize());
            if let Some(l) = local.get_mut(root) {
                l.rotation = (parent.inverse() * Quat::from_rotation_y(degrees.to_radians()) * parent * l.rotation).normalize();
            }
        }
        local
    }

    /// Triangles that stretch when a rest-pose mesh is skinned into a pose.
    #[derive(Debug, Clone, Copy)]
    pub struct Stretched {
        /// All of them.
        pub total: usize,
        /// Those with a corner on a sheet of its own (not the whole surface).
        pub sheet: usize,
    }

    /// Triangles of `data` (at rest, with its vertices' `sheets`) whose longest edge, linearly
    /// skinned into `pose` (local bone transforms), grows past 1.5 cm and to over three times its
    /// rest length: skin torn between surfaces bound to bones that part.
    pub fn stretched(a: &Anatomy, data: &BodyMeshData, sheets: &[u16], pose: &[Transform]) -> Stretched {
        let rest = a.skeleton.model_rest();
        let skin: Vec<Affine3A> = a.skeleton.model_from(pose).iter().zip(&rest).map(|(p, r)| *p * r.inverse()).collect();
        let posed: Vec<Vec3A> = data
            .positions
            .iter()
            .zip(&data.joints)
            .zip(&data.weights)
            .map(|((p, j), w)| {
                let (mut m, mut t) = (Mat3A::ZERO, Vec3A::ZERO);
                for (&j, &w) in j.iter().zip(w) {
                    let s = skin.get(usize::from(j)).copied().unwrap_or(Affine3A::IDENTITY);
                    m += s.matrix3 * w;
                    t += s.translation * w;
                }
                Affine3A { matrix3: m, translation: t }.transform_point3a(Vec3A::from(*p))
            })
            .collect();
        let longest = |x: [Vec3A; 3]| x[0].distance(x[1]).max(x[1].distance(x[2])).max(x[2].distance(x[0]));
        let mut out = Stretched { total: 0, sheet: 0 };
        for t in data.indices.as_chunks::<3>().0 {
            let v = [t[0].idx(), t[1].idx(), t[2].idx()];
            let before = longest(v.map(|i| Vec3A::from(data.positions[i])));
            let after = longest(v.map(|i| posed[i]));
            if after > 0.015 && after > 3.0 * before {
                out.total += 1;
                if v.iter().any(|&i| sheets.get(i).is_some_and(|&s| s != Sheet::WHOLE_ID)) {
                    out.sheet += 1;
                }
            }
        }
        out
    }

    /// `a` as it meshed before wings and tails were meshed apart: only limbs are.
    fn wings_tails_joined(a: &Anatomy) -> Anatomy {
        let mut before = a.clone();
        for part in &mut before.parts {
            part.apart &= part.role.is_some();
        }
        before
    }

    /// Where meshing wings and tails apart changes the surface, per mob: hero-mesh vertices
    /// (meshed with them blended in) whose distance to the field with them apart moved by over
    /// 1.5 mm, by wing or tail and by the part nearest them besides it; and both meshes' triangle
    /// counts and times.
    #[test]
    #[ignore = "probe"]
    fn wing_tail_contacts() {
        use std::collections::BTreeMap;
        let idle = std::env::var_os("ORNISCULPT_PROBE_IDLE").is_some();
        for body in ["bat", "dragon", "wyvern", "bird", "horse", "quadruped", "beetle", "hexapod"] {
            let after = anatomy(body);
            let before = wings_tails_joined(&after);
            let opts = BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: !idle };
            // `ORNISCULPT_PROBE_IDLE`: in the idle pose, wings folded, as the editor's preview has it.
            let field = |a: &Anatomy| if idle { BodyField::with_pose(a, &opts, &a.idle_pose()) } else { BodyField::new(a, &opts) };
            let t = std::time::Instant::now();
            let old = field(&before);
            let (raw, _) = hero_raw(&old);
            let old_ms = t.elapsed().as_secs_f32() * 1000.0;
            let t = std::time::Instant::now();
            let new = field(&after);
            let (raw_new, _) = hero_raw(&new);
            let new_ms = t.elapsed().as_secs_f32() * 1000.0;
            let on_sheets = |r: &RawMesh| r.sheets.iter().filter(|&&s| s != Sheet::WHOLE_ID).count();
            println!(
                "{body}: before {} tris {old_ms:.0} ms ({} on sheets), after {} tris {new_ms:.0} ms ({} on sheets)",
                raw.triangle_count(),
                on_sheets(&raw),
                raw_new.triangle_count(),
                on_sheets(&raw_new)
            );
            let spec = SurfaceSettings::default().levels[0];
            for (name, a) in [("before", &before), ("after", &after)].into_iter().filter(|_| !idle) {
                let t = std::time::Instant::now();
                let m = mesh_lod(&AdaptiveContour, a, LodLevel::Hero, &spec, 1).unwrap();
                println!("  mesh_lod Hero {name}: {} tris, {:.0} ms", m.indices.len() / 3, t.elapsed().as_secs_f32() * 1000.0);
            }
            let prim_part = super::super::body::primitive_parts(&after);
            let mut moved: BTreeMap<(String, String), (usize, f32)> = BTreeMap::new();
            for p in raw.positions.iter().map(|p| Vec3A::from(*p)) {
                let d = new.distance(p) - old.distance(p);
                if d.abs() <= 0.0015 {
                    continue;
                }
                let Some(part) = new.part_at(p) else { continue };
                let Some(own) = super::super::body::limb_of(&after, part) else { continue };
                let other = (0..after.primitives.len())
                    .filter(|&i| super::super::body::limb_of(&after, prim_part[i]) != Some(own))
                    .map(|i| (new.distance_among(i..i + 1, p), prim_part[i]))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map_or_else(|| "?".to_owned(), |(_, q)| after.parts[q.idx()].path.0.clone());
                let e = moved.entry((after.parts[own.idx()].path.0.clone(), other)).or_default();
                e.0 += 1;
                e.1 = e.1.max(d.abs());
            }
            for ((own, other), (n, worst)) in moved {
                println!("  {own} against {other}: {n} verts, up to {:.1} mm", worst * 1000.0);
            }
        }
    }

    /// The mesh edges of `field`'s hero mesh joining a limb's surface clear of where it attaches
    /// to the body's or another limb's, by how far past where it attaches (the nearer of two
    /// limbs): the worst, those past `max_past` (as "limb-limb distance"), and how many vertices
    /// are on sheets of their own.
    fn contacts_past(mob: &Anatomy, field: &BodyField, max_past: f32) -> (f32, Vec<String>, usize) {
        let (raw, _) = hero_raw(field);
        let on_sheets = raw.sheets.iter().filter(|&&s| s != Sheet::WHOLE_ID).count();
        // The limb a vertex's surface is, where that limb is clear of where it attaches: by its
        // sheet, or on the whole surface by its nearest primitive's limb if nearer that limb's own
        // surface than the body's.
        let detached_at = |p: Vec3A| {
            let limb = field.part_at(p).and_then(|part| super::super::body::limb_of(mob, part))?;
            (field.detached(limb, p) && field.sheet(Sheet::Limb(limb)).distance(p).abs() < field.sheet(Sheet::Body).distance(p).abs()).then_some(limb)
        };
        let owner: Vec<Option<u16>> = raw
            .positions
            .iter()
            .zip(&raw.sheets)
            .map(|(p, s)| match Sheet::from_id(*s) {
                Sheet::Limb(l) => Some(l),
                Sheet::Body => None,
                Sheet::Whole => detached_at(Vec3A::from(*p)),
            })
            .collect();
        let past = |limb: u16, v: usize| field.past_attachment(limb, Vec3A::from(raw.positions[v]));
        let body_sheet = |v: usize| raw.sheets.get(v) == Some(&Sheet::BODY_ID);
        let mut seen = HashSet::new();
        let (mut far, mut worst) = (Vec::new(), 0.0f32);
        for tri in raw.indices.as_chunks::<3>().0 {
            for corner in 0..3 {
                let (x, y) = (tri[corner].idx(), tri[(corner + 1) % 3].idx());
                if !seen.insert((x.min(y), x.max(y))) {
                    continue;
                }
                // A detached limb's vertex joined to another limb's (as far past where it
                // attaches as the nearer of the two) or to a body-sheet vertex.
                let (d, limbs) = match (owner[x], owner[y]) {
                    (Some(l), Some(m)) if l != m => (past(l, x).min(past(m, y)), [Some(l), Some(m)]),
                    (Some(l), _) if body_sheet(y) => (past(l, x), [Some(l), None]),
                    (_, Some(m)) if body_sheet(x) => (past(m, y), [Some(m), None]),
                    _ => continue,
                };
                worst = worst.max(d);
                if d > max_past {
                    let name = |l: Option<u16>| l.map_or("the body", |l| mob.parts[l.idx()].path.0.as_str());
                    far.push(format!("{}-{} {d:.3} m", name(limbs[0]), name(limbs[1])));
                }
            }
        }
        println!("{} verts, {on_sheets} on sheets; joins at most {worst:.3} m past where a limb attaches", raw.positions.len());
        (worst, far, on_sheets)
    }

    /// Meshed at rest (arms hanging against the hips, thick's thighs pressed together), the
    /// limbs are sheets of their own, and no mesh edge joins a limb's surface to the body or
    /// another limb more than 2.5 cm past where it attaches: there they are apart, however close.
    #[test]
    fn rest_pose_contacts_stay_apart() {
        const MAX_PAST: f32 = 0.025;
        let mut joined = Vec::new();
        for body in ["curvy", "thick"] {
            let mob = anatomy(body);
            let field = BodyField::new(&mob, &BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: false });
            print!("{body}: ");
            let (_, far, on_sheets) = contacts_past(&mob, &field, MAX_PAST);
            assert!(on_sheets > 0, "{body}: no vertex on a sheet of its own");
            if !far.is_empty() {
                joined.push(format!("{body}: {}", far.join(", ")));
            }
        }
        assert!(joined.is_empty(), "mesh edges join limbs more than {MAX_PAST} m past where they attach: {}", joined.join(", "));
    }

    /// In the idle pose, as the editor's preview shows it, a bat's and a wyvern's folded wings lie
    /// against their legs and flanks: they are sheets of their own there, and no mesh edge joins a
    /// wing to a leg or the body more than 2.5 cm past where it attaches.
    #[test]
    fn folded_wings_stay_apart() {
        const MAX_PAST: f32 = 0.025;
        let mut joined = Vec::new();
        for body in ["bat", "wyvern"] {
            let mob = anatomy(body);
            let opts = BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: false };
            let field = BodyField::with_pose(&mob, &opts, &mob.idle_pose());
            print!("{body}: ");
            let (_, far, on_sheets) = contacts_past(&mob, &field, MAX_PAST);
            assert!(on_sheets > 0, "{body}: no vertex on a sheet of its own");
            if !far.is_empty() {
                joined.push(format!("{body}: {}", far.join(", ")));
            }
        }
        assert!(joined.is_empty(), "mesh edges join limbs more than {MAX_PAST} m past where they attach: {}", joined.join(", "));
    }

    /// Skinned with the legs swung 30° at the hip, the horse (whose upper legs run on inside its
    /// body) tears few triangles, and the biped no more than before limbs had sheets (21).
    #[test]
    fn swung_legs_stretch_few_triangles() {
        // (body, at most stretched with a corner on a sheet, at most stretched in all)
        // The biped's hands hang against its thighs (the thumb toward them), which the swing tears a few more.
        for (body, max_sheet, max_total) in [("horse", 200, 450), ("biped", 32, 32)] {
            let a = anatomy(body);
            let (data, sheets) = hero_bound(&a);
            let s = stretched(&a, &data, &sheets, &legs_swung(&a, 30.0));
            println!("{body}: {} tris, stretched {s:?}", data.indices.len() / 3);
            assert!(s.sheet <= max_sheet && s.total <= max_total, "{body}: {s:?} stretched (at most {max_sheet} on sheets, {max_total} in all)");
        }
    }

    /// Stretched triangles with the wings folded and the tail and legs swung 30°, meshed with
    /// wings and tails blended in (before) and apart (after).
    #[test]
    #[ignore = "probe"]
    fn wing_tail_stretch_before_after() {
        for body in ["dragon", "wyvern", "bat", "bird", "horse", "quadruped"] {
            let after = anatomy(body);
            let before = wings_tails_joined(&after);
            for (name, a) in [("before", &before), ("after", &after)] {
                let (data, sheets) = hero_bound(a);
                let idle = stretched(a, &data, &sheets, &a.idle_pose());
                let s = stretched(a, &data, &sheets, &wings_folded_tail_swung(a, 30.0));
                println!("{body} {name}: {} tris, idle {idle:?}, swung {s:?}", data.indices.len() / 3);
            }
        }
    }

    /// Skinned with the wings folded against the body and the tail and legs swung 30°, a dragon
    /// (four legs, two wings and a tail, meshed apart) tears few triangles.
    #[test]
    fn folded_wings_and_swung_tail_stretch_few_triangles() {
        // At most stretched with a corner on a sheet, and in all (54 and 356 when written).
        let (max_sheet, max_total) = (80, 450);
        let a = anatomy("dragon");
        let (data, sheets) = hero_bound(&a);
        let s = stretched(&a, &data, &sheets, &wings_folded_tail_swung(&a, 30.0));
        println!("dragon: {} tris, stretched {s:?}", data.indices.len() / 3);
        assert!(s.sheet <= max_sheet && s.total <= max_total, "dragon: {s:?} stretched (at most {max_sheet} on sheets, {max_total} in all)");
    }
}
