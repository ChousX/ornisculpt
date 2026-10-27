//! Coat markings: colour laid over the coat's base by region and shape (a pale belly, dark legs, a
//! blaze, bands round a leg, patches), baked per vertex at generation time
//! ([`BodyMeshData::paint`]) and smoothed across the skin so their edges blend.
//!
//! Each marking is a colour, the regions it may cover, and any number of masks that shape it
//! inside them; the masks multiply. Later markings paint over earlier ones.

use bevy::math::Affine3A;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::Coat;
use crate::anatomy::{Anatomy, BoneId, LodLevel, RegionSelector};
use crate::num::{Idx, ToF32, smoothstep};
use crate::surface::{BodyMeshData, MaterialSlot, MobVertexData};

/// One coat marking. Every mask that is set narrows where it shows; the colour shows at
/// `strength` where they all pass.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Marking {
    /// sRGB, 0..1.
    pub color: [f32; 3],
    pub regions: RegionSelector,
    /// How much it covers what was there, 0..1.
    pub strength: f32,
    /// Only surfaces facing a direction (rest pose): a pale belly, a dark back.
    pub facing: Option<Facing>,
    /// A band of heights, as fractions of the body's height (0 = the ground, 1 = the top), faded
    /// from `.0` to `.1` (`.0 > .1` fades the other way): socks, stockings, a dark lower body.
    pub height: Option<(f32, f32)>,
    /// Along the part each vertex belongs to (0 = its root, 1 = its tip), faded from `.0` to `.1`
    /// (`.0 > .1` fades the other way): dark lower legs, pale antenna tips, a tail's tuft.
    pub along: Option<(f32, f32)>,
    /// Toward the far end of each vertex's own bone, as a fraction of how far its surface reaches
    /// along that bone (0 = its rearmost point, 1 = its tip), faded from `.0` to `.1`: a muzzle's
    /// end, lips, horn and claw tips.
    pub tip: Option<(f32, f32)>,
    /// Rings round a part, along it: banded legs, a striped abdomen.
    pub bands: Option<Bands>,
    /// Irregular patches in space: piebald, mottling, spots.
    pub patches: Option<Patches>,
}

impl Default for Marking {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            regions: RegionSelector::All,
            strength: 1.0,
            facing: None,
            height: None,
            along: None,
            tip: None,
            bands: None,
            patches: None,
        }
    }
}

/// A [`Marking`] mask by the way the surface faces in the rest pose: `smoothstep(from, to, n·dir)`.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Facing {
    /// Model space: +Y up, +Z the way the body faces.
    pub dir: Vec3,
    pub from: f32,
    pub to: f32,
}

impl Default for Facing {
    fn default() -> Self {
        Self { dir: Vec3::NEG_Y, from: 0.0, to: 0.5 }
    }
}

/// A [`Marking`] mask of `count` rings along each part, each `width` of its period wide (0..1),
/// edges blurred over `soft` of a period; `offset` shifts them along (periods).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Bands {
    pub count: f32,
    pub width: f32,
    pub soft: f32,
    pub offset: f32,
}

impl Default for Bands {
    fn default() -> Self {
        Self { count: 4.0, width: 0.3, soft: 0.1, offset: 0.0 }
    }
}

/// A [`Marking`] mask of patches about `size` metres across covering `cover` (0..1) of the
/// surface, their edges blurred over `soft`; `seed` picks the pattern.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Patches {
    pub size: f32,
    pub cover: f32,
    pub soft: f32,
    pub seed: u32,
}

impl Default for Patches {
    fn default() -> Self {
        Self { size: 0.2, cover: 0.4, soft: 0.08, seed: 0 }
    }
}

/// Smoothing passes over the paint across the skin, softening region borders over a triangle or
/// two (the masks have soft edges of their own; more passes wash out narrow bands).
const PAINT_SMOOTHING: usize = 2;

/// Bakes `coat`'s markings into `body.paint` (left empty for a coat without any).
///
/// Skin and hard parts take them; eyes, wing sheets and garments keep their own colours.
pub fn paint(body: &mut BodyMeshData, coat: &Coat, anatomy: &Anatomy, level: LodLevel) {
    body.paint.clear();
    if coat.markings.is_empty() {
        return;
    }
    let count = body.positions.len();
    let mut paintable = vec![false; count];
    for slot in [MaterialSlot::Skin, MaterialSlot::Hard] {
        for &i in body.slot_triangles(slot).flatten() {
            if let Some(p) = paintable.get_mut(i.idx()) {
                *p = true;
            }
        }
    }
    let model = anatomy.skeleton.rest_model();
    let (low, high) = body.positions.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    let height = (high - low).max(1e-3);
    let masks: Vec<_> = coat.markings.iter().map(|m| m.regions.mask(anatomy)).collect();
    // Each vertex's bone, and how far along that bone it lies (in bone lengths, unclamped).
    let placed: Vec<Option<(BoneId, f32, f32)>> = (0..count)
        .map(|i| {
            let (true, pos, data) = (*paintable.get(i)?, *body.positions.get(i)?, *body.mob_data.get(i)?) else { return None };
            let bone = MobVertexData::unpack(data).primary_bone;
            let (full, along_bone) = along_bone(anatomy, &model, level, bone, pos)?;
            Some((full, along_bone, along_part(anatomy, full, along_bone)))
        })
        .collect();
    // The reach of every bone's surface along it, for `tip`.
    let mut reach: Vec<(f32, f32)> = vec![(f32::MAX, f32::MIN); anatomy.skeleton.len()];
    for &(bone, at, _) in placed.iter().flatten() {
        if let Some(r) = reach.get_mut(bone.index()) {
            *r = (r.0.min(at), r.1.max(at));
        }
    }
    // (colour × weight, weight): premultiplied, so smoothing blends edges toward what's under.
    let mut paint: Vec<Vec4> = (0..count)
        .map(|i| {
            let (Some(Some((bone, on_bone, along))), Some(&pos), Some(&normal), Some(data)) =
                (placed.get(i).copied(), body.positions.get(i), body.normals.get(i), body.mob_data.get(i))
            else {
                return Vec4::ZERO;
            };
            let region = MobVertexData::unpack(*data).region;
            let tip = reach.get(bone.index()).map_or(0.0, |&(lo, hi)| ((on_bone - lo) / (hi - lo).max(1e-4)).clamp(0.0, 1.0));
            let at = At { pos, normal, height: (pos.y - low) / height, along, tip };
            let mut rgb = Vec3::ZERO;
            let mut alpha = 0.0f32;
            for (m, mask) in coat.markings.iter().zip(&masks) {
                if !mask.contains(region) {
                    continue;
                }
                let w = m.strength.clamp(0.0, 1.0) * m.weight(&at);
                if w <= 0.0 {
                    continue;
                }
                // Over what's there: c' = mix(c, colour, w) with c = mix(coat, rgb, alpha).
                let color = super::linear(m.color, 1.0).truncate();
                let next = (1.0 - w).mul_add(alpha, w);
                rgb = ((1.0 - w) * alpha * rgb + w * color) / next.max(1e-6);
                alpha = next;
            }
            (rgb * alpha).extend(alpha)
        })
        .collect();
    // Skin adjacency.
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); count];
    for &[a, b, c] in body.slot_triangles(MaterialSlot::Skin) {
        for (a, b) in [(a, b), (b, c), (c, a)] {
            if b.idx() >= count || neighbours.get(a.idx()).is_none_or(|ns| ns.contains(&b)) {
                continue;
            }
            if let Some(ns) = neighbours.get_mut(a.idx()) {
                ns.push(b);
            }
            if let Some(ns) = neighbours.get_mut(b.idx()) {
                ns.push(a);
            }
        }
    }
    for _ in 0..PAINT_SMOOTHING {
        let before = paint.clone();
        for (own, ns) in paint.iter_mut().zip(&neighbours) {
            if ns.is_empty() {
                continue;
            }
            let mean = ns.iter().filter_map(|j| before.get(j.idx())).sum::<Vec4>() / ns.len().to_f32();
            *own = (*own + mean) * 0.5;
        }
    }
    body.paint = paint.iter().map(|v| if v.w > 1e-4 { (v.truncate() / v.w).extend(v.w).to_array() } else { [0.0; 4] }).collect();
}

/// Where a vertex is, for the masks: position and normal (rest pose, model space), height as a
/// fraction of the body's, and where it lies along its part and toward its bone's tip (0..1).
#[derive(Clone, Copy, Debug)]
struct At {
    pos: Vec3,
    normal: Vec3,
    height: f32,
    along: f32,
    tip: f32,
}

impl Marking {
    /// The product of the masks at a vertex.
    fn weight(&self, at: &At) -> f32 {
        let At { pos, normal, height, along, tip } = *at;
        let mut w = 1.0;
        if let Some(facing) = self.facing {
            w *= ramp(facing.from, facing.to, normal.dot(facing.dir.normalize_or(Vec3::NEG_Y)));
        }
        if let Some((from, to)) = self.height {
            w *= ramp(from, to, height);
        }
        if let Some((from, to)) = self.along {
            w *= ramp(from, to, along);
        }
        if let Some((from, to)) = self.tip {
            w *= ramp(from, to, tip);
        }
        if let Some(bands) = self.bands {
            // Distance from the middle of the nearest band, in periods (0..0.5).
            let phase = along.mul_add(bands.count.max(0.0), -bands.offset).rem_euclid(1.0);
            let from_mid = (phase - 0.5).abs();
            let half = bands.width.clamp(0.0, 1.0) * 0.5;
            let soft = bands.soft.max(1e-3) * 0.5;
            w *= 1.0 - smoothstep(half - soft, half + soft, from_mid);
        }
        if let Some(pt) = self.patches {
            let v = fbm(pos / pt.size.max(1e-3), pt.seed);
            let soft = pt.soft.max(1e-3);
            // `cover` of the surface lies above the threshold.
            let threshold = 1.0 - pt.cover.clamp(0.0, 1.0);
            w *= smoothstep(threshold - soft, threshold + soft, v);
        }
        w
    }
}

/// `smoothstep(from, to, x)`, or its mirror when `from > to`.
fn ramp(from: f32, to: f32, x: f32) -> f32 {
    if from <= to { smoothstep(from, to, x) } else { 1.0 - smoothstep(to, from, x) }
}

/// The full-skeleton bone of `bone` (crowd levels index the crowd skeleton), and how far along it
/// `p` lies in bone lengths (unclamped: a snout reaches past its skull bone's end).
fn along_bone(anatomy: &Anatomy, model: &[Affine3A], level: LodLevel, bone: BoneId, p: Vec3) -> Option<(BoneId, f32)> {
    let bone = if level == LodLevel::Hero { bone } else { *anatomy.skeleton.crowd.bones.get(bone.index())? };
    let def = anatomy.skeleton.get(bone)?;
    let m = model.get(bone.index())?;
    let axis = Vec3::from(m.matrix3.y_axis);
    Some((bone, (p - Vec3::from(m.translation)).dot(axis) / (axis.length_squared() * def.length.max(1e-4))))
}

/// Where a point `along_bone` bone lengths along `bone` lies along the part the bone belongs to:
/// 0 at its first bone's start, 1 at its last bone's tip.
fn along_part(anatomy: &Anatomy, bone: BoneId, along_bone: f32) -> f32 {
    let Some(range) = anatomy.skeleton.get(bone).and_then(|def| anatomy.parts.get(usize::from(def.part))).map(|part| part.bones.clone())
    else {
        return 0.0;
    };
    let span = f32::from(range.end.saturating_sub(range.start).max(1));
    ((f32::from(bone.0.saturating_sub(range.start)) + along_bone.clamp(0.0, 1.0)) / span).clamp(0.0, 1.0)
}

/// Fractal value noise in 0..1 (three octaves).
fn fbm(p: Vec3, seed: u32) -> f32 {
    let mut sum = 0.0f32;
    let mut amp = 0.5f32;
    let mut q = p;
    for o in 0..3u32 {
        sum = amp.mul_add(value_noise(q, seed.wrapping_add(o.wrapping_mul(0x9e37))), sum);
        q = q * 2.03 + Vec3::splat(17.1);
        amp *= 0.5;
    }
    sum / 0.875
}

/// Trilinear value noise in 0..1 on the integer lattice.
fn value_noise(p: Vec3, seed: u32) -> f32 {
    let i = p.floor();
    let f = p - i;
    let u = f * f * (Vec3::splat(3.0) - 2.0 * f);
    let corner = |dx: f32, dy: f32, dz: f32| lattice(i + Vec3::new(dx, dy, dz), seed);
    let x00 = corner(0.0, 0.0, 0.0).lerp(corner(1.0, 0.0, 0.0), u.x);
    let x10 = corner(0.0, 1.0, 0.0).lerp(corner(1.0, 1.0, 0.0), u.x);
    let x01 = corner(0.0, 0.0, 1.0).lerp(corner(1.0, 0.0, 1.0), u.x);
    let x11 = corner(0.0, 1.0, 1.0).lerp(corner(1.0, 1.0, 1.0), u.x);
    x00.lerp(x10, u.y).lerp(x01.lerp(x11, u.y), u.z)
}

/// A hash of an integer lattice point in 0..1.
fn lattice(corner: Vec3, seed: u32) -> f32 {
    #[allow(clippy::as_conversions, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let [cx, cy, cz] = corner.to_array().map(|v| v as i32 as u32);
    let mut h = seed ^ cx.wrapping_mul(0x8da6_b343) ^ cy.wrapping_mul(0xd816_3841) ^ cz.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    #[allow(clippy::as_conversions, clippy::cast_precision_loss)]
    let v = (h >> 8) as f32 / 16_777_216.0;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{RegionKind, build_src};
    use crate::surface::{AdaptiveContour, SurfaceSettings, mesh_lod};

    /// The darner's coat on its body: the abdomen is blue, darker along the top (the dorsal
    /// stripe) and ringed in black.
    #[test]
    fn markings_paint_the_darner() {
        let anatomy = build_src(include_str!("../../assets/mobs/darner.body.ron"));
        let coat: Coat = ron::from_str(include_str!("../../assets/coats/green_darner.coat.ron")).unwrap_or_default();
        assert!(!coat.markings.is_empty(), "the coat parses with its markings");
        let level = LodLevel::Hero;
        let settings = SurfaceSettings::default();
        let Ok(mut body) = mesh_lod(&AdaptiveContour, &anatomy, level, settings.get(level), 0) else { panic!("meshes") };
        paint(&mut body, &coat, &anatomy, level);
        assert_eq!(body.paint.len(), body.positions.len());
        let tail = |i: usize| {
            let kind = body.mob_data.get(i).and_then(|d| anatomy.regions.get(MobVertexData::unpack(*d).region)).map(|r| r.kind);
            matches!(kind, Some(RegionKind::TailBase | RegionKind::TailTip))
        };
        let lum = |p: [f32; 4]| Vec3::from_slice(&p).dot(Vec3::new(0.2126, 0.7152, 0.0722));
        let (mut dark, mut blue, mut n) = (0, 0, 0);
        for (_, p) in body.paint.iter().enumerate().filter(|(i, _)| tail(*i)) {
            n += 1;
            if p[3] > 0.5 && lum(*p) < 0.02 {
                dark += 1;
            } else if p[3] > 0.5 && p[2] > p[1] {
                blue += 1;
            }
        }
        assert!(n > 0 && blue > n / 4 && dark > n / 10, "abdomen: {n} vertices, {dark} dark, {blue} blue");
    }

    #[test]
    fn ramps_fade_both_ways() {
        assert!(ramp(0.2, 0.4, 0.1) < 1e-6);
        assert!((ramp(0.2, 0.4, 0.5) - 1.0).abs() < 1e-6);
        assert!((ramp(0.4, 0.2, 0.1) - 1.0).abs() < 1e-6);
        assert!(ramp(0.4, 0.2, 0.5) < 1e-6);
    }

    #[test]
    fn bands_ring_the_part() {
        let m = Marking { bands: Some(Bands { count: 2.0, width: 0.4, soft: 0.02, offset: 0.0 }), ..default() };
        let w = |t: f32| m.weight(&At { pos: Vec3::ZERO, normal: Vec3::Y, height: 0.5, along: t, tip: 0.5 });
        // Band middles at 0.25 and 0.75 of the part, gaps at 0, 0.5 and 1.
        assert!(w(0.25) > 0.99 && w(0.75) > 0.99, "{} {}", w(0.25), w(0.75));
        assert!(w(0.5) < 0.01 && w(0.02) < 0.01, "{} {}", w(0.5), w(0.02));
    }

    #[test]
    fn patches_cover_about_their_share() {
        let m = Marking { patches: Some(Patches { size: 0.1, cover: 0.4, soft: 0.01, seed: 3 }), ..default() };
        let samples = 4000;
        let covered = (0..samples)
            .filter(|i| {
                let t = i.to_f32();
                let p = Vec3::new((t * 0.618_034).fract(), (t * 0.414_214).fract(), (t * 0.732_051).fract()) * 2.0;
                m.weight(&At { pos: p, normal: Vec3::Y, height: 0.5, along: 0.5, tip: 0.5 }) > 0.5
            })
            .count();
        let share = covered.to_f32() / samples.to_f32();
        assert!((0.15..0.65).contains(&share), "patches cover {share}");
    }
}
