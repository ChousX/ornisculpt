//! Inverse kinematics on joint positions (math only).

use bevy::prelude::*;

/// FABRIK: moves `joints` (root first, tip last) so the tip reaches `target` while the root stays
/// fixed and segment lengths are kept. Starting from a bent pose keeps the bend plane.
pub fn fabrik(joints: &mut [Vec3], target: Vec3, iterations: u32, tolerance: f32) {
    let lengths: Vec<f32> = joints.array_windows().map(|[a, b]| a.distance(*b)).collect();
    let Some(&root) = joints.first() else { return };
    if lengths.is_empty() {
        return;
    }
    let total: f32 = lengths.iter().sum();
    if root.distance(target) >= total {
        // Out of reach: straighten toward the target.
        place_from_root(joints, &lengths, Some((target - root).normalize_or(Vec3::NEG_Y)));
        return;
    }
    for _ in 0..iterations {
        place_from_tip(joints, &lengths, target);
        if let Some(first) = joints.first_mut() {
            *first = root;
        }
        place_from_root(joints, &lengths, None);
        if joints.last().is_some_and(|tip| tip.distance(target) < tolerance) {
            break;
        }
    }
}

/// FABRIK's forward pass: each joint after the first moves to its segment length from the one
/// before, along `dir` or else toward where it was.
fn place_from_root(joints: &mut [Vec3], lengths: &[f32], dir: Option<Vec3>) {
    let Some((first, rest)) = joints.split_first_mut() else { return };
    let mut prev = *first;
    for (joint, length) in rest.iter_mut().zip(lengths) {
        let d = dir.unwrap_or_else(|| (*joint - prev).normalize_or(Vec3::NEG_Y));
        *joint = prev + d * length;
        prev = *joint;
    }
}

/// FABRIK's backward pass: the tip goes to `target`, each joint before it to its segment length
/// from the next.
fn place_from_tip(joints: &mut [Vec3], lengths: &[f32], target: Vec3) {
    let Some((tip, rest)) = joints.split_last_mut() else { return };
    *tip = target;
    let mut next = target;
    for (joint, length) in rest.iter_mut().rev().zip(lengths.iter().rev()) {
        let d = (*joint - next).normalize_or(Vec3::Y);
        *joint = next + d * length;
        next = *joint;
    }
}

/// Fraction of full extension where [`two_bone_soft`] starts easing toward straight by default.
pub const SOFT_START: f32 = 0.95;

/// Exact IK for a two-bone chain (`joints` = root, middle, tip): the middle joint bends toward
/// `pole`. Bone lengths come from the current joints.
///
/// Soft near full extension: past `soft` of the reach (usually [`SOFT_START`]) the chain
/// approaches straight asymptotically (the tip falls slightly short of the target) instead of
/// locking. A hard lock snaps the knee, because near straight a tiny change in distance swings the
/// knee a long way. A leg that stands nearly straight (a bear's columns) is already past
/// [`SOFT_START`] at rest, so it would stand short of its own feet: start its easing just past its
/// rest reach instead.
pub fn two_bone_soft(joints: &mut [Vec3; 3], target: Vec3, pole: Vec3, soft: f32) {
    let (l1, l2) = (joints[0].distance(joints[1]), joints[1].distance(joints[2]));
    let root = joints[0];
    let to = target - root;
    let axis = to.normalize_or(Vec3::NEG_Y);
    let full = l1 + l2;
    let soft = full * soft.clamp(0.5, 0.995);
    let mut d = to.length();
    if d > soft {
        let span = full - soft;
        d = span.mul_add(1.0 - (-(d - soft) / span).exp(), soft);
    }
    let d = d.clamp((l1 - l2).abs() + 1e-4, full - 1e-4);
    let along = d.mul_add(d, f32::mul_add(l2, -l2, l1 * l1)) / (2.0 * d);
    let out = along.mul_add(-along, l1 * l1).max(0.0).sqrt();
    let side = (pole - axis * pole.dot(axis)).normalize_or(axis.any_orthonormal_vector());
    joints[1] = root + axis * along + side * out;
    joints[2] = root + axis * d;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaches_and_keeps_lengths() {
        let mut j = vec![Vec3::ZERO, Vec3::new(0.0, -0.5, 0.2), Vec3::new(0.0, -1.0, 0.0)];
        let target = Vec3::new(0.0, -0.8, 0.0);
        fabrik(&mut j, target, 20, 1e-4);
        assert!(j[2].distance(target) < 1e-3);
        assert!((j[0].distance(j[1]) - (0.29f32).sqrt()).abs() < 1e-4);
        assert_eq!(j[0], Vec3::ZERO);
        // Bend stays forward (+Z), as in the starting pose.
        assert!(j[1].z > 0.0);
    }

    #[test]
    fn out_of_reach_straightens() {
        let mut j = vec![Vec3::ZERO, Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, -2.0, 0.0)];
        fabrik(&mut j, Vec3::new(0.0, -5.0, 0.0), 10, 1e-4);
        assert!(j[2].distance(Vec3::new(0.0, -2.0, 0.0)) < 1e-5);
    }
}
