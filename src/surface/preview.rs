//! A [`BodyField`] packed for the GPU, so a shader can ray-march the body directly: an editor
//! shows every change on the next frame and meshes only once edits pause.
//!
//! One flat array of 16-byte words (`array<vec4<f32>>` in `sdf_preview.wesl`; integers are
//! bit-cast). Changing the layout here means changing the shader's readers too.
//!
//! ```text
//! 0          header: primitive count, first segment word, first cell word, first item word
//! 1          grid minimum (xyz), cell size
//! 2          cells per axis (xyz, bit-cast)
//! 3          teardrop keel (CrossSection::KEEL)
//! 4          distance bricks: voxels along a brick's edge, first brick word, brick count
//! 5..        primitives, PRIM_WORDS each: model → local rows (3), kind / op / limb / flags,
//!            shape parameters, more parameters (a sweep's largest radius), blend / min scale /
//!            displace height, blend anchor
//! segments   one word per sweep point: position (xyz), radius
//! grids      per authored grid: origin (xyz) and cell, then dims (xyz), then values 4 per word
//! cells      one word per cell: first item, item count, brick + 1 (0: no surface in the cell)
//! items      primitive indices, one per word (x), each cell's in field order
//! bricks     one word per brick: its cell's index
//! ```
//!
//! Each cell lists the primitives whose padded bounds come near it, in field order, so evaluating
//! a cell's list gives what [`BodyField`] gives there (as a meshing [`ChunkView`](super::ChunkView)
//! does). Cells that may hold surface get a brick: the field sampled on a lattice over the cell
//! (`BRICK_VOXELS` + 1 samples a side, walls included), which a compute pass bakes into a 3D
//! texture atlas (`sdf_bake.wesl`) for the preview to march (`sdf_preview.wesl`).

use bevy::math::Vec3A;
use bevy::prelude::*;

use bevy::math::Affine3A;
use bevy::shape::{Aabb3d, BoundingVolume};

use super::BodyField;
use super::body::{PreparedShape, local_bounds, transform_aabb};
use crate::anatomy::{Anatomy, CrossSection, HardKind, HardPart, PrimOp, PrimShape};
use crate::num::{Clamp, Idx, ToF32};

/// Words per primitive.
pub const PRIM_WORDS: usize = 8;
/// Words before the first primitive.
const HEADER_WORDS: usize = 5;
/// Cell edge (metres) for a body of [`REFERENCE_EXTENT`](super::lod::REFERENCE_EXTENT); larger
/// bodies scale it up, as meshing scales its voxels.
const CELL_SIZE: f32 = 0.064;
/// Most cells along any side.
const MAX_CELLS: u32 = 128;
/// Voxels along a brick's edge (`BRICK_VOXELS` in `sdf_field.wesl`): 8 mm on a human-sized body.
pub const BRICK_VOXELS: u32 = 8;
/// Cells list every primitive within this many voxels of them, so a cell's list gives the field
/// exactly that far beyond its walls (`LIST_PAD_VOXELS` in `sdf_bake.wesl`).
const LIST_PAD_VOXELS: f32 = 2.0;

/// Shape kinds (word 3.x of a primitive).
const SPHERE: u32 = 0;
const ELLIPSOID: u32 = 1;
const ROUND_CONE: u32 = 2;
const ROUND_BOX: u32 = 3;
const SWEEP: u32 = 4;
const GRID: u32 = 5;

/// Op kinds (word 3.y).
const UNION: u32 = 0;
const SUBTRACT: u32 = 1;
const DISPLACE: u32 = 2;

/// "No limb" (word 3.z).
const NO_LIMB: u32 = u32::MAX;

/// A [`BodyField`] as GPU words, the bounds it covers, and the hull to draw it with (model
/// space).
#[derive(Clone, Debug, Default)]
pub struct PreviewField {
    pub words: Vec<[f32; 4]>,
    pub min: Vec3,
    pub max: Vec3,
    /// Cells holding surface, each with a brick of distances to bake.
    pub bricks: u32,
    /// The outer faces of the cells that may hold surface, as quads (four corners each, wound
    /// counter-clockwise seen from outside) with their outward normals. Rays start where they
    /// enter it, so the empty space around the body is neither drawn nor marched.
    pub hull: Vec<([Vec3; 4], Vec3)>,
    /// Wing sheets (membranes, flight feathers, insect wings): thin, so drawn as the triangles
    /// the anatomy already has rather than marched.
    pub sheets: PreviewSheets,
}

/// Wing sheets as one triangle list, model space, in the field's pose.
#[derive(Clone, Debug, Default)]
pub struct PreviewSheets {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// Per vertex: thickness (0 = thinnest .. 1 = bone, shaft), and 1 where the sheet's part is
    /// highlighted.
    pub shade: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

impl PreviewSheets {
    /// Every sheet of `anatomy`, skinned to the bones at `model` (linear blend: the sheets bend
    /// little between their bones), flagged where `highlight` holds for its part.
    fn new(anatomy: &Anatomy, model: &[Affine3A], highlight: &impl Fn(u16) -> bool) -> Self {
        let skin: Vec<Affine3A> =
            model.iter().zip(&anatomy.skeleton.inverse_bind).map(|(m, inv)| *m * Affine3A::from_mat4(*inv)).collect();
        let mut out = Self::default();
        for sheet in &anatomy.sheets {
            let base = out.positions.len().clamp_u32();
            let lit = f32::from(u8::from(highlight(sheet.part)));
            let posed: Vec<Vec3> = sheet
                .positions
                .iter()
                .zip(&sheet.joints)
                .zip(&sheet.weights)
                .map(|((p, joints), weights)| {
                    joints.iter().zip(weights).fold(Vec3::ZERO, |acc, (j, w)| {
                        acc + skin.get(j.idx()).map_or(*p, |m| m.transform_point3(*p)) * *w
                    })
                })
                .collect();
            let sheet_positions = &posed;
            let mut normals = vec![Vec3::ZERO; sheet.positions.len()];
            for t in sheet.indices.as_chunks::<3>().0 {
                let [a, b, c] = t.map(Idx::idx);
                let (Some(&pa), Some(&pb), Some(&pc)) = (sheet_positions.get(a), sheet_positions.get(b), sheet_positions.get(c)) else {
                    continue;
                };
                let n = (pb - pa).cross(pc - pa);
                for v in [a, b, c] {
                    if let Some(slot) = normals.get_mut(v) {
                        *slot += n;
                    }
                }
            }
            out.positions.extend(posed.iter());
            out.normals.extend(normals.into_iter().map(|n| n.normalize_or(Vec3::Y)));
            out.shade.extend((0..sheet.positions.len()).map(|i| [sheet.thickness.get(i).copied().unwrap_or(0.0), lit]));
            out.indices.extend(sheet.indices.iter().map(|i| base + i));
        }
        out
    }
}

const fn bits(v: u32) -> f32 {
    f32::from_bits(v)
}

const fn word_u32(a: u32, b: u32, c: u32, d: u32) -> [f32; 4] {
    [bits(a), bits(b), bits(c), bits(d)]
}

/// Appends a sweep's points to `segments`; returns its kind and parameter words (the second
/// holds the largest radius, for the teardrop keel's blend).
fn sweep(segments: &mut Vec<[f32; 4]>, points: impl ExactSizeIterator<Item = Vec3>, radii: &[f32], cross: CrossSection) -> (u32, [f32; 4], [f32; 4]) {
    let first = segments.len();
    let count = points.len();
    segments.extend(points.zip(radii).map(|(p, r)| [p.x, p.y, p.z, *r]));
    let teardrop = matches!(cross, CrossSection::Teardrop { .. });
    let max_radius = radii.iter().copied().fold(0.0, f32::max);
    (
        SWEEP,
        [bits(first.clamp_u32()), bits(count.min(radii.len()).clamp_u32()), cross.aspect(), f32::from(u8::from(teardrop))],
        [max_radius, 0.0, 0.0, 0.0],
    )
}

/// One primitive on its way into the words.
struct Packed {
    to_local: Affine3A,
    min_scale: f32,
    kind: u32,
    params: [f32; 4],
    more: [f32; 4],
    op: u32,
    limb: u32,
    flags: u32,
    blend: f32,
    height: f32,
    anchor: (Vec3A, f32),
    aabb: Aabb3d,
}

/// Flag bits (word 3.w): tinted as selected; an eye; a hard part.
const HIGHLIGHT: u32 = 1;
const EYE: u32 = 2;
const HARD: u32 = 4;

/// A hard part as a stand-in primitive: eyes as spheres, plates as ellipsoids, everything
/// horn-like as a straight round cone (curl and twist left out). Authored meshes are skipped.
fn hard_part(part: &HardPart, model: &[Affine3A], highlight: bool) -> Option<Packed> {
    let s = part.params.size;
    let (kind, params, flag) = match &part.params.kind {
        HardKind::Eye => (SPHERE, [s, 0.0, 0.0, 0.0], EYE),
        HardKind::Plate => (ELLIPSOID, [s * 0.45, s * 0.12f32.mul_add(part.params.curve.clamp(0.0, 1.0), 0.06), s * 0.5, 0.0], HARD),
        HardKind::Mesh { .. } => return None,
        horn => {
            let (length, base) = match horn {
                HardKind::Nail => (s * 0.4, s * 0.5),
                HardKind::Horn | HardKind::Fang => (s, s * 0.22),
                HardKind::Tusk => (s, s * 0.15),
                HardKind::Tooth => (s, s * 0.35),
                HardKind::Spike => (s, s * 0.3),
                HardKind::Beak => (s, s * 0.45),
                HardKind::Antler { .. } => (s, s * 0.08),
                _ => (s, s * 0.25),
            };
            let tip = base * 0.15;
            (ROUND_CONE, [base, tip, (length - tip).max(1e-4), 0.0], HARD)
        }
    };
    let m = *model.get(part.bone.index())? * part.local.compute_affine();
    let shape = match kind {
        SPHERE => PrimShape::Sphere { r: params[0] },
        ELLIPSOID => PrimShape::Ellipsoid { radii: Vec3::new(params[0], params[1], params[2]) },
        _ => PrimShape::RoundCone { r0: params[0], r1: params[1], len: params[2] },
    };
    Some(Packed {
        to_local: m.inverse(),
        min_scale: (0..3).map(|c| m.matrix3.col(c).length()).fold(f32::INFINITY, f32::min),
        kind,
        params,
        more: [0.0; 4],
        op: UNION,
        limb: NO_LIMB,
        flags: flag | if highlight { HIGHLIGHT } else { 0 },
        blend: 0.0,
        height: 0.0,
        anchor: (Vec3A::ZERO, 0.0),
        aabb: transform_aabb(&m, local_bounds(&shape)),
    })
}

impl BodyField {
    /// Packs the field for `sdf_preview.wesl`, plus stand-ins for `anatomy`'s hard parts (eyes,
    /// claws, horns: see `hard_part`) and its wing sheets, on its rest pose. Primitives of a part for which `highlight`
    /// is true are flagged (the shader tints them). Creases are left out (they are shading-scale).
    #[must_use]
    pub fn preview(&self, anatomy: &Anatomy, highlight: impl Fn(u16) -> bool) -> PreviewField {
        let mut segments: Vec<[f32; 4]> = Vec::new();
        let mut grids: Vec<[f32; 4]> = Vec::new();
        // Grids shared by several primitives (mirrored copies) are packed once.
        let mut packed_grids: Vec<(*const crate::anatomy::SdfGrid, usize)> = Vec::new();
        let mut packed = Vec::with_capacity(self.prims.len() + anatomy.hard_parts.len());
        for prim in &self.prims {
            let (kind, params, more) = match &prim.shape {
                PreparedShape::Simple(PrimShape::Sphere { r }) => (SPHERE, [*r, 0.0, 0.0, 0.0], [0.0; 4]),
                PreparedShape::Simple(PrimShape::Ellipsoid { radii }) => (ELLIPSOID, radii.extend(0.0).to_array(), [0.0; 4]),
                PreparedShape::Simple(PrimShape::Capsule { r0, r1, len } | PrimShape::RoundCone { r0, r1, len }) => {
                    (ROUND_CONE, [*r0, *r1, *len, 0.0], [0.0; 4])
                }
                PreparedShape::Simple(PrimShape::RoundBox { half, round }) => (ROUND_BOX, half.extend(*round).to_array(), [0.0; 4]),
                PreparedShape::Sweep { points, radii, cross } => sweep(&mut segments, points.iter().map(|p| Vec3::from(*p)), radii, *cross),
                // `BodyField` converts sweeps; this arm is never taken.
                PreparedShape::Simple(PrimShape::Sweep { points, radii, cross }) => sweep(&mut segments, points.iter().copied(), radii, *cross),
                PreparedShape::Simple(PrimShape::Grid { grid, mirror_x }) => {
                    let key = std::ptr::from_ref(grid.as_ref());
                    let at = if let Some((_, at)) = packed_grids.iter().find(|(k, _)| *k == key) {
                        *at
                    } else {
                        let at = grids.len();
                        grids.push(grid.origin.extend(grid.cell.x).to_array());
                        grids.push(word_u32(grid.dims.x, grid.dims.y, grid.dims.z, 0));
                        grids.extend(grid.values.chunks(4).map(|c| std::array::from_fn(|i| c.get(i).copied().unwrap_or(0.0))));
                        packed_grids.push((key, at));
                        at
                    };
                    (GRID, [bits(at.clamp_u32()), f32::from(u8::from(*mirror_x)), 0.0, 0.0], [0.0; 4])
                }
            };
            let (op, height) = match prim.op {
                PrimOp::Union => (UNION, 0.0),
                PrimOp::Subtract => (SUBTRACT, 0.0),
                PrimOp::Displace { height } => (DISPLACE, height),
            };
            packed.push(Packed {
                to_local: prim.to_local,
                min_scale: prim.min_scale,
                kind,
                params,
                more,
                op,
                limb: prim.limb.map_or(NO_LIMB, u32::from),
                flags: if highlight(prim.part) { HIGHLIGHT } else { 0 },
                blend: prim.blend,
                height,
                anchor: prim.anchor.unwrap_or((Vec3A::ZERO, 0.0)),
                aabb: prim.aabb,
            });
        }
        // Hard parts, after the skin (a hard union: blend 0), in the field's pose.
        let model = self.bone_model();
        for (pi, part) in (0u16..).zip(&anatomy.parts) {
            let lit = highlight(pi);
            for hard in anatomy.hard_parts.get(part.hard_parts.start.idx()..part.hard_parts.end.idx()).unwrap_or_default() {
                packed.extend(hard_part(hard, model, lit));
            }
        }

        // The first `self.prims.len()` packed primitives are the field's own, in its order.
        let skin = |list: &[u32], p: Vec3A| {
            let skin: Vec<usize> = list.iter().map(|i| i.idx()).filter(|&i| i < self.prims.len()).collect();
            self.distance_of(&skin, p)
        };
        let hard = |list: &[u32]| list.iter().any(|i| i.idx() >= self.prims.len());
        let mut field = assemble(&packed, segments, grids, self.bounds, |list, at| hard(list) || skin(list, Vec3A::from(at.truncate())) < at.w);
        field.sheets = PreviewSheets::new(anatomy, self.bone_model(), &highlight);
        field
    }
}

/// Lays out the words: header, primitives, sweep points, grids, then the cell grid over the
/// primitives' bounds (`fallback` when there are none) and its lists.
///
/// `may_hold_surface(list, centre and margin)` says whether a cell can hold surface: the shader
/// steps straight through the others (the empty-space skipping that keeps the march cheap).
fn assemble(
    packed: &[Packed],
    segments: Vec<[f32; 4]>,
    grids: Vec<[f32; 4]>,
    fallback: Aabb3d,
    may_hold_surface: impl Fn(&[u32], Vec4) -> bool + Sync,
) -> PreviewField {
    let bounds = packed.iter().map(|p| p.aabb).reduce(|a, b| a.merge(&b)).unwrap_or(fallback);
    let min = Vec3::from(bounds.min);
    let size = (Vec3::from(bounds.max) - min).max(Vec3::splat(1e-3));
    let cell = CELL_SIZE * (size.max_element() / super::lod::REFERENCE_EXTENT).max(1.0);
    let dims = (size / cell).ceil().as_uvec3().clamp(UVec3::ONE, UVec3::splat(MAX_CELLS));
    let cell = (size / dims.as_vec3()).max_element();

    // Cell lists: every primitive whose padded bounds come within the list pad of the cell, in
    // field order.
    let cell_count = (dims.x * dims.y * dims.z).idx();
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); cell_count];
    let to_cell = |p: Vec3A| ((Vec3::from(p) - min) / cell).floor().as_ivec3().clamp(IVec3::ZERO, dims.as_ivec3() - IVec3::ONE).as_uvec3();
    let pad = Vec3A::splat(cell / BRICK_VOXELS.to_f32() * LIST_PAD_VOXELS);
    for (i, prim) in (0u32..).zip(packed) {
        let (lo, hi) = (to_cell(prim.aabb.min - pad), to_cell(prim.aabb.max + pad));
        for z in lo.z..=hi.z {
            for y in lo.y..=hi.y {
                for x in lo.x..=hi.x {
                    if let Some(list) = lists.get_mut((x + dims.x * (y + dims.y * z)).idx()) {
                        list.push(i);
                    }
                }
            }
        }
    }
    // A cell holds no surface when the field at its centre is further out than the centre is
    // from its corners (with room for the field's bounds not being exact distances).
    let margin = cell * 0.5 * 3f32.sqrt() * 1.5;
    let centre = |i: usize| {
        let i = i.clamp_u32();
        let at = UVec3::new(i % dims.x, (i / dims.x) % dims.y, i / (dims.x * dims.y));
        (min + (at.as_vec3() + 0.5) * cell).extend(margin)
    };
    let indices: Vec<usize> = (0..cell_count).collect();
    let surface = super::par_map(&indices, |&i| lists.get(i).is_some_and(|list| !list.is_empty() && may_hold_surface(list, centre(i))));
    let mut cells = Vec::with_capacity(cell_count);
    let mut items: Vec<u32> = Vec::new();
    let mut bricks: Vec<u32> = Vec::new();
    for ((i, list), &surface) in (0u32..).zip(&lists).zip(&surface) {
        let brick = if surface {
            bricks.push(i);
            bricks.len().clamp_u32()
        } else {
            0
        };
        cells.push(word_u32(items.len().clamp_u32(), list.len().clamp_u32(), brick, 0));
        items.extend(list);
    }

    let seg_start = HEADER_WORDS + packed.len() * PRIM_WORDS;
    let grid_start = seg_start + segments.len();
    let cell_start = grid_start + grids.len();
    let item_start = cell_start + cells.len();
    let brick_start = item_start + items.len();
    let mut words = Vec::with_capacity(brick_start + bricks.len());
    words.push(word_u32(packed.len().clamp_u32(), seg_start.clamp_u32(), cell_start.clamp_u32(), item_start.clamp_u32()));
    words.push(min.extend(cell).to_array());
    words.push(word_u32(dims.x, dims.y, dims.z, grid_start.clamp_u32()));
    words.push(CrossSection::KEEL.extend(0.0).to_array());
    words.push(word_u32(BRICK_VOXELS, brick_start.clamp_u32(), bricks.len().clamp_u32(), 0));
    for p in packed {
        let m = p.to_local;
        let row = |r: usize| {
            let c = m.matrix3.row(r);
            [c.x, c.y, c.z, m.translation[r]]
        };
        words.extend([
            row(0),
            row(1),
            row(2),
            word_u32(p.kind, p.op, p.limb, p.flags),
            p.params,
            p.more,
            [p.blend, p.min_scale, p.height, 0.0],
            [p.anchor.0.x, p.anchor.0.y, p.anchor.0.z, p.anchor.1],
        ]);
    }
    words.extend(segments);
    words.extend(grids);
    words.extend(cells);
    // One per word: picking a vector component by a runtime index is slow on some GPUs.
    words.extend(items.iter().map(|&i| word_u32(i, 0, 0, 0)));
    words.extend(bricks.iter().map(|&i| word_u32(i, 0, 0, 0)));
    let hull = hull(&surface, dims, min, cell);
    PreviewField { words, min, max: min + dims.as_vec3() * cell, bricks: bricks.len().clamp_u32(), hull, sheets: PreviewSheets::default() }
}

/// The faces between cells that may hold surface (`surface`, one flag per cell, x fastest) and
/// cells that don't (or the outside), facing out.
fn hull(surface: &[bool], dims: UVec3, min: Vec3, cell: f32) -> Vec<([Vec3; 4], Vec3)> {
    let at = |c: IVec3| -> bool {
        c.cmpge(IVec3::ZERO).all()
            && c.cmplt(dims.as_ivec3()).all()
            && surface.get((c.x + c.y * dims.as_ivec3().x + c.z * dims.as_ivec3().x * dims.as_ivec3().y).unsigned_abs().idx()).copied().unwrap_or(false)
    };
    // Per face: outward normal and the two edge directions (u × v = normal).
    let faces = [
        (IVec3::X, Vec3::Y, Vec3::Z),
        (IVec3::NEG_X, Vec3::Z, Vec3::Y),
        (IVec3::Y, Vec3::Z, Vec3::X),
        (IVec3::NEG_Y, Vec3::X, Vec3::Z),
        (IVec3::Z, Vec3::X, Vec3::Y),
        (IVec3::NEG_Z, Vec3::Y, Vec3::X),
    ];
    let mut quads = Vec::new();
    for z in 0..dims.z {
        for y in 0..dims.y {
            for x in 0..dims.x {
                let here = UVec3::new(x, y, z).as_ivec3();
                if !at(here) {
                    continue;
                }
                let corner = min + here.as_vec3() * cell;
                for (normal, along, across) in faces {
                    if at(here + normal) {
                        continue;
                    }
                    let outward = normal.as_vec3();
                    let origin = corner + outward.max(Vec3::ZERO) * cell;
                    let (along, across) = (along * cell, across * cell);
                    quads.push(([origin, origin + along, origin + along + across, origin + across], outward));
                }
            }
        }
    }
    quads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{BodyFieldOptions, Field};

    /// Each packed cell lists the primitives that come within the list pad of it, in field order:
    /// evaluating a point's cell list (as the shaders do) gives what a meshing chunk over the
    /// padded cell gives, and every cell the surface passes through is marked as holding it. (Not quite
    /// the whole field's value: which limb is nearest so far depends on primitives beyond the
    /// cell, and a smooth union away from the surface can feel them too.)
    #[test]
    fn cell_lists_reproduce_the_field() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/quadruped.body.ron")).unwrap();
        let anatomy = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
        let field = BodyField::new(&anatomy, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false });
        let packed = field.preview(&anatomy, |_| false);
        let header = packed.words[0].map(f32::to_bits);
        assert!(header[0].idx() > field.prims.len(), "skin and hard parts");
        let dims = UVec3::new(packed.words[2][0].to_bits(), packed.words[2][1].to_bits(), packed.words[2][2].to_bits());
        let cell = packed.words[1][3];
        let (cell_start, item_start) = (header[2].idx(), header[3].idx());
        let item = |i: usize| packed.words[item_start + i][0].to_bits();
        let mut checked = 0;
        for i in 0..200_000u32 {
            // A deterministic scatter through the bounds.
            let scatter = |k: u32| f32::from(u16::try_from(i.wrapping_mul(2_654_435_761).wrapping_add(k.wrapping_mul(i).wrapping_mul(40503)) % 10007).unwrap()) / 10007.0;
            let p = packed.min + (packed.max - packed.min) * Vec3::new(scatter(1), scatter(2), scatter(3));
            let d = field.distance(p.into());
            if d.abs() > 0.002 {
                continue;
            }
            let at = ((p - packed.min) / cell).floor().as_uvec3().min(dims - UVec3::ONE);
            let lo = packed.min + at.as_vec3() * cell;
            let pad = Vec3::splat(cell / BRICK_VOXELS.to_f32() * LIST_PAD_VOXELS);
            let chunk = field.view(Aabb3d { min: (lo - pad).into(), max: (lo + Vec3::splat(cell) + pad).into() });
            let w = packed.words[cell_start + (at.x + dims.x * (at.y + dims.y * at.z)).idx()].map(f32::to_bits);
            assert!(w[2] > 0, "the cell at {p} holds surface but has no brick");
            let list: Vec<usize> = (0..w[1].idx()).map(|k| item(w[0].idx() + k).idx()).filter(|&k| k < field.prims.len()).collect();
            assert!(list.windows(2).all(|w| w[0] < w[1]), "field order");
            let sub = field.distance_of(&list, p.into());
            let want = chunk.distance(p.into());
            assert!((sub - want).abs() < 1e-5, "at {p}: cell {sub}, chunk {want}");
            assert!((sub - d).abs() < 0.005, "at {p}: cell {sub}, field {d}");
            checked += 1;
        }
        assert!(checked > 50, "only {checked} points near the surface");
    }
}

