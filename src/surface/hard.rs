use std::f32::consts::{FRAC_PI_2, TAU};

use bevy::prelude::*;

use super::{BodyMeshData, MaterialSlot, MobVertexData, Submesh};
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
                mesh.indices = src.indices.clone();
                if part.mirror {
                    // Reflection flips handedness: restore triangle winding.
                    mesh.indices.chunks_exact_mut(3).for_each(|t| t.swap(1, 2));
                }
                MaterialSlot::Hard
            }
            HardKind::Eye => {
                sphere(&mut mesh, s, rings.max(4), sides);
                MaterialSlot::Eye
            }
            HardKind::Plate => {
                // A dome flattened onto the skin: wide across (x), long along (z), thin outward (y).
                sphere(&mut mesh, 1.0, rings.max(4), sides);
                let scale = Vec3::new(s * 0.45, s * (0.06 + 0.12 * p.curve.clamp(0.0, 1.0)), s * 0.5);
                for (v, n) in mesh.positions.iter_mut().zip(&mut mesh.normals) {
                    *v *= scale;
                    *n = (*n / scale).normalize_or(Vec3::Y);
                }
                MaterialSlot::Hard
            }
            HardKind::Claw => horn_mesh(&mut mesh, &horn(s, s * 0.25, Vec2::new(0.7, 1.0)), Transform::IDENTITY, rings, sides),
            HardKind::Nail => horn_mesh(&mut mesh, &horn(s * 0.4, s * 0.5, Vec2::new(1.0, 0.25)), Transform::IDENTITY, rings, sides),
            HardKind::Horn => horn_mesh(&mut mesh, &horn(s, s * 0.22, Vec2::ONE), Transform::IDENTITY, rings, sides),
            HardKind::Tusk => horn_mesh(&mut mesh, &horn(s, s * 0.15, Vec2::ONE), Transform::IDENTITY, rings, sides),
            HardKind::Tooth => horn_mesh(&mut mesh, &horn(s, s * 0.35, Vec2::new(1.0, 0.6)), Transform::IDENTITY, rings, sides),
            HardKind::Fang => horn_mesh(&mut mesh, &horn(s, s * 0.22, Vec2::new(1.0, 0.8)), Transform::IDENTITY, rings, sides),
            HardKind::Spike => horn_mesh(&mut mesh, &horn(s, s * 0.3, Vec2::ONE), Transform::IDENTITY, rings, sides),
            HardKind::Beak => horn_mesh(&mut mesh, &horn(s, s * 0.45, Vec2::new(1.0, 0.7)), Transform::IDENTITY, rings, sides),
            HardKind::Antler { tines } => {
                let beam = horn(s, s * 0.08, Vec2::ONE);
                horn_mesh(&mut mesh, &beam, Transform::IDENTITY, rings, sides);
                let tines = (*tines).max(1);
                for i in 0..tines {
                    let t = (i + 1) as f32 / (tines + 1) as f32;
                    let at = horn_frame(&beam, t);
                    let side = if i % 2 == 0 { 1.0 } else { -1.0 };
                    let tine = Transform { rotation: at.rotation * Quat::from_rotation_z(side * 0.9), ..at };
                    horn_mesh(&mut mesh, &horn(s * 0.4 * (1.0 - t * 0.5), s * 0.05, Vec2::ONE), tine, rings.min(5), sides);
                }
                MaterialSlot::Hard
            }
        };

        let model = anatomy.skeleton.model_rest()[part.bone.index()] * part.local.compute_affine();
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
        mesh.joints = vec![[part.bone.0 as u16, 0, 0, 0]; n];
        mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; n];
        mesh.mob_data = vec![data; n];
        mesh.submeshes = vec![Submesh { indices: 0..mesh.indices.len() as u32, slot }];
        Some(mesh)
    }
}

/// Frame on the horn's centreline at `t` (Y along the horn).
fn horn_frame(h: &Horn, t: f32) -> Transform {
    // March along the curve: direction rotates from +Y toward the (twisting) bend direction.
    const STEPS: usize = 16;
    let mut pos = Vec3::ZERO;
    let steps = ((STEPS as f32 * t).ceil() as usize).max(1);
    let dt = t / steps as f32;
    let mut rot = Quat::IDENTITY;
    for k in 0..steps {
        let u = (k as f32 + 0.5) * dt;
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
fn horn_mesh(mesh: &mut BodyMeshData, h: &Horn, at: Transform, rings: usize, sides: usize) -> MaterialSlot {
    let base = mesh.positions.len() as u32;
    for r in 0..=rings {
        let t = r as f32 / rings as f32;
        let f = at * horn_frame(h, t);
        let radius = h.base_radius * (1.0 - t).powf(0.8);
        for s in 0..sides {
            let a = s as f32 / sides as f32 * TAU;
            let local = Vec3::new(a.cos() * h.flatten.x, 0.0, a.sin() * h.flatten.y);
            mesh.positions.push(f.transform_point(local * radius));
            mesh.normals.push((f.rotation * local).normalize_or(Vec3::Y));
        }
    }
    let ring = |r: usize, s: usize| base + (r * sides + s % sides) as u32;
    for r in 0..rings {
        for s in 0..sides {
            let (a, b, c, d) = (ring(r, s), ring(r, s + 1), ring(r + 1, s), ring(r + 1, s + 1));
            mesh.indices.extend([a, c, b, b, c, d]);
        }
    }
    MaterialSlot::Hard
}

fn sphere(mesh: &mut BodyMeshData, r: f32, rings: usize, sides: usize) {
    let base = mesh.positions.len() as u32;
    for i in 0..=rings {
        let phi = i as f32 / rings as f32 * std::f32::consts::PI;
        for j in 0..=sides {
            let theta = j as f32 / sides as f32 * TAU;
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.positions.push(n * r);
            mesh.normals.push(n);
        }
    }
    let w = sides as u32 + 1;
    for i in 0..rings as u32 {
        for j in 0..sides as u32 {
            let a = base + i * w + j;
            mesh.indices.extend([a, a + 1, a + w, a + 1, a + w + 1, a + w]);
        }
    }
}
