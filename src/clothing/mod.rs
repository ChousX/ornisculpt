//! Clothing: authored garments wrapped onto each body region at that region's shape.
//!
//! A garment is a mesh authored in **wrap space**, a unit cylinder standing for one body part:
//! - `y` runs along the part over the garment's [`Garment::span`] (0 = span start, 1 = span end);
//! - the angle around Y is the angle around the part (`-Z` = dorsal/back, `+X` = left on a
//!   torso, the outer side on a limb; mirrored parts mirror it, so asymmetric details mirror);
//! - the distance outside the cylinder, `r - 1`, is the height above the skin in metres.
//!
//! Fitting marches out from the part's axis to the real body surface at every vertex (muscle,
//! fat and tone included), so the same garment hugs a thin arm and a thick one, an elliptical
//! torso and a round one. Vertices beyond the span (`y < 0` or `y > 1`) **hang**: they drop from
//! the span's edge (`|y|` beyond the edge = fraction of [`Hang::length`]) and are skinned to
//! spring-bone chains added to the skeleton, which can swing with the legs. On a garment that
//! doesn't hang, `y` beyond 0..1 instead wraps over the part's end cap (`y = -1` reaches the
//! tip): shorts over a pelvis, a hood over a head end, a mitten over a hand.
//!
//! A garment hugs the whole body, blend bulges included, but never sits further out than its own
//! part's surface plus a small allowance: where its rays run into another part (a torso garment
//! at an arm root), that part emerges through it as through an armhole.
//!
//! An [`Outfit`] (`*.outfit.ron`, worn via [`MobOutfit`]) is baked into the generated body: one
//! cloth submesh per LOD, coloured from the outfit's palette.

mod fit;

pub use fit::{fit_outfit, garment_lods};

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Garments a mob wears, baked into its body.
#[derive(Asset, Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Outfit {
    pub garments: Vec<Garment>,
}

/// One authored piece, wrapped onto every part its pattern matches.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Garment {
    pub name: String,
    /// Part path pattern (`"torso"`, `"torso/arm"` matches `arm.l` and `arm.r`).
    pub on: String,
    /// Stretch of the part the garment's `y` 0..1 covers (part `t`, 0 = root end).
    pub span: (f32, f32),
    /// Authored mesh asset path (`"garments/garments.gltf#Mesh0/Primitive0"`).
    pub mesh: String,
    /// sRGB, 0..1.
    pub color: [f32; 3],
    pub roughness: f32,
    /// Extra height above the skin in metres (cloth thickness plus looseness).
    pub ease: f32,
    /// How far the garment follows the body past its own part, as a fraction of the part's
    /// radius: low lets neighbouring parts emerge through it (arms through a tunic's armholes),
    /// high wraps over their roots (breeches over the tops of the legs).
    pub reach: f32,
    /// Cloth hanging past the span's edges.
    pub hang: Option<Hang>,
}

impl Default for Garment {
    fn default() -> Self {
        Self {
            name: String::new(),
            on: String::new(),
            span: (0.0, 1.0),
            mesh: String::new(),
            color: [0.55, 0.5, 0.42],
            roughness: 0.85,
            ease: 0.006,
            reach: 0.3,
            hang: None,
        }
    }
}

/// Loose cloth hanging from a garment's span edge, on spring-bone chains.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Hang {
    /// Metres the authored `y` range -1..0 (or 1..2) hangs over.
    pub length: f32,
    /// Bone chains around the hanging edge.
    pub chains: u8,
    /// Bones per chain.
    pub segments: u8,
    /// 0 = continue along the part, 1 = straight down.
    pub droop: f32,
    /// How far the hem spreads outward per metre of drop.
    pub flare: f32,
    pub stiffness: f32,
    pub damping: f32,
    /// Share of the nearest leg's swing the chains take (skirts, robes), 0..1.
    pub follow_legs: f32,
}

impl Default for Hang {
    fn default() -> Self {
        Self { length: 0.3, chains: 6, segments: 2, droop: 1.0, flare: 0.2, stiffness: 0.5, damping: 0.4, follow_legs: 0.0 }
    }
}

impl Outfit {
    /// Asset paths of every garment mesh.
    pub fn referenced_meshes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for g in &self.garments {
            if !out.contains(&g.mesh) {
                out.push(g.mesh.clone());
            }
        }
        out
    }

    /// Palette for the shaders: garment colours (linear) with roughness in w; 8 at most.
    pub fn palette(&self) -> [Vec4; MAX_GARMENTS] {
        let mut out = [Vec4::new(0.5, 0.5, 0.5, 0.85); MAX_GARMENTS];
        for (o, g) in out.iter_mut().zip(&self.garments) {
            let l = Color::srgb(g.color[0], g.color[1], g.color[2]).to_linear();
            *o = Vec4::new(l.red, l.green, l.blue, g.roughness);
        }
        out
    }
}

/// Garments per outfit that get their own palette entry (later ones reuse the last colour).
pub const MAX_GARMENTS: usize = 8;

/// Which outfit a mob wears.
#[derive(Component, Clone, Debug)]
pub struct MobOutfit(pub Handle<Outfit>);

/// Loads `*.outfit.ron`.
#[derive(Default, TypePath)]
pub struct OutfitLoader;

#[derive(Debug, Error)]
pub enum OutfitLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for OutfitLoader {
    type Asset = Outfit;
    type Settings = ();
    type Error = OutfitLoaderError;

    async fn load(&self, reader: &mut dyn Reader, _: &(), _: &mut LoadContext<'_>) -> Result<Outfit, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        &["outfit.ron"]
    }
}

pub struct ClothingPlugin;

impl Plugin for ClothingPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<Outfit>().init_asset_loader::<OutfitLoader>().register_asset_reflect::<Outfit>();
    }
}
