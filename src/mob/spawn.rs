use std::collections::HashMap;

use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;

use super::generate::{GenPending, lost_error, ready_body};
use super::crowd::{CrowdSkin, CrowdSlot, CrowdUploaded};
use super::{BodyFailed, BodyReady, GenCache, GenRequest, GeneratedBody, LiveBody, MobBody, MobTier};
use bevy::light::NotShadowCaster;
use bevy::mesh::MeshTag;
use crate::anatomy::{BoneId, LodLevel, PartKind, PartPath};

/// A joint entity of a mob's skeleton (child of its parent joint, roots are children of the mob).
#[derive(Component, Clone, Copy, Debug)]
pub struct MobJoint {
    /// The mob this joint belongs to.
    pub mob: Entity,
    pub bone: BoneId,
    pub rest: Transform,
}

/// Every entity spawned to display a body (joints, meshes); replaced on regeneration.
#[derive(Component, Default)]
pub struct MobVisual;

/// A mesh entity showing one LOD level of a mob.
#[derive(Component, Clone, Copy, Debug)]
pub struct LodMesh(pub LodLevel);

/// Shown while a mob's first body is generating.
#[derive(Component)]
pub struct Placeholder;

/// Joint entities of a mob, indexed by `BoneId`.
#[derive(Component, Clone, Debug, Default)]
#[require(crate::motion::MobPose, crate::motion::CyclePose)]
pub struct MobSkeleton(pub Vec<Entity>);

/// One anatomical part of a mob. Editing `kind` updates the mob's [`LiveBody`] and regenerates.
#[derive(Component, Clone, Debug)]
pub struct Part {
    pub path: PartPath,
    pub kind: PartKind,
}

/// Coarsest level this part exists at; part entities only exist while the mob's tier shows them.
#[derive(Component, Clone, Copy, Debug)]
pub struct PartLod(pub LodLevel);

#[derive(Component, Clone, Copy, Debug)]
#[relationship(relationship_target = MobParts)]
pub struct PartOf(pub Entity);

#[derive(Component, Clone, Debug, Default)]
#[relationship_target(relationship = PartOf, linked_spawn)]
pub struct MobParts(Vec<Entity>);

/// Placeholder visuals shown while a mob's first body generates.
#[derive(Resource)]
pub(super) struct PlaceholderAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

impl FromWorld for PlaceholderAssets {
    fn from_world(world: &mut World) -> Self {
        let material = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial { base_color: Color::srgb(0.62, 0.45, 0.33), ..default() });
        let mesh = world.resource_mut::<Assets<Mesh>>().add(Capsule3d::new(0.15, 0.4));
        Self { mesh, material }
    }
}

/// Spawns a body's coat geometry (fur shells, feather cards) for one LOD level.
fn spawn_coat_meshes(commands: &mut Commands, mob: Entity, body: &GeneratedBody, level: LodLevel, extra: impl Bundle + Clone) {
    let m = body.materials_for(level);
    let c = level.pick(&body.coat_meshes);
    for (mesh, material) in [(&c.shells, &m.shell), (&c.feathers, &m.feather)] {
        if let Some(mesh) = mesh {
            commands.spawn((
                MobVisual,
                LodMesh(level),
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                NotShadowCaster,
                extra.clone(),
                ChildOf(mob),
            ));
        }
    }
}

/// Attaches finished bodies to waiting mobs: crowd meshes (near/far) are spawned now; hero
/// visuals (joints + skinned meshes) are managed by [`sync_hero_visuals`]. Shows placeholders for
/// mobs still waiting.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn attach_body(
    mut commands: Commands,
    cache: Res<GenCache>,
    bodies: Res<Assets<GeneratedBody>>,
    placeholder: Option<Res<PlaceholderAssets>>,
    mut crowd_skin: ResMut<CrowdSkin>,
    pending: Query<(Entity, &GenPending, Option<&MobTier>, Option<&Children>, Option<&CrowdSlot>), Without<GenRequest>>,
    visuals: Query<(), Or<(With<MobVisual>, With<Placeholder>)>>,
    has_body: Query<(), With<MobBody>>,
    placeholders: Query<&ChildOf, With<Placeholder>>,
) {
    let Some(placeholder) = placeholder else {
        commands.init_resource::<PlaceholderAssets>();
        return;
    };
    let with_placeholder: Vec<Entity> = placeholders.iter().map(ChildOf::parent).collect();
    for (mob, pend, tier, children, slot) in &pending {
        if let Some(error) = lost_error(&cache, pend) {
            commands.entity(mob).remove::<GenPending>().trigger(move |entity| BodyFailed { entity, error });
            continue;
        }
        let Some(handle) = ready_body(&cache, pend) else {
            if !has_body.contains(mob) && !with_placeholder.contains(&mob) {
                commands.spawn((
                    Placeholder,
                    Mesh3d(placeholder.mesh.clone()),
                    MeshMaterial3d(placeholder.material.clone()),
                    Transform::from_xyz(0.0, 0.35, 0.0).with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                    ChildOf(mob),
                ));
            }
            continue;
        };
        let Some(body) = bodies.get(&handle) else { continue };

        // Replace previous visuals (hero visuals are rebuilt by `sync_hero_visuals`).
        for child in children.into_iter().flatten() {
            if visuals.contains(*child) {
                commands.entity(*child).despawn();
            }
        }
        let slot = slot.copied().unwrap_or_else(|| CrowdSlot(crowd_skin.alloc()));
        let shown = tier.map_or(LodLevel::Far, |t| t.0);
        for level in [LodLevel::Near, LodLevel::Far] {
            let visibility = if level == shown { Visibility::Inherited } else { Visibility::Hidden };
            for (mat_slot, mesh) in &level.pick(&body.lods).0 {
                commands.spawn((
                    MobVisual,
                    LodMesh(level),
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(body.materials_for(level).get(*mat_slot)),
                    MeshTag::new(slot.0),
                    NotShadowCaster,
                    visibility,
                    ChildOf(mob),
                ));
            }
            spawn_coat_meshes(&mut commands, mob, body, level, (MeshTag::new(slot.0), visibility));
        }
        commands
            .entity(mob)
            .remove::<(GenPending, MobSkeleton)>()
            .insert((MobBody(handle), slot, CrowdUploaded::default()))
            .trigger(|entity| BodyReady { entity });
    }
}

/// Marks hero-only visuals (joint roots and skinned hero meshes).
#[derive(Component, Clone, Default)]
pub struct HeroVisual;

/// Spawns joints + skinned hero meshes for mobs at `Hero` tier and removes them below it.
#[allow(clippy::type_complexity)]
pub(super) fn sync_hero_visuals(
    mut commands: Commands,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<(Entity, &MobBody, &MobTier, Has<MobSkeleton>, Option<&Children>), Or<(Changed<MobTier>, Changed<MobBody>)>>,
    hero_visuals: Query<(), With<HeroVisual>>,
) {
    for (mob, body, tier, has_skeleton, children) in &mobs {
        let hero = tier.0 == LodLevel::Hero;
        if hero && !has_skeleton {
            let Some(body) = bodies.get(&body.0) else { continue };
            let skeleton = &body.anatomy.skeleton;
            let mut joints: Vec<Entity> = Vec::with_capacity(skeleton.len());
            for (id, bone) in (0u8..).map(BoneId).zip(&skeleton.bones) {
                let parent = bone.parent.and_then(|p| joints.get(p.index())).copied().unwrap_or(mob);
                let mut e = commands.spawn((bone.rest, MobJoint { mob, bone: id, rest: bone.rest }, ChildOf(parent)));
                if bone.parent.is_none() {
                    e.insert((MobVisual, HeroVisual));
                }
                joints.push(e.id());
            }
            let skin = SkinnedMesh { inverse_bindposes: body.full_bindposes.clone(), joints: joints.clone() };
            for (slot, mesh) in &LodLevel::Hero.pick(&body.lods).0 {
                commands.spawn((
                    MobVisual,
                    HeroVisual,
                    LodMesh(LodLevel::Hero),
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(body.materials_for(LodLevel::Hero).get(*slot)),
                    skin.clone(),
                    ChildOf(mob),
                ));
            }
            spawn_coat_meshes(&mut commands, mob, body, LodLevel::Hero, (HeroVisual, skin));
            commands.entity(mob).insert(MobSkeleton(joints));
        } else if !hero && has_skeleton {
            for child in children.into_iter().flatten() {
                if hero_visuals.contains(*child) {
                    commands.entity(*child).despawn();
                }
            }
            commands.entity(mob).remove::<MobSkeleton>();
        }
    }
}

/// Keeps part entities in step with the mob's tier: parts exist while their LOD is shown.
#[allow(clippy::type_complexity)]
pub(super) fn sync_part_entities(
    mut commands: Commands,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<(Entity, &MobBody, &MobTier, &LiveBody, Option<&MobParts>), Or<(Changed<MobTier>, Changed<MobBody>)>>,
    parts: Query<&Part>,
) {
    for (mob, body, tier, live, existing) in &mobs {
        let Some(body) = bodies.get(&body.0) else { continue };
        let mut have: HashMap<PartPath, Entity> = existing
            .into_iter()
            .flat_map(RelationshipTarget::iter)
            .filter_map(|e| parts.get(e).ok().map(|p| (p.path.clone(), e)))
            .collect();
        for built in body.anatomy.parts.iter().filter(|p| p.lod >= tier.0) {
            if have.remove(&built.path).is_some() {
                continue;
            }
            let Some(node) = live.0.node(&built.path) else { continue };
            commands.spawn((Part { path: built.path.clone(), kind: node.kind.clone() }, PartLod(built.lod), PartOf(mob)));
        }
        for (_, stale) in have {
            commands.entity(stale).despawn();
        }
    }
}

/// Writes gameplay edits of [`Part`] components back into [`LiveBody`] and requests regeneration.
pub(super) fn sync_part_edits(mut commands: Commands, parts: Query<(Ref<Part>, &PartOf)>, mut mobs: Query<&mut LiveBody>) {
    for (part, owner) in &parts {
        if part.is_added() || !part.is_changed() {
            continue;
        }
        let Ok(mut live) = mobs.get_mut(owner.0) else { continue };
        if let Some(node) = live.0.node_mut(&part.path) {
            node.kind = part.kind.clone();
            commands.entity(owner.0).insert(GenRequest);
        }
    }
}
