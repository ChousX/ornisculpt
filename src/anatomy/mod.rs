//! Anatomy: authored body plans, individual form, derived regions, and the resolved [`Anatomy`].
//!
//! Data flow: [`BodyPlan`] + [`Form`] → [`apply_form`] → [`build_anatomy`] → [`Anatomy`].

// Stub phase: bodies are `todo!()`, so parameters are intentionally unused.
#![allow(unused_variables, dead_code)]

mod build;
mod builder;
mod external;
mod form;
mod lever;
mod loader;
mod lod;
mod part;
mod parts;
mod path;
mod plan;
mod primitive;
mod region;
mod rig;
mod skeleton;
mod tone;
mod viability;

pub use build::{Anatomy, BuiltPart, build_anatomy, referenced_assets};
pub use builder::{
    AnatomyBuilder, AttachPoint, AxisSample, BuildCx, PartBuild, PartSlots, SkullFrame, frame_y_z,
    mirror_point, mirror_transform, relative, sample_axis,
};
pub use external::{Orient, PartAssets, SdfGrid, TriMesh};
pub use form::{Form, FormError, FormPatch, apply_form, path_matches};
pub use lever::{Lever, LeverError, LeverKind};
pub use loader::{BodyPlanLoader, BodyPlanLoaderError};
pub use lod::{LodLevel, PartDetail, PartLodRules};
pub use part::*;
pub use path::PartPath;
pub use plan::{AttachSlot, BodyPlan, PartNode, PartOffset, PartTone, Repeat, Tone};
pub use primitive::{HardPart, PrimLayer, PrimOp, PrimShape, Primitive};
pub use region::{RegionDef, RegionId, RegionKind, RegionMask, RegionSelector, RegionTable, Side};
pub use rig::{ClothFollow, LegInfo, LookDef, SpringChainDef};
pub use skeleton::{
    BoneDef, BoneId, CrowdSkeleton, JointLimit, MAX_BONES, MAX_CROWD_BONES, Skeleton, SocketDef,
    SocketKind,
};
pub use tone::{Stamp, StampAt, StampKind, StampSet, default_stamps};
pub use viability::{
    BoneBudget, FeetReachGround, RestSelfIntersection, StaticBalance, Viability, ViabilityCheck,
    ViabilityChecks, ViabilityReport,
};

use bevy::prelude::*;
use thiserror::Error;

/// Registers anatomy assets, loaders, reflected types and default resources.
pub struct AnatomyPlugin;

impl Plugin for AnatomyPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<BodyPlan>()
            .register_asset_reflect::<BodyPlan>()
            .init_asset_loader::<BodyPlanLoader>()
            .init_resource::<PartLodRules>()
            .insert_resource(ViabilityChecks::with_builtins())
            .register_type::<Form>()
            .register_type::<PartPath>()
            .register_type::<RegionSelector>()
            .register_type::<PartLodRules>();
    }
}

/// Anything that can go wrong turning a [`BodyPlan`] + [`Form`] into an [`Anatomy`].
#[derive(Debug, Error)]
pub enum AnatomyError {
    #[error(transparent)]
    Form(#[from] FormError),
    #[error("bone budget exceeded: {got} bones (max {max})")]
    BoneBudget { got: usize, max: usize },
    #[error("region budget exceeded: {got} regions (max {max})")]
    RegionBudget { got: usize, max: usize },
    #[error("part `{path}` attaches to slot {slot:?}, which its parent does not expose")]
    MissingAttachSlot { path: PartPath, slot: AttachSlot },
    #[error("duplicate part path `{0}`")]
    DuplicatePath(PartPath),
    #[error("part `{path}`: {reason}")]
    InvalidParams { path: PartPath, reason: String },
    #[error("part `{path}` needs mesh asset `{asset}`, which was not provided")]
    MissingAsset { path: PartPath, asset: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_body_plan_parses() {
        let src = include_str!("../../assets/mobs/quadruped.body.ron");
        let plan: BodyPlan = ron::from_str(src).expect("quadruped.body.ron should parse");
        assert_eq!(plan.root.name, "torso");
        assert_eq!(plan.root.children.len(), 4);
        assert!(plan.root.children[1].mirror);
        assert_eq!(
            plan.root.children[1].attach,
            AttachSlot::Surface {
                t: 0.9,
                around_deg: 120.0
            }
        );
        assert!(
            matches!(&plan.root.children[0].kind, PartKind::Chain(c) if c.role == ChainRole::Neck)
        );
    }

    #[test]
    fn form_round_trips_through_ron() {
        let form = Form {
            overall_scale: 1.1,
            patches: vec![FormPatch {
                part: PartPath::root("torso").child("limb.front"),
                lever: Lever::Length,
                value: 1.2,
            }],
        };
        let text = ron::to_string(&form).unwrap();
        assert_eq!(ron::from_str::<Form>(&text).unwrap(), form);
    }

    #[test]
    fn part_offset_converts() {
        let t = PartOffset {
            rotate_deg: Vec3::new(0.0, 90.0, 0.0),
            ..default()
        }
        .to_transform();
        assert!((t.rotation * Vec3::X - Vec3::NEG_Z).length() < 1e-5);
    }

    fn quadruped() -> BodyPlan {
        ron::from_str(include_str!("../../assets/mobs/quadruped.body.ron")).unwrap()
    }

    #[test]
    fn biped_sockets() {
        let plan: BodyPlan =
            ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let model = a.skeleton.model_rest();
        let find = |kind, side| {
            a.sockets
                .iter()
                .find(|s| s.kind == kind && s.side == side)
                .unwrap()
        };
        for kind in [
            SocketKind::Back,
            SocketKind::Chest,
            SocketKind::Head,
            SocketKind::Mouth,
            SocketKind::Neck,
        ] {
            find(kind, Side::Center);
        }
        let (l, r) = (
            find(SocketKind::Grip, Side::Left),
            find(SocketKind::Grip, Side::Right),
        );
        let at = |s: &SocketDef| model[s.bone.index()] * s.local.compute_affine();
        let (l, r) = (at(l), at(r));
        // Mirror images; the tool axis points forward out of a hanging fist.
        assert!((l.translation.x + r.translation.x).abs() < 1e-4 && l.translation.x > 0.0);
        assert!(l.matrix3.y_axis.z > 0.8 && r.matrix3.y_axis.z > 0.8);
        // The back socket faces backward on an upright torso.
        assert!(at(find(SocketKind::Back, Side::Center)).matrix3.y_axis.z < -0.9);
    }

    #[test]
    fn quadruped_builds() {
        let live = apply_form(&quadruped(), &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let bones = a.skeleton.len();
        assert!(bones <= MAX_BONES, "{bones} bones");
        assert!(
            a.skeleton.crowd.bones.len() <= MAX_CROWD_BONES,
            "{} crowd bones",
            a.skeleton.crowd.bones.len()
        );
        assert_eq!(a.legs.len(), 4);
        // Front legs rank 0, hind legs rank 1, on both sides.
        let mut pairs: Vec<_> = a.legs.iter().map(|l| (l.side, l.pair)).collect();
        pairs.sort_by_key(|(s, p)| (*s as u8, *p));
        assert_eq!(
            pairs,
            [
                (Side::Left, 0),
                (Side::Left, 1),
                (Side::Right, 0),
                (Side::Right, 1)
            ]
        );
        // Mirrored legs are symmetric across X.
        let fl = a
            .legs
            .iter()
            .find(|l| l.side == Side::Left && l.pair == 0)
            .unwrap();
        let fr = a
            .legs
            .iter()
            .find(|l| l.side == Side::Right && l.pair == 0)
            .unwrap();
        assert!(
            (fl.rest_foot - Vec3::new(-fr.rest_foot.x, fr.rest_foot.y, fr.rest_foot.z)).length()
                < 1e-4
        );
        assert!(fl.rest_foot.x > 0.0, "left is +X");
        // Grounded: feet near y = 0.
        assert!(fl.rest_foot.y.abs() < 0.1, "foot at {}", fl.rest_foot.y);
        // Toes are hero-only.
        let toe = a.part(&PartPath("torso/limb.front.l/toes".into())).unwrap();
        assert_eq!(toe.lod, LodLevel::Hero);
        assert!(a.look.is_some());
        assert_eq!(a.hard_parts.len(), 2 + 16);
        // Inverse binds undo the model rest pose.
        let model = a.skeleton.model_rest();
        let id = Mat4::from(model[5]) * a.skeleton.inverse_bind[5];
        assert!(id.abs_diff_eq(Mat4::IDENTITY, 1e-4));
    }

    #[test]
    fn region_selectors() {
        let live = apply_form(&quadruped(), &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let count = |s: RegionSelector| s.mask(&a).0.count_ones();
        let total = a.regions.regions.len() as u32;
        assert_eq!(count(RegionSelector::All), total);
        // Per leg: upper, lower, end, plus its toes (Digit) and claws (Hard).
        assert_eq!(count(RegionSelector::Role(LimbRole::Locomotor)), 4 * 5);
        assert_eq!(count(RegionSelector::Kind(RegionKind::LimbEnd)), 4);
        assert_eq!(
            count(RegionSelector::Part(PartPath("torso/limb.front".into()))),
            2 * 5
        );
        let not_feet = RegionSelector::AllExcept(vec![RegionSelector::Kind(RegionKind::LimbEnd)]);
        assert_eq!(count(not_feet), total - 4);
        let left = RegionSelector::And(vec![
            RegionSelector::Side(Side::Left),
            RegionSelector::Kind(RegionKind::LimbUpper),
        ]);
        assert_eq!(count(left), 2);
    }

    #[test]
    fn lod_rules_resolve() {
        let rules = PartLodRules::default();
        assert_eq!(rules.resolve(PartDetail::Always, 0.0), LodLevel::Far);
        assert_eq!(rules.resolve(PartDetail::Auto, 0.01), LodLevel::Hero);
        assert_eq!(rules.resolve(PartDetail::Auto, 0.1), LodLevel::Near);
        assert_eq!(
            rules.resolve(PartDetail::UpTo(LodLevel::Near), 1.0),
            LodLevel::Near
        );
    }
}
