//! Wings: membrane (bat, dragon, pterosaur), feathered (bird) and insect blades. See
//! `docs/design/wings.md` for the anatomy they follow.
//!
//! - Bones are built in the **spread** pose (the bind pose); `WingParams::fold` blends every
//!   wing bone toward its folded rotation at rest ([`WingRig`], applied by the pose solver and
//!   the crowd bake).
//! - Arm and finger bones are thin SDF tubes in the body; everything thinner than the mesh
//!   voxel (membranes, flight feathers, insect blades) is a generated [`Sheet`], skinned across
//!   the bones.

use std::f32::consts::PI;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::num::{Clamp, ToF32};

use super::{
    AnatomyBuilder, AnatomyError, AttachPoint, AttachSlot, BoneId, BuildCx, HardKind, HardParams, LodLevel, PartBuild,
    PartSlots, PrimLayer, PrimOp, PrimShape, RegionId, RegionKind, frame_y_z, sample_axis,
};

// ---------------------------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------------------------

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct WingParams {
    pub kind: WingKind,
    /// Root-to-tip length of one spread wing, metres.
    pub span: f32,
    /// Width at the root (chord), metres.
    pub chord: f32,
    /// Rest pose: 0 = spread, 1 = folded.
    pub fold: f32,
    /// Angle back from straight out when spread, degrees.
    pub sweep_deg: f32,
    /// Arm bone radius at the shoulder, metres (fingers and tips taper from it).
    pub radius: f32,
}

impl Default for WingParams {
    fn default() -> Self {
        Self { kind: WingKind::Membrane(MembraneWing::default()), span: 1.0, chord: 0.5, fold: 1.0, sweep_deg: 10.0, radius: 0.03 }
    }
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
pub enum WingKind {
    /// Bat and dragon (`fingers` 2–4) or pterosaur (`fingers: 1`).
    Membrane(MembraneWing),
    /// Bird: arm bones carrying rows of flight feathers.
    Feathered(FeatheredWing),
    /// Insect: a rigid blade on a hinge, or a hardened elytron.
    Blade(BladeWing),
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct MembraneWing {
    /// Membrane-carrying fingers, 1 (pterosaur) to 4.
    pub fingers: u8,
    /// Where the membrane's trailing edge meets the body.
    pub trailing: Trailing,
    /// How far the trailing edge sags between finger tips, as a fraction of the gap.
    pub scallop: f32,
    /// A free clawed thumb at the wrist.
    pub thumb: bool,
}

impl Default for MembraneWing {
    fn default() -> Self {
        Self { fingers: 3, trailing: Trailing::Body(0.35), scallop: 0.22, thumb: true }
    }
}

/// Where a membrane's trailing edge attaches, on the part the wing grows from.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Trailing {
    /// At `t` along the parent part (0 = its rear end).
    Body(f32),
    /// Low on the rear of the body, where the hind legs join.
    HindLeg,
    /// At the base of the tail.
    Tail,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct FeatheredWing {
    /// Flight feathers on the hand.
    pub primaries: u8,
    /// Flight feathers on the forearm.
    pub secondaries: u8,
    /// Rows of shorter covert feathers over the flight feathers' bases (hero detail only).
    pub covert_rows: u8,
    pub shape: WingShape,
}

impl Default for FeatheredWing {
    fn default() -> Self {
        Self { primaries: 10, secondaries: 12, covert_rows: 2, shape: WingShape::Elliptical }
    }
}

/// Classic bird wing planforms.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WingShape {
    /// Songbirds: short, rounded.
    #[default]
    Elliptical,
    /// Falcons, swifts: swept, pointed.
    HighSpeed,
    /// Albatrosses, gulls: long and narrow.
    HighAspect,
    /// Eagles, vultures: broad with separated ("fingered") tip primaries.
    Slotted,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct BladeWing {
    pub planform: Planform,
    /// A hardened, domed forewing (beetles) instead of a veined membrane.
    pub elytron: bool,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Planform {
    /// Dragonflies: long and narrow.
    #[default]
    Long,
    /// Butterflies, moths: broad.
    Broad,
    /// Bees, flies: short and rounded.
    Round,
}

// ---------------------------------------------------------------------------------------------
// Built output
// ---------------------------------------------------------------------------------------------

/// What a [`Sheet`] is; also tells the shader how to shade it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SheetKind {
    #[default]
    Membrane = 0,
    FlightFeather = 1,
    InsectWing = 2,
    Elytron = 3,
}

impl From<SheetKind> for u8 {
    fn from(kind: SheetKind) -> Self {
        match kind {
            SheetKind::Membrane => 0,
            SheetKind::FlightFeather => 1,
            SheetKind::InsectWing => 2,
            SheetKind::Elytron => 3,
        }
    }
}

/// A thin generated surface skinned across bones (stored model space, bind pose).
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub kind: SheetKind,
    pub part: u16,
    pub region: RegionId,
    /// Coarsest level this sheet appears at.
    pub lod: LodLevel,
    pub positions: Vec<Vec3>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    /// Sheet coordinates for shading: membrane panel (across, along), feather (across, along),
    /// insect wing (along, across).
    pub uvs: Vec<[f32; 2]>,
    /// Shading direction per vertex: finger rib, feather shaft, wing length.
    pub dirs: Vec<Vec3>,
    /// 0 = thinnest (most translucent) .. 1 = thick (bone, shaft, elytron).
    pub thickness: Vec<f32>,
    pub indices: Vec<u32>,
}

/// Folded local rotations for a wing's bones; the pose blends rest → folded by `fold`.
#[derive(Clone, Debug)]
pub struct WingRig {
    pub part: u16,
    pub fold: f32,
    pub bones: Vec<(BoneId, Quat)>,
}

// ---------------------------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------------------------

/// The wing's world-ish axes at its root (logical model space).
struct Axes {
    root: Vec3,
    out: Vec3,
    fwd: Vec3,
    up: Vec3,
}

impl Axes {
    fn new(at: &AttachPoint, cx: &BuildCx) -> Self {
        let base = at.model * cx.offset;
        let n = base.rotation * Vec3::Y;
        let out = Vec3::new(n.x, 0.0, n.z).try_normalize().unwrap_or(Vec3::X);
        // Wings grow sideways: keep "out" lateral even on a dorsal attach.
        let out = if out.x.abs() < 0.3 { Vec3::X } else { Vec3::new(out.x.signum(), 0.0, 0.0).lerp(out, 0.3).normalize() };
        Self { root: base.translation, out, fwd: Vec3::Z, up: Vec3::Y }
    }

    /// `out` turned back (toward -Z) by `deg` and up by `rise` degrees.
    fn dir(&self, back_deg: f32, rise_deg: f32) -> Vec3 {
        let yaw = Quat::from_axis_angle(self.up, back_deg.to_radians() * self.out.x.signum());
        let d = yaw * self.out;
        let pitch_axis = d.cross(self.up).normalize_or(self.fwd);
        (Quat::from_axis_angle(pitch_axis, -rise_deg.to_radians()) * d).normalize()
    }

    fn mix(&self, out: f32, fwd: f32, up: f32) -> Vec3 {
        (self.out * out + self.fwd * fwd + self.up * up).normalize()
    }
}

/// A chain of segments from `start` along `dirs`, as bone frames (Y along, Z = `normal`).
fn frames_along(start: Vec3, dirs: &[(Vec3, f32)], normal: Vec3) -> Vec<Transform> {
    let mut p = start;
    let mut out = Vec::with_capacity(dirs.len() + 1);
    for (d, len) in dirs {
        out.push(Transform::from_translation(p).with_rotation(frame_y_z(*d, normal)));
        p += *d * *len;
    }
    out.push(Transform::from_translation(p).with_rotation(frame_y_z(dirs.last().map_or(Vec3::X, |d| d.0), normal)));
    out
}

/// `frames` or `bones` of a chain whose length the caller fixed.
fn fixed<T, const N: usize>(items: Vec<T>) -> Result<[T; N], AnatomyError> {
    items.try_into().map_err(|_| AnatomyError::Internal("wing chain has an unexpected length"))
}

/// Thin tapered tubes along a bone chain.
fn tubes(b: &mut AnatomyBuilder, bones: &[BoneId], frames: &[Transform], r0: f32, r1: f32, region: RegionId, first_blend: f32) {
    let n = bones.len().to_f32();
    for (i, (bone, [start, end])) in bones.iter().zip(frames.array_windows()).enumerate() {
        let t0 = i.to_f32() / n;
        let t1 = (i + 1).to_f32() / n;
        let (a, c) = ((r1 - r0).mul_add(t0, r0), (r1 - r0).mul_add(t1, r0));
        let len = start.translation.distance(end.translation).max(1e-3);
        let blend = if i == 0 { first_blend } else { 0.4 * a.min(c) };
        b.primitive(*bone, PrimShape::RoundCone { r0: a, r1: c, len }, *start, blend, PrimOp::Union, PrimLayer::Base, region);
        b.anchor_last_blend(Vec3::ZERO, 2.0f32.mul_add(blend, 2.0 * a));
    }
}

/// Adds bones for `frames` (each parented to the previous), returning them.
fn chain_bones(
    b: &mut AnatomyBuilder,
    parent: Option<BoneId>,
    name: &str,
    frames: &[Transform],
    region: RegionId,
) -> Result<Vec<BoneId>, AnatomyError> {
    let mut bones = Vec::with_capacity(frames.len().saturating_sub(1));
    let mut prev = parent;
    for (i, [start, end]) in frames.array_windows().enumerate() {
        let len = start.translation.distance(end.translation).max(1e-3);
        let bone = b.bone(prev, &format!("{name}.{i}"), *start, len, region)?;
        bones.push(bone);
        prev = Some(bone);
    }
    Ok(bones)
}

/// Resamples a polyline to `n + 1` points evenly by arc length, also returning the segment
/// each point lies on. Empty if the polyline has fewer than two points.
fn resample(poly: &[Vec3], n: usize) -> Vec<(Vec3, usize)> {
    let segments: Vec<(Vec3, Vec3, f32)> = poly.array_windows().map(|&[a, b]| (a, b, a.distance(b))).collect();
    let Some(last) = segments.len().checked_sub(1) else { return Vec::new() };
    let total: f32 = segments.iter().map(|s| s.2).sum::<f32>().max(1e-6);
    (0..=n)
        .filter_map(|k| {
            let mut d = total * k.to_f32() / n.to_f32();
            for (s, &(a, b, len)) in segments.iter().enumerate() {
                if d <= len || s == last {
                    return Some((a.lerp(b, (d / len.max(1e-6)).clamp(0.0, 1.0)), s));
                }
                d -= len;
            }
            None
        })
        .collect()
}

/// A ruled grid between two polylines with matching ends; `bone_a`/`bone_b` give the bone at
/// each polyline segment. Returns (positions, joints, weights, uvs, dirs, thickness) appended
/// to `sheet`. `sag(c, k)` offsets interior points (scallops).
#[allow(clippy::too_many_arguments)]
fn ruled_panel(
    sheet: &mut Sheet,
    edge_a: &[Vec3],
    bone_a: &[BoneId],
    edge_b: &[Vec3],
    bone_b: &[BoneId],
    rows: usize,
    cols: usize,
    sag: impl Fn(f32, f32, Vec3, Vec3) -> Vec3,
    rib_thickness: impl Fn(f32, f32) -> f32,
) {
    let ra = resample(edge_a, rows);
    let rb = resample(edge_b, rows);
    if ra.len() != rows + 1 || rb.len() != rows + 1 {
        return;
    }
    // Direction along a resampled edge at row `k`.
    let along = |r: &[(Vec3, usize)], k: usize| match (r.get((k + 1).min(rows)), r.get(k.saturating_sub(1))) {
        (Some(next), Some(prev)) => next.0 - prev.0,
        _ => Vec3::ZERO,
    };
    // The bone at segment `s` of an edge (its last bone past the end).
    let bone_at = |bones: &[BoneId], s: usize| bones.get(s).or_else(|| bones.last()).copied().unwrap_or(BoneId(0));
    let base = sheet.positions.len().clamp_u32();
    for (k, (&(pa, sa), &(pb, sb))) in ra.iter().zip(&rb).enumerate() {
        let v = k.to_f32() / rows.to_f32();
        let (next_a, next_b) = (along(&ra, k), along(&rb, k));
        let (ja, jb) = (bone_at(bone_a, sa), bone_at(bone_b, sb));
        for m in 0..=cols {
            let c = m.to_f32() / cols.to_f32();
            sheet.positions.push(pa.lerp(pb, c) + sag(c, v, pa, pb));
            if ja == jb {
                sheet.joints.push([u16::from(ja.0), 0, 0, 0]);
                sheet.weights.push([1.0, 0.0, 0.0, 0.0]);
            } else {
                sheet.joints.push([u16::from(ja.0), u16::from(jb.0), 0, 0]);
                sheet.weights.push([1.0 - c, c, 0.0, 0.0]);
            }
            sheet.uvs.push([c, v]);
            sheet.dirs.push(next_a.lerp(next_b, c).normalize_or(Vec3::X));
            sheet.thickness.push(rib_thickness(c, v));
        }
    }
    grid_indices(sheet, base, rows.clamp_u32(), cols.clamp_u32());
}

/// Triangles of a `(rows + 1) × (cols + 1)` vertex grid starting at vertex `base`, row-major.
fn grid_indices(sheet: &mut Sheet, base: u32, rows: u32, cols: u32) {
    let stride = cols + 1;
    for k in 0..rows {
        for m in 0..cols {
            let i = base + k * stride + m;
            sheet.indices.extend([i, i + stride, i + 1, i + 1, i + stride, i + stride + 1]);
        }
    }
}

impl PartBuild for WingParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let axes = Axes::new(&at, cx);
        match &self.kind {
            WingKind::Membrane(m) => build_membrane(self, m, b, &at, cx, &axes),
            WingKind::Feathered(f) => build_feathered(self, f, b, &at, cx, &axes),
            WingKind::Blade(w) => build_blade(self, w, b, &at, cx, &axes),
        }
    }
}

fn end_slots(bone: BoneId, tip: Transform) -> PartSlots {
    PartSlots { fixed: std::iter::once((AttachSlot::End, AttachPoint { bone: Some(bone), model: tip })).collect(), ..default() }
}

/// A membrane wing finger: frames root, knuckle, tip and its two bones.
type Finger = ([Transform; 3], [BoneId; 2]);

fn build_membrane(
    wing: &WingParams,
    membrane: &MembraneWing,
    b: &mut AnatomyBuilder,
    at: &AttachPoint,
    cx: &BuildCx,
    ax: &Axes,
) -> Result<PartSlots, AnatomyError> {
    let span = wing.span.max(0.05);
    let count = usize::from(membrane.fingers.clamp(1, 4));
    // Bat/dragon: long forearm, short metacarpals, digit III longest. Pterosaur: one huge
    // fourth finger on a long metacarpal.
    let (hum, fore, meta, finger_lens): (f32, f32, f32, Vec<f32>) = if count == 1 {
        (0.14, 0.22, 0.12, vec![0.52])
    } else {
        (0.16, 0.27, 0.05, (0..count).map(|i| 0.55 * 0.18f32.mul_add(-i.to_f32(), 1.0)).collect())
    };
    let fan = match count {
        1 => 0.0,
        2 => 60.0,
        3 => 95.0,
        _ => 110.0,
    };
    let arm_region = b.region(RegionKind::Wing, cx.side)?;
    let sheet_region = b.region(RegionKind::WingMembrane, cx.side)?;
    let up = ax.up;

    // Spread: humerus out-and-back, forearm out-and-forward (elbow back), fingers fanning back.
    let sweep = wing.sweep_deg;
    let arm: [Transform; 4] = fixed(frames_along(
        ax.root,
        &[(ax.dir(sweep + 22.0, 8.0), hum * span), (ax.dir(sweep - 12.0, 4.0), fore * span), (ax.dir(sweep - 8.0, 0.0), meta * span)],
        up,
    ))?;
    let arm_bones: [BoneId; 3] = fixed(chain_bones(b, at.bone, "wing", &arm, arm_region)?)?;
    let wrist = arm[3].translation;
    let mut fingers: Vec<Finger> = Vec::with_capacity(count);
    for (i, len) in finger_lens.iter().enumerate() {
        let a = if count == 1 { sweep + 12.0 } else { sweep - 6.0 + i.to_f32() * (fan + 6.0) / (count - 1).to_f32() };
        let d0 = ax.dir(a, -2.0);
        let d1 = ax.dir(a + 8.0, -4.0);
        let f = fixed(frames_along(wrist, &[(d0, len * span * 0.55), (d1, len * span * 0.45)], up))?;
        let bones = fixed(chain_bones(b, Some(arm_bones[2]), &format!("wing.finger{i}"), &f, arm_region)?)?;
        fingers.push((f, bones));
    }
    let (Some(&first), Some(&(tip_frames, tip_bones))) = (fingers.first(), fingers.last()) else {
        return Err(AnatomyError::Internal("membrane wing without fingers"));
    };
    let radius = wing.radius;
    tubes(b, &arm_bones, &arm, radius, radius * 0.55, arm_region, radius * 1.4);
    for (f, bones) in &fingers {
        tubes(b, bones, f, radius * 0.42, radius * 0.12, arm_region, radius * 0.2);
    }
    if membrane.thumb {
        let thumb = Transform::from_translation(wrist).with_rotation(frame_y_z(ax.dir(sweep - 50.0, 10.0), up));
        b.hard_part(arm_bones[2], thumb, HardParams { kind: HardKind::Claw, size: span * 0.05, curve: 0.5, twist: 0.0, sclera: 0.0 }, arm_region);
    }

    // Membrane panels between neighbouring fingers (dactylopatagium), scalloped between tips.
    let mut sheet = Sheet { kind: SheetKind::Membrane, region: sheet_region, lod: LodLevel::Far, ..default() };
    let scallop = membrane.scallop;
    for [(fa, ba), (fb, bb)] in fingers.array_windows() {
        let pa = fa.map(|t| t.translation);
        let pb = fb.map(|t| t.translation);
        let gap = pa[2].distance(pb[2]);
        ruled_panel(
            &mut sheet,
            &pa,
            ba,
            &pb,
            bb,
            14,
            8,
            |c, v, pa, pb| {
                let toward_wrist = (wrist - (pa + pb) * 0.5).normalize_or(Vec3::ZERO);
                toward_wrist * scallop * gap * (PI * c).sin() * v.powi(3)
            },
            |c, v| (PI * c).sin().powf(0.6).mul_add(-0.85, 1.0) * 0.3f32.mul_add(-v, 1.0),
        );
    }

    // Main membrane (plagiopatagium): arm + last finger in front, body and trailing edge
    // behind.
    let tip = tip_frames[2].translation;
    let lead = [arm[0].translation, arm[1].translation, arm[2].translation, wrist, tip_frames[1].translation, tip];
    let lead_bones = [arm_bones[0], arm_bones[1], arm_bones[2], tip_bones[0], tip_bones[1]];
    let (trail_point, trail_bone) = trailing_point(b, membrane.trailing, ax, at, span, wing.chord);
    let trail_len = trail_point.distance(tip);
    let back = [ax.root, trail_point, tip];
    let back_bones = [trail_bone.or(at.bone).unwrap_or(arm_bones[0]), tip_bones[1]];
    ruled_panel(
        &mut sheet,
        &lead,
        &lead_bones,
        &back,
        &back_bones,
        18,
        10,
        |c, v, pa, pb| {
            // The free trailing edge (far half of `back`) bows in toward the arm.
            let inward = (pa - pb).normalize_or(Vec3::ZERO);
            inward * scallop * trail_len * 0.35 * c * (PI * v).sin() * ((v - 0.35) / 0.65).clamp(0.0, 1.0)
        },
        |c, v| 0.1f32.mul_add(1.0 - v, f32::mul_add((1.0 - c).powi(3), 0.8, 0.12)),
    );
    b.sheet(sheet);

    let finger_lens: Vec<f32> = finger_lens.iter().map(|len| len * span).collect();
    membrane_fold(b, wing.fold, ax, arm_bones, &fingers, &finger_lens, [hum * span, fore * span, meta * span])?;
    Ok(end_slots(first.1[1], first.0[2]))
}

/// Folds a membrane wing: humerus back and down, forearm forward along the flank, fingers back
/// along it, the wing plane turned to face out. `finger_lens` and `arm` are segment lengths, metres.
fn membrane_fold(
    b: &mut AnatomyBuilder,
    fold: f32,
    ax: &Axes,
    arm_bones: [BoneId; 3],
    fingers: &[Finger],
    finger_lens: &[f32],
    arm: [f32; 3],
) -> Result<(), AnatomyError> {
    let out = ax.out;
    let fold_arm: [Transform; 4] = fixed(frames_along(
        ax.root,
        &[(ax.mix(0.3, -1.0, -0.35), arm[0]), (ax.mix(0.2, 1.0, 0.28), arm[1]), (ax.mix(0.18, 1.0, 0.1), arm[2])],
        out,
    ))?;
    let mut folded: Vec<(BoneId, Transform)> = arm_bones.iter().copied().zip(fold_arm.iter().copied()).collect();
    let fold_wrist = fold_arm[3].translation;
    for (i, ((_, bones), len)) in fingers.iter().zip(finger_lens).enumerate() {
        let tilt = i.to_f32() * 0.06;
        let d = ax.mix(tilt.mul_add(0.3, 0.16), -1.0, -0.42 - tilt);
        let ff = frames_along(fold_wrist, &[(d, len * 0.55), (d, len * 0.45)], out);
        folded.extend(bones.iter().copied().zip(ff.iter().copied()));
    }
    b.wing_rig(fold, &folded);
    Ok(())
}

/// Body point (and its bone) where a membrane's trailing edge attaches.
fn trailing_point(b: &AnatomyBuilder, trailing: Trailing, ax: &Axes, at: &AttachPoint, span: f32, chord: f32) -> (Vec3, Option<BoneId>) {
    let (t, around) = match trailing {
        Trailing::Body(t) => (t, 95.0f32),
        Trailing::HindLeg => (0.12, 115.0),
        Trailing::Tail => (0.0, 60.0),
    };
    let axis = b.parent_axis();
    if let Some(s) = sample_axis(&axis, t) {
        let a = around.to_radians();
        let dorsal = s.model.rotation * Vec3::NEG_Z;
        let side = (s.model.rotation * Vec3::X) * ax.out.x.signum();
        let normal = (dorsal * a.cos() + side * a.sin()).normalize();
        return (s.centre() + normal * s.radius_toward(a) * 0.9, Some(s.bone));
    }
    (ax.root - ax.fwd * chord.max(span * 0.3), at.bone)
}

/// The feather tract bones of a bird wing (see [`build_feathered`]).
#[derive(Clone, Copy)]
struct Tracts {
    secondary: BoneId,
    primary_inner: BoneId,
    primary_outer: BoneId,
}

fn build_feathered(
    wing: &WingParams,
    feathered: &FeatheredWing,
    b: &mut AnatomyBuilder,
    at: &AttachPoint,
    cx: &BuildCx,
    ax: &Axes,
) -> Result<PartSlots, AnatomyError> {
    let span = wing.span.max(0.05);
    let up = ax.up;
    let arm_region = b.region(RegionKind::Wing, cx.side)?;
    let feather_region = b.region(RegionKind::WingMembrane, cx.side)?;
    // The arm is ~55% of the span; primaries carry the rest.
    let (hum, uln, hand) = (0.2 * span, 0.24 * span, 0.13 * span);
    let sweep = wing.sweep_deg;
    let arm: [Transform; 4] =
        fixed(frames_along(ax.root, &[(ax.dir(sweep + 22.0, 6.0), hum), (ax.dir(sweep - 10.0, 3.0), uln), (ax.dir(sweep + 2.0, 0.0), hand)], up))?;
    let bones: [BoneId; 3] = fixed(chain_bones(b, at.bone, "wing", &arm, arm_region)?)?;
    let radius = wing.radius;
    tubes(b, &bones, &arm, radius, radius * 0.4, arm_region, radius * 1.6);
    // Feather tracts: one bone per flight-feather row, so a folding wing can turn its feathers
    // to lie back along the flank (real flight feathers pivot at their roots as the wing
    // closes; rigid on the arm bones they would stand up like a display fan).
    // Primaries get two tracts (inner, outer), each feather weighted between them by its place
    // in the fan: folding turns both to point back, which collapses the fan.
    let tracts = Tracts {
        secondary: b.bone(Some(bones[1]), "wing.secondaries", arm[1], uln, arm_region)?,
        primary_inner: b.bone(Some(bones[2]), "wing.primaries.in", arm[2], hand, arm_region)?,
        primary_outer: b.bone(Some(bones[2]), "wing.primaries.out", arm[2], hand, arm_region)?,
    };
    let Tracts { secondary: secondary_tract, primary_inner, primary_outer } = tracts;

    let primary_len = span * 0.42;
    let secondary_len = wing.chord.max(span * 0.15);
    let mut sheet = Sheet { kind: SheetKind::FlightFeather, region: feather_region, lod: LodLevel::Far, ..default() };
    let mut coverts = Sheet { kind: SheetKind::FlightFeather, region: feather_region, lod: LodLevel::Hero, ..default() };
    let lift = span * 0.0015;

    // Primaries fan from the hand: innermost points back, outermost along the hand.
    let np = usize::from(feathered.primaries.max(1));
    let hand_dir_back = sweep + 2.0;
    for i in 0..np {
        let t = if np > 1 { i.to_f32() / (np - 1).to_f32() } else { 1.0 };
        let root = arm[2].translation.lerp(arm[3].translation, 0.9f32.mul_add(t, 0.1));
        let angle = 88.0f32.mul_add((1.0 - t).powf(0.8), hand_dir_back) + 6.0;
        let len = primary_len * primary_profile(feathered.shape, t);
        let width = primary_len * if feathered.shape == WingShape::Slotted && t > 0.6 { 0.15 } else { 0.22 };
        feather_card(&mut sheet, root + up * lift * (np - i).to_f32(), ax.dir(angle, -2.0), up, len, width, (primary_inner, primary_outer, t));
    }
    // Secondaries line the forearm, pointing back; tertials near the elbow.
    let ns = usize::from(feathered.secondaries.max(1));
    let spacing = uln / ns.to_f32();
    for i in 0..ns {
        let t = (i.to_f32() + 0.5) / ns.to_f32();
        let root = arm[1].translation.lerp(arm[2].translation, t);
        let len = secondary_len * 0.1f32.mul_add(1.0 - t, 0.95);
        feather_card(&mut sheet, root + up * lift * (ns - i + np).to_f32(), ax.dir(10.0f32.mul_add(-t, sweep + 98.0), -2.0), up, len, spacing * 3.0, (secondary_tract, secondary_tract, 0.0));
    }
    for i in 0..3usize {
        let t = 0.18f32.mul_add(i.to_f32(), 0.45);
        let root = arm[0].translation.lerp(arm[1].translation, t);
        feather_card(&mut sheet, root + up * lift * (ns + np + 3 - i).to_f32(), ax.dir(sweep + 110.0, -2.0), up, secondary_len * 0.85, spacing * 2.6, (secondary_tract, secondary_tract, 0.0));
    }
    // Coverts: shorter rows over the flight feathers' bases.
    for row in 1..=usize::from(feathered.covert_rows) {
        let scale = 0.15f32.mul_add(-row.to_f32(), 0.55).max(0.2);
        let h = lift * (np + ns + 6 + row * 4).to_f32();
        for i in 0..ns {
            let t = (i.to_f32() + 0.5) / ns.to_f32();
            let root = arm[1].translation.lerp(arm[2].translation, t) + ax.dir(sweep + 90.0, 0.0) * secondary_len * 0.08 * row.to_f32();
            feather_card(&mut coverts, root + up * h, ax.dir(sweep + 100.0, -2.0), up, secondary_len * scale, spacing * 3.0, (secondary_tract, secondary_tract, 0.0));
        }
        for i in 0..np / 2 {
            let t = i.to_f32() / (np / 2).max(1).to_f32();
            let root = arm[2].translation.lerp(arm[3].translation, 0.8f32.mul_add(t, 0.1));
            feather_card(&mut coverts, root + up * h, ax.dir(70.0f32.mul_add(1.0 - t, hand_dir_back), -2.0), up, primary_len * scale * 0.6, primary_len * 0.18, (primary_inner, primary_outer, t));
        }
    }
    b.sheet(sheet);
    if !coverts.indices.is_empty() {
        b.sheet(coverts);
    }
    feathered_fold(b, wing.fold, ax, bones, tracts, (hum, uln, hand))?;
    Ok(end_slots(bones[2], arm[3]))
}

/// Folds a bird wing (a Z): humerus back, forearm forward, hand back, the wing standing along
/// the flank.
fn feathered_fold(b: &mut AnatomyBuilder, fold: f32, ax: &Axes, bones: [BoneId; 3], tracts: Tracts, lens: (f32, f32, f32)) -> Result<(), AnatomyError> {
    let out = ax.out;
    let (hum, uln, hand) = lens;
    let frames: [Transform; 4] =
        fixed(frames_along(ax.root, &[(ax.mix(0.25, -1.0, -0.3), hum), (ax.mix(0.2, 1.0, 0.1), uln), (ax.mix(0.12, -1.0, -0.05), hand)], out))?;
    let mut folded: Vec<(BoneId, Transform)> = bones.iter().copied().zip(frames.iter().copied()).collect();
    // Each tract turns so its fan centre (`centre_deg` back from the bone in the spread wing)
    // points along `along`, the fan lying flat against the flank (normal = out).
    let tract = |start: Vec3, centre_deg: f32, along: Vec3| {
        let w = (ax.up - along * ax.up.dot(along)).normalize_or(ax.up);
        let (s0, c0) = centre_deg.to_radians().sin_cos();
        Transform::from_translation(start).with_rotation(frame_y_z(along * c0 - w * s0, out))
    };
    folded.push((tracts.secondary, tract(frames[1].translation, 95.0, ax.mix(0.08, -1.0, -0.12))));
    // Inner primaries (94° back in the spread fan) and outer ones (6°) both end up pointing back,
    // stacked just below the secondaries.
    folded.push((tracts.primary_inner, tract(frames[2].translation, 94.0, ax.mix(0.1, -1.0, -0.02))));
    folded.push((tracts.primary_outer, tract(frames[2].translation, 6.0, ax.mix(0.12, -1.0, -0.12))));
    b.wing_rig(fold, &folded);
    Ok(())
}

/// Primary length (fraction of the longest) from the innermost (`t = 0`) to the outermost.
fn primary_profile(shape: WingShape, t: f32) -> f32 {
    match shape {
        WingShape::Elliptical => 0.28f32.mul_add((PI * 0.55f32.mul_add(t, 0.25)).sin(), 0.72),
        WingShape::HighSpeed => 0.45f32.mul_add(t.powf(0.6), 0.55),
        WingShape::HighAspect => 0.35f32.mul_add(t, 0.65),
        WingShape::Slotted => 0.3f32.mul_add((PI * 0.6f32.mul_add(t, 0.15)).sin(), 0.7),
    }
}

/// One flight feather card: a slightly cambered strip from `root` along `dir`.
/// `bones`: (a, b, weight of b): flight feathers blend between two tract bones by fan position.
fn feather_card(sheet: &mut Sheet, root: Vec3, dir: Vec3, up: Vec3, len: f32, width: f32, bones: (BoneId, BoneId, f32)) {
    let (bone_a, bone_b, wb) = bones;
    let side = up.cross(dir).normalize_or(Vec3::Z);
    let base = sheet.positions.len().clamp_u32();
    let rows = 4u32;
    for j in 0..=rows {
        let t = j.to_f32() / rows.to_f32();
        // Vanes taper to a rounded tip; the shaft droops a little (camber).
        let half = width * 0.5 * t.powi(4).mul_add(-0.55, 1.0);
        let centre = root + dir * len * t - up * len * 0.04 * (PI * t).sin();
        for (k, sgn) in [(0.0, -1.0), (1.0, 1.0)] {
            sheet.positions.push(centre + side * half * sgn);
            sheet.joints.push([u16::from(bone_a.0), u16::from(bone_b.0), 0, 0]);
            sheet.weights.push([1.0 - wb, wb, 0.0, 0.0]);
            sheet.uvs.push([k, t]);
            sheet.dirs.push(dir);
            sheet.thickness.push(0.35);
        }
    }
    for j in 0..rows {
        let i = base + j * 2;
        sheet.indices.extend([i, i + 2, i + 1, i + 1, i + 2, i + 3]);
    }
}

fn build_blade(
    wing: &WingParams,
    blade: &BladeWing,
    b: &mut AnatomyBuilder,
    at: &AttachPoint,
    cx: &BuildCx,
    ax: &Axes,
) -> Result<PartSlots, AnatomyError> {
    let span = wing.span.max(0.01);
    let up = ax.up;
    let region = b.region(RegionKind::WingMembrane, cx.side)?;
    let hinge_region = b.region(RegionKind::Wing, cx.side)?;
    let dir = ax.dir(wing.sweep_deg, 2.0);
    let frames: [Transform; 2] = fixed(frames_along(ax.root, &[(dir, span)], up))?;
    let [hinge] = fixed(chain_bones(b, at.bone, "wing", &frames, hinge_region)?)?;
    let back = up.cross(dir).normalize_or(-ax.fwd);
    let chord = wing.chord.max(span * 0.1);
    let mut sheet = Sheet {
        kind: if blade.elytron { SheetKind::Elytron } else { SheetKind::InsectWing },
        region,
        lod: LodLevel::Far,
        ..default()
    };
    let (rows, cols) = (14u32, 6u32);
    let base = sheet.positions.len().clamp_u32();
    for k in 0..=rows {
        let u = k.to_f32() / rows.to_f32();
        let width = chord * planform(blade.planform, blade.elytron, u);
        for m in 0..=cols {
            let v = m.to_f32() / cols.to_f32();
            let dome = if blade.elytron { width * 0.35 * (PI * v).sin() } else { 0.0 };
            let pos = ax.root + dir * span * u + back * (v - 0.15) * width + up * dome;
            sheet.positions.push(pos);
            sheet.joints.push([u16::from(hinge.0), 0, 0, 0]);
            sheet.weights.push([1.0, 0.0, 0.0, 0.0]);
            sheet.uvs.push([u, v]);
            sheet.dirs.push(dir);
            sheet.thickness.push(if blade.elytron { 1.0 } else { 0.3f32.mul_add((1.0 - v).powi(4), 0.08) });
        }
    }
    grid_indices(&mut sheet, base, rows, cols);
    b.sheet(sheet);
    // Folded back over the abdomen, flat.
    let fold_dir = ax.mix(0.1, -1.0, 0.06);
    let [folded, _] = fixed(frames_along(ax.root, &[(fold_dir, span)], up))?;
    b.wing_rig(wing.fold, &[(hinge, folded)]);
    Ok(end_slots(hinge, frames[1]))
}

/// Chord at `u` along the wing, as a fraction of the root chord.
fn planform(p: Planform, elytron: bool, u: f32) -> f32 {
    if elytron {
        return 0.1f32.mul_add(1.0 - u, (1.0 - u.powi(3)).max(0.0).sqrt() * 0.9);
    }
    let round = 2.0f32.mul_add(u, -1.0).mul_add(-2.0f32.mul_add(u, -1.0), 1.0).max(0.0).sqrt();
    match p {
        Planform::Long => (0.65 * round.powf(0.7)).mul_add(0.2f32.mul_add(-u, 1.0), 0.35),
        Planform::Broad => 0.9f32.mul_add(round.sqrt(), 0.4),
        Planform::Round => 0.7f32.mul_add(round, 0.3),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::num::Idx;

    fn build(src: &str) -> crate::anatomy::Anatomy {
        let plan: BodyPlan = ron::from_str(src).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    /// Every winged sample builds, with mirrored sheets, a fold rig per wing, and skin weights
    /// that sum to one on every sheet vertex.
    #[test]
    fn winged_samples_build() {
        for (name, src, kind) in [
            ("dragon", include_str!("../../assets/mobs/dragon.body.ron"), SheetKind::Membrane),
            ("wyvern", include_str!("../../assets/mobs/wyvern.body.ron"), SheetKind::Membrane),
            ("bat", include_str!("../../assets/mobs/bat.body.ron"), SheetKind::Membrane),
            ("bird", include_str!("../../assets/mobs/bird.body.ron"), SheetKind::FlightFeather),
            ("beetle", include_str!("../../assets/mobs/beetle.body.ron"), SheetKind::Elytron),
            ("dragonfly", include_str!("../../assets/mobs/dragonfly.body.ron"), SheetKind::InsectWing),
        ] {
            let a = build(src);
            assert!(a.sheets.iter().any(|s| s.kind == kind), "{name}: no {kind:?} sheet");
            assert!(a.wings.len() >= 2 && a.wings.len() % 2 == 0, "{name}: {} wing rigs", a.wings.len());
            assert!(a.skeleton.crowd.bones.len() <= crate::anatomy::MAX_CROWD_BONES, "{name}: crowd budget");
            for s in &a.sheets {
                assert_eq!(s.positions.len(), s.weights.len());
                assert!(s.indices.iter().all(|i| i.idx() < s.positions.len()));
                for w in &s.weights {
                    assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-4, "{name}: weights {w:?}");
                }
            }
            // Left and right wings mirror each other.
            let xs: Vec<f32> = a.sheets.iter().map(|s| s.positions.iter().map(|p| p.x).sum::<f32>() / s.positions.len().to_f32()).collect();
            assert!(xs.iter().any(|x| *x > 0.05) && xs.iter().any(|x| *x < -0.05), "{name}: sheets on one side only {xs:?}");
        }
    }
}
