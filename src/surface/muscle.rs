//! Muscle flex: per-vertex data the vertex shader uses to bulge muscles as they shorten.
//!
//! A muscle stamp (a [`PrimLayer::Muscle`] displacement) has its origin on its own bone and its
//! insertion just across the joint nearest it, on the child bone. The shader measures how much
//! closer a skinned vertex has come to the posed insertion than at rest, as a share of the
//! muscle's length: that is the contraction, and the vertex moves out along its normal by
//! `amplitude · contraction` (in, a little, when stretched). The skeleton's joint matrices are
//! all it needs at run time.

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;

use super::body::primitive_parts;
use super::{BodyMeshData, MaterialSlot, MuscleFlex};
use crate::anatomy::{Anatomy, BoneId, LodLevel, PrimLayer, PrimOp, PrimShape, Skeleton};
use crate::num::{Idx, min_axis_scale, smoothstep};

/// Outward push per unit of contraction, as a multiple of the muscle's rest height (plus a share of
/// its thickness, so that flat limb muscles still read when they flex).
const GAIN: f32 = 30.0;
/// Share of the stamp's thickness (`radii.z`) added to its height for the flex amplitude.
const THICKNESS: f32 = 0.25;
/// How far along the child bone the insertion sits (fraction of its length).
const INSERTION_ALONG: f32 = 0.3;
/// Flex fades in between these distances from the spanned joint (multiples of the belly centre's
/// distance from it): next to the joint the skin creases as it bends instead.
const CREASE: (f32, f32) = (0.35, 0.85);
/// A belly's flex reaches this far past its stamp (multiple of its radii), so that it covers
/// enough of the mesh to bulge smoothly.
const SPREAD: f32 = 1.6;
/// Smoothing passes over the per-vertex amplitudes.
const SMOOTHING: usize = 12;
/// At full flex a belly swells at most this share of its long radius. Without it the gain, set
/// for slim limb muscles, swells a broad flat one (a glute, a pectoral) many times its own
/// thickness: a walker's buttock ballooned out at each push-off.
const MAX_SWELL: f32 = 0.4;
/// Contraction at full flex: the shader clamps it there (`muscle_offset` in `coat/deform.wesl`).
const FULL_FLEX: f32 = 0.3;

/// One muscle belly, in the rest pose.
struct Belly {
    to_local: Affine3A,
    radii: Vec3A,
    /// Amplitude at the belly's centre.
    amplitude: f32,
    insertion: Vec3,
    /// Belly centre to insertion (rest pose).
    length: f32,
    /// The spanned joint (head of the child bone) and the belly centre's distance from it.
    head: Vec3,
    reach: f32,
    /// Joint carrying the insertion, as the mesh at this level indexes joints.
    joint: u16,
}

/// Muscle flex data for every vertex of `mesh` (rest pose, model space) at `level`.
///
/// Vertices outside the skin, or over no muscle, get a zero amplitude. Joint indices follow the
/// mesh: the crowd skeleton below `Hero`.
#[must_use]
pub fn muscle_flex(mesh: &BodyMeshData, anatomy: &Anatomy, level: LodLevel) -> Vec<MuscleFlex> {
    let bellies = bellies(anatomy, level);
    let mut out = vec![MuscleFlex::default(); mesh.positions.len()];
    if bellies.is_empty() {
        return out;
    }
    let skin = mesh.skin_vertex_mask();
    for ((flex, p), _) in out.iter_mut().zip(&mesh.positions).zip(&skin).filter(|(_, s)| **s) {
        // The strongest belly under the vertex drives it, less the runner-up: where two bellies
        // overlap the flex fades to nothing at the switch (a groove between the muscles) rather
        // than jumping from one insertion to the other.
        let (mut best, mut second) = (None::<(&Belly, f32)>, 0.0f32);
        for b in &bellies {
            let s = 1.0 - (b.to_local.transform_point3a(Vec3A::from(*p)) / (b.radii * SPREAD)).length_squared();
            if s <= 0.0 {
                continue;
            }
            // Fade out towards the joint, where the skin creases rather than bulges.
            let fade = smoothstep(CREASE.0 * b.reach, CREASE.1 * b.reach, p.distance(b.head));
            let a = b.amplitude * s * s * fade;
            match best {
                Some((_, top)) if a <= top => second = second.max(a),
                _ => {
                    second = best.map_or(second, |(_, top)| top);
                    best = Some((b, a));
                }
            }
        }
        if let Some((b, a)) = best {
            *flex = MuscleFlex { insertion: b.insertion, amplitude: a - second, length: b.length, joint: b.joint };
        }
    }
    smooth_amplitudes(&mut out, mesh);
    out
}

/// Relaxes the amplitudes over the skin's edges ([`SMOOTHING`] passes): bellies are small next to
/// the mesh's coarser triangles, and a lone vertex pushed out reads as a spike.
fn smooth_amplitudes(flex: &mut [MuscleFlex], mesh: &BodyMeshData) {
    let edges: Vec<(usize, usize)> = mesh
        .slot_triangles(MaterialSlot::Skin)
        .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
        .map(|(a, b)| (a.idx(), b.idx()))
        .collect();
    for _ in 0..SMOOTHING {
        // (amplitude sum, count, strongest neighbour)
        let mut sum: Vec<(f32, f32, Option<MuscleFlex>)> = flex.iter().map(|f| (f.amplitude, 1.0, None)).collect();
        for &(a, b) in &edges {
            let (Some(&fa), Some(&fb)) = (flex.get(a), flex.get(b)) else { continue };
            for (i, other) in [(a, fb), (b, fa)] {
                if let Some(s) = sum.get_mut(i) {
                    s.0 += other.amplitude;
                    s.1 += 1.0;
                    if other.length > 0.0 && s.2.is_none_or(|n| other.amplitude > n.amplitude) {
                        s.2 = Some(other);
                    }
                }
            }
        }
        for (f, (total, n, strongest)) in flex.iter_mut().zip(sum) {
            if f.length > 0.0 {
                f.amplitude = total / n;
            } else if let Some(m) = strongest {
                // A vertex just past a belly takes its neighbour's muscle, so that the bulge
                // spreads over the skin instead of stopping dead at the stamp's edge.
                *f = MuscleFlex { amplitude: total / n, ..m };
            }
        }
    }
}

/// The muscle stamps present at `level`, each with the joint it spans.
fn bellies(anatomy: &Anatomy, level: LodLevel) -> Vec<Belly> {
    let skeleton = &anatomy.skeleton;
    let rest = skeleton.rest_model();
    let parts = primitive_parts(anatomy);
    anatomy
        .primitives
        .iter()
        .zip(&parts)
        .filter(|(p, part)| p.layer == PrimLayer::Muscle && p.lod >= level && anatomy.parts.get(part.idx()).is_some_and(|q| q.lod >= level))
        .filter_map(|(p, _)| {
            let (PrimOp::Displace { height }, PrimShape::Ellipsoid { radii }) = (p.op, &p.shape) else { return None };
            if height <= 0.0 {
                return None;
            }
            let model = *rest.get(p.bone.index())? * p.local.compute_affine();
            let scale = min_axis_scale(&model);
            let centre = Vec3::from(model.translation);
            let normal = Vec3::from(model.matrix3.z_axis).normalize_or_zero();
            // The joint spanned: the child bone whose head is nearest the belly.
            let (joint, head) = skeleton
                .children(p.bone)
                .filter_map(|c| Some((c, Vec3::from(rest.get(c.index())?.translation))))
                .min_by(|a, b| a.1.distance_squared(centre).total_cmp(&b.1.distance_squared(centre)))?;
            let child = rest.get(joint.index())?;
            let along = Vec3::from(child.matrix3.y_axis).normalize_or_zero() * skeleton.get(joint)?.length * INSERTION_ALONG;
            // On the muscle's side of the limb, so that bending towards it shortens it.
            let lift = normal * (centre - head).dot(normal).max(0.0);
            let insertion = head + along + lift;
            Some(Belly {
                to_local: model.inverse(),
                radii: Vec3A::from(*radii),
                amplitude: (GAIN * scale * THICKNESS.mul_add(radii.z, height)).min(MAX_SWELL * scale * radii.x.max(radii.y) / FULL_FLEX),
                insertion,
                length: centre.distance(insertion).max(skeleton.get(joint)?.length * 0.5).max(1e-3),
                head,
                reach: centre.distance(head).max(1e-3),
                joint: mesh_joint(skeleton, level, p.bone, joint)?,
            })
        })
        .collect()
}

/// The joint index a mesh at `level` uses for `joint` (the crowd skeleton below `Hero`), or `None`
/// when the crowd skeleton folds the joint into the muscle's own bone: nothing bends across it.
fn mesh_joint(skeleton: &Skeleton, level: LodLevel, bone: BoneId, joint: BoneId) -> Option<u16> {
    if level == LodLevel::Hero {
        return Some(u16::from(joint.0));
    }
    let map = &skeleton.crowd.map;
    let (own, across) = (*map.get(bone.index())?, *map.get(joint.index())?);
    (own != across).then_some(u16::from(across))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::build_src;
    use crate::surface::{AdaptiveContour, SurfaceSettings, mesh_lod};

    fn quadruped() -> Anatomy {
        build_src(include_str!("../../assets/mobs/quadruped.body.ron"))
    }

    #[test]
    fn muscles_span_a_joint() {
        let a = quadruped();
        let found = bellies(&a, LodLevel::Hero);
        assert!(found.len() > 4, "only {} muscle bellies", found.len());
        for b in &found {
            assert!(b.amplitude > 0.0);
            let joint = BoneId(u8::try_from(b.joint).unwrap());
            assert!(a.skeleton.get(joint).is_some_and(|j| j.parent.is_some()));
        }
        assert!(bellies(&a, LodLevel::Far).is_empty(), "muscle stamps are Near and finer");
    }

    #[test]
    fn crowd_levels_flex_about_crowd_joints() {
        let a = quadruped();
        let settings = SurfaceSettings::default();
        let mesh = mesh_lod(&AdaptiveContour, &a, LodLevel::Near, settings.get(LodLevel::Near), 0).unwrap();
        let flex = muscle_flex(&mesh, &a, LodLevel::Near);
        assert_eq!(flex.len(), mesh.positions.len());
        let crowd = &a.skeleton.crowd.bones;
        let flexed: Vec<_> = flex.iter().filter(|f| f.amplitude > 0.0).collect();
        assert!(!flexed.is_empty(), "no muscle flexes at Near");
        for f in flexed {
            assert!(f.joint.idx() < crowd.len(), "joint {} is not a crowd bone", f.joint);
        }
        // Off the skin (hard parts, sheets) nothing flexes.
        let skin = mesh.skin_vertex_mask();
        assert!(flex.iter().zip(&skin).all(|(f, s)| *s || f.amplitude == 0.0));
        // Far keeps no muscle stamps: its mesh gets no flex at all.
        let far = mesh_lod(&AdaptiveContour, &a, LodLevel::Far, settings.get(LodLevel::Far), 0).unwrap();
        assert!(muscle_flex(&far, &a, LodLevel::Far).iter().all(|f| f.amplitude == 0.0));
    }

    #[test]
    fn amplitudes_have_no_spikes() {
        let a = quadruped();
        let settings = SurfaceSettings::default();
        let mesh = mesh_lod(&AdaptiveContour, &a, LodLevel::Near, settings.get(LodLevel::Near), 0).unwrap();
        let flex = muscle_flex(&mesh, &a, LodLevel::Hero);
        let peak = flex.iter().map(|f| f.amplitude).fold(0.0, f32::max);
        assert!(peak > 0.0);
        // Along any skin edge the amplitude changes by well under the peak: no lone vertex sticks out.
        let jump = mesh
            .slot_triangles(MaterialSlot::Skin)
            .flat_map(|t| [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])])
            .map(|(i, j)| (flex[i.idx()].amplitude - flex[j.idx()].amplitude).abs())
            .fold(0.0, f32::max);
        assert!(jump < peak * 0.35, "edge jump {jump} against peak {peak}");
    }

    #[test]
    fn bending_shortens_a_muscle_on_the_inside() {
        let a = quadruped();
        let settings = SurfaceSettings::default();
        let mesh = mesh_lod(&AdaptiveContour, &a, LodLevel::Near, settings.get(LodLevel::Near), 0).unwrap();
        let flex = muscle_flex(&mesh, &a, LodLevel::Hero);
        let flexed = flex.iter().filter(|f| f.amplitude > 0.0).count();
        assert!(flexed > mesh.positions.len() / 50, "{flexed} of {} vertices flex", mesh.positions.len());
        // Bend every muscle's joint by 60° about its rest x axis: some muscles shorten, some stretch.
        let rest = a.skeleton.rest_model();
        let (mut shorter, mut longer) = (0, 0);
        for (f, p) in flex.iter().zip(&mesh.positions).filter(|(f, _)| f.amplitude > 0.0) {
            let joint = rest[f.joint.idx()];
            let bend = joint * Affine3A::from_rotation_x(60f32.to_radians()) * joint.inverse();
            let (before, after) = (p.distance(f.insertion), p.distance(bend.transform_point3(f.insertion)));
            if after < before * 0.98 {
                shorter += 1;
            } else if after > before * 1.02 {
                longer += 1;
            }
        }
        assert!(shorter > 0 && longer > 0, "{shorter} shorter, {longer} longer");
    }
}
