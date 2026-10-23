//! Soft tissue (`soft-tissue` feature): fat pads and muscle bellies that jiggle, sag with gravity
//! and tense under load.
//!
//! Each fat or muscle stamp becomes a [`SoftRegion`]: one damped spring that carries a point of
//! tissue behind its bone. The spring is driven by its anchor's acceleration, rests where gravity
//! pulls it (relative to the rest pose, which is already sculpted sagging), and a muscle's spring
//! stiffens and swells as it works. Everything that does not change frame to frame is decided when
//! the body is generated and kept in the [`GeneratedBody`](crate::mob::GeneratedBody):
//!
//! - the regions and their tuning ([`SoftTissue::of`], from the stamps' size and kind),
//! - each skin vertex's share of up to two regions ([`soft_weights`], a mesh attribute),
//! - crowd tiers' jiggle: the springs are run while the crowd motion is baked, and recorded as one
//!   extra matrix per region after the crowd skeleton's (`motion::bake`). Crowds cost no CPU.
//!
//! On the GPU a region is a pair of joints: the "soft joint" is its bone's skin matrix moved by the
//! spring's offset, so `S·p − B·p` is that offset wherever the vertex is (`deform.wesl`). Hero mobs
//! get one joint entity per region, a child of its bone (`mob::spawn`).

use std::f32::consts::{PI, TAU};

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;

use crate::anatomy::{Anatomy, BoneId, CROWD_SLOT, LodLevel, PrimLayer, PrimOp, PrimShape, WATER};
use crate::num::{Idx, ToF32};
use crate::surface::{BodyMeshData, SoftVertex, primitive_parts};

/// Most regions on a hero body (the heaviest are kept).
pub const MAX_HERO_REGIONS: usize = 32;
/// Most regions on a crowd body, within what the crowd skeleton leaves of its slot.
pub const MAX_CROWD_REGIONS: usize = 8;

/// A region's weight reaches this far past its stamp (multiple of its radii).
const SPREAD: f32 = 1.5;
/// Fat pads ring at this rate (Hz) at the reference size, muscles at the other when relaxed.
const FAT_HZ: f32 = 3.2;
const MUSCLE_HZ: f32 = 6.5;
/// Damping ratios: fat wobbles a few times, a relaxed muscle once.
const FAT_DAMPING: f32 = 0.16;
const MUSCLE_DAMPING: f32 = 0.3;
/// Footprint size (metres, geometric mean of the long radii) the rates above are for; bigger pads
/// are slower, smaller ones quicker.
const REFERENCE_SIZE: f32 = 0.08;
/// A working muscle rings this much faster (×) and this close to critically damped.
const TENSE_STIFFEN: f32 = 1.4;
const TENSE_DAMPING: f32 = 0.75;
/// Anchor accelerations past this (m/s², per metre of scale) are teleports or hitches: ignored.
const MAX_ACCEL: f32 = 120.0;
/// How quickly activation builds and lets go (per second).
const TENSE_RATE: f32 = 18.0;
const RELAX_RATE: f32 = 6.0;
const GRAVITY: f32 = 9.81;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftKind {
    Fat,
    Muscle,
}

/// How a region's tissue sits on the body, which decides how its skin follows the spring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionShape {
    /// A raised stamp: the skin passes through its middle, and follows it less towards the rim.
    Raise,
    /// A body of tissue (a mass): all of its skin moves with it, fading only past its edge.
    Mass,
    /// A teardrop: like a mass, but held at its top where it leaves the body, so the bulb swings.
    Drop,
}

/// One spring of tissue (see the module docs). Positions are rest-pose model space unless noted.
#[derive(Clone, Debug)]
pub struct SoftRegion {
    pub kind: SoftKind,
    pub shape: RegionShape,
    pub bone: BoneId,
    /// The stamp's centre on its bone (bone space).
    pub anchor: Vec3,
    /// Outward surface normal at the stamp (bone space).
    pub normal: Vec3,
    /// Gravity's direction in the bone's frame at rest: the sculpted shape already hangs this way.
    pub rest_down: Vec3,
    /// Model space → stamp space with the footprint's radii as units (for the vertex weights).
    pub footprint: Affine3A,
    /// Kilograms of tissue (ranks regions for the budgets).
    pub mass: f32,
    /// Ringing rate, Hz (relaxed), and damping ratio.
    pub hz: f32,
    pub damping: f32,
    /// Furthest the tissue moves from its rest place (metres).
    pub limit: f32,
    /// How far it hangs when its bone turns fully against gravity's rest direction (metres).
    pub sag: f32,
    /// How far a muscle swells out at full activation (metres; 0 for fat).
    pub bulge: f32,
    /// The leg carrying this muscle, whose stance load tenses it.
    pub leg: Option<u8>,
}

/// A body's soft tissue: every hero region, and the ones crowd tiers keep.
#[derive(Clone, Debug, Default)]
pub struct SoftTissue {
    /// Heaviest first.
    pub hero: Vec<SoftRegion>,
    /// Crowd-tier regions (stamps shown at `Near`), heaviest first, within the crowd slot.
    pub crowd: Vec<SoftRegion>,
}

impl SoftTissue {
    /// Regions for every fat and muscle stamp of `anatomy`.
    #[must_use]
    pub fn of(anatomy: &Anatomy) -> Self {
        let skeleton = &anatomy.skeleton;
        let rest = skeleton.rest_model();
        let parts = primitive_parts(anatomy);
        let mut all: Vec<(LodLevel, SoftRegion)> = anatomy
            .primitives
            .iter()
            .zip(&parts)
            .filter_map(|(p, part)| {
                let kind = match p.layer {
                    PrimLayer::Fat => SoftKind::Fat,
                    PrimLayer::Muscle => SoftKind::Muscle,
                    _ => return None,
                };
                // Raised stamps (a displacement of `height`), masses (a blended ellipsoid, as thick as
                // it is deep along the normal) and drops (a round cone: the ellipsoid round it, its
                // bulb's radius thick).
                let (radii, centre, height, shape) = match (&p.shape, p.op) {
                    (PrimShape::Ellipsoid { radii }, PrimOp::Displace { height }) if height > 0.0 => (*radii, Vec3::ZERO, height, RegionShape::Raise),
                    (PrimShape::Ellipsoid { radii }, PrimOp::Union) => (*radii, Vec3::ZERO, radii.z, RegionShape::Mass),
                    (PrimShape::RoundCone { r0, r1, len }, PrimOp::Union) => {
                        (Vec3::new(*r1, 0.5 * (len + r0 + r1), *r1), Vec3::Y * 0.5 * (len + r1 - r0), *r1, RegionShape::Drop)
                    }
                    _ => return None,
                };
                let solid = shape != RegionShape::Raise;
                // Only stamps are soft tissue; a part's own bulge (a torso's belly) is body.
                let tune = p.soft?;
                let radii = &radii;
                let lod = p.lod.min(anatomy.parts.get(part.idx())?.lod);
                let bone_model = *rest.get(p.bone.index())?;
                let model = bone_model * p.local.compute_affine() * Affine3A::from_translation(centre);
                let scale = (0..3).map(|c| model.matrix3.col(c).length()).fold(f32::INFINITY, f32::min);
                let (r, h) = (*radii * scale, height * scale);
                let size = (r.x * r.y).sqrt().max(1e-3);
                let to_bone = bone_model.inverse();
                let normal = Vec3::from(model.matrix3.z_axis).normalize_or_zero();
                // A raised stamp's volume is the displacement profile (1 − q²)² over the footprint;
                // a mass is half sunk in the body.
                let mass = if solid { 2.0 / 3.0 * PI * r.x * r.y * r.z } else { PI / 3.0 * r.x * r.y * h } * WATER;
                let pace = (REFERENCE_SIZE / size).powf(0.35).clamp(0.6, 1.6);
                let (hz, damping, limit, bulge) = match kind {
                    // A mass moves within a share of its own thickness.
                    SoftKind::Fat if solid => (FAT_HZ * pace, FAT_DAMPING, 0.12f32.mul_add(size, 0.35 * h), 0.0),
                    SoftKind::Fat => (FAT_HZ * pace, FAT_DAMPING, 0.12f32.mul_add(size, h), 0.0),
                    SoftKind::Muscle => {
                        let bulge = 0.02f32.mul_add(size, 0.4 * h);
                        // Room to jiggle on top of its full swell.
                        (MUSCLE_HZ * pace, MUSCLE_DAMPING, 0.06f32.mul_add(size, 0.5f32.mul_add(h, bulge)), bulge)
                    }
                };
                // The stamp's own tuning on top.
                let (hz, damping, limit) = (hz * tune.firmness.max(0.05), (damping * tune.damping).clamp(0.02, 0.95), limit * tune.travel.max(0.0));
                let omega = TAU * hz;
                let sag = match kind {
                    SoftKind::Fat => (GRAVITY / (omega * omega)).min(0.5 * limit),
                    SoftKind::Muscle => 0.0,
                };
                let leg = (kind == SoftKind::Muscle)
                    .then(|| anatomy.legs.iter().position(|l| l.chain.contains(&p.bone)))
                    .flatten()
                    .and_then(|i| u8::try_from(i).ok());
                // Unit footprint: radii as units, a little past the stamp.
                let footprint = Affine3A::from_scale((*radii * SPREAD).recip()) * model.inverse();
                Some((
                    lod,
                    SoftRegion {
                        kind,
                        shape,
                        bone: p.bone,
                        anchor: to_bone.transform_point3(Vec3::from(model.translation)),
                        normal: to_bone.transform_vector3(normal).normalize_or_zero(),
                        rest_down: to_bone.transform_vector3(Vec3::NEG_Y).normalize_or_zero(),
                        footprint,
                        mass,
                        hz,
                        damping,
                        limit,
                        sag,
                        bulge,
                        leg,
                    },
                ))
            })
            .collect();
        all.sort_by(|a, b| b.1.mass.total_cmp(&a.1.mass));
        let room = MAX_CROWD_REGIONS.min(CROWD_SLOT.saturating_sub(skeleton.crowd.bones.len()));
        let crowd = whole_pairs(all.iter().filter(|(lod, _)| *lod >= LodLevel::Near).map(|(_, r)| r.clone()).collect(), room);
        let hero = whole_pairs(all.into_iter().map(|(_, r)| r).collect(), MAX_HERO_REGIONS);
        Self { hero, crowd }
    }

    /// The regions a mesh at `level` carries (none at `Far`: too small on screen to jiggle).
    #[must_use]
    pub fn at(&self, level: LodLevel) -> &[SoftRegion] {
        match level {
            LodLevel::Hero => &self.hero,
            LodLevel::Near => &self.crowd,
            LodLevel::Far => &[],
        }
    }
}

/// The first `n` of `regions` (heaviest first), without splitting a mirrored pair at the cut: a
/// pad on one side only would jiggle lopsided. Mirrored copies weigh the same, so an odd count of
/// the cut's weight among those kept means a pair was split.
fn whole_pairs(mut regions: Vec<SoftRegion>, n: usize) -> Vec<SoftRegion> {
    if regions.len() > n {
        let cut = regions.get(n).map_or(0.0, |r| r.mass);
        regions.truncate(n);
        let same = regions.iter().rev().take_while(|r| (r.mass - cut).abs() <= 1e-4 * cut).count();
        if same % 2 == 1 {
            regions.pop();
        }
    }
    regions
}

/// Each skin vertex's two strongest regions at `level`, with the joints the shader reads: the soft
/// joint (after the skeleton's joints: the full skeleton at `Hero`, the crowd one below) and its bone.
#[must_use]
pub fn soft_weights(mesh: &BodyMeshData, anatomy: &Anatomy, tissue: &SoftTissue, level: LodLevel) -> Vec<SoftVertex> {
    let regions = tissue.at(level);
    let mut out = vec![SoftVertex::default(); mesh.positions.len()];
    if regions.is_empty() {
        return out;
    }
    let skeleton = &anatomy.skeleton;
    let joints: Vec<Option<(u16, u16)>> = regions
        .iter()
        .enumerate()
        .map(|(k, r)| {
            let (first, bone) = if level == LodLevel::Hero {
                (skeleton.len(), r.bone.0)
            } else {
                (skeleton.crowd.bones.len(), *skeleton.crowd.map.get(r.bone.index())?)
            };
            Some((u16::try_from(first + k).ok()?, u16::from(bone)))
        })
        .collect();
    let skin = mesh.skin_vertex_mask();
    for ((v, p), _) in out.iter_mut().zip(&mesh.positions).zip(&skin).filter(|(_, s)| **s) {
        // The three strongest; the third is taken off the other two so that a region dropping out
        // of the top two fades rather than pops.
        let mut top = [(0.0f32, 0usize); 3];
        for (k, r) in regions.iter().enumerate() {
            let w = region_weight(r, r.footprint.transform_point3a(Vec3A::from(*p)));
            if w > 0.0 {
                // Insertion into the sorted three: the weakest falls off the end.
                let mut entry = (w, k);
                for t in &mut top {
                    if entry.0 > t.0 {
                        std::mem::swap(t, &mut entry);
                    }
                }
            }
        }
        let [(w0, k0), (w1, k1), (w2, _)] = top;
        let (w0, w1) = (w0 - w2, w1 - w2);
        // Overlapping regions share a vertex rather than each moving it fully.
        let norm = (w0 + w1).max(1.0);
        for ((joint, weight), (w, k)) in v.joints.iter_mut().zip(&mut v.weights).zip([(w0, k0), (w1, k1)]) {
            if let (true, Some(Some((soft, bone)))) = (w > 1e-4, joints.get(k)) {
                *joint = u32::from(*soft) | u32::from(*bone) << 16;
                *weight = w / norm;
            }
        }
    }
    out
}

/// How much of region `r`'s motion the skin at footprint point `q` takes.
///
/// A raised stamp's skin passes through the footprint's middle and follows less towards its rim.
/// A mass's or drop's skin is its own outside, at `1 / SPREAD` of the footprint: everything within
/// that moves fully, fading out past it. A drop is also held at its top (local −y, where it leaves
/// the body), so its top stays and its bulb swings.
fn region_weight(r: &SoftRegion, q: Vec3A) -> f32 {
    let q2 = q.length_squared();
    if q2 >= 1.0 {
        return 0.0;
    }
    let s = match r.shape {
        RegionShape::Raise => 1.0 - q2,
        RegionShape::Mass | RegionShape::Drop => ((1.0 - q2) / (1.0 - 1.0 / (SPREAD * SPREAD))).min(1.0),
    };
    let held = if r.shape == RegionShape::Drop { smoothstep(-1.0, 0.25, q.y * SPREAD) } else { 1.0 };
    s * s * held
}

/// Hermite step from 0 at `lo` to 1 at `hi`.
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * 2.0f32.mul_add(-t, 3.0)
}

/// One region's spring, frame to frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct SoftSpring {
    /// Tissue offset from its anchor (world) and its rate of change.
    pub offset: Vec3,
    /// The offset as shown: eased into the region's limit rather than stopped dead at it.
    pub shown: Vec3,
    pub velocity: Vec3,
    anchor: Vec3,
    anchor_velocity: Vec3,
    /// Muscle activation, 0 relaxed … 1 working hard.
    pub activation: f32,
    /// The bone's local rotation last frame (for how hard its joint is working).
    turn: Quat,
    /// Frames seen since a reset: velocities need one, accelerations two.
    primed: u8,
}

/// Every region's spring on one body (one per region of the list it was made for).
#[derive(Component, Clone, Debug, Default)]
pub struct SoftState {
    pub springs: Vec<SoftSpring>,
    /// The offsets in each region's bone frame, for the soft joints (hero tier).
    pub local: Vec<Vec3>,
    /// Scratch: the bones' world transforms this frame.
    world: Vec<Affine3A>,
}

impl SoftState {
    #[must_use]
    pub fn new(regions: usize) -> Self {
        Self { springs: vec![SoftSpring::default(); regions], local: vec![Vec3::ZERO; regions], world: Vec::new() }
    }
}

/// Steps every spring of `regions` over `dt`, for the pose `local` under `root`, with `planted`
/// feet (in `Anatomy::legs` order). Leaves world offsets in the springs and bone-frame ones in
/// `state.local`.
pub fn step(anatomy: &Anatomy, regions: &[SoftRegion], root: Affine3A, local: &[Transform], planted: &[bool], dt: f32, state: &mut SoftState) {
    let skeleton = &anatomy.skeleton;
    let SoftState { springs, local: out, world } = state;
    springs.resize(regions.len(), SoftSpring::default());
    out.resize(regions.len(), Vec3::ZERO);
    world.clear();
    for (i, b) in skeleton.bones.iter().enumerate() {
        let l = local.get(i).unwrap_or(&b.rest).compute_affine();
        let parent = b.parent.and_then(|p| world.get(p.index()).copied()).unwrap_or(root);
        world.push(parent * l);
    }
    let scale = root.matrix3.x_axis.length().max(1e-6);
    // A planted foot's share of the weight: 1 with half the feet down, less with more.
    let down = planted.iter().filter(|p| **p).count();
    let load = |leg: u8| match planted.get(usize::from(leg)) {
        Some(true) => (planted.len().to_f32() / (2.0 * down.max(1).to_f32())).min(1.0),
        _ => 0.0,
    };
    // Paused: the tissue holds where it is. A long frame (a hitch, a slow baked gait) steps as a
    // tenth of a second rather than resetting: the exact step is stable at any length.
    if dt <= 1e-5 {
        return;
    }
    let dt = dt.min(0.1);
    for ((r, s), out) in regions.iter().zip(springs.iter_mut()).zip(out.iter_mut()) {
        let Some(bone) = world.get(r.bone.index()) else { continue };
        let anchor = bone.transform_point3(r.anchor);
        let turn = local.get(r.bone.index()).map_or(Quat::IDENTITY, |t| t.rotation);
        if s.primed == 0 || anchor.distance(s.anchor) > scale {
            *s = SoftSpring { anchor, turn, ..default() };
        }
        // The anchor's acceleration, from its last two moves.
        let mut accel = Vec3::ZERO;
        if s.primed > 0 {
            let velocity = (anchor - s.anchor) / dt;
            if s.primed > 1 {
                accel = ((velocity - s.anchor_velocity) / dt).clamp_length_max(MAX_ACCEL * scale);
            }
            s.anchor_velocity = velocity;
        }
        s.anchor = anchor;
        s.primed = s.primed.saturating_add(1).min(2);
        // How hard a muscle works: its leg's share of the weight, and how fast its bone turns.
        let effort = if r.kind == SoftKind::Muscle && s.primed > 1 {
            let spin = 2.0 * turn.dot(s.turn).abs().min(1.0).acos() / dt;
            r.leg.map_or(0.0, load).mul_add(0.8, (spin * 0.08).min(0.6)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        s.turn = turn;
        let rate = if effort > s.activation { TENSE_RATE } else { RELAX_RATE };
        s.activation = (effort - s.activation).mul_add(1.0 - (-rate * dt).exp(), s.activation);
        let a = s.activation;
        let omega = TAU * r.hz * TENSE_STIFFEN.mul_add(a, 1.0 - a);
        let zeta = (TENSE_DAMPING - r.damping).mul_add(a, r.damping);
        // Where the tissue rests: hanging from where gravity pulls now rather than at rest, and
        // out along the normal as the muscle swells.
        let rotate = |v: Vec3| bone.transform_vector3(v).normalize_or_zero();
        let target = (Vec3::NEG_Y - rotate(r.rest_down)) * (r.sag * scale) + rotate(r.normal) * (r.bulge * scale * a);
        // The anchor accelerating drags the tissue the other way: it rests behind it.
        let rest = target - accel / (omega * omega);
        let (y, v) = damped(s.offset - rest, s.velocity, omega, zeta, dt);
        s.offset = rest + y;
        s.velocity = v;
        let limit = r.limit * scale;
        let len = s.offset.length();
        if len > 2.0 * limit {
            // Held well past the limit: drop the motion that pushes further.
            let dir = s.offset / len;
            s.offset = dir * (2.0 * limit);
            s.velocity -= dir * s.velocity.dot(dir).max(0.0);
        }
        // Shown eased: linear while small, approaching the limit smoothly (no visible stop).
        let len = s.offset.length();
        s.shown = if len > 1e-9 { s.offset * (limit * (len / limit).tanh() / len) } else { Vec3::ZERO };
        *out = (bone.matrix3.inverse() * Vec3A::from(s.shown)).into();
    }
}

/// Exact step of a damped spring (rate `omega`, ratio `zeta` < 1) from offset `y` and velocity
/// `v`: stable whatever `dt`, so tight muscles need no substeps.
fn damped(y: Vec3, v: Vec3, omega: f32, zeta: f32, dt: f32) -> (Vec3, Vec3) {
    let zeta = zeta.clamp(0.0, 0.95);
    let wd = omega * zeta.mul_add(-zeta, 1.0).sqrt();
    let decay = (-zeta * omega * dt).exp();
    let (sin, cos) = (wd * dt).sin_cos();
    let y1 = (y * cos + (v + y * (zeta * omega)) * (sin / wd)) * decay;
    let v1 = (v * cos - (y * (omega * omega) + v * (zeta * omega)) * (sin / wd)) * decay;
    (y1, v1)
}

/// The soft matrices for one baked crowd frame, appended to `out`: each region's bone skin matrix
/// (from `skin`, the crowd skeleton's) moved by its spring's offset (world, into mob space by
/// `root`).
pub fn crowd_matrices(anatomy: &Anatomy, regions: &[SoftRegion], root: Affine3A, state: &SoftState, skin: &[[f32; 16]], out: &mut Vec<[f32; 16]>) {
    let map = &anatomy.skeleton.crowd.map;
    let to_mob = root.matrix3.inverse();
    for (r, spring) in regions.iter().zip(&state.springs) {
        let Some(mut m) = map.get(r.bone.index()).and_then(|c| skin.get(c.idx())).copied() else { continue };
        let off = to_mob * Vec3A::from(spring.shown);
        m[12] += off.x;
        m[13] += off.y;
        m[14] += off.z;
        out.push(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::motion::{BAKE_FRAMES, Gait, bake_motion};
    use crate::surface::{AdaptiveContour, MaterialSlot, ProceduralHardParts, SurfaceSettings, mesh_lod};

    fn body(ron: &str) -> Anatomy {
        let plan: BodyPlan = ron::from_str(ron).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    fn quadruped() -> Anatomy {
        body(include_str!("../assets/mobs/quadruped.body.ron"))
    }

    #[test]
    fn regions_fit_their_budgets() {
        for (name, ron) in [
            ("quadruped", include_str!("../assets/mobs/quadruped.body.ron")),
            ("biped", include_str!("../assets/mobs/biped.body.ron")),
            ("dragon", include_str!("../assets/mobs/dragon.body.ron")),
            ("thick", include_str!("../assets/mobs/thick.body.ron")),
        ] {
            let a = body(ron);
            let t = SoftTissue::of(&a);
            assert!(t.hero.iter().any(|r| r.kind == SoftKind::Fat), "{name}: no fat");
            assert!(t.hero.iter().any(|r| r.kind == SoftKind::Muscle), "{name}: no muscle");
            assert!(t.hero.len() <= MAX_HERO_REGIONS && a.skeleton.len() + t.hero.len() <= 256, "{name}");
            assert!(a.skeleton.crowd.bones.len() + t.crowd.len() <= CROWD_SLOT, "{name}: crowd slot");
            assert!(t.hero.windows(2).all(|w| w[0].mass >= w[1].mass), "{name}: heaviest first");
            for r in &t.hero {
                assert!(r.hz > 1.0 && r.hz < 15.0 && r.limit > 0.0 && r.sag <= r.limit, "{name}: {r:?}");
            }
        }
    }

    #[test]
    fn a_drop_swings_its_bulb_and_holds_its_top() {
        let a = body(include_str!("../assets/mobs/thick.body.ron"));
        let t = SoftTissue::of(&a);
        let drop = t.hero.iter().find(|r| r.shape == RegionShape::Drop).expect("the thick plan has drops");
        // Points on the drop's own outside (shape units: the footprint is SPREAD times larger).
        let at = |y: f32, z: f32| region_weight(drop, Vec3A::new(0.0, y, z) / SPREAD);
        // Skin on the bulb (low, out along the normal) moves fully; at the top, little.
        assert!(at(0.5, 0.85) > 0.9, "bulb {}", at(0.5, 0.85));
        assert!(at(-0.95, 0.25) < 0.15, "top {}", at(-0.95, 0.25));
    }

    #[test]
    fn stamp_tuning_reaches_the_springs() {
        let mut a = quadruped();
        let before = SoftTissue::of(&a);
        for p in a.primitives.iter_mut().filter(|p| p.layer == PrimLayer::Fat && p.soft.is_some()) {
            p.soft = Some(crate::anatomy::SoftTune { firmness: 0.5, damping: 0.5, travel: 2.0 });
        }
        let after = SoftTissue::of(&a);
        let fat = |t: &SoftTissue| t.hero.iter().find(|r| r.kind == SoftKind::Fat).cloned().unwrap();
        let (b, f) = (fat(&before), fat(&after));
        assert!(0.5f32.mul_add(-b.hz, f.hz).abs() < 1e-4 && 0.5f32.mul_add(-b.damping, f.damping).abs() < 1e-4);
        assert!(2.0f32.mul_add(-b.limit, f.limit).abs() < 1e-5);
        // Softer tissue hangs further (up to its limit).
        assert!(f.sag >= b.sag);
    }

    #[test]
    fn damped_step_is_exact_and_stable() {
        // Many small steps and one big one land in the same place, and a stiff spring at a long
        // frame doesn't blow up.
        let (y0, v0, w, z) = (Vec3::X, Vec3::Y * 2.0, 30.0, 0.2);
        let mut fine = (y0, v0);
        for _ in 0..100 {
            fine = damped(fine.0, fine.1, w, z, 0.001);
        }
        let coarse = damped(y0, v0, w, z, 0.1);
        assert!(fine.0.distance(coarse.0) < 1e-4 && fine.1.distance(coarse.1) < 1e-3);
        let mut s = (y0, v0);
        for _ in 0..200 {
            s = damped(s.0, s.1, 400.0, 0.05, 0.1);
        }
        assert!(s.0.length() < 1.0 && s.0.is_finite());
    }

    #[test]
    fn tissue_swings_and_settles() {
        let a = quadruped();
        let t = SoftTissue::of(&a);
        let mut state = SoftState::new(t.hero.len());
        let rest: Vec<Transform> = a.skeleton.bones.iter().map(|b| b.rest).collect();
        let planted = vec![true; a.legs.len()];
        let dt = 1.0 / 60.0;
        // Bob the body up and down, then hold it still.
        let mut peak = 0.0f32;
        for i in 0..240usize {
            let y = if i < 120 { (i.to_f32() * dt * 12.0).sin() * 0.05 } else { 0.0 };
            let root = Affine3A::from_translation(Vec3::Y * y);
            step(&a, &t.hero, root, &rest, &planted, dt, &mut state);
            peak = state.springs.iter().fold(peak, |m, s| m.max(s.offset.length()));
            for (r, s) in t.hero.iter().zip(&state.springs) {
                assert!(s.shown.length() <= r.limit * 1.001, "past its limit");
            }
        }
        assert!(peak > 1e-3, "nothing moved: {peak}");
        // Standing still for two seconds the fat settles back (muscles rest swollen by their load).
        for _ in 0..120 {
            step(&a, &t.hero, Affine3A::IDENTITY, &rest, &planted, dt, &mut state);
        }
        for (r, s) in t.hero.iter().zip(&state.springs).filter(|(r, _)| r.kind == SoftKind::Fat) {
            assert!(s.offset.length() < 0.05f32.mul_add(r.limit, 1e-4), "{r:?} still at {}", s.offset.length());
        }
    }

    #[test]
    fn fat_hangs_when_the_body_tips() {
        let a = quadruped();
        let t = SoftTissue::of(&a);
        let mut state = SoftState::new(t.hero.len());
        let rest: Vec<Transform> = a.skeleton.bones.iter().map(|b| b.rest).collect();
        // Rolled onto its side: the fat hangs towards the new down.
        let root = Affine3A::from_rotation_z(std::f32::consts::FRAC_PI_2);
        for _ in 0..600 {
            step(&a, &t.hero, root, &rest, &[], 1.0 / 60.0, &mut state);
        }
        let hung = t.hero.iter().zip(&state.springs).filter(|(r, s)| r.kind == SoftKind::Fat && s.offset.y < -0.5 * r.sag).count();
        assert!(hung > 0, "no fat hangs");
    }

    #[test]
    fn loaded_legs_tense_their_muscles() {
        let a = quadruped();
        let t = SoftTissue::of(&a);
        let rest: Vec<Transform> = a.skeleton.bones.iter().map(|b| b.rest).collect();
        let run = |planted: &[bool]| {
            let mut state = SoftState::new(t.hero.len());
            for _ in 0..60 {
                step(&a, &t.hero, Affine3A::IDENTITY, &rest, planted, 1.0 / 60.0, &mut state);
            }
            t.hero.iter().zip(&state.springs).filter(|(r, _)| r.leg == Some(0)).map(|(_, s)| s.activation).sum::<f32>()
        };
        let mut lifted = vec![true; a.legs.len()];
        lifted[0] = false;
        let (stance, swing) = (run(&vec![true; a.legs.len()]), run(&lifted));
        assert!(stance > 0.1 && swing < 0.01, "stance {stance}, swing {swing}");
    }

    #[test]
    fn weights_are_shared_and_point_at_soft_joints() {
        let a = quadruped();
        let t = SoftTissue::of(&a);
        let settings = SurfaceSettings::default();
        for level in [LodLevel::Hero, LodLevel::Near] {
            let mesh = mesh_lod(&AdaptiveContour, &ProceduralHardParts, &a, level, settings.get(level), 0).unwrap();
            let w = soft_weights(&mesh, &a, &t, level);
            let first = if level == LodLevel::Hero { a.skeleton.len() } else { a.skeleton.crowd.bones.len() };
            let regions = t.at(level).len();
            let moved = w.iter().filter(|v| v.weights[0] > 0.0).count();
            assert!(moved > mesh.positions.len() / 20, "{level:?}: {moved} of {}", mesh.positions.len());
            for v in &w {
                assert!(v.weights[0] + v.weights[1] <= 1.0 + 1e-5);
                for (j, wt) in v.joints.iter().zip(v.weights) {
                    if wt > 0.0 {
                        let soft = usize::try_from(*j & 0xffff).unwrap();
                        assert!((first..first + regions).contains(&soft), "{level:?}: joint {soft}");
                    }
                }
            }
        }
    }

    /// The weights survive splitting the body into one mesh per material, hero and crowd alike:
    /// the GPU meshes carry them.
    #[test]
    fn split_meshes_carry_the_weights() {
        let a = quadruped();
        let t = SoftTissue::of(&a);
        let settings = SurfaceSettings::default();
        for level in [LodLevel::Hero, LodLevel::Near] {
            let mut mesh = mesh_lod(&AdaptiveContour, &ProceduralHardParts, &a, level, settings.get(level), 0).unwrap();
            mesh.soft = soft_weights(&mesh, &a, &t, level);
            let split = if level == LodLevel::Hero { mesh.to_bevy_meshes() } else { mesh.to_crowd_meshes() };
            let skin = split.iter().find(|(slot, _)| *slot == MaterialSlot::Skin).map(|(_, m)| m).unwrap();
            assert!(skin.attribute(crate::surface::ATTRIBUTE_SOFT).is_some(), "{level:?} skin mesh lost its soft weights");
        }
    }

    #[test]
    fn crowd_clips_carry_the_springs() {
        let a = quadruped();
        let t = SoftTissue::of(&a);
        let m = bake_motion(&a, &Gait::for_legs(a.legs.len()));
        let bones = a.skeleton.crowd.bones.len();
        assert_eq!(m.idle.len(), bones + t.crowd.len());
        // Idle: each soft matrix is its bone's.
        for (r, soft) in t.crowd.iter().zip(&m.idle[bones..]) {
            assert_eq!(*soft, m.idle[usize::from(a.skeleton.crowd.map[r.bone.index()])]);
        }
        // Walking, the tissue moves off its bone in some frames.
        let clip = &m.clips[0];
        assert_eq!(clip.frames.len(), BAKE_FRAMES);
        let off = clip
            .frames
            .iter()
            .flat_map(|f| t.crowd.iter().zip(&f[bones..]).map(|(r, s)| {
                let b = f[usize::from(a.skeleton.crowd.map[r.bone.index()])];
                Vec3::new(s[12] - b[12], s[13] - b[13], s[14] - b[14]).length()
            }))
            .fold(0.0f32, f32::max);
        assert!(off > 1e-4, "baked tissue never moves: {off}");
    }
}

#[cfg(test)]
mod amplitude {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::motion::{Gait, bake_motion};

    /// How far tissue moves over a baked walk and run (prints; for tuning).
    #[test]
    #[ignore = "prints tuning numbers"]
    fn walk_amplitudes() {
        for (name, ron) in [("quadruped", include_str!("../assets/mobs/quadruped.body.ron")), ("biped", include_str!("../assets/mobs/biped.body.ron")), ("curvy", include_str!("../assets/mobs/curvy.body.ron")), ("thick", include_str!("../assets/mobs/thick.body.ron"))] {
            let plan: BodyPlan = ron::from_str(ron).unwrap();
            let a = build_anatomy(&apply_form(&plan, &Form::default()).unwrap(), &PartLodRules::default(), &PartAssets::default()).unwrap();
            let t = SoftTissue::of(&a);
            let m = bake_motion(&a, &Gait::for_legs(a.legs.len()));
            let bones = a.skeleton.crowd.bones.len();
            for (c, clip) in m.clips.iter().enumerate() {
                for (k, r) in t.crowd.iter().enumerate() {
                    let b = usize::from(a.skeleton.crowd.map[r.bone.index()]);
                    let offs: Vec<f32> = clip.frames.iter().map(|f| Vec3::new(f[bones + k][12] - f[b][12], f[bones + k][13] - f[b][13], f[bones + k][14] - f[b][14]).length()).collect();
                    let (lo, hi) = offs.iter().fold((f32::MAX, 0.0f32), |(l, h), o| (l.min(*o), h.max(*o)));
                    eprintln!("{name} mode {c} {:?} {} hz {:.1} limit {:.1}mm: offset {:.1}..{:.1} mm", r.kind, a.skeleton.bones[r.bone.index()].name, r.hz, r.limit * 1000.0, lo * 1000.0, hi * 1000.0);
                }
            }
        }
    }
}
