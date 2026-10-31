use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::prelude::*;

use super::{BodyMeshData, MaterialSlot, MobVertexData, Submesh};
use crate::num::{Clamp, Idx, Saturate, ToF32};
use crate::anatomy::{Anatomy, HardKind, HardParams, HardPart, HardShape, LodLevel, PrimLayer};

/// Shape recipe for one tapered, curved horn in hard-part local space (+Y out, bends toward +Z
/// turned by `bend`).
struct Horn {
    length: f32,
    base_radius: f32,
    /// Bend over the length in quarter turns.
    curve: f32,
    /// Spiral turns of the bend plane (and of the ridges).
    twist: f32,
    /// Cross-section scale (x, z).
    flatten: Vec2,
    /// Heading of the bend about the axis, radians from +Z toward +X.
    bend: f32,
    /// Exponent of the radius's fall toward the tip (see [`HardShape::taper`]).
    taper: f32,
    /// Spiral ridges round the section and rings along it (see [`HardShape`]); none on a plain
    /// horn, which then builds exactly as it did before they existed.
    ridges: u8,
    ridge_depth: f32,
    rings: u8,
    ring_depth: f32,
}

impl Horn {
    /// Whether the section is modulated (ridges or rings), which also means smoothed normals.
    const fn detailed(&self) -> bool {
        self.ridges > 0 || self.rings > 0
    }
}

/// Mesh for one hard part (claw, horn, tooth, beak, eye, plate, antler or authored mesh), 100%
/// weighted to the part's bone, in model space at rest pose, using [`MaterialSlot::Hard`] or `Eye`.
/// Procedural shapes are curved tapered horns, branching horns (antlers), flattened horns (beaks,
/// nails) and spheres (eyes); tessellation drops with `level`.
///
/// `index` is the part's place in [`Anatomy::hard_parts`], carried in the vertex data for the
/// coat's markings. `None` for an authored mesh not yet loaded (see [`HardPart::mesh`]) or a bone
/// out of range.
pub(super) fn hard_part_mesh(index: usize, part: &HardPart, anatomy: &Anatomy, level: LodLevel) -> Option<BodyMeshData> {
    let p = &part.params;
    let s = p.size;
    let (rings, sides) = match level {
        LodLevel::Hero => (8, 10),
        LodLevel::Near => (5, 6),
        LodLevel::Far => (3, 4),
    };
    // Ridges and rings need the mesh to follow them: more rings along a spiral or a row of rings,
    // more sides round a lobed section. Not at `Far`, where they are below a pixel.
    let shape = if level == LodLevel::Far { HardShape { ridges: 0, rings: 0, ..p.shape } } else { p.shape };
    let (rings, sides) = if shape.ridges > 0 || shape.rings > 0 {
        let turns = if shape.ridges > 0 { p.twist.abs() } else { 0.0 };
        let along = f32::from(shape.rings).mul_add(4.0, turns.mul_add(12.0, 8.0)).ceil().sat_u32();
        let around = (f32::from(shape.ridges) * 10.0).sat_u32().max(12);
        if level == LodLevel::Hero { (along.max(rings), around.max(sides)) } else { ((along / 2).max(rings), (around / 2).max(sides)) }
    } else {
        (rings, sides)
    };
    let horn = |length: f32, base: f32, flatten: Vec2| Horn {
        length,
        base_radius: base,
        curve: p.curve,
        twist: p.twist,
        flatten,
        bend: shape.bend_deg.to_radians(),
        taper: shape.taper,
        ridges: shape.ridges,
        ridge_depth: shape.ridge_depth,
        rings: shape.rings,
        ring_depth: shape.ring_depth,
    };
    let mut mesh = BodyMeshData::default();
    let slot = match &p.kind {
        HardKind::Mesh { scale, .. } => {
            let src = part.mesh.as_ref()?;
            let flip = if part.mirror { Vec3::new(-1.0, 1.0, 1.0) } else { Vec3::ONE };
            mesh.positions = src.positions.iter().map(|v| *v * *scale * flip).collect();
            mesh.normals = if src.normals.len() == src.positions.len() {
                src.normals.iter().map(|n| *n * flip).collect()
            } else {
                vec![Vec3::Y; src.positions.len()]
            };
            mesh.indices.clone_from(&src.indices);
            if part.mirror {
                // Reflection flips handedness: restore triangle winding.
                mesh.indices.as_chunks_mut::<3>().0.iter_mut().for_each(|t| t.swap(1, 2));
            }
            MaterialSlot::Hard
        }
        HardKind::Eye => {
            // Twice the usual resolution: eyes are looked at, and round.
            sphere(&mut mesh, s, (rings * 2).max(4), sides * 2);
            // uv = (cosine from forward (+Y), 1 + white showing) for the iris shading.
            let white = 1.0 + p.sclera.clamp(0.0, 1.0);
            mesh.uvs = mesh.normals.iter().map(|n| [n.y, white]).collect();
            MaterialSlot::Eye
        }
        HardKind::Plate => {
            // A dome flattened onto the skin: wide across (x), long along (z), thin outward (y).
            sphere(&mut mesh, 1.0, rings.max(4), sides);
            let scale = plate_radii(s, p.curve);
            for (v, n) in mesh.positions.iter_mut().zip(&mut mesh.normals) {
                *v *= scale;
                *n = (*n / scale).normalize_or(Vec3::Y);
            }
            MaterialSlot::Hard
        }
        HardKind::Antler { tines } => {
            let (length, base, flatten) = horn_dims(p)?;
            let beam = horn(length, base, flatten);
            horn_mesh(&mut mesh, &beam, Transform::IDENTITY, rings, sides);
            let tines = (*tines).max(1);
            for i in 0..tines {
                let t = (f32::from(i) + 1.0) / (f32::from(tines) + 1.0);
                let at = horn_frame(&beam, t);
                let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                let tine = Transform { rotation: at.rotation * Quat::from_rotation_z(side * 0.9), ..at };
                // Tines are plain: the beam alone takes the shape.
                let tine_horn = Horn { ridges: 0, rings: 0, ..horn(s * 0.4 * t.mul_add(-0.5, 1.0), s * 0.05, Vec2::ONE) };
                horn_mesh(&mut mesh, &tine_horn, tine, rings.min(5), sides);
            }
            MaterialSlot::Hard
        }
        _ => {
            let (length, base, flatten) = horn_dims(p)?;
            horn_mesh(&mut mesh, &horn(length, base, flatten), Transform::IDENTITY, rings, sides)
        }
    };

    let model = *anatomy.skeleton.model_rest().get(part.bone.index())? * part.local.compute_affine();
    let data = MobVertexData {
        region: part.region,
        cavity: 0,
        shade: 0,
        limb_t: 255,
        primary_bone: part.bone,
        layer: PrimLayer::Base,
        hard: u8::try_from(index).ok().filter(|_| index < MobVertexData::MAX_HARD),
    }
    .pack();
    for p in &mut mesh.positions {
        *p = model.transform_point3(*p);
    }
    for n in &mut mesh.normals {
        *n = model.transform_vector3(*n).normalize_or(Vec3::Y);
    }
    let n = mesh.positions.len();
    mesh.joints = vec![[u16::from(part.bone.0), 0, 0, 0]; n];
    mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; n];
    mesh.mob_data = vec![data; n];
    mesh.submeshes = vec![Submesh { indices: 0..mesh.indices.len().clamp_u32(), slot }];
    Some(mesh)
}

/// Length, base radius and cross-section scale (x, z) of a horn-like part (an antler's beam): the
/// kind's proportions of its `size`, the base radius replaced by the shape's own fraction when it
/// sets one. `None` for eyes, plates and authored meshes. The SDF preview stands in with the same.
pub(super) fn horn_dims(p: &HardParams) -> Option<(f32, f32, Vec2)> {
    let s = p.size;
    let (length, base, flatten) = match p.kind {
        HardKind::Claw => (s, 0.25, Vec2::new(0.7, 1.0)),
        HardKind::Nail => (s * 0.4, 0.5, Vec2::new(1.0, 0.25)),
        HardKind::Horn => (s, 0.22, Vec2::ONE),
        HardKind::Tusk => (s, 0.15, Vec2::ONE),
        HardKind::Tooth => (s, 0.35, Vec2::new(1.0, 0.6)),
        HardKind::Fang => (s, 0.22, Vec2::new(1.0, 0.8)),
        HardKind::Spike => (s, 0.3, Vec2::ONE),
        HardKind::Beak => (s, 0.45, Vec2::new(1.0, 0.7)),
        HardKind::Antler { .. } => (s, 0.08, Vec2::ONE),
        HardKind::Eye | HardKind::Plate | HardKind::Mesh { .. } => return None,
    };
    Some((length, p.shape.base.unwrap_or(base) * s, flatten))
}

/// Radii of a plate of `size`: wide across (x), thin outward (y, thicker with `curve`), long along (z).
pub(super) fn plate_radii(size: f32, curve: f32) -> Vec3 {
    Vec3::new(size * 0.45, size * 0.12f32.mul_add(curve.clamp(0.0, 1.0), 0.06), size * 0.5)
}

/// Frame on the horn's centreline at `t` (Y along the horn).
fn horn_frame(h: &Horn, t: f32) -> Transform {
    // March along the curve: direction rotates from +Y toward the (twisting) bend direction.
    const STEPS: f32 = 16.0;
    let mut pos = Vec3::ZERO;
    let steps = (STEPS * t).ceil().sat_u32().max(1);
    let dt = t / steps.to_f32();
    let mut rot = Quat::IDENTITY;
    for k in 0..steps {
        let u = (k.to_f32() + 0.5) * dt;
        rot = direction_at(h, u);
        pos += rot * Vec3::Y * h.length * dt;
    }
    if t == 0.0 {
        rot = direction_at(h, 0.0);
    }
    Transform::from_translation(pos).with_rotation(rot)
}

fn direction_at(h: &Horn, u: f32) -> Quat {
    let bend_axis = Quat::from_rotation_y(h.bend + h.twist * TAU * u) * Vec3::X;
    Quat::from_axis_angle(bend_axis, h.curve * FRAC_PI_2 * u)
}

/// The section's radius at `t` along the horn and `angle` round it, over the plain taper: rings
/// stand out as bumps along it, ridges as lobes round it that turn with the twist and flatten out
/// toward the tip (as a spiral horn's do). `1.0` on a plain horn.
fn section_scale(h: &Horn, t: f32, angle: f32) -> f32 {
    let mut scale = 1.0;
    if h.rings > 0 {
        let bump = (f32::from(h.rings) * TAU * t).cos().mul_add(0.5, 0.5).powi(2);
        scale *= h.ring_depth.mul_add(bump, 1.0);
    }
    if h.ridges > 0 {
        let depth = h.ridge_depth * (1.0 - t).sqrt();
        let phase = h.twist * TAU * t;
        scale *= depth.mul_add((f32::from(h.ridges) * (angle - phase)).cos(), 1.0 - depth);
    }
    scale
}

/// Appends a tapered tube with a closed tip, placed by `at`. Returns the material slot.
fn horn_mesh(mesh: &mut BodyMeshData, h: &Horn, at: Transform, rings: u32, sides: u32) -> MaterialSlot {
    let base = mesh.positions.len().clamp_u32();
    let first_index = mesh.indices.len();
    for ri in 0..=rings {
        let t = ri.to_f32() / rings.to_f32();
        let frame = at * horn_frame(h, t);
        let radius = h.base_radius * (1.0 - t).powf(h.taper);
        for si in 0..sides {
            let angle = si.to_f32() / sides.to_f32() * TAU;
            let local = Vec3::new(angle.cos() * h.flatten.x, 0.0, angle.sin() * h.flatten.y);
            let radius = if h.detailed() { radius * section_scale(h, t, angle) } else { radius };
            mesh.positions.push(frame.transform_point(local * radius));
            mesh.normals.push((frame.rotation * local).normalize_or(Vec3::Y));
        }
    }
    let ring = |ri: u32, si: u32| base + ri * sides + si % sides;
    for ri in 0..rings {
        for si in 0..sides {
            let (v00, v01, v10, v11) = (ring(ri, si), ring(ri, si + 1), ring(ri + 1, si), ring(ri + 1, si + 1));
            mesh.indices.extend([v00, v10, v01, v01, v10, v11]);
        }
    }
    if h.detailed() {
        // Radial normals miss the lobes and bumps: take them from the triangles.
        let smooth = super::mesh_data::vertex_normals(&mesh.positions, mesh.indices.get(first_index..).unwrap_or_default());
        for (n, s) in mesh.normals.iter_mut().zip(&smooth).skip(base.idx()) {
            *n = *s;
        }
    }
    MaterialSlot::Hard
}

fn sphere(mesh: &mut BodyMeshData, r: f32, rings: u32, sides: u32) {
    let base = mesh.positions.len().clamp_u32();
    for i in 0..=rings {
        let phi = i.to_f32() / rings.to_f32() * std::f32::consts::PI;
        for j in 0..=sides {
            let theta = j.to_f32() / sides.to_f32() * TAU;
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.positions.push(n * r);
            mesh.normals.push(n);
        }
    }
    let w = sides + 1;
    for i in 0..rings {
        for j in 0..sides {
            let a = base + i * w + j;
            mesh.indices.extend([a, a + 1, a + w, a + 1, a + w + 1, a + w]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BoneId, Side, build_src};

    /// The horn-like kinds at a few bends and twists, as built before shapes had parameters.
    fn cases() -> Vec<HardPart> {
        let kinds = [
            HardKind::Claw,
            HardKind::Nail,
            HardKind::Horn,
            HardKind::Tusk,
            HardKind::Tooth,
            HardKind::Fang,
            HardKind::Spike,
            HardKind::Beak,
            HardKind::Antler { tines: 3 },
            HardKind::Plate,
            HardKind::Eye,
        ];
        let mut out = Vec::new();
        for (i, kind) in kinds.into_iter().enumerate() {
            for (curve, twist) in [(0.0, 0.0), (0.4, 0.0), (0.55, 0.1), (0.3, -0.25)] {
                let size = 0.05 + 0.03 * i.to_f32();
                let params = crate::anatomy::HardParams { kind: kind.clone(), size, curve, twist, sclera: 0.3, ..Default::default() };
                let local = Transform::from_translation(Vec3::new(0.1, 0.2, 0.05)).with_rotation(Quat::from_rotation_z(0.4));
                out.push(HardPart { bone: BoneId(3), local, params, region: crate::anatomy::RegionId(0), part: 0, side: Side::Left, mesh: None, mirror: i % 2 == 1 });
            }
        }
        out
    }

    /// A mesh's fingerprint: counts, and sums of positions and normals.
    fn fingerprint(mesh: &BodyMeshData) -> (usize, usize, f64, f64) {
        let sum = |vs: &[Vec3]| -> f64 { vs.iter().enumerate().map(|(i, v)| f64::from(v.x * 1.0 + v.y * 2.0 + v.z * 3.0) * (1.0 + f64::from(i.to_f32()) * 1e-3)).sum() };
        (mesh.positions.len(), mesh.indices.len(), sum(&mesh.positions), sum(&mesh.normals))
    }

    /// Fingerprints of every case at every level as built before `HardShape` existed
    /// (2026-10-31, d3fcb48), in case order, hero, near, far.
    #[allow(clippy::unreadable_literal)]
    const BEFORE: [(usize, usize, f64, f64); 132] = [
        (90, 480, 209.650822, 0.201308),
        (36, 180, 81.672225, 0.026873),
        (16, 72, 35.938478, -0.001316),
        (90, 480, 208.424830, 0.288130),
        (36, 180, 81.182138, 0.045823),
        (16, 72, 35.709446, 0.003163),
        (90, 480, 208.449634, 0.331236),
        (36, 180, 81.197093, 0.058252),
        (16, 72, 35.720438, 0.007589),
        (90, 480, 208.473200, 0.215886),
        (36, 180, 81.203851, 0.026627),
        (16, 72, 35.721494, -0.002987),
        (90, 480, 207.577534, 0.042978),
        (36, 180, 80.871608, -0.010129),
        (16, 72, 35.587411, -0.001317),
        (90, 480, 206.792957, 0.083359),
        (36, 180, 80.557964, -0.002006),
        (16, 72, 35.440834, 0.003161),
        (90, 480, 206.808985, 0.111602),
        (36, 180, 80.567588, 0.006953),
        (16, 72, 35.447887, 0.007588),
        (90, 480, 206.823730, 0.041294),
        (36, 180, 80.571798, -0.013923),
        (16, 72, 35.448522, -0.002989),
        (90, 480, 216.556076, 0.173999),
        (36, 180, 84.338600, 0.022606),
        (16, 72, 37.107653, -0.001316),
        (90, 480, 213.858800, 0.252811),
        (36, 180, 83.260386, 0.040304),
        (16, 72, 36.603780, 0.003162),
        (90, 480, 213.913355, 0.293354),
        (36, 180, 83.293288, 0.052333),
        (16, 72, 36.627965, 0.007589),
        (90, 480, 213.965238, 0.185770),
        (36, 180, 83.308157, 0.021951),
        (16, 72, 36.630285, -0.002987),
        (90, 480, 220.007936, 0.173999),
        (36, 180, 85.671712, 0.022606),
        (16, 72, 37.692264, -0.001316),
        (90, 480, 216.574762, 0.252811),
        (36, 180, 84.299384, 0.040304),
        (16, 72, 37.050959, 0.003162),
        (90, 480, 216.644069, 0.293354),
        (36, 180, 84.341226, 0.052333),
        (16, 72, 37.081730, 0.007589),
        (90, 480, 216.710411, 0.185770),
        (36, 180, 84.360229, 0.021951),
        (16, 72, 37.084704, -0.002987),
        (90, 480, 223.460267, 0.128612),
        (36, 180, 87.004556, 0.012443),
        (16, 72, 38.276647, -0.001316),
        (90, 480, 219.291708, 0.194115),
        (36, 180, 85.338218, 0.027173),
        (16, 72, 37.497934, 0.003162),
        (90, 480, 219.376096, 0.230399),
        (36, 180, 85.389096, 0.038250),
        (16, 72, 37.535321, 0.007588),
        (90, 480, 219.456124, 0.135725),
        (36, 180, 85.412015, 0.010817),
        (16, 72, 37.538883, -0.002988),
        (90, 480, 226.913166, 0.154823),
        (36, 180, 88.338011, 0.018756),
        (16, 72, 38.861392, -0.001316),
        (90, 480, 222.008777, 0.228007),
        (36, 180, 86.377573, 0.035326),
        (16, 72, 37.945247, 0.003161),
        (90, 480, 222.107894, 0.266751),
        (36, 180, 86.437379, 0.046994),
        (16, 72, 37.989217, 0.007589),
        (90, 480, 222.202423, 0.164625),
        (36, 180, 86.464455, 0.017729),
        (16, 72, 37.993444, -0.002987),
        (90, 480, 230.369008, 0.173999),
        (36, 180, 89.671760, 0.022606),
        (16, 72, 39.446055, -0.001316),
        (90, 480, 224.729777, 0.252811),
        (36, 180, 87.417426, 0.040304),
        (16, 72, 38.392525, 0.003162),
        (90, 480, 224.844099, 0.293354),
        (36, 180, 87.486285, 0.052333),
        (16, 72, 38.443114, 0.007589),
        (90, 480, 224.951989, 0.185770),
        (36, 180, 87.517229, 0.021951),
        (16, 72, 38.447925, -0.002987),
        (90, 480, 233.820581, 0.142831),
        (36, 180, 91.004349, 0.015999),
        (16, 72, 40.030338, -0.001316),
        (90, 480, 227.445910, 0.212504),
        (36, 180, 88.455993, 0.031766),
        (16, 72, 38.839397, 0.003162),
        (90, 480, 227.575329, 0.250122),
        (36, 180, 88.533897, 0.043175),
        (16, 72, 38.896605, 0.007588),
        (90, 480, 227.696876, 0.151404),
        (36, 180, 88.568738, 0.014712),
        (16, 72, 38.901994, -0.002987),
        (270, 1380, 782.065862, 0.537034),
        (144, 720, 394.010045, 0.099446),
        (64, 288, 168.428763, -0.001251),
        (270, 1380, 752.766485, 0.793498),
        (144, 720, 378.973409, 0.181507),
        (64, 288, 161.895773, 0.021374),
        (270, 1380, 752.507123, 0.917277),
        (144, 720, 378.826611, 0.225430),
        (64, 288, 161.876845, 0.035747),
        (270, 1380, 753.643408, 0.625365),
        (144, 720, 379.432337, 0.123801),
        (64, 288, 162.133309, 0.003112),
        (99, 480, 226.777974, -1.350971),
        (42, 180, 93.904292, 1.000548),
        (25, 96, 55.611077, 2.337066),
        (99, 480, 226.701221, 0.241269),
        (42, 180, 93.890261, 2.267889),
        (25, 96, 55.606066, 2.770995),
        (99, 480, 226.672441, 0.771825),
        (42, 180, 93.885000, 2.646982),
        (25, 96, 55.604188, 2.926749),
        (99, 480, 226.720410, -0.133033),
        (42, 180, 93.893769, 1.987614),
        (25, 96, 55.607319, 2.664834),
        (357, 1920, 899.071932, -37.393681),
        (143, 720, 333.639046, 4.148133),
        (63, 288, 143.026153, 6.125139),
        (357, 1920, 899.071932, -37.393681),
        (143, 720, 333.639046, 4.148133),
        (63, 288, 143.026153, 6.125139),
        (357, 1920, 899.071932, -37.393681),
        (143, 720, 333.639046, 4.148133),
        (63, 288, 143.026153, 6.125139),
        (357, 1920, 899.071932, -37.393681),
        (143, 720, 333.639046, 4.148133),
        (63, 288, 143.026153, 6.125139),
    ];

    /// A plan without a `shape` builds the mesh it did before shapes had parameters.
    #[test]
    fn default_shapes_build_as_before() {
        let anatomy = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let mut expected = BEFORE.iter();
        for (i, part) in cases().iter().enumerate() {
            for level in [LodLevel::Hero, LodLevel::Near, LodLevel::Far] {
                let mesh = hard_part_mesh(i, part, &anatomy, level).unwrap();
                let (verts, tris, pos, norm) = fingerprint(&mesh);
                let &(ev, et, ep, en) = expected.next().unwrap();
                assert_eq!((verts, tris), (ev, et), "case {i} {level:?}: {:?}", part.params);
                assert!((pos - ep).abs() < 1e-6 && (norm - en).abs() < 1e-6, "case {i} {level:?}: {pos} {norm} vs {ep} {en} ({:?})", part.params);
            }
        }
    }

    /// Ridges and rings reshape the section and refine the mesh to follow; the base-radius
    /// fraction, the taper and the bend heading move the vertices without changing the count.
    #[test]
    fn shapes_change_the_mesh() {
        let anatomy = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let horn = |shape: HardShape, twist: f32| {
            let params = crate::anatomy::HardParams { kind: HardKind::Horn, size: 0.5, curve: 0.3, twist, shape, ..Default::default() };
            let part = HardPart { bone: BoneId(3), local: Transform::IDENTITY, params, region: crate::anatomy::RegionId(0), part: 0, side: Side::Center, mesh: None, mirror: false };
            hard_part_mesh(0, &part, &anatomy, LodLevel::Hero).unwrap()
        };
        let plain = horn(HardShape::default(), 0.0);
        let spiral = horn(HardShape { ridges: 2, ridge_depth: 0.13, ..HardShape::default() }, 4.5);
        assert!(spiral.positions.len() > 4 * plain.positions.len(), "{} vs {}", spiral.positions.len(), plain.positions.len());
        assert!(spiral.normals.iter().all(|n| n.is_finite() && (n.length() - 1.0).abs() < 1e-3));
        // Lobes: the section's radius varies round each ring (the base ring, about its centre;
        // the mesh is in model space, on its bone).
        let ring: Vec<Vec3> = spiral.positions.iter().take(20).copied().collect();
        let centre = ring.iter().sum::<Vec3>() / 20.0;
        let radii: Vec<f32> = ring.iter().map(|p| p.distance(centre)).collect();
        let (lo, hi) = radii.iter().fold((f32::MAX, f32::MIN), |(lo, hi), r| (lo.min(*r), hi.max(*r)));
        assert!(hi > lo * 1.2, "{lo} .. {hi}");
        let ringed = horn(HardShape { rings: 4, ring_depth: 0.1, ..HardShape::default() }, 0.0);
        assert!(ringed.positions.len() > plain.positions.len());
        for (shape, what) in [
            (HardShape { base: Some(0.4), ..HardShape::default() }, "base"),
            (HardShape { taper: 1.2, ..HardShape::default() }, "taper"),
            (HardShape { bend_deg: 90.0, ..HardShape::default() }, "bend"),
        ] {
            let shaped = horn(shape, 0.0);
            assert_eq!(shaped.positions.len(), plain.positions.len(), "{what}");
            assert!(shaped.positions.iter().zip(&plain.positions).any(|(p, q)| p.distance(*q) > 1e-3), "{what} changed nothing");
        }
        // The bend heading turns the tip: at 90 degrees it leans toward the part's +X, not +Z
        // (the mesh is in model space: back through its bone's rest transform).
        let bent = horn(HardShape { bend_deg: 90.0, ..HardShape::default() }, 0.0);
        let to_local = anatomy.skeleton.model_rest().get(3).copied().unwrap().inverse();
        let tip = |mesh: &BodyMeshData| to_local.transform_point3(mesh.positions.last().copied().unwrap());
        assert!(tip(&plain).z > 0.02 && tip(&plain).x.abs() < 1e-3, "{:?}", tip(&plain));
        assert!(tip(&bent).x > 0.02 && tip(&bent).z.abs() < 1e-3, "{:?}", tip(&bent));
    }
}
