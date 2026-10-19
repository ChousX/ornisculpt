//! The pose bodies are meshed in: arms raised clear of the body and fingers fanned, so nothing
//! that moves apart touches while the surface is built (an A-pose, not a T-pose: hanging arms
//! skinned from a T-pose collapse at the armpit). The animation rest pose is unchanged; the mesh
//! is skinned back into it afterwards (see `surface::BodyField`).

use bevy::math::Affine3A;
use bevy::prelude::*;

use super::{Anatomy, BoneId, LimbRole};
use crate::num::Idx;

/// How far the meshing pose spreads the body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spread {
    /// Arms hanging nearer than this to straight down are raised outward to it, degrees.
    pub arms: f32,
    /// Each finger turns this much further from the hand's middle, degrees.
    pub fingers: f32,
}

impl Default for Spread {
    fn default() -> Self {
        Self { arms: 45.0, fingers: 10.0 }
    }
}

/// Local transform of every bone in the meshing pose.
pub fn spread_pose(a: &Anatomy, spread: &Spread) -> Vec<Transform> {
    let bones = &a.skeleton.bones;
    let mut local: Vec<Transform> = bones.iter().map(|b| b.rest).collect();
    let model = |local: &[Transform]| a.skeleton.model_from(local);
    let rot = |m: &Affine3A| Quat::from_mat3a(&m.matrix3).normalize();
    // A model-space rotation about a bone's own position.
    let turn = |local: &mut [Transform], world: &[Affine3A], b: BoneId, q: Quat| {
        let parent = bones.get(b.index()).and_then(|def| def.parent).and_then(|p| world.get(p.index())).map_or(Quat::IDENTITY, rot);
        let Some(l) = local.get_mut(b.index()) else { return };
        l.rotation = (parent.inverse() * q * parent * l.rotation).normalize();
    };

    // Arms: raise any that hang near straight down out to the side.
    let world = model(&local);
    let arms: Vec<usize> = a.parts.iter().enumerate().filter(|(_, p)| matches!(p.role, Some(LimbRole::Manipulator | LimbRole::Both))).map(|(i, _)| i).collect();
    for &pi in &arms {
        let Some(p) = a.parts.get(pi) else { continue };
        if p.bones.start >= p.bones.end {
            continue;
        }
        // The arm's shoulder joint (past its clavicle, if it has one).
        let shoulder = a.clavicles.iter().find(|c| c.bone.0 == p.bones.start).map_or(BoneId(p.bones.start), |c| c.arm);
        let Some(m) = world.get(shoulder.index()) else { continue };
        let side = m.translation.x;
        if side.abs() < 1e-3 {
            continue;
        }
        let dir = (m.matrix3 * Vec3A::Y).normalize_or(Vec3A::NEG_Y);
        let from_down = dir.angle_between(Vec3A::NEG_Y);
        let want = spread.arms.to_radians();
        if from_down >= want {
            continue;
        }
        let out = Vec3A::X * side.signum();
        let axis = dir.cross(out).normalize_or(Vec3A::Z * side.signum());
        turn(&mut local, &world, shoulder, Quat::from_axis_angle(axis.into(), want - from_down));
    }

    // Fingers: fan each away from the hand's middle direction.
    let world = model(&local);
    for p in &a.parts {
        if !p.parent.is_some_and(|q| arms.contains(&q.idx())) {
            continue;
        }
        let range = p.bones.clone();
        let roots: Vec<BoneId> =
            range.clone().map(BoneId).filter(|b| bones.get(b.index()).is_some_and(|def| def.parent.is_none_or(|q| !range.contains(&q.0)) && def.name.starts_with("digit"))).collect();
        if roots.len() < 2 {
            continue;
        }
        let dirs: Vec<Vec3A> = roots.iter().map(|b| world.get(b.index()).map_or(Vec3A::NEG_Y, |m| (m.matrix3 * Vec3A::Y).normalize_or(Vec3A::NEG_Y))).collect();
        let middle = dirs.iter().copied().sum::<Vec3A>().normalize_or(Vec3A::NEG_Y);
        for (b, d) in roots.iter().zip(&dirs) {
            let axis = middle.cross(*d);
            if axis.length_squared() < 1e-6 {
                continue;
            }
            turn(&mut local, &world, *b, Quat::from_axis_angle(axis.normalize().into(), spread.fingers.to_radians()));
        }
    }
    local
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};

    fn body(src: &str) -> Anatomy {
        let plan: BodyPlan = ron::from_str(src).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    fn model(a: &Anatomy, local: &[Transform]) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::new();
        for (i, b) in a.skeleton.bones.iter().enumerate() {
            let l = local[i].compute_affine();
            out.push(b.parent.map_or(l, |p| out[p.index()] * l));
        }
        out
    }

    #[test]
    fn arms_rise_to_an_a_pose_and_legs_stay() {
        let a = body(include_str!("../../assets/mobs/biped.body.ron"));
        let spread = model(&a, &spread_pose(&a, &Spread::default()));
        let rest = a.skeleton.model_rest();
        for p in a.parts.iter().filter(|p| p.role == Some(LimbRole::Manipulator)) {
            let root = a.clavicles.iter().find(|c| c.bone.0 == p.bones.start).map_or_else(|| usize::from(p.bones.start), |c| c.arm.index());
            let angle = (spread[root].matrix3 * Vec3A::Y).angle_between(Vec3A::NEG_Y).to_degrees();
            let out = (spread[root].matrix3 * Vec3A::Y).x * rest[root].translation.x.signum();
            assert!(angle > 44.0 && out > 0.0, "{}: {angle:.1}° from down, outward {out:.2}", p.path.0);
        }
        for l in &a.legs {
            let hip = l.chain[0].index();
            assert!(spread[hip].abs_diff_eq(rest[hip], 1e-5), "legs keep their rest pose");
        }
    }

    #[test]
    fn bodies_without_arms_mesh_at_rest() {
        let a = body(include_str!("../../assets/mobs/quadruped.body.ron"));
        let pose = spread_pose(&a, &Spread::default());
        assert!(a.skeleton.bones.iter().zip(&pose).all(|(b, p)| b.rest == *p));
    }
}
