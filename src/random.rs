//! Deterministic randomness and noise shared by every generation stage.
//!
//! - RNG: [`rand_pcg::Pcg64Mcg`], documented as reproducible across platforms.
//! - Stream forking: xxh3 of `(seed, label)`. xxh3 is a specified algorithm, so labels hash the
//!   same on every platform and version (unlike `std`'s `DefaultHasher`).
//! - Noise: [`fastnoise_lite`]. The reference library also ships shader ports, so CPU geometry
//!   noise and GPU shading noise can be kept identical.

use bevy::prelude::*;
use fastnoise_lite::{FastNoiseLite, FractalType, NoiseType};
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_64_with_seed;

pub use rand::{Rng, RngExt};

/// RNG used for all generation.
pub type MobRng = rand_pcg::Pcg64Mcg;

/// Stable 64-bit hash of `label` under `seed`.
pub fn stable_hash(seed: u64, label: &str) -> u64 {
    xxh3_64_with_seed(label.as_bytes(), seed)
}

/// Independent RNG stream for `label` (usually a part path) under a mob seed.
/// Editing one part never reshuffles another part's stream.
pub fn fork(seed: u64, label: &str) -> MobRng {
    MobRng::seed_from_u64(stable_hash(seed, label))
}

/// Authored noise description (serializable stand-in for [`FastNoiseLite`]'s settings).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct NoiseSettings {
    pub kind: NoiseKind,
    /// Features per metre.
    pub frequency: f32,
    pub fractal: Fractal,
    pub octaves: u8,
    pub lacunarity: f32,
    pub gain: f32,
    /// Output is scaled to `-amplitude..=amplitude`.
    pub amplitude: f32,
}

impl Default for NoiseSettings {
    fn default() -> Self {
        Self {
            kind: NoiseKind::OpenSimplex2,
            frequency: 8.0,
            fractal: Fractal::Fbm,
            octaves: 3,
            lacunarity: 2.0,
            gain: 0.5,
            amplitude: 1.0,
        }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NoiseKind {
    #[default]
    OpenSimplex2,
    OpenSimplex2S,
    Perlin,
    Value,
    ValueCubic,
    /// Voronoi cells (scales, spots).
    Cellular,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fractal {
    None,
    #[default]
    Fbm,
    /// Sharp creases (wrinkles, veins).
    Ridged,
    PingPong,
}

/// A ready-to-sample noise generator.
pub struct Noise {
    inner: FastNoiseLite,
    amplitude: f32,
}

impl Noise {
    /// Builds a generator seeded from `(seed, label)`.
    pub fn new(settings: &NoiseSettings, seed: u64, label: &str) -> Self {
        let mut n = FastNoiseLite::with_seed(stable_hash(seed, label) as i32);
        n.set_noise_type(Some(match settings.kind {
            NoiseKind::OpenSimplex2 => NoiseType::OpenSimplex2,
            NoiseKind::OpenSimplex2S => NoiseType::OpenSimplex2S,
            NoiseKind::Perlin => NoiseType::Perlin,
            NoiseKind::Value => NoiseType::Value,
            NoiseKind::ValueCubic => NoiseType::ValueCubic,
            NoiseKind::Cellular => NoiseType::Cellular,
        }));
        n.set_frequency(Some(settings.frequency));
        n.set_fractal_type(Some(match settings.fractal {
            Fractal::None => FractalType::None,
            Fractal::Fbm => FractalType::FBm,
            Fractal::Ridged => FractalType::Ridged,
            Fractal::PingPong => FractalType::PingPong,
        }));
        n.set_fractal_octaves(Some(settings.octaves as i32));
        n.set_fractal_lacunarity(Some(settings.lacunarity));
        n.set_fractal_gain(Some(settings.gain));
        Self { inner: n, amplitude: settings.amplitude }
    }

    pub fn sample(&self, p: Vec3) -> f32 {
        self.inner.get_noise_3d(p.x, p.y, p.z) * self.amplitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_is_stable_and_independent() {
        let a = fork(42, "torso/limb.front.l").next_u64();
        assert_eq!(a, fork(42, "torso/limb.front.l").next_u64());
        assert_ne!(a, fork(42, "torso/limb.front.r").next_u64());
        assert_ne!(a, fork(43, "torso/limb.front.l").next_u64());
    }

    #[test]
    fn noise_is_deterministic_and_bounded() {
        let s = NoiseSettings { amplitude: 0.5, ..default() };
        let a = Noise::new(&s, 7, "skin");
        let b = Noise::new(&s, 7, "skin");
        let p = Vec3::new(0.3, 1.2, -0.7);
        assert_eq!(a.sample(p), b.sample(p));
        assert!(a.sample(p).abs() <= 0.5 + 1e-4);
    }
}
