use std::collections::HashMap;

use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;

use super::generate::GenPending;
use super::crowd::{CrowdSkin, CrowdSlot, CrowdUploaded};
use super::{BodyFailed, BodyReady, CrowdCaster, GenCache, GenRequest, GeneratedBody, LiveBody, MobBody, MobTier};
use bevy::camera::visibility::{Layer, RenderLayers};
use bevy::light::{NotShadowCaster, NotShadowReceiver, WithLight};
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

/// A hero mob's soft-tissue joint: a child of its region's bone, moved by the region's spring
/// (`soft::SoftState::local`, index `region`).
#[cfg(feature = "soft-tissue")]
#[derive(Component, Clone, Copy, Debug)]
pub struct SoftJoint {
    pub mob: Entity,
    pub region: usize,
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

/// Spawns a body's meshes for one LOD level: one per material slot (with `extra` and `slot_extra`),
/// then its coat geometry (fur shells, feather and hair cards; with `extra`, never casting shadows).
fn spawn_level_meshes(
    commands: &mut Commands,
    mob: Entity,
    body: &GeneratedBody,
    level: LodLevel,
    extra: impl Bundle + Clone,
    slot_extra: impl Bundle + Clone,
) {
    let m = body.materials_for(level);
    for (slot, mesh) in &level.pick(&body.lods).0 {
        commands.spawn((MobVisual, LodMesh(level), Mesh3d(mesh.clone()), MeshMaterial3d(m.get(*slot)), extra.clone(), slot_extra.clone(), ChildOf(mob)));
    }
    let c = level.pick(&body.coat_meshes);
    for (mesh, material) in [(&c.shells, &m.shell), (&c.feathers, &m.feather), (&c.hair, &m.hair)] {
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
    pending: Query<(Entity, &GenPending, Option<&MobTier>, Option<&Children>, Option<&CrowdSlot>, Has<CrowdCaster>), Without<GenRequest>>,
    visuals: Query<(), Or<(With<MobVisual>, With<Placeholder>)>>,
    has_body: Query<(), With<MobBody>>,
    placeholders: Query<&ChildOf, With<Placeholder>>,
) {
    let Some(placeholder) = placeholder else {
        commands.init_resource::<PlaceholderAssets>();
        return;
    };
    let with_placeholder: Vec<Entity> = placeholders.iter().map(ChildOf::parent).collect();
    for (mob, pend, tier, children, slot, casting) in &pending {
        if let Some(error) = cache.lost_error(pend) {
            commands.entity(mob).remove::<GenPending>().trigger(move |entity| BodyFailed { entity, error });
            continue;
        }
        let Some(handle) = cache.ready(pend) else {
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
            // A mob already inside the shadow budget keeps casting through a regeneration;
            // one outside it gets `NotShadowCaster` here, and `tier::cast_shadows` takes that
            // off when it next crosses.
            let common = (MeshTag::new(slot.0), visibility);
            if casting {
                spawn_level_meshes(&mut commands, mob, body, level, common, CrowdCastable);
            } else {
                spawn_level_meshes(&mut commands, mob, body, level, common, (NotShadowCaster, CrowdCastable));
            }
        }
        commands
            .entity(mob)
            .remove::<(GenPending, MobSkeleton)>()
            .insert((MobBody(handle), slot, CrowdUploaded::default()))
            .trigger(|entity| BodyReady { entity });
    }
}

/// A crowd-tier body mesh, which the shadow budget may let cast.
///
/// See `tier::apply_shadow_budget`. Coat shells and cards never take it: they are alpha-tested,
/// and far dearer to cast than the silhouette is worth.
#[derive(Component, Clone, Default)]
pub struct CrowdCastable;

/// Marks hero-only visuals (joint roots and skinned hero meshes).
#[derive(Component, Clone, Default)]
pub struct HeroVisual;

/// The render layer a hero mob's shadow proxy lives on.
///
/// A hero's own meshes are `NotShadowCaster`: at 74 k triangles a hero costs 0.47 ms a frame in
/// the shadow passes on a Steam Deck, 95 % of what the whole tier costs, while the same mob's
/// `Near` geometry casts for 5 µs (`bench/LOG.md`, 2026-11-02). So the hero casts through a
/// proxy — the `Near` mesh, skinned by the mob's own joints so it follows the played pose — and
/// the proxy is kept off camera by this layer.
///
/// # Warning: this rests on behaviour Bevy does not document
///
/// Bevy has `NotShadowCaster` and `NotShadowReceiver` and nothing for "cast but do not draw".
/// The proxy is invisible because the camera's layers don't include this one, and it still casts
/// because `check_dir_light_mesh_visibility` (`bevy_light`) gathers casters from
/// `InheritedVisibility` and filters them by the *light's* layers, not the camera's.
/// `RenderLayers` documents cameras only, and the light types never mention it, so a Bevy
/// release could drop the interaction without calling it a breaking change. If hero mobs ever
/// lose their shadows after an engine update, look here first.
///
/// [`shadow_proxy_layers`] keeps every shadow-casting light on this layer, so apps need do
/// nothing. An app that sets `RenderLayers` on a light keeps whatever it chose, with this layer
/// added. A camera must *not* include this layer, or the proxy will be drawn over the mob.
///
/// The number is high to stay clear of the layers apps pick for themselves, and under 64 so the
/// mask stays one word.
pub const SHADOW_PROXY_LAYER: Layer = 61;

/// Marks a hero mob's shadow proxy (see [`SHADOW_PROXY_LAYER`]).
#[derive(Component, Clone, Default)]
pub struct ShadowProxy;

/// Keeps every shadow-casting light on [`SHADOW_PROXY_LAYER`], so that hero mobs' proxies cast
/// for it. Lights keep the layers they already had; this one is added.
pub(super) fn shadow_proxy_layers(mut commands: Commands, lights: Query<(Entity, Option<&RenderLayers>), WithLight>) {
    let proxy = RenderLayers::layer(SHADOW_PROXY_LAYER);
    for (light, layers) in &lights {
        let layers = layers.cloned().unwrap_or_default();
        if !layers.intersects(&proxy) {
            commands.entity(light).insert(layers.with(SHADOW_PROXY_LAYER));
        }
    }
}

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
            #[allow(unused_mut)]
            let mut skinned = joints.clone();
            // Soft joints follow the bones in the skin (the bind poses match, see `generate`).
            #[cfg(feature = "soft-tissue")]
            {
                let regions = &body.soft.hero;
                for (region, r) in regions.iter().enumerate() {
                    let Some(&bone) = joints.get(r.bone.index()) else { continue };
                    skinned.push(commands.spawn((Transform::IDENTITY, SoftJoint { mob, region }, ChildOf(bone))).id());
                }
                commands.entity(mob).insert(crate::soft::SoftState::new(regions.len()));
            }
            let skin = SkinnedMesh { inverse_bindposes: body.full_bindposes.clone(), joints: skinned };
            spawn_level_meshes(&mut commands, mob, body, LodLevel::Hero, (HeroVisual, skin), NotShadowCaster);
            // The shadow proxy in their place: the same mob at `Near` detail, skinned by its own
            // joints in crowd-skeleton order, so the shadow follows the pose being played rather
            // than the baked one a crowd instance would carry. Off camera, on for the lights
            // (see `SHADOW_PROXY_LAYER`, and the warning there).
            let crowd_joints: Vec<Entity> = skeleton.crowd.bones.iter().filter_map(|b| joints.get(b.index()).copied()).collect();
            commands.spawn((
                MobVisual,
                HeroVisual,
                ShadowProxy,
                Mesh3d(body.shadow_mesh.clone()),
                MeshMaterial3d(body.materials_for(LodLevel::Hero).skin.clone()),
                SkinnedMesh { inverse_bindposes: body.crowd_bindposes.clone(), joints: crowd_joints },
                RenderLayers::layer(SHADOW_PROXY_LAYER),
                NotShadowReceiver,
                ChildOf(mob),
            ));
            commands.entity(mob).insert(MobSkeleton(joints));
        } else if !hero && has_skeleton {
            for child in children.into_iter().flatten() {
                if hero_visuals.contains(*child) {
                    commands.entity(*child).despawn();
                }
            }
            commands.entity(mob).remove::<MobSkeleton>();
            #[cfg(feature = "soft-tissue")]
            commands.entity(mob).remove::<crate::soft::SoftState>();
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
