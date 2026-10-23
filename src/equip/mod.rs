//! Equipment: tools held in a mob's sockets.
//!
//! A tool is any entity the game spawns (a mesh, a scene, a physics body); the plugin only places
//! it. Insert [`HeldBy`] on it to put it in a socket of a mob; remove it to let go (the tool stays
//! where it was). Held tools are kept as root entities and their `Transform` is written in world
//! space every frame, so they survive tier changes, regeneration and the holder's despawn.
//!
//! - Hero tier: the socket follows the mob's joint entities (procedural pose, IK, levers).
//! - Crowd tiers: the socket bone's skin matrix is blended on the CPU from the same baked frames
//!   and weights the GPU blends, so the tool stays in the hand of the GPU-skinned body.
//!
//! Socket frames (see [`SocketDef`]): origin at the hold point, +Y along the held tool's main axis
//! (across the fist for grips, out of the skin for body sockets, forward out of the mouth). Author
//! tools with their main axis along +Y and set [`HeldBy::grip`] to where the hand holds them.

use bevy::math::Affine3A;
use bevy::prelude::*;
use bevy::transform::TransformSystems;

use crate::anatomy::{Anatomy, BoneId, Side, SocketDef, SocketKind};
use crate::num::Idx;
use crate::mob::{CrowdSkin, CrowdUploaded, GeneratedBody, MobBody, MobJoint, MobSkeleton};

/// Which socket of the holder a tool goes in.
#[derive(Reflect, Clone, Debug, PartialEq, Eq)]
pub enum SocketSel {
    /// The first socket of this kind on this side.
    Kind(SocketKind, Side),
    /// A socket by name (`"grip.r"`, `"back"`, `"hip.l"`).
    Named(String),
}

impl Default for SocketSel {
    fn default() -> Self {
        Self::grip(Side::Right)
    }
}

impl SocketSel {
    #[must_use]
    pub const fn grip(side: Side) -> Self {
        Self::Kind(SocketKind::Grip, side)
    }

    #[must_use]
    pub fn find<'a>(&self, sockets: &'a [SocketDef]) -> Option<&'a SocketDef> {
        match self {
            Self::Kind(kind, side) => sockets.iter().find(|s| s.kind == *kind && s.side == *side),
            Self::Named(name) => sockets.iter().find(|s| s.name == *name),
        }
    }
}

/// Holds this entity in a socket of `mob`.
#[derive(Component, Reflect, Clone, Debug)]
#[relationship(relationship_target = Holding)]
pub struct HeldBy {
    #[relationship]
    pub mob: Entity,
    pub socket: SocketSel,
    /// Where the tool is held, in the tool's own space (its frame is matched to the socket's).
    pub grip: Transform,
}

impl HeldBy {
    #[must_use]
    pub const fn new(mob: Entity, socket: SocketSel) -> Self {
        Self { mob, socket, grip: Transform::IDENTITY }
    }

    #[must_use]
    pub const fn with_grip(mut self, grip: Transform) -> Self {
        self.grip = grip;
        self
    }
}

/// The tools a mob holds.
#[derive(Component, Reflect, Debug, Default)]
#[relationship_target(relationship = HeldBy)]
pub struct Holding(Vec<Entity>);

/// Places held tools after the mob's pose is final for the frame.
pub struct EquipPlugin;

impl Plugin for EquipPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<HeldBy>()
            .register_type::<Holding>()
            .add_observer(take_in_hand)
            .add_systems(PostUpdate, place_held.before(TransformSystems::Propagate));
    }
}

/// Held tools are placed in world space, so they must not have a parent.
fn take_in_hand(ev: On<Add<HeldBy>>, mut commands: Commands, parented: Query<(), With<ChildOf>>) {
    if parented.contains(ev.entity) {
        commands.entity(ev.entity).remove::<ChildOf>();
    }
}

/// Model-space rest transform of `bone` (forward kinematics up its chain).
fn rest_model(anatomy: &Anatomy, bone: BoneId) -> Affine3A {
    let mut m = Affine3A::IDENTITY;
    let mut cur = Some(bone);
    while let Some(b) = cur {
        let Some(def) = anatomy.skeleton.get(b) else { break };
        m = def.rest.compute_affine() * m;
        cur = def.parent;
    }
    m
}

/// The socket bone's posed model-space transform for a crowd-tier mob: the GPU's blend of baked
/// skin matrices, applied to the bone's rest transform.
fn crowd_bone(body: &GeneratedBody, base: u32, up: &CrowdUploaded, bone: BoneId) -> Option<Affine3A> {
    let i = up.0;
    if i.bones == 0 {
        return None;
    }
    let crowd = u32::from(*body.anatomy.skeleton.crowd.map.get(bone.index())?);
    let mut skin = Mat4::ZERO;
    for (frame, w) in [(i.frame_a, i.weight_a), (i.frame_b, i.weight_b), (i.idle, i.weight_idle)] {
        if w != 0.0 {
            let m = body.motion.matrix(frame.checked_sub(base)?.idx() + crowd.idx())?;
            skin += Mat4::from_cols_array(m) * w;
        }
    }
    Some(Affine3A::from_mat4(skin * Mat4::from(rest_model(&body.anatomy, bone))))
}

type Holders<'a> = (&'a MobBody, Option<&'a MobSkeleton>, Option<&'a CrowdUploaded>);

/// Writes every held tool's world `Transform` from its holder's socket.
pub(crate) fn place_held(
    bodies: Res<Assets<GeneratedBody>>,
    skin: Res<CrowdSkin>,
    holders: Query<Holders, Without<HeldBy>>,
    nodes: Query<(&Transform, Option<&ChildOf>), Without<HeldBy>>,
    joints: Query<&Transform, (With<MobJoint>, Without<HeldBy>)>,
    mut tools: Query<(&HeldBy, &mut Transform)>,
) {
    // World transform of an entity from its local transforms (mobs may be parented by the game).
    let world = |mut e: Entity| -> Option<Affine3A> {
        let mut m = Affine3A::IDENTITY;
        loop {
            let (t, parent) = nodes.get(e).ok()?;
            m = t.compute_affine() * m;
            match parent {
                Some(p) => e = p.parent(),
                None => return Some(m),
            }
        }
    };
    tools.par_iter_mut().for_each(|(held, mut transform)| {
        let Ok((body_handle, skeleton, uploaded)) = holders.get(held.mob) else { return };
        let Some(body) = bodies.get(&body_handle.0) else { return };
        let anatomy = &body.anatomy;
        let Some(socket) = held.socket.find(&anatomy.sockets) else { return };
        let pose = match (skeleton, uploaded) {
            (Some(skeleton), _) => {
                let mut m = Affine3A::IDENTITY;
                let mut cur = Some(socket.bone);
                while let Some(b) = cur {
                    let Some(t) = skeleton.0.get(b.index()).and_then(|j| joints.get(*j).ok()) else { return };
                    m = t.compute_affine() * m;
                    cur = anatomy.skeleton.get(b).and_then(|def| def.parent);
                }
                m
            }
            (None, Some(up)) => {
                let Some(layout) = skin.layout(body_handle.0.id()) else { return };
                let Some(m) = crowd_bone(body, layout.base, up, socket.bone) else { return };
                m
            }
            (None, None) => return,
        };
        let Some(root) = world(held.mob) else { return };
        let m = root * pose * socket.local.compute_affine() * held.grip.compute_affine().inverse();
        let placed = Transform::from_matrix(Mat4::from(m));
        if *transform != placed {
            *transform = placed;
        }
    });
}
