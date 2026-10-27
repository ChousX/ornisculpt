//! Coat: the look of a mob, composed with its body plan. Colours plus coverings (scales, fur,
//! feathers), each applied to the regions its [`RegionSelector`] picks on whatever body it wears.
//!
//! - Scales are procedural in the skin shader (cellular relief), so they cost nothing per tier.
//!   Big plates/scutes are hard parts (`HardKind::Plate`) on the body plan.
//! - Fur renders as shells: offset copies of the covered skin, alpha-tested by strand noise.
//! - Feathers are cards scattered over the covered skin at generation time, layered along the
//!   body's flow and skinned to it.
//!
//! Loaded from `*.coat.ron`; a mob wears one through [`MobCoat`] (none = plain skin).

pub mod dqs;
mod geometry;
mod markings;
mod material;

pub use geometry::{CoatMeshes, annotate, build_coat_meshes};
pub use self::HairCards as Hair;
pub use markings::{Bands, Facing, Marking, Patches};
pub use material::{CoatExt, CoatKey, CoatLayer, MobMaterial};

use bevy::prelude::*;
use bevy::render::render_resource::ShaderType;
use serde::{Deserialize, Serialize};

use crate::anatomy::{Anatomy, RegionId, RegionMask, RegionSelector};

/// The look of a mob.
#[derive(Asset, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Coat {
    /// sRGB colours, 0..1.
    pub base: [f32; 3],
    pub secondary: [f32; 3],
    pub accent: [f32; 3],
    pub roughness: f32,
    /// Later coverings win where selectors overlap. Fur coverings after the first set only their
    /// regions' strand length (see [`Fur::length`]).
    pub coverings: Vec<Covering>,
    /// Colour laid over the skin and hard parts by region and shape, in order (see [`Marking`]).
    pub markings: Vec<Marking>,
    /// Claws, horns, teeth, beaks and plates (sRGB); markings can paint them too.
    pub hard: [f32; 3],
    /// The eyes' iris (sRGB).
    pub iris: [f32; 3],
}

impl Default for Coat {
    fn default() -> Self {
        Self {
            base: [0.62, 0.45, 0.33],
            secondary: [0.45, 0.32, 0.22],
            accent: [0.85, 0.75, 0.6],
            roughness: 0.75,
            coverings: Vec::new(),
            markings: Vec::new(),
            hard: Self::HARD,
            iris: Self::IRIS,
        }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Covering {
    pub regions: RegionSelector,
    pub kind: CoveringKind,
    /// Only the parts of its regions facing a direction (rest pose), faded like a marking's: a
    /// hairline over the top and back of a skull, a pale belly's fur under a dark back's. Where
    /// it fades out, whatever covered the surface before shows.
    #[serde(default)]
    pub facing: Option<Facing>,
}

/// What covers one vertex: the weights of scales, fur and feathers (0..1 each), the fur's
/// length there, and the hair cards of the fur covering holding there, if it has them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cover {
    pub weights: Vec3,
    pub fur_length: f32,
    pub cards: Option<HairCards>,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum CoveringKind {
    Scales(Scales),
    Fur(Fur),
    Feathers(Feathers),
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Scales {
    /// Scale diameter in metres.
    pub size: f32,
    /// Bump strength, 0..1.
    pub relief: f32,
    /// How much individual scales vary between base and secondary colour, 0..1.
    pub contrast: f32,
}

impl Default for Scales {
    fn default() -> Self {
        Self { size: 0.012, relief: 0.6, contrast: 0.5 }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Fur {
    /// Strand length in metres. The one fur parameter that varies over a body: a second fur
    /// covering (a mane, a beard, hair) gives its regions their own length, while the rest of
    /// its parameters come from the body's first fur covering.
    pub length: f32,
    /// Strands per metre (along each axis).
    pub density: f32,
    /// Strand thickness at the root, 0..1 of a strand cell.
    pub thickness: f32,
    /// How much tips shift toward the accent colour, 0..1.
    pub tip: f32,
    /// Shells at hero detail (near detail uses a third, far none).
    pub shells: u8,
    /// Long hair: cards hanging from the covered skin at hero detail, besides the shells.
    pub cards: Option<HairCards>,
}

impl Default for Fur {
    fn default() -> Self {
        Self { length: 0.025, density: 500.0, thickness: 0.7, tip: 0.4, shells: 12, cards: None }
    }
}

/// Hair cards: flat strips hanging from the skin under gravity (combed down the surface, then
/// drooping free), a few strands painted on each, ragged at the tip. Hero detail only.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct HairCards {
    /// Card length in metres.
    pub length: f32,
    /// Card width in metres.
    pub width: f32,
    /// Cards per square metre of covered skin.
    pub density: f32,
    /// Angle the cards lift off the skin at the root, degrees.
    pub lift_deg: f32,
    /// How far each card turns from lying along the skin toward hanging straight down by its
    /// tip (0 = along the skin, 1 = plumb).
    pub droop: f32,
}

impl Default for HairCards {
    fn default() -> Self {
        Self { length: 0.12, width: 0.012, density: 2500.0, lift_deg: 8.0, droop: 0.6 }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Feathers {
    pub length: f32,
    pub width: f32,
    /// Feathers per square metre at hero detail (near detail uses a third, far none).
    pub density: f32,
    /// Angle the feathers lift off the skin, degrees.
    pub lift_deg: f32,
    /// How much tips shift toward the accent colour, 0..1.
    pub tip: f32,
}

impl Default for Feathers {
    fn default() -> Self {
        Self { length: 0.07, width: 0.03, density: 900.0, lift_deg: 12.0, tip: 0.5 }
    }
}

/// Which coat a mob wears.
#[derive(Component, Clone, Debug)]
pub struct MobCoat(pub Handle<Coat>);

/// An sRGB colour as linear RGB, with `w` in the fourth lane.
pub(crate) fn linear(c: [f32; 3], w: f32) -> Vec4 {
    let l = Color::srgb(c[0], c[1], c[2]).to_linear();
    Vec4::new(l.red, l.green, l.blue, w)
}

/// A 64-bit mask as (low, high) words.
fn split(mask: RegionMask) -> UVec2 {
    let low = u32::try_from(mask.0 & u64::from(u32::MAX)).unwrap_or_default();
    let high = u32::try_from(mask.0 >> 32).unwrap_or_default();
    UVec2::new(low, high)
}

/// Coat parameters as the shaders see them (one covering of each kind; masks per kind).
#[derive(ShaderType, Reflect, Clone, Copy, Debug, Default, PartialEq)]
pub struct CoatUniform {
    pub base: Vec4,
    pub secondary: Vec4,
    pub accent: Vec4,
    pub scale_mask: UVec2,
    pub fur_mask: UVec2,
    pub feather_mask: UVec2,
    /// Muscle flex gain (x; 0 = muscles stay still, 1 = as baked), soft-tissue gain (y; 0 = still).
    pub muscle: Vec2,
    /// size, relief, contrast, fur shell count
    pub scales: Vec4,
    /// length, density, thickness, tip
    pub fur: Vec4,
    /// length, width, tip, _
    pub feathers: Vec4,
    /// Outfit palette: linear colour + roughness (w), indexed by the cloth vertex colour's w.
    pub cloth: [Vec4; 8],
}

/// Region masks per covering kind on one anatomy (later coverings take regions from earlier).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoverageMasks {
    pub scales: RegionMask,
    pub fur: RegionMask,
    pub feathers: RegionMask,
}

impl Coat {
    #[must_use]
    pub fn coverage(&self, anatomy: &Anatomy) -> CoverageMasks {
        let mut m = CoverageMasks::default();
        for c in &self.coverings {
            let mask = c.regions.mask(anatomy);
            let keep = mask.complement();
            m.scales = m.scales.intersection(keep);
            m.fur = m.fur.intersection(keep);
            m.feathers = m.feathers.intersection(keep);
            match c.kind {
                CoveringKind::Scales(_) => m.scales = m.scales.union(mask),
                CoveringKind::Fur(_) => m.fur = m.fur.union(mask),
                CoveringKind::Feathers(_) => m.feathers = m.feathers.union(mask),
            }
        }
        m
    }

    /// The first covering `pick` accepts.
    fn first<T>(&self, pick: impl Fn(&CoveringKind) -> Option<T>) -> Option<T> {
        self.coverings.iter().find_map(|c| pick(&c.kind))
    }

    #[must_use]
    pub fn scales(&self) -> Option<Scales> {
        self.first(|k| if let CoveringKind::Scales(s) = k { Some(*s) } else { None })
    }

    #[must_use]
    pub fn fur(&self) -> Option<Fur> {
        self.first(|k| if let CoveringKind::Fur(f) = k { Some(*f) } else { None })
    }

    #[must_use]
    pub fn feathers(&self) -> Option<Feathers> {
        self.first(|k| if let CoveringKind::Feathers(f) = k { Some(*f) } else { None })
    }

    /// Each covering's region mask on `anatomy`, for [`cover_at`](Self::cover_at).
    #[must_use]
    pub fn covering_masks(&self, anatomy: &Anatomy) -> Vec<RegionMask> {
        self.coverings.iter().map(|c| c.regions.mask(anatomy)).collect()
    }

    /// What covers a vertex of `region` whose rest-pose normal is `normal`: the coverings in
    /// order, each taking the surface where its regions and facing hold (a later one over an
    /// earlier one where it fades). Fur coverings differ in length only; density, thickness, tip
    /// colour and shell count come from the first (one shell set per mob), which also gives the
    /// length where no fur covering holds.
    #[must_use]
    pub fn cover_at(&self, masks: &[RegionMask], region: RegionId, normal: Vec3) -> Cover {
        let mut cover = Cover { weights: Vec3::ZERO, fur_length: self.fur().map_or(0.0, |f| f.length), cards: None };
        for (c, mask) in self.coverings.iter().zip(masks) {
            if !mask.contains(region) {
                continue;
            }
            let w = c.facing.map_or(1.0, |f| f.weight(normal));
            if w <= 0.0 {
                continue;
            }
            let own = match c.kind {
                CoveringKind::Scales(_) => Vec3::X,
                CoveringKind::Fur(f) => {
                    cover.fur_length = (f.length - cover.fur_length).mul_add(w, cover.fur_length);
                    if w > 0.5 {
                        cover.cards = f.cards;
                    }
                    Vec3::Y
                }
                CoveringKind::Feathers(_) => Vec3::Z,
            };
            cover.weights = cover.weights.lerp(own, w);
        }
        cover
    }

    /// The shader uniform for this coat on `anatomy`.
    #[must_use]
    pub fn uniform(&self, anatomy: &Anatomy) -> CoatUniform {
        let masks = self.coverage(anatomy);
        let s = self.scales().unwrap_or_default();
        let f = self.fur().unwrap_or_default();
        let fe = self.feathers().unwrap_or_default();
        CoatUniform {
            base: linear(self.base, 1.0),
            secondary: linear(self.secondary, 1.0),
            accent: linear(self.accent, 1.0),
            scale_mask: split(masks.scales),
            fur_mask: split(masks.fur),
            feather_mask: split(masks.feathers),
            muscle: Vec2::new(1.0, 1.0),
            scales: Vec4::new(s.size, s.relief, s.contrast, f32::from(f.shells)),
            fur: Vec4::new(f.length, f.density, f.thickness, f.tip),
            feathers: Vec4::new(fe.length, fe.width, fe.tip, 0.0),
            cloth: crate::clothing::DEFAULT_PALETTE,
        }
    }

    /// Hard parts' colour without one set: ivory.
    pub const HARD: [f32; 3] = [0.85, 0.8, 0.68];
    /// Iris colour without one set: dark brown.
    pub const IRIS: [f32; 3] = [0.13, 0.075, 0.04];

    /// A plain coat in one colour (hard parts, eyes).
    #[must_use]
    pub const fn plain(color: [f32; 3], roughness: f32) -> Self {
        Self {
            base: color,
            secondary: color,
            accent: color,
            roughness,
            coverings: Vec::new(),
            markings: Vec::new(),
            hard: Self::HARD,
            iris: Self::IRIS,
        }
    }
}

impl crate::ron_loader::RonAsset for Coat {
    const EXTENSIONS: &'static [&'static str] = &["coat.ron"];
}

/// Loads `*.coat.ron`.
pub type CoatLoader = crate::ron_loader::RonLoader<Coat>;
pub type CoatLoaderError = crate::ron_loader::RonLoaderError;

pub struct CoatPlugin;

impl Plugin for CoatPlugin {
    fn build(&self, app: &mut App) {
        // The dual quaternion skinning module `coat_vertex.wesl` imports (kept loaded as a library).
        bevy::shader::load_shader_library!(app, "dqs.wesl");
        // The deformation every vertex stage shares (main pass and prepass).
        bevy::shader::load_shader_library!(app, "deform.wesl");
        bevy::shader::load_shader_library!(app, "paint_io.wesl");
        bevy::asset::embedded_asset!(app, "coat_vertex.wesl");
        bevy::asset::embedded_asset!(app, "coat_prepass.wesl");
        bevy::asset::embedded_asset!(app, "coat_fragment.wesl");
        app.init_asset::<Coat>()
            .init_asset_loader::<CoatLoader>()
            .register_asset_reflect::<Coat>()
            .add_plugins(MaterialPlugin::<MobMaterial>::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{LimbRole, LodLevel, RegionKind, build_src};
    use crate::num::Idx;
    use crate::surface::{AdaptiveContour, BodyMeshData, MaterialSlot, SurfaceSettings, centres_of_rotation, mesh_all_lods, muscle_flex};

    fn kinds(kinds: &[RegionKind]) -> RegionSelector {
        RegionSelector::Or(kinds.iter().map(|k| RegionSelector::Kind(*k)).collect())
    }

    /// Scales on the legs, feathers on the torso, fur on the neck, head and tail; bare skin elsewhere.
    fn mixed() -> Coat {
        Coat {
            coverings: vec![
                Covering { regions: kinds(&[RegionKind::Neck, RegionKind::Skull, RegionKind::Jaw, RegionKind::TailBase, RegionKind::TailTip]), kind: CoveringKind::Fur(Fur::default()), facing: None },
                Covering { regions: kinds(&[RegionKind::TorsoFront, RegionKind::TorsoMid, RegionKind::TorsoRear, RegionKind::Belly]), kind: CoveringKind::Feathers(Feathers::default()), facing: None },
                Covering { regions: RegionSelector::Role(LimbRole::Locomotor), kind: CoveringKind::Scales(Scales::default()), facing: None },
            ],
            ..Coat::default()
        }
    }

    /// Every per-vertex stream is empty or one per vertex, so the mesh builds.
    fn assert_consistent(what: &str, d: &BodyMeshData) {
        let n = d.positions.len();
        for (name, len) in [
            ("normals", d.normals.len()),
            ("joints", d.joints.len()),
            ("weights", d.weights.len()),
            ("mob_data", d.mob_data.len()),
            ("uvs", d.uvs.len()),
            ("coverage", d.coverage.len()),
            ("flow", d.flow.len()),
            ("cor", d.cor.len()),
            ("muscle", d.muscle.len()),
        ] {
            assert!(len == 0 || len == n, "{what}: {name} has {len} entries for {n} vertices");
        }
        assert!(d.indices.iter().all(|&i| i.idx() < n), "{what}: index out of range");
    }

    #[test]
    fn scales_fur_and_feathers_share_one_body() {
        let coat = mixed();
        for (name, src) in [
            ("quadruped", include_str!("../../assets/mobs/quadruped.body.ron")),
            ("horse", include_str!("../../assets/mobs/horse.body.ron")),
            ("dragon", include_str!("../../assets/mobs/dragon.body.ron")),
        ] {
            let anatomy = build_src(src);

            let m = coat.coverage(&anatomy);
            assert!(m.scales.0 != 0 && m.fur.0 != 0 && m.feathers.0 != 0, "{name}: a covering lost its regions: {m:?}");
            assert_eq!(m.scales.0 & m.fur.0, 0, "{name}: scales and fur overlap");
            assert_eq!(m.scales.0 & m.feathers.0, 0, "{name}: scales and feathers overlap");
            assert_eq!(m.fur.0 & m.feathers.0, 0, "{name}: fur and feathers overlap");
            let u = coat.uniform(&anatomy);
            assert!(u.scales.x > 0.0 && u.fur.x > 0.0 && u.feathers.x > 0.0, "{name}: uniform lost a covering");

            let mut lods = mesh_all_lods(&AdaptiveContour, &anatomy, &SurfaceSettings::default(), 0).unwrap();
            for (lod, level) in lods.iter_mut().zip(LodLevel::ALL) {
                annotate(lod, &coat, &anatomy, level);
                lod.cor = centres_of_rotation(lod);
                lod.muscle = muscle_flex(lod, &anatomy, level);
            }
            for (lod, level) in lods.iter().zip(LodLevel::ALL) {
                assert_consistent(&format!("{name} {level:?} skin"), lod);
                // Each covering shows on the skin somewhere (border smoothing keeps coarse tiers a little under 1).
                for k in 0..3 {
                    assert!(lod.coverage.iter().any(|c| c[k] > 0.5), "{name} {level:?}: covering {k} missing from the skin");
                }
                // Every slot mesh keeps its centres of rotation (else it skins with dual quaternions).
                let split = if level == LodLevel::Hero { lod.to_bevy_meshes() } else { lod.to_crowd_meshes() };
                for (slot, mesh) in &split {
                    assert!(mesh.attribute(crate::surface::ATTRIBUTE_COR).is_some(), "{name} {level:?} {slot:?}: no centres of rotation");
                }
            }
            let hero = build_coat_meshes(&coat, &anatomy, LodLevel::Hero.pick(&lods), LodLevel::Hero);
            let shells = hero.shells.as_ref().unwrap_or_else(|| panic!("{name}: no fur shells"));
            let cards = hero.feathers.as_ref().unwrap_or_else(|| panic!("{name}: no feather cards"));
            assert_consistent(&format!("{name} shells"), shells);
            assert_consistent(&format!("{name} feathers"), cards);
            // Shells and cards reach the GPU with their own coordinates in UV0 (shell height,
            // card across/along), not the skin's coat weights.
            for (what, d) in [("shells", shells), ("feathers", cards)] {
                let mesh = d.to_bevy_mesh();
                let Some(bevy::mesh::VertexAttributeValues::Float32x2(uv)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0) else {
                    panic!("{name} {what}: no UV0");
                };
                assert_eq!(uv, &d.uvs, "{name} {what}: UV0 replaced");
            }
            for level in [LodLevel::Near, LodLevel::Far] {
                let c = build_coat_meshes(&coat, &anatomy, level.pick(&lods), level);
                assert!(c.shells.is_none() && c.feathers.is_none(), "{name} {level:?}: crowd tiers shade coverings on the skin");
            }
        }
    }

    /// Eyes keep their own UV0 (iris cosine, white) beside a coat with coverings: the skin's
    /// coat weights must not replace them.
    #[test]
    fn eyes_keep_their_uvs_under_a_furry_coat() {
        let anatomy = build_src(include_str!("../../assets/mobs/man.body.ron"));
        let coat: Coat = ron::from_str(include_str!("../../assets/coats/skin_fair.coat.ron")).unwrap();
        let mut lods = mesh_all_lods(&AdaptiveContour, &anatomy, &SurfaceSettings::default(), 0).unwrap();
        let hero = &mut lods[0];
        annotate(hero, &coat, &anatomy, LodLevel::Hero);
        let meshes = hero.to_bevy_meshes();
        let (_, eye) = meshes.iter().find(|(slot, _)| *slot == MaterialSlot::Eye).expect("an eye mesh");
        let Some(bevy::mesh::VertexAttributeValues::Float32x2(uv)) = eye.attribute(Mesh::ATTRIBUTE_UV_0) else {
            panic!("eye without UV0");
        };
        assert!(uv.iter().all(|[_, white]| *white >= 1.0), "eye UV0 replaced: {:?}", &uv[..4]);
    }

    #[test]
    fn later_coverings_take_regions() {
        let anatomy = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let coat = Coat {
            coverings: vec![
                Covering { regions: RegionSelector::All, kind: CoveringKind::Fur(Fur::default()), facing: None },
                Covering { regions: RegionSelector::Role(LimbRole::Locomotor), kind: CoveringKind::Scales(Scales::default()), facing: None },
            ],
            ..Coat::default()
        };
        let m = coat.coverage(&anatomy);
        let legs = RegionSelector::Role(LimbRole::Locomotor).mask(&anatomy);
        assert_eq!(m.scales, legs);
        assert_eq!(m.fur.0, RegionSelector::All.mask(&anatomy).0 & !legs.0);
    }
}
