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
