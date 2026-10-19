//! Runtime levers: things the game drives. The plugin never decides what they mean (age, injury,
//! condition); it only applies them.

use bevy::prelude::*;
use smallvec::SmallVec;

use super::spawn::{MobJoint, MobSkeleton};
use super::{GenRequest, GeneratedBody, LiveBody, MobBody};
use crate::anatomy::{BodyPlan, BoneId, PartNode, PartPath};

/// Per-bone proportions, applied every frame without regenerating.
///
/// `length` stretches the bone along its axis (moving its children), `girth` widens it.
/// Children divide out their parent's scale so it doesn't compound down a chain; this is exact
/// for children aligned with their parent and approximate for strongly bent ones.
#[derive(Component, Clone, Debug, Default)]
pub struct BoneScales(pub SmallVec<[BoneScale; 8]>);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoneScale {
    pub bone: BoneId,
    pub length: f32,
    pub girth: f32,
}

impl BoneScales {
    fn get(&self, bone: BoneId) -> Vec3 {
        self.0
            .iter()
            .find(|s| s.bone == bone)
            .map_or(Vec3::ONE, |s| Vec3::new(s.girth, s.length, s.girth))
    }
}

/// Removes a part (and everything attached to it) from a mob's [`LiveBody`], then regenerates.
#[derive(EntityEvent, Clone, Debug)]
pub struct SeverPart {
    /// The mob.
    pub entity: Entity,
    pub path: PartPath,
}

/// Adds `node` as a child of the part at `parent` in a mob's [`LiveBody`], then regenerates.
/// The node's symmetry is expanded first, so `mirror: true` grafts a pair.
#[derive(EntityEvent, Clone, Debug)]
pub struct GraftPart {
    /// The mob.
    pub entity: Entity,
    pub parent: PartPath,
    pub node: PartNode,
}

pub(super) fn sever_part(ev: On<SeverPart>, mut commands: Commands, mut mobs: Query<&mut LiveBody>) {
    let Ok(mut live) = mobs.get_mut(ev.entity) else { return };
    let (Some(parent), Some(name)) = (ev.path.parent(), ev.path.segments().last()) else {
        warn!("sever: cannot remove the root part");
        return;
    };
    let Some(node) = live.0.node_mut(&parent) else {
        warn!("sever: no part at {}", ev.path);
        return;
    };
    let before = node.children.len();
    node.children.retain(|c| c.name != name);
    if node.children.len() != before {
        commands.entity(ev.entity).insert(GenRequest);
    }
}

pub(super) fn graft_part(ev: On<GraftPart>, mut commands: Commands, mut mobs: Query<&mut LiveBody>) {
    let Ok(mut live) = mobs.get_mut(ev.entity) else { return };
    let Some(parent) = live.0.node_mut(&ev.parent) else {
        warn!("graft: no part at {}", ev.parent);
        return;
    };
    // Expand the node's own symmetry by wrapping it under a throwaway root.
    let root = PartNode { name: "_".into(), children: vec![ev.node.clone()], mirror: false, repeat: default(), ..ev.node.clone() };
    let copies = BodyPlan { root, tone: default() }.expand_symmetry().root.children;
    parent.children.extend(copies);
    commands.entity(ev.entity).insert(GenRequest);
}

/// Applies [`BoneScales`] to joint scales. Translation/rotation are left to animation.
#[allow(clippy::type_complexity)]
pub(super) fn apply_bone_scales(
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<(&BoneScales, &MobSkeleton, &MobBody), Or<(Changed<BoneScales>, Changed<MobSkeleton>)>>,
    mut joints: Query<(&MobJoint, &mut Transform)>,
) {
    for (scales, skeleton, body) in &mobs {
        let Some(body) = bodies.get(&body.0) else { continue };
        let bones = &body.anatomy.skeleton.bones;
        for &entity in &skeleton.0 {
            let Ok((joint, mut tf)) = joints.get_mut(entity) else { continue };
            let parent = bones.get(joint.bone.index()).and_then(|b| b.parent).map_or(Vec3::ONE, |p| scales.get(p));
            tf.scale = scales.get(joint.bone) / parent;
        }
    }
}
