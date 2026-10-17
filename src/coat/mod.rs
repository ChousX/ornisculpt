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

mod geometry;
mod material;

pub use geometry::{CoatMeshes, annotate, build_coat_meshes, feather_cards, fur_shells};
pub use material::{COAT_FRAGMENT, COAT_VERTEX, CoatExt, CoatKey, CoatLayer, MobMaterial};

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use bevy::render::render_resource::ShaderType;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::anatomy::{Anatomy, RegionMask, RegionSelector};

/// The look of a mob.
#[derive(Asset, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Coat {
    /// sRGB colours, 0..1.
    pub base: [f32; 3],
    pub secondary: [f32; 3],
    pub accent: [f32; 3],
    pub roughness: f32,
    /// Later coverings win where selectors overlap.
    pub coverings: Vec<Covering>,
}

impl Default for Coat {
    fn default() -> Self {
        Self {
            base: [0.62, 0.45, 0.33],
            secondary: [0.45, 0.32, 0.22],
            accent: [0.85, 0.75, 0.6],
            roughness: 0.75,
            coverings: Vec::new(),
        }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Covering {
    pub regions: RegionSelector,
    pub kind: CoveringKind,
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
    /// Strand length in metres.
    pub length: f32,
    /// Strands per metre (along each axis).
    pub density: f32,
    /// Strand thickness at the root, 0..1 of a strand cell.
    pub thickness: f32,
    /// How much tips shift toward the accent colour, 0..1.
    pub tip: f32,
    /// Shells at hero detail (near detail uses a third, far none).
    pub shells: u8,
}

impl Default for Fur {
    fn default() -> Self {
        Self { length: 0.025, density: 500.0, thickness: 0.7, tip: 0.4, shells: 12 }
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

fn linear(c: [f32; 3]) -> Vec4 {
    let l = Color::srgb(c[0], c[1], c[2]).to_linear();
    Vec4::new(l.red, l.green, l.blue, 1.0)
}

fn split(mask: RegionMask) -> UVec2 {
    UVec2::new(mask.0 as u32, (mask.0 >> 32) as u32)
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
    pub _pad: UVec2,
    /// size, relief, contrast, _
    pub scales: Vec4,
    /// length, density, thickness, tip
    pub fur: Vec4,
    /// length, width, tip, _
    pub feathers: Vec4,
    /// Outfit palette: linear colour + roughness (w), indexed by the cloth vertex colour's w.
    pub cloth: [Vec4; 8],
}

/// Region masks per covering kind on one anatomy (later coverings take regions from earlier).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CoverageMasks {
    pub scales: RegionMask,
    pub fur: RegionMask,
    pub feathers: RegionMask,
}

impl Coat {
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

    pub fn scales(&self) -> Option<Scales> {
        self.coverings.iter().find_map(|c| match c.kind {
            CoveringKind::Scales(s) => Some(s),
            _ => None,
        })
    }

    pub fn fur(&self) -> Option<Fur> {
        self.coverings.iter().find_map(|c| match c.kind {
            CoveringKind::Fur(f) => Some(f),
            _ => None,
        })
    }

    pub fn feathers(&self) -> Option<Feathers> {
        self.coverings.iter().find_map(|c| match c.kind {
            CoveringKind::Feathers(f) => Some(f),
            _ => None,
        })
    }

    /// The shader uniform for this coat on `anatomy`.
    pub fn uniform(&self, anatomy: &Anatomy) -> CoatUniform {
        let masks = self.coverage(anatomy);
        let s = self.scales().unwrap_or_default();
        let f = self.fur().unwrap_or_default();
        let fe = self.feathers().unwrap_or_default();
        CoatUniform {
            base: linear(self.base),
            secondary: linear(self.secondary),
            accent: linear(self.accent),
            scale_mask: split(masks.scales),
            fur_mask: split(masks.fur),
            feather_mask: split(masks.feathers),
            _pad: UVec2::ZERO,
            scales: Vec4::new(s.size, s.relief, s.contrast, 0.0),
            fur: Vec4::new(f.length, f.density, f.thickness, f.tip),
            feathers: Vec4::new(fe.length, fe.width, fe.tip, 0.0),
            cloth: [Vec4::new(0.5, 0.5, 0.5, 0.85); 8],
        }
    }

    /// A plain coat in one colour (hard parts, eyes).
    pub fn plain(color: [f32; 3], roughness: f32) -> Coat {
        Coat { base: color, secondary: color, accent: color, roughness, coverings: Vec::new() }
    }
}

/// Loads `*.coat.ron`.
#[derive(Default, TypePath)]
pub struct CoatLoader;

#[derive(Debug, Error)]
pub enum CoatLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for CoatLoader {
    type Asset = Coat;
    type Settings = ();
    type Error = CoatLoaderError;

    async fn load(&self, reader: &mut dyn Reader, _: &(), _: &mut LoadContext<'_>) -> Result<Coat, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        &["coat.ron"]
    }
}

pub struct CoatPlugin;

impl Plugin for CoatPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "coat_vertex.wesl");
        bevy::asset::embedded_asset!(app, "coat_fragment.wesl");
        app.init_asset::<Coat>()
            .init_asset_loader::<CoatLoader>()
            .register_asset_reflect::<Coat>()
            .add_plugins(MaterialPlugin::<MobMaterial>::default());
    }
}
