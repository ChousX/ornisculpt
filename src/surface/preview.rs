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
//! 4          voxels along a brick's edge, first part word, part count
//! 5..        primitives, PRIM_WORDS each: model → local rows (3), kind / op / limb / flags,
//!            shape parameters, more parameters (a sweep's largest radius), blend / min scale /
//!            displace height / part, blend anchor
//! segments   one word per sweep point: position (xyz), radius
//! grids      per authored grid: origin (xyz) and cell, then dims (xyz), then values 4 per word
//! cells      one word per cell: first item, item count, brick + 1 (0: no surface in the cell)
//! items      primitive indices, one per word (x), each cell's in field order
//! parts      one word per part of the anatomy: 1 (x) where it is selected
//! ```
//!
//! Each cell lists the primitives whose padded bounds come near it, in field order, so evaluating
//! a cell's list gives what [`BodyField`] gives there (as a meshing [`ChunkView`](super::ChunkView)
//! does). Cells that may hold surface get a brick: the field sampled on a lattice over the cell
//! (`BRICK_VOXELS` + 1 samples a side, walls included), with the distances to the nearest eye and
//! other hard part and the nearest primitive's part, which a compute pass bakes into a 3D texture
//! atlas (`sdf_bake.wesl`) for the preview to march and colour (`sdf_preview.wesl`).
//!
//! The cells lie on a lattice that holds still while edits move the body's bounds, and each brick
//! comes with a key: a hash of its cell's position and of everything its list evaluates (the
//! primitives in order, their sweep points and grids; not the highlight). Equal keys mean equal
//! distances, so after an edit only the bricks whose key changed are baked again (see
//! [`SdfPreview`](super::SdfPreview)), and a change of selection bakes none.

use bevy::math::Vec3A;
use bevy::prelude::*;

use bevy::math::Affine3A;
use bevy::shape::{Aabb3d, BoundingVolume};
use xxhash_rust::xxh3::Xxh3;

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
/// Voxels along a brick's edge: about 5 mm on a human-sized body.
///
/// At 8 (8 mm) folded wing fingers beaded and thin claws dropped out; 12 keeps them for 0.2-0.5 ms
/// more a frame on the march (laptop GPU), and since only the bricks an edit changes are baked,
/// edits cost about what they did. 16 marches 1-2 ms dearer for little more.
pub const BRICK_VOXELS: u32 = 12;
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

/// A [`BodyField`] as GPU words, the bounds it covers, and the hull to draw it with, in the
/// body's model space before grounding: `offset` takes it to model space.
#[derive(Clone, Debug, Default)]
pub struct PreviewField {
    pub words: Vec<[f32; 4]>,
    pub min: Vec3,
    pub max: Vec3,
    /// Cells holding surface, each with a brick of distances to bake, in cell order. Their cell
    /// words (z) count them from 1 until [`SdfPreview::show`](super::SdfPreview::show) gives each
    /// its slot in the atlas.
    pub bricks: Vec<Brick>,
    /// The outer faces of the cells that may hold surface, as quads (four corners each, wound
    /// counter-clockwise seen from outside) with their outward normals. Rays start where they
    /// enter it, so the empty space around the body is neither drawn nor marched.
    pub hull: Vec<([Vec3; 4], Vec3)>,
    /// Wing sheets (membranes, flight feathers, insect wings): thin, so drawn as the triangles
    /// the anatomy already has rather than marched.
    pub sheets: PreviewSheets,
    /// Field space → model space: the body's grounding (`-Anatomy::ground` along y). The sheets
    /// are in model space already.
    pub offset: Vec3,
}

/// A cell holding surface, and the key of the distances in its brick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brick {
    /// Index of the cell (x fastest).
    pub cell: u32,
    /// Hash of what the brick's distances depend on; equal keys, equal distances. Never 0.
    pub key: u64,
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
    /// The anatomy part it belongs to.
    part: u16,
    blend: f32,
    height: f32,
    anchor: (Vec3A, f32),
    aabb: Aabb3d,
}

/// Flag bits (word 3.w): an eye; a hard part (`EYE` and `HARD` in `sdf_field.wesl`).
const EYE: u32 = 2;
const HARD: u32 = 4;

/// A hard part of anatomy part `of` as a stand-in primitive: eyes as spheres, plates as ellipsoids,
/// everything horn-like as a straight round cone (curl and twist left out). Authored meshes are
/// skipped.
fn hard_part(part: &HardPart, of: u16, model: &[Affine3A]) -> Option<Packed> {
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
        flags: flag,
        part: of,
        blend: 0.0,
        height: 0.0,
        anchor: (Vec3A::ZERO, 0.0),
        aabb: transform_aabb(&m, local_bounds(&shape)),
    })
}

impl BodyField {
    /// Packs the field for `sdf_preview.wesl`, plus stand-ins for `anatomy`'s hard parts (eyes,
    /// claws, horns: see `hard_part`) and its wing sheets, in the pose the field was built in.
    /// Parts for which `highlight` is true are listed as selected (the shader tints them).
    /// Creases are left out (they are shading-scale).
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
                flags: 0,
                part: prim.part,
                blend: prim.blend,
                height,
                anchor: prim.anchor.unwrap_or((Vec3A::ZERO, 0.0)),
                aabb: prim.aabb,
            });
        }
        // Hard parts, after the skin (a hard union: blend 0), in the field's pose.
        let model = self.bone_model();
        for (pi, part) in (0u16..).zip(&anatomy.parts) {
            for hard in anatomy.hard_parts.get(part.hard_parts.start.idx()..part.hard_parts.end.idx()).unwrap_or_default() {
                packed.extend(hard_part(hard, pi, model));
            }
        }

        // Packed as built, before grounding (`Anatomy::ground`): an edit that moves the lowest
        // point (a longer leg) moves the whole body, but not its bricks.
        let up = Vec3A::Y * anatomy.ground;
        for p in &mut packed {
            p.to_local *= Affine3A::from_translation((-up).into());
            p.aabb = Aabb3d { min: p.aabb.min + up, max: p.aabb.max + up };
        }
        let fallback = Aabb3d { min: self.bounds.min + up, max: self.bounds.max + up };

        // The first `self.prims.len()` packed primitives are the field's own, in its order.
        let skin = |list: &[u32], p: Vec3A| {
            let skin: Vec<usize> = list.iter().map(|i| i.idx()).filter(|&i| i < self.prims.len()).collect();
            self.distance_of(&skin, p - up)
        };
        let hard = |list: &[u32]| list.iter().any(|i| i.idx() >= self.prims.len());
        let lit: Vec<bool> = (0..anatomy.parts.len()).map(|i| highlight(u16::try_from(i).unwrap_or(u16::MAX))).collect();
        let mut field = assemble(&packed, segments, grids, &lit, fallback, |list, at| hard(list) || skin(list, Vec3A::from(at.truncate())) < at.w);
        field.offset = (-up).into();
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
    lit: &[bool],
    fallback: Aabb3d,
    may_hold_surface: impl Fn(&[u32], Vec4) -> bool + Sync,
) -> PreviewField {
    let bounds = packed.iter().map(|p| p.aabb).reduce(|a, b| a.merge(&b)).unwrap_or(fallback);
    let (min, cell, dims) = lattice(Vec3::from(bounds.min), Vec3::from(bounds.max));

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
    // The grid's corner in whole cells from the model origin: bricks are keyed by where they are
    // on the lattice, not by their index in this grid.
    let origin = (min / cell).round().as_ivec3();
    let prim_keys: Vec<u64> = packed.iter().map(|p| prim_key(p, &segments, &grids)).collect();
    let mut cells = Vec::with_capacity(cell_count);
    let mut items: Vec<u32> = Vec::new();
    let mut bricks: Vec<Brick> = Vec::new();
    for ((i, list), &surface) in (0u32..).zip(&lists).zip(&surface) {
        let brick = if surface {
            let at = UVec3::new(i % dims.x, (i / dims.x) % dims.y, i / (dims.x * dims.y));
            bricks.push(Brick { cell: i, key: brick_key(origin + at.as_ivec3(), cell, list, &prim_keys) });
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
    let part_start = item_start + items.len();
    let mut words = Vec::with_capacity(part_start + lit.len());
    words.push(word_u32(packed.len().clamp_u32(), seg_start.clamp_u32(), cell_start.clamp_u32(), item_start.clamp_u32()));
    words.push(min.extend(cell).to_array());
    words.push(word_u32(dims.x, dims.y, dims.z, grid_start.clamp_u32()));
    words.push(CrossSection::KEEL.extend(0.0).to_array());
    words.push(word_u32(BRICK_VOXELS, part_start.clamp_u32(), lit.len().clamp_u32(), 0));
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
            [p.blend, p.min_scale, p.height, bits(u32::from(p.part))],
            [p.anchor.0.x, p.anchor.0.y, p.anchor.0.z, p.anchor.1],
        ]);
    }
    words.extend(segments);
    words.extend(grids);
    words.extend(cells);
    // One per word: picking a vector component by a runtime index is slow on some GPUs.
    words.extend(items.iter().map(|&i| word_u32(i, 0, 0, 0)));
    words.extend(lit.iter().map(|&on| word_u32(u32::from(on), 0, 0, 0)));
    let hull = hull(&surface, dims, min, cell);
    PreviewField { words, min, max: min + dims.as_vec3() * cell, bricks, hull, sheets: PreviewSheets::default(), offset: Vec3::ZERO }
}

/// The cell grid over `lo..hi`: its corner, cell size and cells per axis. Cells are
/// [`CELL_SIZE`] scaled up with the body in steps of a quarter octave (as meshing scales its
/// voxels, but stepped), and the corner sits on a multiple of the cell size, so edits that move the
/// bounds leave the cells they don't reach where they were.
fn lattice(lo: Vec3, hi: Vec3) -> (Vec3, f32, UVec3) {
    let size = (hi - lo).max(Vec3::splat(1e-3));
    let scale = (size.max_element() / super::lod::REFERENCE_EXTENT).max(1.0);
    let mut cell = CELL_SIZE * ((scale.log2() * 4.0).ceil() / 4.0).exp2();
    loop {
        let min = (lo / cell).floor() * cell;
        let dims = ((hi - min) / cell).ceil().as_uvec3().max(UVec3::ONE);
        if dims.max_element() <= MAX_CELLS {
            return (min, cell, dims);
        }
        cell *= 0.25f32.exp2();
    }
}

/// Steps per metre (and per unit of a matrix entry) that keys tell apart: changes finer than a
/// quarter millimetre, far under a voxel, keep a brick's key. Exact bits would not do: grounding
/// and ungrounding a body leaves its transforms a few ulps off.
const KEY_STEPS: f32 = 4096.0;

/// Values into a key, rounded to [`KEY_STEPS`].
fn hash_values(h: &mut Xxh3, values: &[f32]) {
    for v in values {
        let r = (v * KEY_STEPS).round();
        // Integers in f32: exact below 2^24 steps (4 km). -0 and 0 alike.
        h.update(&(if r == 0.0 { 0 } else { r.to_bits() }).to_le_bytes());
    }
}

/// Integers (and bit-cast words) into a key, exactly.
fn hash_bits(h: &mut Xxh3, bits: &[u32]) {
    for b in bits {
        h.update(&b.to_le_bytes());
    }
}

/// What a primitive contributes to the field and the bricks, hashed: its values (rounded) and
/// integers (its part's index too), without the offsets of its sweep points and grid (those move when data before
/// them changes), plus that data itself.
fn prim_key(p: &Packed, segments: &[[f32; 4]], grids: &[[f32; 4]]) -> u64 {
    let mut h = Xxh3::new();
    hash_values(&mut h, &p.to_local.matrix3.to_cols_array());
    hash_values(&mut h, &p.to_local.translation.to_array());
    hash_bits(&mut h, &[p.kind, p.op, p.limb, p.flags, u32::from(p.part)]);
    let [offset, count, more @ ..] = p.params;
    match p.kind {
        SWEEP => {
            let first = offset.to_bits().idx();
            hash_bits(&mut h, &[count.to_bits()]);
            hash_values(&mut h, &more);
            let count = count.to_bits().idx();
            for point in segments.get(first..first + count).unwrap_or_default() {
                hash_values(&mut h, point);
            }
        }
        GRID => {
            // The second parameter is the mirror flag, not a count.
            hash_values(&mut h, &p.params[1..]);
            let at = offset.to_bits().idx();
            if let (Some(origin), Some(dims)) = (grids.get(at), grids.get(at + 1)) {
                let dims = dims.map(f32::to_bits);
                hash_values(&mut h, origin);
                hash_bits(&mut h, &dims);
                let n = dims[..3].iter().map(|d| d.idx()).product::<usize>().div_ceil(4);
                for values in grids.get(at + 2..at + 2 + n).unwrap_or_default() {
                    hash_values(&mut h, values);
                }
            }
        }
        _ => hash_values(&mut h, &p.params),
    }
    hash_values(&mut h, &p.more);
    hash_values(&mut h, &[p.blend, p.min_scale, p.height]);
    hash_values(&mut h, &[p.anchor.0.x, p.anchor.0.y, p.anchor.0.z, p.anchor.1]);
    h.digest()
}

/// The key of the brick of the cell at `at` on the lattice (whole cells of edge `cell` from the
/// model origin) listing `list`.
fn brick_key(at: IVec3, cell: f32, list: &[u32], prim_keys: &[u64]) -> u64 {
    let mut h = Xxh3::new();
    let at = at.as_uvec3();
    hash_bits(&mut h, &[at.x, at.y, at.z, cell.to_bits(), BRICK_VOXELS]);
    hash_values(&mut h, &CrossSection::KEEL.to_array());
    for &i in list {
        h.update(&prim_keys.get(i.idx()).copied().unwrap_or(0).to_le_bytes());
    }
    h.digest() | 1
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
            // The packed field is before grounding; the body field after.
            let m = p + packed.offset;
            let d = field.distance(m.into());
            if d.abs() > 0.002 {
                continue;
            }
            let at = ((p - packed.min) / cell).floor().as_uvec3().min(dims - UVec3::ONE);
            let lo = packed.min + at.as_vec3() * cell;
            let pad = Vec3::splat(cell / BRICK_VOXELS.to_f32() * LIST_PAD_VOXELS);
            let lo = lo + packed.offset;
            let chunk = field.view(Aabb3d { min: (lo - pad).into(), max: (lo + Vec3::splat(cell) + pad).into() });
            let w = packed.words[cell_start + (at.x + dims.x * (at.y + dims.y * at.z)).idx()].map(f32::to_bits);
            assert!(w[2] > 0, "the cell at {p} holds surface but has no brick");
            let list: Vec<usize> = (0..w[1].idx()).map(|k| item(w[0].idx() + k).idx()).filter(|&k| k < field.prims.len()).collect();
            assert!(list.windows(2).all(|w| w[0] < w[1]), "field order");
            let sub = field.distance_of(&list, m.into());
            let want = chunk.distance(m.into());
            assert!((sub - want).abs() < 1e-5, "at {p}: cell {sub}, chunk {want}");
            assert!((sub - d).abs() < 0.005, "at {p}: cell {sub}, field {d}");
            checked += 1;
        }
        assert!(checked > 50, "only {checked} points near the surface");
    }
    /// Bricks are keyed by what their distances depend on: an edit to a small part changes the
    /// keys of the bricks around it only, and changing the selection (the highlight) changes none.
    #[test]
    fn brick_keys_follow_local_edits() {
        let build = |src: &str| {
            let plan: BodyPlan = ron::from_str(src).unwrap();
            build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap()
        };
        let opts = BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false };
        let src = include_str!("../../assets/mobs/quadruped.body.ron");
        let anatomy = build(src);
        let field = BodyField::new(&anatomy, &opts);
        let plain = field.preview(&anatomy, |_| false);
        let keys = |f: &PreviewField| f.bricks.iter().map(|b| b.key).collect::<std::collections::HashSet<u64>>();
        assert!(plain.bricks.iter().all(|b| b.key != 0));
        assert_eq!(keys(&plain).len(), plain.bricks.len(), "keys are unique");

        let lit = field.preview(&anatomy, |part| part % 2 == 0);
        assert_eq!(plain.bricks, lit.bricks, "highlight leaves the keys alone");
        assert_ne!(plain.words, lit.words, "but not the words");

        // Larger ears: a few bricks around the ears change, the rest keep their keys.
        let ears = build(&src.replacen("length: 0.08,", "length: 0.09,", 1));
        let edited = BodyField::new(&ears, &opts).preview(&ears, |_| false);
        let before = keys(&plain);
        let changed = edited.bricks.iter().filter(|b| !before.contains(&b.key)).count();
        assert!(changed > 0, "the ears' bricks change");
        assert!(changed * 10 < edited.bricks.len(), "{changed} of {} bricks changed", edited.bricks.len());

        // Longer front legs lift the whole body (grounding), but the bricks are keyed before it:
        // the legs' bricks change, the rest keep their keys.
        let legs = build(&src.replacen("length: 0.42,", "length: 0.45,", 1));
        assert!((legs.ground - anatomy.ground).abs() > 1e-3, "the edit regrounds the body");
        let edited = BodyField::new(&legs, &opts).preview(&legs, |_| false);
        let changed = edited.bricks.iter().filter(|b| !before.contains(&b.key)).count();
        assert!(changed * 2 < edited.bricks.len(), "{changed} of {} bricks changed", edited.bricks.len());
        assert!((edited.offset.y + legs.ground).abs() < 1e-6);
    }
}
