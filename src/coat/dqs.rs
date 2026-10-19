//! Dual quaternion skinning, with scale blended linearly before the rigid blend.
//!
//! After Kavan, Collins, Žára & O'Sullivan, "Geometric Skinning with Approximate Dual Quaternion
//! Blending", 2008: each joint's stretch is blended linearly before the rigid blend. Linear blend skinning averages matrices, which
//! shrinks the limb at bent and twisted joints (the "candy wrapper"); blending the rigid parts as
//! dual quaternions keeps its volume.
//!
//! The shader (`coat_vertex.wesl`) does this per vertex; this is the reference it is tested
//! against, written the same way.

use bevy::math::{Mat3, Quat, Vec3, Vec4};

/// A joint's skin matrix (`world · inverse bind`) split for blending: rotation, translation,
/// and the stretch left over (`Rᵀ · linear`, the identity for a rigid joint).
#[derive(Clone, Copy, Debug)]
pub struct JointSplit {
    pub rotation: Quat,
    pub translation: Vec3,
    pub stretch: Mat3,
}

impl JointSplit {
    #[must_use]
    pub fn new(linear: Mat3, translation: Vec3) -> Self {
        // Gram–Schmidt on the columns: the nearest rotation for a rotation · scale.
        let c0 = linear.x_axis.normalize_or(Vec3::X);
        let c1 = (linear.y_axis - c0 * linear.y_axis.dot(c0)).normalize_or(Vec3::Y);
        let r = Mat3::from_cols(c0, c1, c0.cross(c1));
        Self { rotation: Quat::from_mat3(&r).normalize(), translation, stretch: r.transpose() * linear }
    }
}

fn mul(a: Vec4, b: Vec4) -> Vec4 {
    let (av, bv) = (a.truncate(), b.truncate());
    (bv * a.w + av * b.w + av.cross(bv)).extend(a.w.mul_add(b.w, -av.dot(bv)))
}

/// Skins a point and a normal by up to four joints.
#[must_use]
pub fn skin(joints: &[JointSplit], weights: &[f32], p: Vec3, n: Vec3) -> (Vec3, Vec3) {
    let Some(first) = joints.first().map(|j| Vec4::from(j.rotation)) else { return (p, n) };
    let (mut real, mut dual, mut stretch) = (Vec4::ZERO, Vec4::ZERO, Mat3::ZERO);
    for (j, &w) in joints.iter().zip(weights) {
        let mut q = Vec4::from(j.rotation);
        // Same hemisphere as the first joint, or the blend takes the long way round.
        let sign = if q.dot(first) < 0.0 { -1.0 } else { 1.0 };
        q *= sign;
        let d = mul(j.translation.extend(0.0), q) * 0.5;
        real += q * w;
        dual += d * w;
        stretch += j.stretch * w;
    }
    let len = real.length().max(1e-8);
    let (real, dual) = (real / len, dual / len);
    let conj = Vec4::new(-real.x, -real.y, -real.z, real.w);
    let t = mul(dual, conj).truncate() * 2.0;
    let rot = Quat::from_vec4(real);
    let p = rot * (stretch * p) + t;
    let n = rot * (stretch.inverse().transpose() * n).normalize_or(n);
    (p, n)
}

/// Optimized-centre skinning (Le & Hodgins 2016): the blended rotation turns the vertex about its
/// centre of rotation `cor`, which linear blending carries: `R·S·(p − cor) + LBS(cor)`.
#[must_use]
pub fn skin_cor(joints: &[(Mat3, Vec3)], weights: &[f32], p: Vec3, n: Vec3, cor: Vec3) -> (Vec3, Vec3) {
    let splits: Vec<JointSplit> = joints.iter().map(|(l, t)| JointSplit::new(*l, *t)).collect();
    let Some(first) = splits.first().map(|j| Vec4::from(j.rotation)) else { return (p, n) };
    let (mut q, mut stretch) = (Vec4::ZERO, Mat3::ZERO);
    for (j, &w) in splits.iter().zip(weights) {
        let r = Vec4::from(j.rotation);
        q += if r.dot(first) < 0.0 { -r } else { r } * w;
        stretch += j.stretch * w;
    }
    let rot = Quat::from_vec4(q.normalize());
    let centre = skin_linear(joints, weights, cor);
    (rot * (stretch * (p - cor)) + centre, rot * (stretch.inverse().transpose() * n).normalize_or(n))
}

/// Linear blend skinning, for comparison.
#[must_use]
pub fn skin_linear(joints: &[(Mat3, Vec3)], weights: &[f32], p: Vec3) -> Vec3 {
    joints.iter().zip(weights).map(|((l, t), w)| (*l * p + *t) * *w).sum()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    /// A twist of 90° split evenly between two joints: linear blending pulls the surface toward
    /// the axis (to cos 45° ≈ 0.71 of its radius); dual quaternions keep it.
    #[test]
    fn twist_keeps_volume() {
        let twist = Mat3::from_rotation_y(FRAC_PI_2);
        let joints = [JointSplit::new(Mat3::IDENTITY, Vec3::ZERO), JointSplit::new(twist, Vec3::ZERO)];
        let p = Vec3::new(1.0, 0.0, 0.0);
        let (dq, _) = skin(&joints, &[0.5, 0.5], p, Vec3::X);
        let lbs = skin_linear(&[(Mat3::IDENTITY, Vec3::ZERO), (twist, Vec3::ZERO)], &[0.5, 0.5], p);
        assert!((dq.length() - 1.0).abs() < 1e-4, "dual quaternion radius {}", dq.length());
        assert!((lbs.length() - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4, "linear radius {}", lbs.length());
    }

    /// One joint: exactly its matrix, translation and scale included.
    #[test]
    fn single_joint_is_exact() {
        let linear = Mat3::from_rotation_z(0.7) * Mat3::from_diagonal(Vec3::new(1.5, 0.8, 1.2));
        let t = Vec3::new(0.3, -2.0, 1.0);
        let p = Vec3::new(0.2, 0.5, -0.4);
        let (dq, _) = skin(&[JointSplit::new(linear, t)], &[1.0], p, Vec3::Y);
        assert!(dq.distance(linear * p + t) < 1e-5, "{dq} vs {}", linear * p + t);
    }

    /// Joints rotated by `q` and `-q` (the same rotation) blend to it, not to a collapse.
    #[test]
    fn antipodal_quaternions_agree() {
        let a = JointSplit::new(Mat3::from_rotation_x(0.4), Vec3::ZERO);
        let mut b = a;
        b.rotation = -b.rotation;
        let p = Vec3::new(0.0, 1.0, 0.0);
        let (dq, _) = skin(&[a, b], &[0.5, 0.5], p, Vec3::Y);
        assert!(dq.distance(Mat3::from_rotation_x(0.4) * p) < 1e-5);
    }

    /// Bending 90° about a joint at the origin, a vertex weighted half and half with its centre at
    /// the joint keeps its distance from it; linear blending pulls it in.
    #[test]
    fn centre_of_rotation_keeps_bends_round() {
        let bend = Mat3::from_rotation_z(FRAC_PI_2);
        let joints = [(Mat3::IDENTITY, Vec3::ZERO), (bend, Vec3::ZERO)];
        let p = Vec3::new(0.3, 0.0, 0.0);
        let (c, _) = skin_cor(&joints, &[0.5, 0.5], p, Vec3::X, Vec3::ZERO);
        let lbs = skin_linear(&joints, &[0.5, 0.5], p);
        assert!((c.length() - 0.3).abs() < 1e-5, "{}", c.length());
        assert!(lbs.length() < 0.25);
    }

    /// One joint: exact whatever the centre.
    #[test]
    fn centre_of_rotation_single_joint_is_exact() {
        let linear = Mat3::from_rotation_x(0.9) * Mat3::from_diagonal(Vec3::new(1.2, 1.0, 0.7));
        let t = Vec3::new(1.0, 2.0, -0.5);
        let p = Vec3::new(0.1, -0.3, 0.4);
        let (c, _) = skin_cor(&[(linear, t)], &[1.0], p, Vec3::Y, Vec3::new(5.0, -3.0, 2.0));
        assert!(c.distance(linear * p + t) < 1e-4);
    }

    /// The rest pose (identity skin matrices) leaves vertices where they are.
    #[test]
    fn rest_pose_is_identity() {
        let id = JointSplit::new(Mat3::IDENTITY, Vec3::ZERO);
        let p = Vec3::new(0.3, 1.2, -0.7);
        let (dq, n) = skin(&[id; 4], &[0.4, 0.3, 0.2, 0.1], p, Vec3::Z);
        assert!(dq.distance(p) < 1e-6 && n.distance(Vec3::Z) < 1e-6);
    }
}
