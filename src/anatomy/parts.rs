//! [`PartBuild`] implementations: the geometry of every part kind.
//!
//! Conventions (logical model frame): Y up, mob faces +Z, left = +X. Bones extend along local
//! +Y. Segmented parts keep local -Z toward the dorsal (top) side.

use std::f32::consts::FRAC_PI_2;

use bevy::prelude::*;
use smallvec::SmallVec;

use crate::num::ToF32;

use super::builder::{AxisSample, SkullFrame, frame_y_z};
use super::{
    AnatomyBuilder, AnatomyError, AttachPoint, AttachSlot, BoneId, BuildCx, ChainParams, ChainPart,
    ChainRole, CrossSection, DigitParams, FaceParams, ClothFollow, HandDef, HardParams, HeadParams, JointPlan, LimbEnd, LimbJoints, LimbParams, LimbRole,
    HardKind, LookDef, MassParams, MassShape, MuzzleParams, Orient, PartBuild, PartSlots, PrimLayer, PrimOp, PrimShape, Profile,
    SculptParams,
    RegionId, RegionKind, Side, SocketKind, SpringChainDef, Stance, TorsoParams, Weigh,
};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Piecewise-linear lookup in `(t, value)` keys; `default` if empty.
fn curve_at(keys: &[(f32, f32)], t: f32, default: f32) -> f32 {
    match keys {
        [] => default,
        [(_, v)] => *v,
        [(t_first, v_first), .., (_, v_last)] => {
            if t <= *t_first {
                return *v_first;
            }
            for &[(t0, v0), (t1, v1)] in keys.array_windows() {
                if t <= t1 {
                    let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                    return f32::mul_add(v1 - v0, k, v0);
                }
            }
            *v_last
        }
    }
}

fn radius_at(p: &Profile, t: f32) -> f32 {
    (curve_at(&p.curve, t, 1.0) * p.scale).max(1e-3)
}

/// Non-uniform scale (local X = width, local Z = height) for a cross-section's main ellipse.
fn cross_scale(c: CrossSection) -> Vec3 {
    let aspect = match c {
        CrossSection::Round => 1.0,
        c => c.aspect().max(0.05),
    };
    let s = aspect.sqrt();
    Vec3::new(s, 1.0, 1.0 / s)
}

/// A chain's cross-section at one `t`: the half width (x) and half height (y) of its main
/// ellipse, and the ventral offset of its centre from the axis, in metres.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Section {
    radii: Vec2,
    drop: f32,
}

/// Part of a keel's volume outside the ellipse it hangs from (with the blend fillets).
const KEEL_OUTSIDE: f32 = 0.2;

impl Section {
    /// The section of `p` at `t`, where its radius is `r`.
    fn at(p: &Profile, t: f32, r: f32) -> Self {
        let s = cross_scale(p.cross);
        let width = curve_at(&p.width, t, 1.0).max(0.05);
        let depth = curve_at(&p.depth, t, 1.0).max(0.05);
        Self { radii: Vec2::new(s.x * width, s.z * depth) * r, drop: curve_at(&p.drop, t, 0.0) * r }
    }

    /// A teardrop's keel under this section.
    fn keel(self) -> Self {
        Self {
            radii: self.radii * CrossSection::KEEL.truncate(),
            drop: CrossSection::KEEL.z.mul_add(self.radii.y, self.drop),
        }
    }

    /// The ellipse round the whole section (a teardrop's keel included), for axis samples.
    fn hull(self, cross: CrossSection) -> Self {
        match cross {
            CrossSection::Teardrop { .. } => {
                let half = 0.5 * CrossSection::TEARDROP_KEEL * self.radii.y;
                Self { radii: self.radii + Vec2::Y * half, drop: self.drop + half }
            }
            _ => self,
        }
    }
}

/// Where a chain bone's surface (`t0..t1` of the chain) is split into pieces, as fractions of
/// the bone from `0.0` to `1.0`: at the section curves' keys inside it (the curves are linear
/// between keys), else evenly into two or three where the section's aspect changes along it
/// (each piece has one aspect). One piece if nothing varies.
fn pieces(p: &Profile, (t0, t1): (f32, f32)) -> SmallVec<[f32; 4]> {
    let mut ks: SmallVec<[f32; 4]> = SmallVec::new();
    ks.push(0.0);
    if !p.is_plain() {
        let mut keys: SmallVec<[f32; 8]> = [&p.width, &p.depth, &p.drop]
            .into_iter()
            .flatten()
            .map(|(t, _)| (t - t0) / (t1 - t0).max(1e-6))
            .filter(|k| (0.1..=0.9).contains(k))
            .collect();
        keys.sort_by(f32::total_cmp);
        for k in keys {
            if ks.len() < 3 && ks.last().is_none_or(|last| k - last >= 0.1) {
                ks.push(k);
            }
        }
        if ks.len() == 1 {
            let aspect = |t: f32| curve_at(&p.width, t, 1.0).max(0.05) / curve_at(&p.depth, t, 1.0).max(0.05);
            let change = (aspect(t1) / aspect(t0)).ln().abs();
            let n = if change > 0.2 { 3 } else if change > 0.03 { 2 } else { 1 };
            ks.extend((1..n).map(|j| j.to_f32() / n.to_f32()));
        }
    }
    ks.push(1.0);
    ks
}

/// One round cone of a chain bone's surface, from section `a` at fraction `ka` of the bone to
/// section `b` at `kb`, each centred `drop` below the bone's axis. Its scale makes the cone's
/// ends the sections' ellipses (their aspect averaged; a bone with a changing aspect gets
/// several pieces).
#[allow(clippy::too_many_arguments)]
fn emit_piece(
    b: &mut AnatomyBuilder,
    bone: BoneId,
    start: &Transform,
    len: f32,
    (ka, a): (f32, Section),
    (kb, z): (f32, Section),
    blend: f32,
    region: RegionId,
) -> f32 {
    let at = |k: f32, s: Section| start.translation + start.rotation * Vec3::new(0.0, len * k, s.drop);
    let (pa, pb) = (at(ka, a), at(kb, z));
    let aspect = (a.radii.x / a.radii.y * (z.radii.x / z.radii.y)).sqrt();
    let s = aspect.sqrt();
    let (r0, r1) = ((a.radii.x * a.radii.y).sqrt(), (z.radii.x * z.radii.y).sqrt());
    let model = Transform { translation: pa, rotation: frame_y_z(pb - pa, start.rotation * Vec3::Z), scale: Vec3::new(s, 1.0, 1.0 / s) };
    let shape = PrimShape::RoundCone { r0, r1, len: pa.distance(pb).max(1e-3) };
    b.base(bone, shape, model, blend, region);
    r0
}

/// The surface of one chain bone from `start` (`len` long, `t0..t1` of the chain, radius `r0`
/// to `r1`), shaped by the profile's section curves: two or three pieces where they vary, the
/// first blending with the bone before by `blend`, plus a keel under each for a teardrop.
#[allow(clippy::too_many_arguments)]
fn emit_sections(
    b: &mut AnatomyBuilder,
    bone: BoneId,
    profile: &Profile,
    start: &Transform,
    len: f32,
    (t0, t1): (f32, f32),
    (r0, r1): (f32, f32),
    blend: f32,
    region: RegionId,
) -> SmallVec<[(f32, Section); 3]> {
    let section = |k: f32| (k, Section::at(profile, t0.lerp(t1, k), r0.lerp(r1, k)));
    let mut starts = SmallVec::new();
    for (j, &[ka, kz]) in pieces(profile, (t0, t1)).array_windows().enumerate() {
        let (from, to) = (section(ka), section(kz));
        let first = j == 0;
        // Pieces within a bone meet nearly flush: a small blend smooths their aspect step
        // without the bulge a large one raises at the joint.
        let piece_blend = if first { blend } else { 0.2 * from.1.radii.min_element().min(to.1.radii.min_element()) };
        let radius = emit_piece(b, bone, start, len, from, to, piece_blend, region);
        b.anchor_last_blend(Vec3::ZERO, 2.0f32.mul_add(piece_blend, 2.0 * radius));
        if !first {
            b.weigh_last(Weigh::Continues);
        }
        if matches!(profile.cross, CrossSection::Teardrop { .. }) {
            let (keel_from, keel_to) = (from.1.keel(), to.1.keel());
            let keel_blend = 0.5 * keel_from.radii.x.min(keel_to.radii.x);
            emit_piece(b, bone, start, len, (ka, keel_from), (kz, keel_to), keel_blend, region);
            b.weigh_last(Weigh::Fraction(KEEL_OUTSIDE));
        }
        starts.push(from);
    }
    starts
}

fn with_offset(at: &AttachPoint, cx: &BuildCx) -> Transform {
    at.model * cx.offset
}

fn moved(t: Transform, local: Vec3) -> Transform {
    Transform { translation: t.translation + t.rotation * local, ..t }
}

fn suffixed(base: &str, cx: &BuildCx) -> String {
    match cx.side {
        Side::Left => format!("{base}.l"),
        Side::Right => format!("{base}.r"),
        Side::Center => base.to_owned(),
    }
}

/// `n + 1` frames along a chain: `frames[i]` starts segment `i`, the last is the tip.
/// Segment `i` is rotated by `bend(i)` (local) relative to the previous segment.
fn chain_frames(start: Transform, lens: &[f32], bend: impl Fn(usize) -> Quat) -> Vec<Transform> {
    let n = lens.len();
    let mut frames = Vec::with_capacity(n + 1);
    let mut f = Transform { rotation: start.rotation * bend(0), ..start };
    for (i, len) in lens.iter().enumerate() {
        frames.push(f);
        let tip = f.translation + f.rotation * Vec3::Y * *len;
        let rot = if i + 1 < n { f.rotation * bend(i + 1) } else { f.rotation };
        f = Transform::from_translation(tip).with_rotation(rot);
    }
    frames.push(f);
    frames
}

/// Per-joint bends of a chain curling `curl` degrees per metre: each joint bends by the curl over
/// the segment before it.
fn curl_bend(lens: &[f32], curl: f32) -> impl Fn(usize) -> Quat + '_ {
    move |i| i.checked_sub(1).and_then(|j| lens.get(j)).map_or(Quat::IDENTITY, |len| Quat::from_rotation_x((curl * len).to_radians()))
}

/// [`ChainParams::segmented`] at full strength: how much narrower a segment is at its joints, how
/// much it swells between them, and where along it the swell peaks.
const SEGMENT_PINCH: f32 = 0.32;
const SEGMENT_SWELL: f32 = 0.08;
const SEGMENT_SWELL_AT: f32 = 0.4;

/// Emits one bone per segment, and its surface: one round cone, or a few where the profile's
/// section curves vary (all owned by that bone). Returns bones and axis samples (one at each
/// bone's start and at each piece's start, plus the tip).
fn emit_chain(
    b: &mut AnatomyBuilder,
    parent: Option<BoneId>,
    name: &str,
    frames: &[Transform],
    chain: &ChainParams,
    region: impl Fn(f32) -> RegionId,
) -> Result<(Vec<BoneId>, SmallVec<[AxisSample; 9]>), AnatomyError> {
    let n = frames.len().saturating_sub(1);
    let mut bones = Vec::with_capacity(n);
    let mut axis = SmallVec::new();
    let scale = cross_scale(chain.radius.cross);
    let mut prev = parent;
    // `t` runs along the chain's length (segments may differ), so radius profiles stay put.
    let mut along = Vec::with_capacity(n + 1);
    along.push(0.0f32);
    for [start, end] in frames.array_windows() {
        along.push(along.last().copied().unwrap_or(0.0) + start.translation.distance(end.translation));
    }
    let total = along.last().copied().unwrap_or(0.0).max(1e-6);
    for (i, ([start, end], [a0, a1])) in frames.array_windows().zip(along.array_windows()).enumerate() {
        let (t0, t1) = (a0 / total, a1 / total);
        let len = start.translation.distance(end.translation).max(1e-3);
        let bone = b.bone(prev, &format!("{name}.{i}"), *start, len, region(t0))?;
        let (r0, r1) = (radius_at(&chain.radius, t0), radius_at(&chain.radius, t1));
        let blend = if i == 0 { chain.blend } else { 0.5 * r0.min(r1) };
        // Blend only around the segment's own joint, so a limb hanging beside the body (or a
        // tail beside a leg) stays separate along its length.
        if chain.radius.is_plain() && !matches!(chain.radius.cross, CrossSection::Teardrop { .. }) {
            let seg = chain.segmented.clamp(0.0, 1.0);
            if seg > 0.0 {
                // Narrow joints at both ends, a slight swell between; joints barely blend.
                let (ra, rz) = (r0 * seg.mul_add(-SEGMENT_PINCH, 1.0), r1 * seg.mul_add(-SEGMENT_PINCH, 1.0));
                let rm = r0.lerp(r1, SEGMENT_SWELL_AT) * seg.mul_add(SEGMENT_SWELL, 1.0);
                let blend = if i == 0 { blend } else { blend.mul_add(1.0 - seg, 0.06 * seg * ra.min(rz)) };
                let cut = len * SEGMENT_SWELL_AT;
                b.base(bone, PrimShape::RoundCone { r0: ra, r1: rm, len: cut }, Transform { scale, ..*start }, blend, region(t0));
                b.anchor_last_blend(Vec3::ZERO, 2.0f32.mul_add(blend, 2.0 * ra));
                let mid = moved(*start, Vec3::Y * cut);
                b.base(bone, PrimShape::RoundCone { r0: rm, r1: rz, len: len - cut }, Transform { scale, ..mid }, 0.3 * rm, region(t0));
                b.weigh_last(Weigh::Continues);
            } else {
                b.base(bone, PrimShape::RoundCone { r0, r1, len }, Transform { scale, ..*start }, blend, region(t0));
                b.anchor_last_blend(Vec3::ZERO, 2.0f32.mul_add(blend, 2.0 * r0));
            }
            axis.push(AxisSample { t: t0, bone, model: *start, radii: Vec2::new(scale.x, scale.z) * r0, drop: 0.0 });
        } else {
            let sections = emit_sections(b, bone, &chain.radius, start, len, (t0, t1), (r0, r1), blend, region(t0));
            for (k, s) in sections {
                let s = s.hull(chain.radius.cross);
                let model = moved(*start, Vec3::Y * len * k);
                axis.push(AxisSample { t: t0.lerp(t1, k), bone, model, radii: s.radii, drop: s.drop });
            }
        }
        bones.push(bone);
        prev = Some(bone);
    }
    let (Some(&last), Some(&tip)) = (bones.last(), frames.last()) else {
        return Err(AnatomyError::Internal("chain without segments"));
    };
    let r_end = radius_at(&chain.radius, 1.0);
    let end = Section::at(&chain.radius, 1.0, r_end).hull(chain.radius.cross);
    axis.push(AxisSample { t: 1.0, bone: last, model: tip, radii: end.radii, drop: end.drop });
    Ok((bones, axis))
}

const fn slot(slot: AttachSlot, bone: BoneId, model: Transform) -> (AttachSlot, AttachPoint) {
    (slot, AttachPoint { bone: Some(bone), model })
}

/// Start/End/axis slots of a plain chain.
fn chain_slots(bones: &[BoneId], frames: &[Transform], axis: SmallVec<[AxisSample; 9]>) -> Result<PartSlots, AnatomyError> {
    let (Some(&first), Some(&last), Some(&start), Some(&tip)) = (bones.first(), bones.last(), frames.first(), frames.last()) else {
        return Err(AnatomyError::Internal("chain without segments"));
    };
    // Flip about local Z: points back along the chain while keeping the dorsal side (-Z) up.
    let back = Transform { rotation: start.rotation * Quat::from_rotation_z(std::f32::consts::PI), ..start };
    Ok(PartSlots {
        fixed: [slot(AttachSlot::Start, first, back), slot(AttachSlot::End, last, tip)].into_iter().collect(),
        axis,
        skull: None,
    })
}

// ---------------------------------------------------------------------------------------------
// Torso
// ---------------------------------------------------------------------------------------------

impl PartBuild for TorsoParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let n = usize::from(self.chain.segments.max(1));
        let len = self.chain.length.max(0.01);
        let base = with_offset(&at, cx);
        let points: Vec<Vec3> = (0..=n)
            .map(|i| {
                let t = i.to_f32() / n.to_f32();
                base.transform_point(Vec3::new(0.0, curve_at(&self.arch, t, 0.0), t.mul_add(len, -len * 0.5)))
            })
            .collect();
        let frames: Vec<Transform> = points
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                // Toward the next point; the last continues from the one before.
                let dir = match (points.get(i + 1), i.checked_sub(1).and_then(|j| points.get(j))) {
                    (Some(next), _) => *next - p,
                    (None, Some(prev)) => p - *prev,
                    (None, None) => Vec3::Y,
                };
                Transform::from_translation(p).with_rotation(frame_y_z(dir, base.rotation * Vec3::NEG_Y))
            })
            .collect();

        let rear = b.region(RegionKind::TorsoRear, cx.side)?;
        let mid = b.region(RegionKind::TorsoMid, cx.side)?;
        let front = b.region(RegionKind::TorsoFront, cx.side)?;
        let region = |t: f32| if t < 0.34 { rear } else if t < 0.66 { mid } else { front };
        let (bones, axis) = emit_chain(b, at.bone, "torso", &frames, &self.chain, region)?;
        b.spine(&bones);

        if self.belly > 0.0
            && let Some(&mid_bone) = bones.get(n / 2)
            && let Some(&s) = axis.iter().find(|s| s.bone == mid_bone)
        {
            let belly = b.region(RegionKind::Belly, cx.side)?;
            let r = s.min_radius();
            b.primitive(
                mid_bone,
                PrimShape::Ellipsoid { radii: Vec3::new(r * 0.85, len / n.to_f32() * 1.2, r * 0.4f32.mul_add(self.belly, 0.6)) },
                moved(s.model, Vec3::new(0.0, 0.0, (r * 0.35).mul_add(self.belly, s.drop))),
                r * 0.5,
                PrimOp::Union,
                PrimLayer::Fat,
                belly,
            );
        }
        let slots = chain_slots(&bones, &frames, axis)?;
        for (name, kind, side, t, around) in [
            ("back", SocketKind::Back, Side::Center, 0.6, 0.0),
            ("saddle", SocketKind::Saddle, Side::Center, 0.45, 0.0),
            ("chest", SocketKind::Chest, Side::Center, 0.85, 180.0),
            ("hip.l", SocketKind::Hip, Side::Left, 0.12, 90.0),
            ("hip.r", SocketKind::Hip, Side::Right, 0.12, -90.0),
            ("shoulder.l", SocketKind::Shoulder, Side::Left, 0.9, 60.0),
            ("shoulder.r", SocketKind::Shoulder, Side::Right, 0.9, -60.0),
        ] {
            surface_socket(b, &slots, name, kind, side, t, around);
        }
        Ok(slots)
    }
}

/// A socket on the surface of a segmented part: +Y out of the skin, +Z toward the part's end.
fn surface_socket(b: &mut AnatomyBuilder, slots: &PartSlots, name: &str, kind: SocketKind, side: Side, t: f32, around_deg: f32) {
    let (Some(at), Some(s)) = (slots.resolve(AttachSlot::Surface { t, around_deg }), slots.sample(t)) else { return };
    let Some(bone) = at.bone else { return };
    b.socket(name, kind, side, bone, at.model, s.radius_toward(around_deg.to_radians()) * 2.0);
}

// ---------------------------------------------------------------------------------------------
// Chain (neck, tail, ears, ...)
// ---------------------------------------------------------------------------------------------

impl PartBuild for ChainPart {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let c = &self.chain;
        let lens = c.segment_lengths();
        let frames = chain_frames(with_offset(&at, cx), &lens, curl_bend(&lens, c.curl));

        let (r0, r1) = match self.role {
            ChainRole::Neck => (b.region(RegionKind::Neck, cx.side)?, None),
            ChainRole::Tail => (b.region(RegionKind::TailBase, cx.side)?, Some(b.region(RegionKind::TailTip, cx.side)?)),
            _ => (b.region(RegionKind::Appendage, cx.side)?, None),
        };
        let region = |t: f32| match r1 {
            Some(tip) if t >= 0.5 => tip,
            _ => r0,
        };
        let name = suffixed(&format!("{:?}", self.role).to_lowercase(), cx);
        let (bones, axis) = emit_chain(b, at.bone, &name, &frames, c, region)?;

        let (stiffness, damping) = match self.role {
            ChainRole::Neck | ChainRole::Tongue => (0.0, 0.0),
            ChainRole::Tail => (0.4, 0.3),
            ChainRole::Ear => (0.7, 0.4),
            ChainRole::Antenna | ChainRole::Whisker => (0.6, 0.2),
            ChainRole::Tentacle | ChainRole::Trunk => (0.2, 0.25),
        };
        if stiffness > 0.0 {
            b.spring_chain(SpringChainDef { bones: bones.iter().copied().collect(), stiffness, damping, gravity: 1.0 });
        }
        let slots = chain_slots(&bones, &frames, axis)?;
        match self.role {
            ChainRole::Neck => surface_socket(b, &slots, &suffixed("neck", cx), SocketKind::Neck, cx.side, 0.5, 180.0),
            ChainRole::Tail => {
                let size = slots.axis.last().map_or(0.02, |s| s.min_radius() * 2.0);
                if let (Some(&last), Some(&tip)) = (bones.last(), frames.last()) {
                    b.socket(&suffixed("tail_tip", cx), SocketKind::TailTip, cx.side, last, tip, size);
                }
            }
            _ => {}
        }
        Ok(slots)
    }
}

// ---------------------------------------------------------------------------------------------
// Limb
// ---------------------------------------------------------------------------------------------

/// Per-segment bends: `(about local X = toward forward, about local Z = toward up)` degrees.
fn limb_bends(role: LimbRole, stance: Stance, i: usize) -> (f32, f32) {
    let pick = |table: &[(f32, f32)]| table.get(i).copied().unwrap_or((0.0, 0.0));
    match (role, stance) {
        (LimbRole::Manipulator | LimbRole::Display, _) => pick(&[(0.0, 0.0), (15.0, 0.0)]),
        (_, Stance::Plantigrade) => pick(&[(10.0, 0.0), (-25.0, 0.0), (10.0, 0.0)]),
        (_, Stance::Digitigrade) => pick(&[(25.0, 0.0), (-60.0, 0.0), (40.0, 0.0)]),
        (_, Stance::Unguligrade) => pick(&[(20.0, 0.0), (-50.0, 0.0), (30.0, 0.0)]),
        (_, Stance::Sprawl) => pick(&[(0.0, 0.0), (0.0, -80.0), (0.0, 10.0)]),
        (_, Stance::Arched) => pick(&[(0.0, 35.0), (0.0, -110.0), (0.0, -20.0)]),
    }
}

/// The least splay, from `splay` up, at which an arm hanging from `pos` (swung about `axis`) clears
/// the body built so far: from mid upper arm to `reach` past its end, every point stands off the
/// surface by nearly its own radius (an arm may rest against the body, not sink into it; next to
/// the shoulder it always meets it). Stops at 50° more. A slim body keeps its authored splay.
fn clear_splay(b: &AnatomyBuilder, pos: Vec3, axis: Vec3, splay: f32, lens: &[f32], radius: &Profile, reach: f32) -> f32 {
    const STEP: f32 = 2.0;
    const MOST: f32 = 50.0;
    const SAMPLES: usize = 10;
    let total: f32 = lens.iter().sum();
    if total <= 1e-4 {
        return splay;
    }
    // How far each point must stand off the body: next to the shoulder an arm always meets the
    // body, so nothing; from mid upper arm down, nearly its own radius (touching, not sunk).
    let need = |t: f32| radius_at(radius, t.min(1.0)) * 0.9 * ((t - 0.2) / 0.4).clamp(0.0, 1.0);
    let clear = |deg: f32| {
        let dir = Quat::from_axis_angle(axis, deg.to_radians()) * Vec3::NEG_Y;
        (0..=SAMPLES).all(|i| {
            // From a third down the upper arm (higher, the arm leaves the shoulder) to past the end.
            let t = 0.3 + 0.7 * i.to_f32() / SAMPLES.to_f32();
            let along = (total + reach) * t;
            b.body_distance(pos + dir * along) >= need(t)
        })
    };
    let mut deg = splay;
    while deg < splay + MOST && !clear(deg) {
        deg += STEP;
    }
    deg
}

impl PartBuild for LimbParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let lens = self.chain.segment_lengths();
        let pos = with_offset(&at, cx).translation;
        let outward = at.model.rotation * Vec3::Y;

        // Initial direction: straight down rotated toward the attach normal by the splay.
        let splay = match self.stance {
            Stance::Sprawl | Stance::Arched if self.role != LimbRole::Manipulator => self.splay_deg.max(85.0),
            _ => self.splay_deg,
        };
        let axis = Vec3::NEG_Y.cross(outward).try_normalize().unwrap_or(Vec3::Z);
        // Arms hang clear of whatever the body has grown (fat, breasts, hips): out as far as they
        // must, so they don't sink into the padding at rest, let alone when posed.
        let splay = if self.role == LimbRole::Manipulator && at.bone.is_some() {
            let reach = match self.end {
                LimbEnd::Hand { palm } => palm * 0.5,
                _ => 0.0,
            };
            clear_splay(b, pos, axis, splay, &lens, &self.chain.radius, reach)
        } else {
            splay
        };
        let dir0 = Quat::from_axis_angle(axis, splay.to_radians().min(FRAC_PI_2 * 1.2)) * Vec3::NEG_Y;
        let base = Transform::from_translation(pos)
            .with_rotation(frame_y_z(dir0, Vec3::Z) * cx.offset.rotation);

        let (role, stance) = (self.role, self.stance);
        let bends: Vec<(f32, f32)> = match &self.joints {
            Some(j) => joint_bends(j, &self.bends, lens.len(), cx)?,
            None => (0..lens.len()).map(|i| self.bends.get(i).copied().unwrap_or_else(|| limb_bends(role, stance, i))).collect(),
        };
        let frames = chain_frames(base, &lens, |i| {
            let (x, z) = bends.get(i).copied().unwrap_or_default();
            Quat::from_rotation_x(x.to_radians()) * Quat::from_rotation_z(z.to_radians())
        });
        let upper = b.region(RegionKind::LimbUpper, cx.side)?;
        let lower = b.region(RegionKind::LimbLower, cx.side)?;
        let end_region = b.region(RegionKind::LimbEnd, cx.side)?;
        // Arms hang from a clavicle reaching in from the shoulder toward the chest, so raises can
        // share their rotation with it (a shoulder lifts; it doesn't fold).
        let clavicle_len = match self.clavicle {
            Some(l) => l,
            None if self.role == LimbRole::Manipulator => lens.first().copied().unwrap_or(0.0) * 0.45,
            None => 0.0,
        };
        let clavicle = if self.role == LimbRole::Manipulator && clavicle_len > 1e-3 && at.bone.is_some() {
            let frame = Transform::from_translation(pos - outward * clavicle_len).with_rotation(frame_y_z(outward, Vec3::Z));
            Some(b.bone(at.bone, &suffixed("clavicle", cx), frame, clavicle_len, upper)?)
        } else {
            None
        };
        let name = suffixed("limb", cx);
        let (bones, axis_samples) = emit_chain(b, clavicle.or(at.bone), &name, &frames, &self.chain, |t| {
            if t < 0.5 { upper } else { lower }
        })?;
        let (Some(&first), Some(&last), Some(&tip)) = (bones.first(), bones.last(), frames.last()) else {
            return Err(AnatomyError::Internal("limb without segments"));
        };
        if let Some(c) = clavicle {
            b.clavicle(c, first, cx.side);
        }
        if let Some(j) = &self.joints {
            b.limit(first, j.plan.root());
            for (bone, hinge) in bones.iter().skip(1).zip(j.plan.hinges()) {
                b.limit(*bone, hinge.limit());
            }
        }
        let r = radius_at(&self.chain.radius, 1.0);
        let end_slot = limb_end(self.end, b, last, tip, r, end_region);

        if matches!(self.role, LimbRole::Manipulator | LimbRole::Both) && self.end.grasps() {
            let size = match self.end {
                LimbEnd::Hand { palm } => palm,
                LimbEnd::Pincer { size } => size,
                _ => r * 2.0,
            };
            // Socket +Y = the held tool's axis: across the fist (forward when the arm hangs);
            // +Z runs down the limb toward the fingers.
            let fist_turn = Quat::from_mat3(&Mat3::from_cols(Vec3::NEG_X, Vec3::Z, Vec3::Y));
            let grip = moved(tip, Vec3::Y * size * 0.5).with_rotation(tip.rotation * fist_turn);
            b.socket(&suffixed("grip", cx), SocketKind::Grip, cx.side, last, grip, size);
        }
        if matches!(self.role, LimbRole::Locomotor | LimbRole::Both) {
            let (hip, foot) = (frames.first().map_or(tip.translation, |f| f.translation), tip.translation);
            let axis = (foot - hip).normalize_or(Vec3::NEG_Y);
            // The IK pole is the rest pose's own knee: elbows back, stifles forward, a spider's
            // knees up. With declared joints that rest bend is checked, so it can't point the
            // wrong way; a leg standing straight at its first hinge (where the rest pose shows
            // no side) takes the side the hinge closes toward.
            let off_axis = |v: Vec3| v - axis * v.dot(axis);
            let knee = off_axis(frames.get(1).unwrap_or(&tip).translation - hip);
            let declared = self.joints.as_ref().and_then(|j| j.plan.hinges().first());
            let pole = match (knee.try_normalize(), declared, frames.first()) {
                (Some(k), _, _) if knee.length() > 1e-3 * lens.iter().sum::<f32>() => k,
                (_, Some(h), Some(f)) => off_axis(f.rotation * h.flexes.points()).try_normalize().unwrap_or(Vec3::Z),
                (k, ..) => k.unwrap_or(Vec3::Z),
            };
            let leg = b.leg_count();
            b.leg(bones.iter().copied().collect(), cx.side, self.stance, tip.translation, pole);
            if self.joints.as_ref().is_some_and(|j| j.plan == JointPlan::Leg)
                && let (Some(pelvis), Ok(leg)) = (at.bone, u8::try_from(leg))
            {
                glute(b, pelvis, hip, radius_at(&self.chain.radius, 0.0), leg, cx)?;
            }
        }

        let mut slots = chain_slots(&bones, &frames, axis_samples)?;
        if let Some(end) = slots.fixed.iter_mut().find(|(s, _)| *s == AttachSlot::End) {
            *end = slot(AttachSlot::End, last, end_slot);
        }
        Ok(slots)
    }
}

/// Share of the thigh's swing a glute takes: enough that the buttock and its fold travel with the
/// leg, not so much that the pelvis seems to come apart.
const GLUTE_FOLLOW: f32 = 0.45;

/// An upright leg's glute: a short bone from the hip joint back into the buttock, turning with the
/// thigh by [`GLUTE_FOLLOW`], so the buttock and its fold move with the leg instead of the thigh
/// sliding under a buttock fixed to the pelvis. Skin weights come from distance to bone segments,
/// so the buttock binds to it; its muscle stays on the pelvis and still firms as the hip extends.
fn glute(b: &mut AnatomyBuilder, pelvis: BoneId, hip: Vec3, thigh_radius: f32, leg: u8, cx: &BuildCx) -> Result<(), AnatomyError> {
    // Back from the hip joint and a little up, into the middle of the buttock.
    let dir = Vec3::new(0.0, 0.2, -1.0).normalize();
    let frame = Transform::from_translation(hip).with_rotation(frame_y_z(dir, Vec3::Y));
    let region = b.bone_region(pelvis);
    let bone = b.bone(Some(pelvis), &suffixed("glute", cx), frame, thigh_radius * 2.0, region)?;
    b.follow(ClothFollow { root: bone, leg, weight: GLUTE_FOLLOW });
    Ok(())
}

/// A jointed limb's rest bends (as [`limb_bends`] gives them), checked against its joints: one
/// flexion per hinge, each within the hinge's range.
fn joint_bends(j: &LimbJoints, bends: &[(f32, f32)], segments: usize, cx: &BuildCx) -> Result<Vec<(f32, f32)>, AnatomyError> {
    let invalid = |reason: String| AnatomyError::InvalidParams { path: cx.path.clone(), reason };
    if !bends.is_empty() {
        return Err(invalid("set `bends` or `joints`, not both".into()));
    }
    let hinges = j.plan.hinges();
    if j.flex.len() + 1 != segments || j.flex.len() > hinges.len() {
        return Err(invalid(format!(
            "{} segments need {} flexions, {:?} has {} hinges; got {}",
            segments,
            segments.saturating_sub(1),
            j.plan,
            hinges.len(),
            j.flex.len()
        )));
    }
    let mut out = vec![(j.swing, j.lift)];
    for (flex, h) in j.flex.iter().zip(hinges) {
        let (lo, hi) = h.flex;
        if !(lo..=hi).contains(flex) {
            return Err(invalid(format!("{} flexed {flex}°, outside {lo}..{hi}", h.name)));
        }
        let angle = h.flexes.to_angle(*flex);
        out.push(if h.flexes.axis() == Vec3::X { (angle, 0.0) } else { (0.0, angle) });
    }
    Ok(out)
}

/// Adds a limb's end shape (hand, foot, hoof, ...) at `tip` on bone `last` (`r` = the limb's
/// radius there); returns the frame its children attach to.
fn limb_end(end: LimbEnd, b: &mut AnatomyBuilder, last: BoneId, tip: Transform, r: f32, end_region: RegionId) -> Transform {
    // Ground-contact frame at the tip: Y forward (horizontal), Z down.
    let ground = Transform::from_translation(tip.translation).with_rotation(frame_y_z(Vec3::Z, Vec3::NEG_Y));

    let prim = |b: &mut AnatomyBuilder, shape: PrimShape, model: Transform| {
        b.base(last, shape, model, r * 0.6, end_region);
    };
    match end {
        LimbEnd::None | LimbEnd::Point => tip,
        LimbEnd::Hand { palm } => {
            let m = moved(tip, Vec3::Y * palm * 0.5);
            // A flat, square-edged palm the fingers line up along (not a pillow).
            let round = palm * 0.1;
            let half = (Vec3::new(palm * 0.42, palm * 0.5, palm * 0.14) - Vec3::splat(round)).max(Vec3::splat(palm * 0.02));
            prim(b, PrimShape::RoundBox { half, round }, m);
            moved(tip, Vec3::Y * palm)
        }
        LimbEnd::Foot { length } => {
            // In the ground frame (Y forward, Z down), the sole 0.65 r under the ankle: a narrow
            // heel behind the ankle; the instep high under the ankle, falling forward to the broad
            // ball; then the toes, thinner and tapering.
            let at = |y: f32, z: f32| moved(ground, Vec3::new(0.0, y, z));
            let sole = r * 0.65;
            prim(b, PrimShape::Ellipsoid { radii: Vec3::new(r * 0.95, r * 1.05, r * 0.8) }, at(-r * 0.3, r.mul_add(-0.8, sole)));
            let ball_y = length.mul_add(0.7, -r * 0.5);
            let ball = Vec3::new(r * 1.45, r * 0.8, r * 0.55);
            // The instep: from under the ankle down to the ball, flattened top to bottom.
            let (from, to) = (Vec3::new(0.0, r * 0.1, r.mul_add(-1.2, sole)), Vec3::new(0.0, ball_y, sole - ball.z));
            let instep = Transform {
                translation: ground.translation + ground.rotation * from,
                rotation: ground.rotation * frame_y_z(to - from, Vec3::Z),
                scale: Vec3::new(1.25, 1.0, 0.75),
            };
            b.base(last, PrimShape::RoundCone { r0: r * 1.1, r1: r * 0.75, len: from.distance(to) }, instep, r * 0.45, end_region);
            b.base(last, PrimShape::Ellipsoid { radii: ball }, at(ball_y, sole - ball.z), r * 0.35, end_region);
            let toes = Vec3::new(r * 1.25, length * 0.13, r * 0.42);
            b.base(last, PrimShape::Ellipsoid { radii: toes }, at(length.mul_add(0.85, -r * 0.5), sole - toes.z), r * 0.3, end_region);
            moved(ground, Vec3::new(0.0, r.mul_add(-0.5, length), r * 0.2))
        }
        LimbEnd::Pad => {
            prim(b, PrimShape::Ellipsoid { radii: Vec3::new(r * 1.3, r * 1.4, r * 0.7) }, moved(ground, Vec3::Y * r * 0.4));
            moved(ground, Vec3::new(0.0, r * 1.5, r * 0.2))
        }
        LimbEnd::Hoof { split } => {
            // The wall slopes forward to the toe (about 50° to the ground), so the hoof's axis
            // leans its toe ahead of the coronet; the sole is wider than the coronet.
            let lean = (Vec3::NEG_Y + Vec3::Z * 0.45).normalize();
            let down = Transform::from_translation(tip.translation).with_rotation(frame_y_z(lean, Vec3::Z));
            if split {
                // Coronet joining the leg, then two claws with a cleft between them, each a little
                // turned in at the toe.
                prim(b, PrimShape::Sphere { r: r * 1.0 }, down);
                for x in [1.0f32, -1.0] {
                    let claw = Transform {
                        translation: down.translation + down.rotation * Vec3::new(x * r * 0.5, r * 0.15, 0.0),
                        rotation: down.rotation * Quat::from_rotation_z(x * 0.08),
                        ..down
                    };
                    b.base(last, PrimShape::RoundCone { r0: r * 0.58, r1: r * 0.68, len: r * 1.05 }, claw, r * 0.06, end_region);
                }
            } else {
                prim(b, PrimShape::RoundCone { r0: r * 1.05, r1: r * 1.3, len: r * 1.2 }, down);
            }
            moved(ground, Vec3::Y * r * 1.3)
        }
        LimbEnd::Pincer { size } => {
            // Palm (propodus): swollen and flattened, the fingers' muscle inside it.
            prim(b, PrimShape::Ellipsoid { radii: Vec3::new(size * 0.48, size * 0.68, size * 0.3) }, moved(tip, Vec3::Y * size * 0.62));
            // Fixed finger (pollex) and hinged finger (dactyl), tapering to points, slightly open:
            // two cones each, bowing toward each other at the tips.
            for (x, r0) in [(1.0f32, 0.19), (-1.0, 0.17)] {
                let base = tip.translation + tip.rotation * Vec3::new(x * size * 0.17, size * 1.12, 0.0);
                let knee = tip.translation + tip.rotation * Vec3::new(x * size * 0.2, size * 1.5, 0.0);
                let point = tip.translation + tip.rotation * Vec3::new(x * size * 0.07, size * 2.0, 0.0);
                let up = tip.rotation * Vec3::Z;
                let seg = |from: Vec3, to: Vec3| Transform::from_translation(from).with_rotation(frame_y_z(to - from, up));
                let (ra, rb, rc) = (size * r0, size * r0 * 0.62, size * 0.03);
                b.base(last, PrimShape::RoundCone { r0: ra, r1: rb, len: base.distance(knee) }, seg(base, knee), size * 0.12, end_region);
                b.base(last, PrimShape::RoundCone { r0: rb, r1: rc, len: knee.distance(point) }, seg(knee, point), size * 0.04, end_region);
            }
            moved(tip, Vec3::Y * size * 2.0)
        }
        LimbEnd::Blade { length } => {
            let m = Transform { scale: Vec3::new(0.3, 1.0, 1.0), ..tip };
            prim(b, PrimShape::RoundCone { r0: r, r1: r * 0.2, len: length }, m);
            moved(tip, Vec3::Y * length)
        }
        LimbEnd::Paddle { width } => {
            prim(b, PrimShape::Ellipsoid { radii: Vec3::new(width * 0.5, width * 0.8, r * 0.3) }, moved(ground, Vec3::Y * width * 0.6));
            moved(ground, Vec3::Y * width * 1.4)
        }
    }
}


// ---------------------------------------------------------------------------------------------
// Head
// ---------------------------------------------------------------------------------------------

impl HeadParams {
    /// The snout's centre line and half sizes in the skull frame.
    fn muzzle_line(&self) -> MuzzleLine {
        let s = self.skull.size;
        let m = &self.muzzle;
        let drop = m.drop.to_radians();
        let dir = Vec3::new(0.0, drop.cos(), drop.sin());
        let start = Vec3::new(0.0, s.z * 1.2, -m.rise * s.y);
        let min = Vec2::splat(0.02);
        MuzzleLine { start, dir, tip: start + dir * self.snout, base: m.base.max(min) * s.y, end: m.tip.max(min) * s.y }
    }

    /// The eyeball radius a face's lids are built round: plans size their `Eye` hard parts to match.
    fn eye_radius(&self) -> f32 {
        self.skull.size.y * 0.11
    }

    /// A face's eye centre in the skull frame (X left, Y forward, Z down): halfway from crown to
    /// chin, set back into the lids so the cornea stands about level with the brow and cheek.
    fn face_eye(&self) -> Vec3 {
        let s = self.skull.size;
        let cy = s.z * 0.8;
        Vec3::new(s.x * self.eye_spacing.max(0.2), self.eye_radius().mul_add(-0.15, s.z.mul_add(0.72, cy)), -s.y * 0.05)
    }

    /// The head's fixed slots in model space, from its `skull` frame (Y forward, Z down, X left)
    /// and the snout's centre `line`.
    fn slots(&self, skull: Transform, line: &MuzzleLine) -> [(AttachSlot, Transform); 7] {
        let s = self.skull.size;
        // Local placement in the skull frame: (position, +Y direction, +Z hint).
        let place = |p: Vec3, dir: Vec3, hint: Vec3| skull * Transform::from_translation(p).with_rotation(frame_y_z(dir, hint));
        let eye_dir = Vec3::Y.lerp(Vec3::X, self.eye_lateral.clamp(0.0, 1.0));
        let eye_x = s.x * self.eye_spacing.max(0.2);
        // The front of the snout's tip.
        let nose = line.tip + line.dir * line.end.y;
        // A face (see `build_face` for its layout): eyes halfway down the head, just under the
        // surface; ears flat on the sides from nose to brow (wide across the head's depth, thin
        // outward); nose and mouth on the face. An animal's ears stand on top.
        let (eye, ear, nose, mouth) = if self.face.is_some() {
            let cy = s.z * 0.8;
            (
                place(self.face_eye(), eye_dir, Vec3::Z),
                place(Vec3::new(s.x * 0.92, s.z.mul_add(-0.1, cy), s.y * 0.2), Vec3::new(0.25, -0.3, -1.0), Vec3::X),
                place(Vec3::new(0.0, s.z.mul_add(1.12, cy), s.y * 0.3), Vec3::Y, Vec3::Z),
                place(Vec3::new(0.0, s.z.mul_add(0.95, cy), s.y * 0.55), Vec3::Y, Vec3::Z),
            )
        } else {
            (
                place(Vec3::new(eye_x, s.z * 1.3, -s.y * 0.3), eye_dir, Vec3::Z),
                place(Vec3::new(s.x * 0.6, s.z * 0.5, -s.y * 0.85), Vec3::new(0.5, -0.2, -1.0), Vec3::Y),
                place(nose, line.dir, Vec3::Z),
                place(Vec3::new(0.0, nose.y * 0.9, line.tip.z + line.end.y), line.dir, Vec3::Z),
            )
        };
        [
            (AttachSlot::Eye, eye),
            (AttachSlot::Ear, ear),
            (AttachSlot::Horn, place(Vec3::new(s.x * 0.45, s.z * 1.1, -s.y * 0.85), Vec3::new(0.3, 0.4, -1.0), Vec3::Y)),
            (AttachSlot::Crown, place(Vec3::new(0.0, s.z * 0.8, -s.y), Vec3::NEG_Z, Vec3::Y)),
            (AttachSlot::Nose, nose),
            (AttachSlot::Mouth, mouth),
            (AttachSlot::End, nose),
        ]
    }
}

impl PartBuild for HeadParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let base = with_offset(&at, cx);
        let fwd = base.rotation * Vec3::Y;
        let level = Vec3::new(fwd.x, 0.0, fwd.z).try_normalize().unwrap_or(Vec3::Z);
        let pitch = self.pitch.clamp(-80.0, 80.0).to_radians();
        let level = level * pitch.cos() - Vec3::Y * pitch.sin();
        // Skull frame: Y forward (level, or pitched nose-down), Z down, X left.
        let skull = Transform::from_translation(base.translation).with_rotation(frame_y_z(level, Vec3::NEG_Y));
        let s = self.skull.size; // half extents: x width, y height, z depth
        let skull_region = b.region(RegionKind::Skull, cx.side)?;
        let snout_region = b.region(RegionKind::Snout, cx.side)?;
        let bone = b.bone(at.bone, "skull", skull, s.z.mul_add(2.0, self.snout), skull_region)?;
        // A face sits upright on its neck: the neck enters the skull's base from below, with the
        // braincase over it and the back of the head overhanging the nape. An animal's head hangs
        // forward of its neck. The bone still pivots at the neck's top.
        let skull = if self.face.is_some() { moved(skull, Vec3::new(0.0, -s.z * 0.5, -s.y * 0.5)) } else { skull };

        // The cranium. Under a face it is shorter and sits higher: the face and jaw hang below its
        // front instead of the braincase filling down to the chin.
        let (cranium_radii, cranium_at) = if self.face.is_some() {
            (Vec3::new(s.x, s.z, s.y * 0.85), Vec3::new(0.0, s.z * 0.8, -s.y * 0.15))
        } else {
            (Vec3::new(s.x, s.z, s.y), Vec3::Y * s.z * 0.8)
        };
        b.base(bone, PrimShape::Ellipsoid { radii: cranium_radii }, moved(skull, cranium_at), self.skull.blend, skull_region);
        let line = self.muzzle_line();
        let mut fixed: SmallVec<[(AttachSlot, AttachPoint); 6]> = SmallVec::new();
        if let Some(face) = &self.face {
            if self.snout > 0.0 {
                let shape = PrimShape::RoundCone { r0: s.y * 0.55, r1: s.y * 0.35, len: self.snout };
                b.base(bone, shape, moved(skull, Vec3::new(0.0, s.z * 1.2, s.y * 0.15)), s.y * 0.3, snout_region);
            }
            let jaw_region = if self.jaw.is_some() { Some(b.region(RegionKind::Jaw, cx.side)?) } else { None };
            let eye = (self.face_eye(), self.eye_radius());
            let regions = FaceRegions {
                face: b.region(RegionKind::Face, cx.side)?,
                brow: b.region(RegionKind::Brow, cx.side)?,
                eyelid: b.region(RegionKind::Eyelid, cx.side)?,
                lip: b.region(RegionKind::Lip, cx.side)?,
                jaw: jaw_region,
            };
            let jaw = build_face(b, face, skull, s, eye, bone, &regions)?;
            if let Some((jaw, chin)) = jaw {
                fixed.push(slot(AttachSlot::Jaw, jaw, chin));
            }
        } else {
            let muzzle = Muzzle { params: &self.muzzle, line, skull, s, bone, region: snout_region };
            muzzle.build(b, self.snout);
            if let Some(j) = &self.jaw {
                let jaw_region = b.region(RegionKind::Jaw, cx.side)?;
                fixed.push(muzzle.build_jaw(b, j, jaw_region)?);
            } else if self.muzzle.cheek > 0.0 {
                muzzle.build_cheeks(b, bone, snout_region);
            }
        }

        let slots = self.slots(skull, &line);
        let [_, _, _, (_, crown), _, (_, mouth), _] = slots;
        fixed.extend(slots.into_iter().map(|(sl, m)| slot(sl, bone, m)));
        b.socket("head", SocketKind::Head, cx.side, bone, crown, s.x * 2.0);
        b.socket("mouth", SocketKind::Mouth, cx.side, bone, mouth, s.y);

        // Look-at: neck bones (root to head) plus the skull, sharing the turn.
        let mut look: Vec<BoneId> = at.bone.map_or_default(|p| b.ancestor_bones(p, RegionKind::Neck).into_vec());
        look.reverse();
        look.push(bone);
        let share = 1.0 / look.len().to_f32();
        b.look(LookDef { bones: look.into_iter().map(|b| (b, share)).collect(), limit_deg: 70.0 });

        let axis = [0.0, 1.0]
            .into_iter()
            .map(|t| AxisSample { t, bone, model: moved(skull, Vec3::Y * s.z * 1.6 * t), radii: Vec2::new(s.x, s.y), drop: 0.0 })
            .collect();
        let skull_frame = SkullFrame { bone, model: moved(skull, cranium_at), radii: cranium_radii };
        Ok(PartSlots { fixed, axis, skull: Some(skull_frame) })
    }
}

/// The centre line of an animal's snout in the skull frame (X left, Y forward, Z down): from
/// `start` (where it leaves the skull) along `dir` to `tip`, with half (width, height) `base` at
/// the start and `end` at the tip.
#[derive(Clone, Copy)]
struct MuzzleLine {
    start: Vec3,
    dir: Vec3,
    tip: Vec3,
    base: Vec2,
    end: Vec2,
}

impl MuzzleLine {
    /// Half (width, height) of the snout `t` of the way from its start to its tip.
    fn size_at(&self, t: f32) -> Vec2 {
        self.base.lerp(self.end, t)
    }
}

/// Builds an animal's muzzle: the snout on the skull bone, then the lower jaw, its cheeks and lip.
struct Muzzle<'a> {
    params: &'a MuzzleParams,
    line: MuzzleLine,
    /// The skull frame (X left, Y forward, Z down).
    skull: Transform,
    /// Skull half extents: x width, y height, z depth.
    s: Vec3,
    bone: BoneId,
    region: RegionId,
}

impl Muzzle<'_> {
    /// A model-space frame at skull-frame point `p` whose +Y runs along the snout.
    fn frame(&self, p: Vec3) -> Transform {
        Transform::from_translation(self.skull.transform_point(p)).with_rotation(self.skull.rotation * frame_y_z(self.line.dir, Vec3::Z))
    }

    /// A round cone along the snout from `t0` to `t1` (fractions of its length), its height
    /// tapering with the snout and its width following the start's aspect.
    fn cone(&self, b: &mut AnatomyBuilder, snout: f32, (t0, t1): (f32, f32), blend: f32) {
        let l = &self.line;
        let (a, z) = (l.size_at(t0), l.size_at(t1));
        let model = Transform { scale: Vec3::new(a.x / a.y, 1.0, 1.0), ..self.frame(l.start + l.dir * snout * t0) };
        b.base(self.bone, PrimShape::RoundCone { r0: a.y, r1: z.y, len: (snout * (t1 - t0)).max(1e-3) }, model, blend, self.region);
    }

    /// The snout: a tapering cone (two, if its cross-section changes shape on the way, each with
    /// its own width), then the dish of the nose bridge, the nostrils' rims and the upper lip.
    fn build(&self, b: &mut AnatomyBuilder, snout: f32) {
        if snout <= 0.0 {
            return;
        }
        let line = &self.line;
        let aspect = |v: Vec2| v.x / v.y;
        if (aspect(line.base) - aspect(line.end)).abs() < 1e-3 {
            self.cone(b, snout, (0.0, 1.0), self.s.y * 0.3);
        } else {
            self.cone(b, snout, (0.0, 0.5), self.s.y * 0.3);
            // Shares the middle's height, so the two blend into one smooth taper.
            let mid = line.size_at(0.5);
            let model = Transform { scale: Vec3::new(aspect(line.end), 1.0, 1.0), ..self.frame(line.start + line.dir * snout * 0.5) };
            let shape = PrimShape::RoundCone { r0: mid.y, r1: line.end.y, len: snout * 0.5 };
            b.base(self.bone, shape, model, mid.y * 0.6, self.region);
        }
        if self.params.dish > 0.0 {
            let at = line.size_at(0.35);
            let centre = line.start + line.dir * snout * 0.35 - Vec3::Z * at.y;
            let footprint = PrimShape::Ellipsoid { radii: Vec3::new(at.x * 0.9, snout * 0.3, at.y * 0.6) };
            let op = PrimOp::Displace { height: -self.params.dish * at.y * 0.4 };
            b.primitive(self.bone, footprint, self.frame(centre), 0.0, op, PrimLayer::Base, self.region);
        }
        self.build_tip(b);
    }

    /// The nostrils' rims and the upper lip round the tip of the snout.
    fn build_tip(&self, b: &mut AnatomyBuilder) {
        let (line, params) = (&self.line, self.params);
        let tip = line.end;
        if params.nostrils > 0.0 {
            // The rims stand out round the front and sides of the tip, a little above its middle.
            let scale = params.nostrils.min(2.0).sqrt();
            for x in [0.62, -0.62] {
                let centre = line.tip + line.dir * (tip.y * 0.45) + Vec3::new(x * tip.x, 0.0, -tip.y * 0.15);
                let radii = Vec3::new(tip.x * 0.32, tip.y * 0.5, tip.y * 0.45) * scale;
                b.base(self.bone, PrimShape::Ellipsoid { radii }, self.frame(centre), tip.y * 0.3, self.region);
            }
        }
        if params.lips > 0.0 {
            // The upper lip: a fleshy roll under the front of the tip, overhanging the jaw.
            let centre = line.tip - line.dir * (tip.y * 0.15) + Vec3::Z * (tip.y * 0.65);
            let radii = Vec3::new(tip.x * 0.9, tip.y * 0.8, tip.y * 0.42 * params.lips.min(2.0));
            b.base(self.bone, PrimShape::Ellipsoid { radii }, self.frame(centre), tip.y * 0.3, self.region);
        }
    }

    /// The lower jaw under the snout, hinged below the skull, with its cheeks and lower lip.
    /// Returns the `Jaw` slot (the chin).
    fn build_jaw(&self, b: &mut AnatomyBuilder, j: &ChainParams, region: RegionId) -> Result<(AttachSlot, AttachPoint), AnatomyError> {
        let params = self.params;
        let start = self.frame(Vec3::new(0.0, self.s.z * 0.6, self.s.y * 0.45));
        let jaw = b.bone(Some(self.bone), "jaw", start, j.length, region)?;
        let (r0, r1) = (radius_at(&j.radius, 0.0) * 0.8, radius_at(&j.radius, 1.0) * 0.6);
        let scale = Vec3::new(1.4 * params.jaw_width.max(0.05), 1.0, 0.6 * params.jaw_depth.max(0.05));
        b.base(jaw, PrimShape::RoundCone { r0, r1, len: j.length }, Transform { scale, ..start }, j.blend, region);
        if params.cheek > 0.0 {
            self.build_cheeks(b, jaw, region);
        }
        let chin = moved(start, Vec3::Y * j.length);
        if params.lips > 0.0 {
            let tip = self.line.end;
            let radii = Vec3::new(tip.x * 0.7, tip.y * 0.7, tip.y * 0.3 * params.lips.min(2.0));
            let model = moved(chin, Vec3::new(0.0, -tip.y * 0.2, -r1 * 0.3));
            b.base(jaw, PrimShape::Ellipsoid { radii }, model, tip.y * 0.25, region);
        }
        Ok(slot(AttachSlot::Jaw, jaw, chin))
    }

    /// Broad, flat cheeks (the jowls) over the back of the lower jaw, one each side.
    fn build_cheeks(&self, b: &mut AnatomyBuilder, bone: BoneId, region: RegionId) {
        let (c, s) = (self.params.cheek.min(2.0), self.s);
        for x in [0.6, -0.6] {
            let centre = Vec3::new(x * s.x, s.z * 1.15, s.y * 0.4f32.mul_add(c, 0.5));
            let radii = Vec3::new(s.x * 0.32, s.z * 0.7, s.y * 0.6) * Vec3::new(c.sqrt(), c.sqrt(), c);
            let model = moved(self.skull, centre);
            b.base(bone, PrimShape::Ellipsoid { radii }, model, s.y * 0.35, region);
        }
    }
}

/// The regions a face is built from: `Face` for the midface, cheeks and nose, `Brow`, `Eyelid`
/// and `Lip` for what coats dress with brows, lashes and lip colour, and the jaw's for the
/// mandible (`None`: no jaw).
struct FaceRegions {
    face: RegionId,
    brow: RegionId,
    eyelid: RegionId,
    lip: RegionId,
    jaw: Option<RegionId>,
}

/// A primate face on the skull (`skull` frame: X left, Y forward, Z down; `s` = skull half
/// extents: x width, y height, z depth): midface, brows, cheekbones, eyelids, nose and upper lip
/// on the skull bone; mandible, chin and lower lip on a jaw bone hinged in front of the ears.
/// `eye` is the left eye's centre in the skull frame and the eyeball's radius. Returns the jaw
/// bone and its chin frame.
///
/// Layout, in skull radii down from the skull centre (crown at -1): brow -0.22, eyes -0.05
/// (halfway from crown to chin), nose base 0.32, mouth 0.55, chin 0.95 — the facial thirds.
fn build_face(
    b: &mut AnatomyBuilder,
    face: &FaceParams,
    skull: Transform,
    s: Vec3,
    eye: (Vec3, f32),
    bone: BoneId,
    regions: &FaceRegions,
) -> Result<Option<(BoneId, Transform)>, AnatomyError> {
    let face_region = regions.face;
    let (rx, ry, rz) = (s.x, s.z, s.y);
    let cy = ry * 0.8; // skull centre, forward of the head's root
    // A point on the face: (across, forward from the skull centre, down), in skull radii.
    let p = |x: f32, y: f32, z: f32| Vec3::new(x * rx, y.mul_add(ry, cy), z * rz);
    let at = |q: Vec3| moved(skull, q);
    // A frame at `from` whose +Y runs to `to`, and the distance.
    let span = |from: Vec3, to: Vec3| {
        let delta = to - from;
        (Transform::from_translation(skull.transform_point(from)).with_rotation(skull.rotation * frame_y_z(delta, Vec3::Z)), delta.length())
    };
    let ellipsoid = |x: f32, y: f32, z: f32| PrimShape::Ellipsoid { radii: Vec3::new(x * rx, y * ry, z * rz) };

    // Midface (upper jaw and cheeks): the front of the face from under the eyes to the mouth.
    b.base(bone, ellipsoid(0.56, 0.38, 0.4), at(p(0.0, 0.5, 0.3)), rz * 0.3, face_region);
    // Brow ridges: one over each eye, meeting over the nose bridge where the blend joins them.
    if face.brow > 0.0 {
        for x in [0.3, -0.3] {
            b.base(bone, ellipsoid(0.32 * face.brow.sqrt(), 0.13, 0.07), at(p(x, 0.84, -0.22)), rz * 0.12, regions.brow);
        }
    }
    if face.cheekbones > 0.0 {
        let scale = face.cheekbones.sqrt();
        for x in [0.6, -0.6] {
            b.base(bone, ellipsoid(0.24 * scale, 0.2 * scale, 0.11), at(p(x, 0.62, 0.17)), rz * 0.2, face_region);
        }
    }
    // Nose: a ridge from between the eyes down and out to the tip, nostril wings either side.
    if face.nose > 0.0 {
        let tip = p(0.0, 0.25f32.mul_add(face.nose, 0.9), 0.27);
        let (model, len) = span(p(0.0, 0.86, -0.1), tip);
        b.base(bone, PrimShape::RoundCone { r0: rz * 0.05, r1: rz * 0.085, len }, model, rz * 0.04, face_region);
        for x in [0.14, -0.14] {
            let wing = PrimShape::Ellipsoid { radii: Vec3::new(rz * 0.045, rz * 0.055, rz * 0.045) };
            b.base(bone, wing, at(tip + Vec3::new(x * rx, -ry * 0.17, rz * 0.03)), rz * 0.08, face_region);
        }
    }
    if face.lips > 0.0 {
        // Small blends: the lips' region (and so their colour) stays on the lips.
        b.base(bone, ellipsoid(0.27, 0.06 * face.lips.sqrt(), 0.045), at(p(0.0, 0.86, 0.465)), rz * 0.03, regions.lip);
    }
    // Cheeks: the soft fill under each cheekbone, out to the jaw.
    if face.cheeks > 0.0 {
        let scale = face.cheeks.sqrt();
        for x in [0.55, -0.55] {
            b.base(bone, ellipsoid(0.3 * scale, 0.34, 0.3 * scale), at(p(x, 0.44, 0.5)), rz * 0.35, face_region);
        }
    }
    // Eyelids: a thin skin shell round each eyeball (a mound just bigger than the ball, blended
    // into the face), then the almond opening cut through its front, a little below the ball's
    // centre, that the cornea shows through. Last, so nothing fills the opening back in.
    let (eye_at, eye_r) = eye;
    if face.eyelids > 0.0 {
        for x in [1.0, -1.0] {
            let centre = Vec3::new(x * eye_at.x, eye_at.y, eye_at.z);
            // A small blend: a wide one would swell the lids a few millimetres clear of the ball.
            let lids = PrimShape::Ellipsoid { radii: Vec3::new(eye_r * 1.25, eye_r * 1.05, eye_r * 1.2) };
            b.base(bone, lids, at(centre), rz * 0.06, regions.eyelid);
            let opening = PrimShape::Ellipsoid { radii: Vec3::new(eye_r * 1.0, eye_r * 0.9, eye_r * 0.45 * face.eyelids.min(2.0)) };
            let model = at(centre + Vec3::new(0.0, eye_r * 0.55, eye_r * 0.05));
            b.primitive(bone, opening, model, eye_r * 0.1, PrimOp::Subtract, PrimLayer::Base, regions.eyelid);
        }
    }

    // The mouth: a thin cut along the line where the lips meet.
    if face.lips > 0.0 {
        let seam = ellipsoid(0.24, 0.1, 0.016);
        b.primitive(bone, seam, at(p(0.0, 0.9, 0.515)), rz * 0.02, PrimOp::Subtract, PrimLayer::Base, regions.lip);
    }

    let Some(jaw_region) = regions.jaw else { return Ok(None) };
    // Mandible: hinged in front of the ears, running down and forward to the chin.
    let chin = p(0.0, 0.08f32.mul_add(face.chin, 0.8), 0.95);
    let (jaw_model, jaw_len) = span(p(0.0, 0.0, 0.15), chin);
    let jaw = b.bone(Some(bone), "jaw", jaw_model, jaw_len, jaw_region)?;
    let mut add = |shape: PrimShape, model: Transform, blend: f32| {
        b.base(jaw, shape, model, blend, jaw_region);
    };
    // Each side of the jaw, from its angle below the ear to the chin.
    for x in [0.7 * face.jaw_width, -0.7 * face.jaw_width] {
        let (model, len) = span(p(x, -0.05, 0.72), chin + Vec3::new(x * rx * 0.3, -ry * 0.15, -rz * 0.05));
        add(PrimShape::RoundCone { r0: rz * 0.1, r1: rz * 0.09, len }, model, rz * 0.4);
    }
    // Floor of the mouth, so the jaw is solid underneath.
    add(ellipsoid(0.42, 0.38, 0.14), at(p(0.0, 0.45, 0.67)), rz * 0.3);
    if face.chin > 0.0 {
        let scale = face.chin.sqrt();
        add(ellipsoid(0.27 * scale, 0.12 * scale, 0.11), at(chin - Vec3::new(0.0, ry * 0.06, rz * 0.08)), rz * 0.1);
    }
    if face.lips > 0.0 {
        b.base(jaw, ellipsoid(0.24, 0.06 * face.lips.sqrt(), 0.045), at(p(0.0, 0.83, 0.565)), rz * 0.03, regions.lip);
    }
    Ok(Some((jaw, moved(jaw_model, Vec3::Y * jaw_len))))
}

// ---------------------------------------------------------------------------------------------
// Digits
// ---------------------------------------------------------------------------------------------

impl PartBuild for DigitParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let base = with_offset(&at, cx);
        let n = usize::from(self.count.max(1));
        let lens = self.chain.segment_lengths();
        let spacing = self.chain.radius.scale * 2.4;
        let curl = curl_bend(&lens, self.chain.curl);
        let region = b.region(RegionKind::Digit, cx.side)?;
        let hard_region = if self.tip.is_some() { Some(b.region(RegionKind::Hard, cx.side)?) } else { None };
        let mut end = None;
        // An opposable first digit (a thumb) roots back toward the wrist on the palm side, shorter
        // and thicker, turned to close across the palm; the rest line up along the end.
        let thumb = self.opposable && n > 1;
        let row = if thumb { n - 1 } else { n };
        let reach: f32 = lens.iter().sum();
        let radius = self.chain.radius.scale;
        let thumb_chain = ChainParams { radius: Profile { scale: radius * 1.2, ..self.chain.radius.clone() }, ..self.chain.clone() };
        let thumb_lens: Vec<f32> = lens.iter().map(|l| l * 0.75).collect();
        let mut digits = Vec::with_capacity(n);
        for i in 0..n {
            let is_thumb = thumb && i == 0;
            let place = if thumb { i.saturating_sub(1) } else { i };
            let k = if row > 1 { place.to_f32() / (row - 1).to_f32() - 0.5 } else { 0.0 };
            let start = if is_thumb {
                let edge = (spacing * 0.5).mul_add((row - 1).to_f32(), radius);
                base * Transform::from_translation(Vec3::new(-edge, -reach * 0.6, radius * 0.6))
                    .with_rotation(Quat::from_rotation_z(40f32.to_radians()) * Quat::from_rotation_y(70f32.to_radians()))
            } else {
                base * Transform::from_translation(Vec3::X * spacing * k * (row - 1).to_f32())
                    .with_rotation(Quat::from_rotation_z((-self.spread_deg * k).to_radians()))
            };
            let (chain, seg_lens) = if is_thumb { (&thumb_chain, &thumb_lens) } else { (&self.chain, &lens) };
            let frames = chain_frames(start, seg_lens, &curl);
            let name = suffixed(&format!("digit{i}"), cx);
            let (bones, _) = emit_chain(b, at.bone, &name, &frames, chain, |_| region)?;
            digits.push(bones.iter().copied().collect());
            let (Some(&last), Some(&tip_frame)) = (bones.last(), frames.last()) else {
                return Err(AnatomyError::Internal("digit without segments"));
            };
            if let (Some(tip), Some(hr)) = (&self.tip, hard_region) {
                b.hard_part(last, tip_frame, tip.clone(), hr);
            }
            if i == n / 2 {
                end = Some(slot(AttachSlot::End, last, tip_frame));
            }
        }
        b.hand(HandDef { side: cx.side, mirrored: b.mirrored(), digits, thumb });
        Ok(PartSlots { fixed: end.into_iter().collect(), ..default() })
    }
}

// ---------------------------------------------------------------------------------------------
// Mass and hard parts (no bones of their own)
// ---------------------------------------------------------------------------------------------

fn parent_bone(at: &AttachPoint, cx: &BuildCx) -> Result<BoneId, AnatomyError> {
    at.bone.ok_or_else(|| AnatomyError::InvalidParams {
        path: cx.path.clone(),
        reason: "needs a parent part with bones".into(),
    })
}

impl PartBuild for MassParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let bone = parent_bone(&at, cx)?;
        let region = b.region(RegionKind::Mass, cx.side)?;
        let s = self.size;
        let model = moved(with_offset(&at, cx), Vec3::Y * s.y * 0.5);
        let shape = match self.shape {
            MassShape::Sphere => PrimShape::Sphere { r: s.max_element() },
            MassShape::Ellipsoid | MassShape::Hump => PrimShape::Ellipsoid { radii: s },
            MassShape::Crest => PrimShape::Ellipsoid { radii: Vec3::new(s.x * 0.25, s.y, s.z) },
            MassShape::Flap => PrimShape::Ellipsoid { radii: Vec3::new(s.x, s.y, s.z * 0.2) },
            MassShape::Sac => PrimShape::Sphere { r: s.y },
        };
        let layer = if self.shape == MassShape::Sac { PrimLayer::Fat } else { PrimLayer::Base };
        b.primitive(bone, shape, model, self.blend, PrimOp::Union, layer, region);
        Ok(PartSlots::end(bone, moved(model, Vec3::Y * s.y)))
    }
}

/// The frame an authored part is placed in: the slot frame, or just its position (`Model`).
fn authored_frame(at: &AttachPoint, cx: &BuildCx, orient: Orient) -> Transform {
    match orient {
        Orient::Slot => with_offset(at, cx),
        Orient::Model => Transform::from_translation(at.model.translation) * cx.offset,
    }
}

impl PartBuild for HardParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let bone = parent_bone(&at, cx)?;
        let region = b.region(RegionKind::Hard, cx.side)?;
        let model = match &self.kind {
            HardKind::Mesh { path, orient, .. } => {
                if b.assets().mesh(path).is_none() {
                    return Err(AnatomyError::MissingAsset { path: cx.path.clone(), asset: path.clone() });
                }
                authored_frame(&at, cx, *orient)
            }
            _ => with_offset(&at, cx),
        };
        b.hard_part(bone, model, self.clone(), region);
        Ok(PartSlots::default())
    }
}

// ---------------------------------------------------------------------------------------------
// Sculpt (authored soft part)
// ---------------------------------------------------------------------------------------------

impl PartBuild for SculptParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let grid = b
            .assets()
            .sdf(&self.mesh, self.resolution)
            .ok_or_else(|| AnatomyError::MissingAsset { path: cx.path.clone(), asset: self.mesh.clone() })?;
        let base = authored_frame(&at, cx, self.orient);
        let (lo, hi) = grid.bounds();
        let (lo, hi) = (lo * self.scale, hi * self.scale);
        let region = b.region(self.region, cx.side)?;
        let bone = if self.own_bone {
            let bone_frame = Transform::from_translation(base.translation).with_rotation(at.model.rotation);
            b.bone(at.bone, &suffixed("sculpt", cx), bone_frame, (hi - lo).max_element(), region)?
        } else {
            parent_bone(&at, cx)?
        };
        b.base(bone, PrimShape::Grid { grid, mirror_x: false }, Transform { scale: Vec3::splat(self.scale), ..base }, self.blend, region);
        // Slots along the part's local +Y, through the middle of its bounds.
        let mid = (lo + hi) * 0.5;
        let radius = ((hi.x - lo.x) + (hi.z - lo.z)) * 0.25;
        let sample = |t: f32| moved(base, Vec3::new(mid.x, f32::mul_add(hi.y - lo.y, t, lo.y), mid.z));
        let axis = [0.0, 0.5, 1.0]
            .into_iter()
            .map(|t| AxisSample::round(t, bone, sample(t), radius))
            .collect();
        Ok(PartSlots {
            fixed: [slot(AttachSlot::Start, bone, sample(0.0)), slot(AttachSlot::End, bone, sample(1.0))]
                .into_iter()
                .collect(),
            axis,
            skull: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{Anatomy, build_src, sample_axis};

    /// A lone torso (3 bones, 1 m, radius 0.2) with the given `Profile` fields.
    fn torso(profile: &str) -> Anatomy {
        build_src(&format!(
            "(root: (name: \"torso\", kind: Torso((chain: (segments: 3, length: 1.0, radius: (curve: [(0.0, 1.0), (1.0, 0.8)], scale: 0.2, {profile}))))))"
        ))
    }

    /// The torso's base-layer cones, in model space: (start, end, r0, r1, scale).
    fn cones(a: &Anatomy) -> Vec<(Vec3, Vec3, f32, f32, Vec3)> {
        let model = a.skeleton.model_rest();
        a.primitives
            .iter()
            .filter(|p| p.layer == PrimLayer::Base)
            .filter_map(|p| {
                let PrimShape::RoundCone { r0, r1, len } = p.shape else { return None };
                let m = model[p.bone.index()] * p.local.compute_affine();
                Some((m.transform_point3(Vec3::ZERO), m.transform_point3(Vec3::Y * len), r0, r1, p.local.scale))
            })
            .collect()
    }

    #[test]
    fn empty_and_identity_section_curves_change_nothing() {
        let plain = torso("cross: Ellipse(aspect: 0.8)");
        let identity = torso("cross: Ellipse(aspect: 0.8), width: [(0.0, 1.0), (1.0, 1.0)], depth: [(0.0, 1.0)], drop: [(0.0, 0.0)]");
        let (a, b) = (cones(&plain), cones(&identity));
        assert_eq!(a.len(), 3);
        assert_eq!(a.len(), b.len(), "constant curves keep one piece per bone");
        for (x, y) in a.iter().zip(&b) {
            assert!(x.0.distance(y.0) < 1e-5 && x.1.distance(y.1) < 1e-5, "{x:?} vs {y:?}");
            assert!((x.2 - y.2).abs() < 1e-5 && (x.3 - y.3).abs() < 1e-5 && x.4.distance(y.4) < 1e-5);
        }
        let (pa, pb) = (&plain.parts[0].axis, &identity.parts[0].axis);
        assert_eq!(pa.len(), pb.len());
        for (s, u) in pa.iter().zip(pb) {
            assert!(s.radii.distance(u.radii) < 1e-5 && u.drop.abs() < 1e-6);
        }
        // The same within 1% by weight, though keys inside the bones split them into pieces.
        let split = torso("cross: Ellipse(aspect: 0.8), width: [(0.0, 1.0), (0.2, 1.0), (0.5, 1.0), (0.8, 1.0), (1.0, 1.0)]");
        assert_eq!(cones(&split).len(), 6, "split at the keys");
        let (m0, m1) = (plain.mass.total, split.mass.total);
        assert!((m1 / m0 - 1.0).abs() < 0.01, "{m0} kg vs {m1} kg");
    }

    #[test]
    fn axis_samples_follow_the_section_curves() {
        let a = torso("cross: Ellipse(aspect: 0.64), width: [(0.0, 1.0), (1.0, 2.0)], depth: [(0.0, 1.0), (0.5, 0.5), (1.0, 1.0)], drop: [(0.0, 0.0), (1.0, 0.5)]");
        let axis = &a.parts[0].axis;
        assert!(cones(&a).len() > 3, "varying sections split the bones");
        // Ellipse(0.64): width × 0.8, height × 1.25 of the radius.
        for t in [0.0, 0.25, 0.5, 0.7, 1.0] {
            let s = sample_axis(axis, t).unwrap();
            let r = 0.2f32.lerp(0.16, t);
            let width = 1.0f32.lerp(2.0, t);
            let depth = if t < 0.5 { 1.0f32.lerp(0.5, t * 2.0) } else { 0.5f32.lerp(1.0, t.mul_add(2.0, -1.0)) };
            let want = Vec2::new(0.8 * width, 1.25 * depth) * r;
            // Samples sit at piece ends, so a radius between them is interpolated (≤ 2%).
            assert!((s.radii - want).abs().max_element() < 0.02 * want.max_element(), "t {t}: {} vs {want}", s.radii);
            let want_drop = 0.5 * t * r;
            assert!((s.drop - want_drop).abs() < 0.01f32.mul_add(r, 1e-4), "t {t}: drop {} vs {want_drop}", s.drop);
            // The ventral surface point is the dropped centre plus the half height.
            let below = s.surface_toward(std::f32::consts::PI);
            let want_below = s.model.translation + s.model.rotation * Vec3::Z * (s.drop + s.radii.y);
            assert!(below.distance(want_below) < 1e-4);
        }
    }

    #[test]
    fn teardrop_hangs_a_keel_under_the_ellipse() {
        let ellipse = torso("cross: Ellipse(aspect: 0.9)");
        let teardrop = torso("cross: Teardrop(aspect: 0.9)");
        assert_eq!(cones(&ellipse).len(), 3);
        assert_eq!(cones(&teardrop).len(), 6, "a keel under each bone");
        let (e, d) = (&ellipse.parts[0].axis[1], &teardrop.parts[0].axis[1]);
        // Same top and width, deeper below.
        let top = |s: &AxisSample| s.drop - s.radii.y;
        assert!((top(e) - top(d)).abs() < 1e-5 && (e.radii.x - d.radii.x).abs() < 1e-5);
        let bottom = |s: &AxisSample| s.drop + s.radii.y;
        assert!((bottom(d) / bottom(e) - (1.0 + CrossSection::TEARDROP_KEEL)).abs() < 1e-4);
        // The keel is narrow and adds a little weight.
        let keel = cones(&teardrop)[1];
        assert!(keel.2 < cones(&ellipse)[0].2);
        assert!(teardrop.mass.total > ellipse.mass.total && teardrop.mass.total < ellipse.mass.total * 1.1);
    }
}

#[cfg(test)]
mod arm_clearance {
    use bevy::math::Vec3;

    use crate::anatomy::build_src;

    /// How far a plan's left arm hangs out from straight down, degrees.
    fn arm_angle(ron: &str) -> f32 {
        let a = build_src(ron);
        let rest = a.skeleton.rest_model();
        let arm = a.clavicles.first().unwrap().arm;
        let elbow = a.skeleton.children(arm).next().unwrap();
        let down = (Vec3::from(rest[elbow.index()].translation) - Vec3::from(rest[arm.index()].translation)).normalize();
        down.angle_between(Vec3::NEG_Y).to_degrees()
    }

    /// A slim body keeps its authored splay (10°); padding pushes the arms out, more for more.
    #[test]
    fn arms_clear_padding() {
        let biped = arm_angle(include_str!("../../assets/mobs/biped.body.ron"));
        let curvy = arm_angle(include_str!("../../assets/mobs/curvy.body.ron"));
        let thick = arm_angle(include_str!("../../assets/mobs/thick.body.ron"));
        assert!((biped - 10.0).abs() < 0.5, "biped arm {biped}°");
        assert!(curvy >= biped && thick > curvy + 4.0, "biped {biped}°, curvy {curvy}°, thick {thick}°");
    }
}
