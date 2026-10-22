use bevy::math::Affine3A;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{RegionId, Side};

/// Hard cap on bones per mob at hero detail (fingers included).
pub const MAX_BONES: usize = 64;
/// Cap on bones in the crowd skeleton (finer parts folded into ancestors).
pub const MAX_CROWD_BONES: usize = 32;

/// Index into [`Skeleton::bones`].
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoneId(pub u8);

impl BoneId {
    #[must_use]
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Bones in topological order: a parent's index is always lower than its children's.
#[derive(Clone, Debug, Default)]
pub struct Skeleton {
    pub bones: Vec<BoneDef>,
    /// Model-space rest pose → bone space, one per bone.
    pub inverse_bind: Vec<Mat4>,
    /// Model-space rest transform of every bone, cached by [`Anatomy::finish_skeleton`](super::Anatomy::finish_skeleton).
    pub rest_model: Vec<Affine3A>,
    pub crowd: CrowdSkeleton,
}

#[derive(Clone, Debug)]
pub struct BoneDef {
    /// e.g. `"torso.2"`, `"limb.front.l.1"`.
    pub name: String,
    pub parent: Option<BoneId>,
    /// Local rest transform, relative to the parent bone.
    pub rest: Transform,
    /// Extends along local +Y.
    pub length: f32,
    pub region: RegionId,
    /// Index into [`Anatomy::parts`](super::Anatomy::parts).
    pub part: u16,
}

/// Mapping from the full skeleton to the reduced crowd skeleton. The crowd skeleton keeps the bones
/// of parts whose LOD is `Near` or coarser; all other bones fold into the nearest kept ancestor.
#[derive(Clone, Debug, Default)]
pub struct CrowdSkeleton {
    /// Full bone index → crowd bone index.
    pub map: Vec<u8>,
    /// Crowd bone index → full bone.
    pub bones: Vec<BoneId>,
}

impl Skeleton {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bones.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bones.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: BoneId) -> Option<&BoneDef> {
        self.bones.get(id.index())
    }

    #[must_use]
    pub fn find(&self, name: &str) -> Option<BoneId> {
        (0u8..).zip(&self.bones).find(|(_, b)| b.name == name).map(|(i, _)| BoneId(i))
    }

    pub fn children(&self, id: BoneId) -> impl Iterator<Item = BoneId> + '_ {
        (0u8..).zip(&self.bones).filter(move |(_, b)| b.parent == Some(id)).map(|(i, _)| BoneId(i))
    }

    /// The cached rest pose, or a fresh one if the bones changed since it was cached.
    #[must_use]
    pub fn rest_model(&self) -> std::borrow::Cow<'_, [Affine3A]> {
        if self.rest_model.len() == self.bones.len() { self.rest_model.as_slice().into() } else { self.model_rest().into() }
    }

    /// Model-space rest transform of every bone (forward kinematics over `rest`).
    #[must_use]
    pub fn model_rest(&self) -> Vec<Affine3A> {
        self.model_from(&[])
    }

    /// Model-space transform of every bone for the local pose `local` (forward kinematics); bones
    /// past the end of `local` keep their rest pose.
    #[must_use]
    pub fn model_from(&self, local: &[Transform]) -> Vec<Affine3A> {
        let mut out: Vec<Affine3A> = Vec::with_capacity(self.bones.len());
        for (i, b) in self.bones.iter().enumerate() {
            let l = local.get(i).unwrap_or(&b.rest).compute_affine();
            let parent = b.parent.and_then(|p| out.get(p.index()));
            out.push(parent.map_or(l, |m| *m * l));
        }
        out
    }

    /// Bones from `root` down to `tip`, inclusive. `None` if `tip` is not under `root`.
    #[must_use]
    pub fn chain(&self, root: BoneId, tip: BoneId) -> Option<Vec<BoneId>> {
        let mut chain = vec![tip];
        let mut cur = tip;
        while cur != root {
            cur = self.get(cur)?.parent?;
            chain.push(cur);
        }
        chain.reverse();
        Some(chain)
    }
}

/// How far a joint may turn, in degrees: what the motion stages (with the `joint-limits`
/// feature) clamp a pose to, and what an authored rest bend must lie within.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum JointLimit {
    /// Turns about one axis only (elbows, knees, hocks): the bone's rotation about its parent's
    /// local X or Z (see [`Flexes::axis`]). `flex` is the allowed flexion range, `0` = straight;
    /// negative is extension past straight (a dog's carpus stands a little over-extended).
    Hinge { flexes: Flexes, flex: (f32, f32) },
    /// Swings within a cone around its rest direction (hips, shoulders).
    Ball { cone: f32 },
}

impl JointLimit {
    /// `local` (a bone's rotation relative to its parent) brought within the limit; `rest` is the
    /// bone's rest rotation, which a ball's cone is centred on. Within the limit, `local` comes
    /// back unchanged.
    ///
    /// A hinge clamps only its turn about its axis and keeps any turn off it (an IK solve on a
    /// splayed leg leaves some), so it never fights the solve except to stop a joint folding the
    /// wrong way or past closed.
    #[must_use]
    pub fn clamp(self, rest: Quat, local: Quat) -> Quat {
        match self {
            Self::Hinge { flexes, flex: (lo, hi) } => {
                // Swing-twist about the axis: `local = swing * twist`.
                let local = if local.w < 0.0 { -local } else { local };
                let axis = flexes.axis();
                let along = local.xyz().dot(axis);
                let len = along.hypot(local.w);
                if len < 1e-6 {
                    return local;
                }
                let twist = Quat::from_axis_angle(axis, 2.0 * along.atan2(local.w));
                let flex = flexes.from_angle(2.0 * along.atan2(local.w).to_degrees());
                if (lo..=hi).contains(&flex) {
                    return local;
                }
                local * twist.inverse() * flexes.turn(flex.clamp(lo, hi))
            }
            Self::Ball { cone } => {
                let rel = rest.inverse() * local;
                let dir = rel * Vec3::Y;
                let angle = dir.angle_between(Vec3::Y).to_degrees();
                if angle <= cone {
                    return local;
                }
                // Keep the twist about the bone; pull its swing back to the cone's edge.
                let swing = Quat::from_rotation_arc(Vec3::Y, dir.normalize());
                let twist = swing.inverse() * rel;
                rest * Quat::IDENTITY.slerp(swing, cone / angle) * twist
            }
        }
    }
}

impl JointLimit {
    /// The limit for a reflected (right-side) copy of the joint. Reflecting across X keeps a
    /// rotation's X component and negates its Z, so a hinge about Z closes the other way.
    #[must_use]
    pub const fn mirrored(self) -> Self {
        match self {
            Self::Hinge { flexes, flex } => Self::Hinge { flexes: flexes.mirrored(), flex },
            Self::Ball { .. } => self,
        }
    }
}

/// Which way a hinge closes: where the bone below it swings as the joint flexes.
///
/// Seen in the limb's own frame: local +Z is forward, and local -X up for a leg splayed out
/// sideways, as an insect's is.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flexes {
    /// The bone below swings forward: elbows, hocks, a human elbow. About local X.
    Forward,
    /// The bone below swings back: knees (stifles), carpi, fetlocks. About local X.
    Back,
    /// The bone below swings up. About local Z.
    Up,
    /// The bone below swings down: an insect's femur-tibia joint, its knee up. About local Z.
    Down,
}

impl Flexes {
    /// The hinge's axis in its parent bone's frame.
    #[must_use]
    pub const fn axis(self) -> Vec3 {
        match self {
            Self::Forward | Self::Back => Vec3::X,
            Self::Up | Self::Down => Vec3::Z,
        }
    }

    /// Rotation about [`axis`](Self::axis), in degrees, for `flex` degrees of flexion.
    #[must_use]
    pub const fn to_angle(self, flex: f32) -> f32 {
        match self {
            Self::Forward | Self::Up => flex,
            Self::Back | Self::Down => -flex,
        }
    }

    /// Flexion for a rotation of `angle` degrees about [`axis`](Self::axis) (the inverse of
    /// [`to_angle`](Self::to_angle)).
    #[must_use]
    pub const fn from_angle(self, angle: f32) -> f32 {
        self.to_angle(angle)
    }

    /// The same hinge reflected across X (see [`JointLimit::mirrored`]).
    #[must_use]
    pub const fn mirrored(self) -> Self {
        match self {
            Self::Forward | Self::Back => self,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }

    /// The local rotation that flexes the hinge by `flex` degrees.
    #[must_use]
    pub fn turn(self, flex: f32) -> Quat {
        Quat::from_axis_angle(self.axis(), self.to_angle(flex).to_radians())
    }

    /// Where the joint itself points, in the parent bone's frame: away from the side the bone
    /// below swings to as it flexes (an elbow back, a knee forward, an insect's knee up).
    #[must_use]
    pub fn points(self) -> Vec3 {
        // A small positive turn about X swings +Y toward +Z; about Z, toward -X.
        let swings = match self.axis() {
            a if a == Vec3::X => Vec3::Z,
            _ => Vec3::NEG_X,
        };
        -swings * self.to_angle(1.0)
    }
}

/// Attachment point for tools (and later clothing anchors).
#[derive(Clone, Debug, PartialEq)]
pub struct SocketDef {
    /// e.g. `"grip.r"`, `"back"`.
    pub name: String,
    pub kind: SocketKind,
    pub side: Side,
    pub bone: BoneId,
    pub local: Transform,
    /// Characteristic size (palm width, back width) for tool scaling.
    pub size: f32,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SocketKind {
    /// Hands and pincers.
    Grip,
    Mouth,
    Head,
    Neck,
    Shoulder,
    Chest,
    Back,
    /// Riding position on the back.
    Saddle,
    Hip,
    Wrist,
    TailTip,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hinge_clamps_flexion_and_keeps_its_swing() {
        let knee = JointLimit::Hinge { flexes: Flexes::Back, flex: (-10.0, 140.0) };
        let insect = JointLimit::Hinge { flexes: Flexes::Down, flex: (-10.0, 170.0) };
        let z = |deg: f32| Quat::from_rotation_z(deg.to_radians());
        assert_eq!(insect.clamp(Quat::IDENTITY, z(-115.0)), z(-115.0));
        assert!(insect.clamp(Quat::IDENTITY, z(40.0)).angle_between(z(10.0)) < 1e-4);
        let x = |deg: f32| Quat::from_rotation_x(deg.to_radians());
        // Within range: untouched. Flexing back is a turn about -X.
        assert_eq!(knee.clamp(Quat::IDENTITY, x(-60.0)), x(-60.0));
        // Bent the elbow's way: held at its 10° of over-extension.
        assert!(knee.clamp(Quat::IDENTITY, x(50.0)).angle_between(x(10.0)) < 1e-4);
        // Past closed: held at 140°.
        assert!(knee.clamp(Quat::IDENTITY, x(-170.0)).angle_between(x(-140.0)) < 1e-4);
        // A turn off the hinge's axis survives the clamp.
        let off = Quat::from_rotation_z(0.2);
        let got = knee.clamp(Quat::IDENTITY, off * x(50.0));
        assert!(got.angle_between(off * x(10.0)) < 1e-4, "{got}");
    }

    #[test]
    fn ball_holds_its_cone_round_rest() {
        let hip = JointLimit::Ball { cone: 45.0 };
        let rest = Quat::from_rotation_x(0.5);
        assert_eq!(hip.clamp(rest, rest * Quat::from_rotation_z(0.5)), rest * Quat::from_rotation_z(0.5));
        let got = hip.clamp(rest, rest * Quat::from_rotation_z(1.5));
        let swing = ((rest.inverse() * got) * Vec3::Y).angle_between(Vec3::Y).to_degrees();
        assert!((swing - 45.0).abs() < 1e-2, "{swing}");
    }
}
