//! Inverse kinematics on joint positions (math only).

use bevy::prelude::*;

/// FABRIK: moves `joints` (root first, tip last) so the tip reaches `target` while the root stays
/// fixed and segment lengths are kept. Starting from a bent pose keeps the bend plane.
pub fn fabrik(joints: &mut [Vec3], target: Vec3, iterations: u32, tolerance: f32) {
    let n = joints.len();
    if n < 2 {
        return;
    }
    let lengths: Vec<f32> = joints.windows(2).map(|w| w[0].distance(w[1])).collect();
    let total: f32 = lengths.iter().sum();
    let root = joints[0];
    if root.distance(target) >= total {
        // Out of reach: straighten toward the target.
        let dir = (target - root).normalize_or(Vec3::NEG_Y);
        for i in 1..n {
            joints[i] = joints[i - 1] + dir * lengths[i - 1];
        }
        return;
    }
    for _ in 0..iterations {
        joints[n - 1] = target;
        for i in (0..n - 1).rev() {
            let dir = (joints[i] - joints[i + 1]).normalize_or(Vec3::Y);
            joints[i] = joints[i + 1] + dir * lengths[i];
        }
        joints[0] = root;
        for i in 1..n {
            let dir = (joints[i] - joints[i - 1]).normalize_or(Vec3::NEG_Y);
            joints[i] = joints[i - 1] + dir * lengths[i - 1];
        }
        if joints[n - 1].distance(target) < tolerance {
            break;
        }
    }
}

/// Fraction of full extension where [`two_bone`] starts easing toward straight.
const SOFT_START: f32 = 0.95;

/// Exact IK for a two-bone chain (`joints` = root, middle, tip): the middle joint bends toward
/// `pole`. Bone lengths come from the current joints.
///
/// Soft near full extension: past [`SOFT_START`] of the reach the chain approaches straight
/// asymptotically (the tip falls slightly short of the target) instead of locking. A hard lock
/// snaps the knee, because near straight a tiny change in distance swings the knee a long way.
pub fn two_bone(joints: &mut [Vec3; 3], target: Vec3, pole: Vec3) {
    let (l1, l2) = (joints[0].distance(joints[1]), joints[1].distance(joints[2]));
    let root = joints[0];
    let to = target - root;
    let axis = to.normalize_or(Vec3::NEG_Y);
    let full = l1 + l2;
    let soft = full * SOFT_START;
    let mut d = to.length();
    if d > soft {
        let span = full - soft;
        d = soft + span * (1.0 - (-(d - soft) / span).exp());
    }
    let d = d.clamp((l1 - l2).abs() + 1e-4, full - 1e-4);
    let along = (l1 * l1 - l2 * l2 + d * d) / (2.0 * d);
    let out = (l1 * l1 - along * along).max(0.0).sqrt();
    let side = (pole - axis * pole.dot(axis)).normalize_or(axis.any_orthonormal_vector());
    joints[1] = root + axis * along + side * out;
    joints[2] = root + axis * d;
}

/// Turns the interior joints about the root→tip line so the chain bends toward `pole` (knees
/// forward, elbows back), keeping the shape of the chain; the first interior joint sets the side.
/// Without this a leg whose solve starts near-straight can pick either side from one frame to
/// the next, and the knee snaps across. A straight chain is left alone.
pub fn orient_to_pole(joints: &mut [Vec3], pole: Vec3) {
    let n = joints.len();
    if n < 3 {
        return;
    }
    let (root, tip) = (joints[0], joints[n - 1]);
    let axis = (tip - root).normalize_or_zero();
    let want = (pole - axis * pole.dot(axis)).normalize_or_zero();
    if axis == Vec3::ZERO || want == Vec3::ZERO {
        return;
    }
    let perpendicular = |p: Vec3| {
        let d = p - root;
        d - axis * d.dot(axis)
    };
    // The first joint (knee/elbow) sets the side; later joints (a hock bending back) ride along.
    let bend = perpendicular(joints[1]);
    let length: f32 = joints.windows(2).map(|w| w[0].distance(w[1])).sum();
    if bend.length() < length * 1e-3 {
        return;
    }
    let bend = bend.normalize();
    let rot = if bend.dot(want) < -0.9999 { Quat::from_axis_angle(axis, std::f32::consts::PI) } else { Quat::from_rotation_arc(bend, want) };
    for j in &mut joints[1..n - 1] {
        *j = root + rot * (*j - root);
    }
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
