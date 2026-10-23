use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::prelude::*;

use super::{BodyMeshData, MaterialSlot, MobVertexData, Submesh};
use crate::num::{Clamp, Saturate, ToF32};
use crate::anatomy::{Anatomy, HardKind, HardPart, LodLevel, PrimLayer};

/// Generates meshes for hard parts (claws, horns, teeth, beaks, eyes). Output is 100% weighted
/// to the part's bone, in model space at rest pose, using [`MaterialSlot::Hard`] or `Eye`.
pub trait HardPartGen: Send + Sync {
    /// `None` if this generator can't make the kind (e.g. `HardKind::Mesh`, which needs the
    /// asset server and is resolved by the Bevy-side pipeline).
    fn generate(&self, part: &HardPart, anatomy: &Anatomy, level: LodLevel) -> Option<BodyMeshData>;
}

/// Built-in procedural shapes: curved tapered horns (claws, horns, tusks, fangs, spikes, teeth),
/// branching horns (antlers), flattened horns (beaks, nails), spheres (eyes).
/// Tessellation drops with `level`.
#[derive(Default, Clone, Copy)]
pub struct ProceduralHardParts;

/// Shape recipe for one tapered, curved horn in hard-part local space (+Y out, bends toward +Z).
struct Horn {
    length: f32,
    base_radius: f32,
    /// Bend over the length in quarter turns.
    curve: f32,
    /// Spiral turns of the bend plane.
    twist: f32,
    /// Cross-section scale (x, z).
    flatten: Vec2,
}

impl HardPartGen for ProceduralHardParts {
    fn generate(&self, part: &HardPart, anatomy: &Anatomy, level: LodLevel) -> Option<BodyMeshData> {
        let p = &part.params;
        let s = p.size;
        let (rings, sides) = match level {
            LodLevel::Hero => (8, 10),
            LodLevel::Near => (5, 6),
            LodLevel::Far => (3, 4),
        };
        let horn = |length: f32, base: f32, flatten: Vec2| Horn {
            length,
            base_radius: base,
            curve: p.curve,
            twist: p.twist,
            flatten,
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
                let (length, base, flatten) = horn_dims(&p.kind, s)?;
                let beam = horn(length, base, flatten);
                horn_mesh(&mut mesh, &beam, Transform::IDENTITY, rings, sides);
                let tines = (*tines).max(1);
                for i in 0..tines {
                    let t = (f32::from(i) + 1.0) / (f32::from(tines) + 1.0);
                    let at = horn_frame(&beam, t);
                    let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                    let tine = Transform { rotation: at.rotation * Quat::from_rotation_z(side * 0.9), ..at };
                    horn_mesh(&mut mesh, &horn(s * 0.4 * t.mul_add(-0.5, 1.0), s * 0.05, Vec2::ONE), tine, rings.min(5), sides);
                }
                MaterialSlot::Hard
            }
            kind => {
                let (length, base, flatten) = horn_dims(kind, s)?;
                horn_mesh(&mut mesh, &horn(length, base, flatten), Transform::IDENTITY, rings, sides)
            }
        };

        let model = *anatomy.skeleton.model_rest().get(part.bone.index())? * part.local.compute_affine();
        let data = MobVertexData {
            region: part.region,
            cavity: 0,
            curvature: 128,
            limb_t: 255,
            primary_bone: part.bone,
            layer: PrimLayer::Base,
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
}

/// Length, base radius and cross-section scale (x, z) of a horn-like part of size `s` (an antler's
/// beam); `None` for eyes, plates and authored meshes. The SDF preview stands in with the same.
pub(super) fn horn_dims(kind: &HardKind, s: f32) -> Option<(f32, f32, Vec2)> {
    Some(match kind {
        HardKind::Claw => (s, s * 0.25, Vec2::new(0.7, 1.0)),
        HardKind::Nail => (s * 0.4, s * 0.5, Vec2::new(1.0, 0.25)),
        HardKind::Horn => (s, s * 0.22, Vec2::ONE),
        HardKind::Tusk => (s, s * 0.15, Vec2::ONE),
        HardKind::Tooth => (s, s * 0.35, Vec2::new(1.0, 0.6)),
        HardKind::Fang => (s, s * 0.22, Vec2::new(1.0, 0.8)),
        HardKind::Spike => (s, s * 0.3, Vec2::ONE),
        HardKind::Beak => (s, s * 0.45, Vec2::new(1.0, 0.7)),
        HardKind::Antler { .. } => (s, s * 0.08, Vec2::ONE),
        HardKind::Eye | HardKind::Plate | HardKind::Mesh { .. } => return None,
    })
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
    let bend_axis = Quat::from_rotation_y(h.twist * TAU * u) * Vec3::X;
    Quat::from_axis_angle(bend_axis, h.curve * FRAC_PI_2 * u)
}

/// Appends a tapered tube with a closed tip, placed by `at`. Returns the material slot.
fn horn_mesh(mesh: &mut BodyMeshData, h: &Horn, at: Transform, rings: u32, sides: u32) -> MaterialSlot {
    let base = mesh.positions.len().clamp_u32();
    for ri in 0..=rings {
        let t = ri.to_f32() / rings.to_f32();
        let frame = at * horn_frame(h, t);
        let radius = h.base_radius * (1.0 - t).powf(0.8);
        for si in 0..sides {
            let angle = si.to_f32() / sides.to_f32() * TAU;
            let local = Vec3::new(angle.cos() * h.flatten.x, 0.0, angle.sin() * h.flatten.y);
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
