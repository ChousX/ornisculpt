//! Anatomy: authored body plans, individual form, derived regions, and the resolved [`Anatomy`].
//!
//! Data flow: [`BodyPlan`] + [`Form`] → [`apply_form`] → [`build_anatomy`] → [`Anatomy`].

mod build;
mod builder;
mod external;
mod form;
#[cfg(feature = "genetics")]
pub mod genome;
mod lever;
mod loader;
mod lod;
mod mass;
mod part;
mod parts;
mod path;
mod plan;
mod primitive;
mod region;
mod rig;
mod skeleton;
mod spread;
mod tone;
mod viability;
mod wing;

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
pub use mass::{BodyMass, WATER, measure as measure_mass};
pub use part::*;
pub use path::PartPath;
pub use plan::{AttachSlot, BodyPlan, PartNode, PartOffset, PartTone, Repeat, Tone};
pub use primitive::{HardPart, PrimLayer, PrimOp, PrimShape, Primitive, Weigh};
pub use region::{RegionDef, RegionId, RegionKind, RegionMask, RegionSelector, RegionTable, Side};
pub use rig::{Clavicle, ClothFollow, HandDef, LegInfo, LookDef, SpringChainDef};
pub use spread::{Spread, spread_pose};
pub use skeleton::{
    BoneDef, BoneId, CrowdSkeleton, Flexes, JointLimit, CROWD_SLOT, MAX_BONES, MAX_CROWD_BONES, Skeleton, SocketDef,
    SocketKind,
};
pub use tone::{SoftTune, Stamp, StampAt, StampForm, StampKind, StampSet, default_stamps, upright_torso_stamps};
pub use wing::{
    BladeWing, FeatheredWing, MembraneWing, Planform, Sheet, SheetKind, Trailing, WingKind, WingParams, WingPose, WingRig, WingShape,
};
pub use viability::{
    BoneBudget, FeetReachGround, RestSelfIntersection, StaticBalance, Viability, ViabilityCheck,
    ViabilityChecks, ViabilityReport, sole_heights,
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
        #[cfg(feature = "genetics")]
        app.add_plugins(genome::GenomePlugin);
        #[cfg(feature = "interchange")]
        app.add_plugins(crate::interchange::InterchangePlugin);
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
    /// A builder invariant did not hold (a bug, reported instead of panicking).
    #[error("internal error: {0}")]
    Internal(&'static str),
}

/// Builds a body plan from RON source with the default form and LOD rules, and the glTF part
/// meshes it references ([`test_assets`]).
#[cfg(test)]
pub(crate) fn build_src(src: &str) -> Anatomy {
    let plan: BodyPlan = ron::from_str(src).unwrap();
    Anatomy::from_plan(&plan, &PartLodRules::default(), &test_assets(&plan)).unwrap()
}

/// The glTF part meshes a plan references, read from `assets/` (the first primitive of the named
/// mesh; positions, normals and indices, as a little-endian embedded buffer, which is how
/// `tools/` write them).
#[cfg(test)]
pub(crate) fn test_assets(plan: &BodyPlan) -> PartAssets {
    let mut assets = PartAssets::default();
    for path in referenced_assets(plan) {
        let (file, _) = path.split_once('#').unwrap_or((path.as_str(), ""));
        let gltf: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/assets/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
        let uri = gltf["buffers"][0]["uri"].as_str().unwrap();
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, uri.split_once(',').unwrap().1).unwrap();
        let index = |v: &serde_json::Value| usize::try_from(v.as_u64().unwrap_or(0)).unwrap();
        let view = |accessor: &serde_json::Value| {
            let acc = &gltf["accessors"][index(accessor)];
            let v = &gltf["bufferViews"][index(&acc["bufferView"])];
            let (o, l) = (index(&v["byteOffset"]), index(&v["byteLength"]));
            (&bytes[o..o + l], acc["componentType"].as_u64().unwrap())
        };
        let prim = &gltf["meshes"][0]["primitives"][0];
        let vec3s = |accessor: &serde_json::Value| -> Vec<Vec3> {
            let (b, _) = view(accessor);
            b.as_chunks::<12>().0.iter().map(|c| Vec3::new(f32::from_le_bytes([c[0], c[1], c[2], c[3]]), f32::from_le_bytes([c[4], c[5], c[6], c[7]]), f32::from_le_bytes([c[8], c[9], c[10], c[11]]))).collect()
        };
        let positions = vec3s(&prim["attributes"]["POSITION"]);
        let normals = vec3s(&prim["attributes"]["NORMAL"]);
        let (ib, kind) = view(&prim["indices"]);
        let indices: Vec<u32> = match kind {
            5125 => ib.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect(),
            _ => ib.as_chunks::<2>().0.iter().map(|c| u32::from(u16::from_le_bytes(*c))).collect(),
        };
        assets.insert(path, std::sync::Arc::new(TriMesh::new(positions, normals, indices)));
    }
    assets
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
                t: 0.88,
                around_deg: 135.0
            }
        );
        // Real limb proportions: unequal segments, elbow bending back.
        let PartKind::Limb(front) = &plan.root.children[1].kind else { panic!("front leg is a limb") };
        assert_eq!(front.chain.ratios, vec![1.0, 1.05, 0.4]);
        let joints = front.joints.as_ref().expect("front leg declares its joints");
        assert_eq!(joints.plan, JointPlan::Foreleg);
        assert!(joints.swing < 0.0);
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

    const QUADRUPED: &str = include_str!("../../assets/mobs/quadruped.body.ron");

    fn quadruped() -> BodyPlan {
        ron::from_str(QUADRUPED).unwrap()
    }

    #[test]
    fn biped_sockets() {
        let a = build_src(include_str!("../../assets/mobs/biped.body.ron"));
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
        let a = build_src(QUADRUPED);
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
        pairs.sort_by_key(|(s, p)| (*s != Side::Left, *p));
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
        let a = build_src(QUADRUPED);
        let count = |s: RegionSelector| s.mask(&a).0.count_ones();
        let total = u32::try_from(a.regions.regions.len()).unwrap();
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

    /// Hind legs of the quadruped plan with their joints edited by `f`.
    fn hind_joints(f: impl Fn(&mut LimbJoints)) -> Result<Anatomy, AnatomyError> {
        let mut plan = quadruped();
        plan.visit_mut(|p, n| {
            if let PartKind::Limb(l) = &mut n.kind
                && p.0.ends_with("limb.hind")
            {
                f(l.joints.as_mut().unwrap());
            }
        });
        Anatomy::from_plan(&plan, &PartLodRules::default(), &PartAssets::default())
    }

    #[test]
    fn jointed_legs_limit_and_point_their_knees() {
        let a = hind_joints(|_| {}).unwrap();
        // Root ball plus two hinges on each of four legs.
        assert_eq!(a.joint_limits.len(), 4 * 3);
        for l in &a.legs {
            // Elbows point back, stifles forward.
            let want = if l.pair == 0 { -1.0 } else { 1.0 };
            assert!(l.pole.z * want > 0.9, "{:?} leg {} pole {}", l.side, l.pair, l.pole);
        }
    }

    #[test]
    fn a_reversed_knee_does_not_build() {
        // The stifle flexed the elbow's way: the old `bends` sign flip.
        let err = hind_joints(|j| j.flex[0] = -72.0).unwrap_err();
        assert!(matches!(&err, AnatomyError::InvalidParams { reason, .. } if reason.contains("stifle")), "{err}");
        assert!(hind_joints(|j| j.flex.push(10.0)).is_err(), "one flexion per hinge");
    }

    const PLANS: [(&str, &str); 11] = [
        ("bat", include_str!("../../assets/mobs/bat.body.ron")),
        ("beast", include_str!("../../assets/mobs/beast.body.ron")),
        ("beetle", include_str!("../../assets/mobs/beetle.body.ron")),
        ("biped", include_str!("../../assets/mobs/biped.body.ron")),
        ("bird", include_str!("../../assets/mobs/bird.body.ron")),
        ("dragon", include_str!("../../assets/mobs/dragon.body.ron")),
        ("dragonfly", include_str!("../../assets/mobs/dragonfly.body.ron")),
        ("hexapod", include_str!("../../assets/mobs/hexapod.body.ron")),
        ("horse", include_str!("../../assets/mobs/horse.body.ron")),
        ("quadruped", include_str!("../../assets/mobs/quadruped.body.ron")),
        ("wyvern", include_str!("../../assets/mobs/wyvern.body.ron")),
    ];

    /// Every sample limb declares its joints, and each leg's declared knee points the way its
    /// rest pose bends (the IK pole used to be read off the rest pose).
    #[test]
    fn sample_limbs_declare_their_joints() {
        for (name, src) in PLANS {
            let plan: BodyPlan = ron::from_str(src).unwrap();
            plan.visit(|path, n| {
                if let PartKind::Limb(l) = &n.kind {
                    assert!(l.joints.is_some() && l.bends.is_empty(), "{name}: {path} has no joints");
                }
            });
            // Plans with authored meshes (the beast's crest) need assets loaded to build.
            let a = match Anatomy::from_plan(&plan, &PartLodRules::default(), &PartAssets::default()) {
                Ok(a) => a,
                Err(AnatomyError::MissingAsset { .. }) => continue,
                Err(e) => panic!("{name}: {e}"),
            };
            let model = a.skeleton.rest_model();
            for l in &a.legs {
                let at = |i: usize| Vec3::from(model[l.chain[i].index()].translation);
                let (last, def) = (l.chain[l.chain.len() - 1], a.skeleton.get(l.chain[l.chain.len() - 1]).unwrap());
                let tip = model[last.index()].transform_point3(Vec3::Y * def.length);
                let axis = (tip - at(0)).normalize();
                let knee = at(1) - at(0);
                let rest_pole = (knee - axis * knee.dot(axis)).normalize();
                println!("{name} {:?}/{} pole {:.2} rest {:.2}", l.side, l.pair, l.pole, rest_pole);
                assert!(l.pole.dot(rest_pole) > 0.9, "{name}: {:?} leg {} pole {} vs rest {}", l.side, l.pair, l.pole, rest_pole);
            }
        }
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
