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

use bevy::math::Affine3A;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::num::{Clamp, Idx, ToF32};

use super::{
    AnatomyBuilder, AnatomyError, AttachPoint, AttachSlot, AxisSample, BoneId, BuildCx, HardKind, HardParams, LodLevel, PartBuild,
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
    /// How far the membrane bows out of the wing plane between its spars (camber), as a fraction
    /// of the gap; positive bows up from the spread wing, out from the folded one.
    pub billow: f32,
    /// A free clawed thumb at the wrist.
    pub thumb: bool,
    /// The spread wing (the bind pose); its `sweep` adds to [`WingParams::sweep_deg`].
    pub open: WingPose,
    /// Half-way through the fold: a wing folding (and opening) passes through it, so a part fold
    /// holds the wings up rather than letting them droop.
    pub half: WingPose,
    /// The folded wing, against the flank; `WingParams::fold` blends toward it at rest.
    pub closed: WingPose,
}

impl Default for MembraneWing {
    fn default() -> Self {
        Self { fingers: 3, trailing: Trailing::Body(0.35), scallop: 0.22, billow: 0.05, thumb: true, open: WingPose::OPEN, half: WingPose::HALF, closed: WingPose::CLOSED }
    }
}

/// A membrane wing's joint angles, degrees.
///
/// The arm and fingers lie in one plane (elbow, wrist and knuckles are hinges across it, as in a
/// bat, whose wing folds in its own plane like an umbrella); the shoulder turns that plane.
/// Blending two poses turns each hinge by its own angle, so a half-folded wing is half-way at
/// every joint.
///
/// A joint must turn less than 180° between the open and closed poses (the blend takes the
/// shorter way round). A pose written in a plan lists all six angles.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct WingPose {
    /// Humerus: back from straight out, in the wing plane.
    pub sweep: f32,
    /// The wing plane turned about the body's long axis: up from level (negative lowers the wing
    /// down the flank).
    pub raise: f32,
    /// Elbow: the forearm turned forward from the humerus's line.
    pub elbow: f32,
    /// Wrist: the hand turned back from the forearm's line.
    pub wrist: f32,
    /// Fan between the first and last finger (a lone pterosaur finger turns back off the hand by a
    /// fifth of it).
    pub spread: f32,
    /// Each finger's knuckle, bent back.
    pub curl: f32,
}

impl WingPose {
    /// Spread, as a bat in flight: elbow a little bent, fingers fanned, the plane nearly level.
    pub const OPEN: Self = Self { sweep: 22.0, raise: 4.0, elbow: 34.0, wrist: 4.0, spread: 105.0, curl: 8.0 };
    /// Half-folded: the wings lifted and mostly closed, the fingers drawn together, so the second
    /// half of the fold only lowers a compact wing.
    pub const HALF: Self = Self { sweep: 55.0, raise: 32.0, elbow: 130.0, wrist: 150.0, spread: 28.0, curl: 12.0 };
    /// Folded: humerus back along the body, forearm forward, the fingers bundled back along it
    /// (the ribs of a closed umbrella), the plane turned down against the flank.
    pub const CLOSED: Self = Self { sweep: 78.0, raise: -68.0, elbow: 166.0, wrist: 164.0, spread: 10.0, curl: 14.0 };
}

impl Default for WingPose {
    fn default() -> Self {
        Self::OPEN
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

/// Folded local rotations for a wing's bones; the pose blends rest → folded by `fold`, through
/// `half` at a fold of one half where the wing has one.
#[derive(Clone, Debug)]
pub struct WingRig {
    pub part: u16,
    pub fold: f32,
    pub bones: Vec<(BoneId, Quat)>,
    /// Local rotations half-way through the fold, one per entry of `bones` (empty: straight from
    /// rest to folded).
    pub half: Vec<Quat>,
}

impl WingRig {
    /// Turns the wing's bones in `local` (local transforms, one per bone, at rest) to `fold`.
    pub fn apply(&self, fold: f32, local: &mut [Transform]) {
        let fold = fold.clamp(0.0, 1.0);
        for (i, (bone, folded)) in self.bones.iter().enumerate() {
            let Some(t) = local.get_mut(bone.index()) else { continue };
            t.rotation = match self.half.get(i) {
                Some(half) if fold < 0.5 => t.rotation.slerp(*half, fold * 2.0),
                Some(half) => half.slerp(*folded, fold.mul_add(2.0, -1.0)),
                None => t.rotation.slerp(*folded, fold),
            };
        }
    }
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
/// each point lies on and how far along it. Empty if the polyline has fewer than two points.
fn resample(poly: &[Vec3], n: usize) -> Vec<(Vec3, usize, f32)> {
    let segments: Vec<(Vec3, Vec3, f32)> = poly.array_windows().map(|&[a, b]| (a, b, a.distance(b))).collect();
    let Some(last) = segments.len().checked_sub(1) else { return Vec::new() };
    let total: f32 = segments.iter().map(|s| s.2).sum::<f32>().max(1e-6);
    (0..=n)
        .filter_map(|k| {
            let mut d = total * k.to_f32() / n.to_f32();
            for (s, &(a, b, len)) in segments.iter().enumerate() {
                if d <= len || s == last {
                    let f = (d / len.max(1e-6)).clamp(0.0, 1.0);
                    return Some((a.lerp(b, f), s, f));
                }
                d -= len;
            }
            None
        })
        .collect()
}

/// What carries one segment of a membrane edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carry {
    /// A bone along the segment (arm, finger).
    Bone(BoneId),
    /// A free stretch (trailing edge, body line) between the bone at its start and the one at its
    /// end, blended along it.
    Span(BoneId, BoneId),
}

/// A membrane edge: a polyline and what carries each of its segments.
#[derive(Clone, Copy)]
struct Edge<'a> {
    points: &'a [Vec3],
    carry: &'a [Carry],
}

/// Fraction of a bone, at each end, over which an edge's weight eases into the next bone, so the
/// membrane bends round a joint rather than creasing at it.
const JOINT_EASE: f32 = 0.2;

impl Edge<'_> {
    /// Bone weights at `f` along segment `s`.
    fn weights(&self, s: usize, f: f32) -> [(BoneId, f32); 2] {
        let carry = |s: usize| self.carry.get(s).or_else(|| self.carry.last()).copied().unwrap_or(Carry::Bone(BoneId(0)));
        match carry(s) {
            Carry::Span(a, b) => [(a, 1.0 - f), (b, f)],
            Carry::Bone(bone) => {
                // Half-way into the neighbouring bone at the joint itself.
                let ease = |t: f32| 0.5 * (1.0 - t / JOINT_EASE).max(0.0);
                let next = match carry(s + 1) {
                    Carry::Bone(n) if s + 1 < self.carry.len() && n != bone => Some((n, ease(1.0 - f))),
                    _ => None,
                };
                let prev = match s.checked_sub(1).map(carry) {
                    Some(Carry::Bone(p)) if p != bone => Some((p, ease(f))),
                    _ => None,
                };
                match (next, prev) {
                    (Some((n, w)), _) if w > 0.0 => [(bone, 1.0 - w), (n, w)],
                    (_, Some((p, w))) if w > 0.0 => [(bone, 1.0 - w), (p, w)],
                    _ => [(bone, 1.0), (bone, 0.0)],
                }
            }
        }
    }
}

/// Up to four bone weights from weighted contributions, summing to one (largest kept).
fn skin(parts: impl IntoIterator<Item = (BoneId, f32)>) -> ([u16; 4], [f32; 4]) {
    let mut merged: Vec<(BoneId, f32)> = Vec::with_capacity(4);
    for (bone, w) in parts {
        if w <= 0.0 {
            continue;
        }
        match merged.iter_mut().find(|(b, _)| *b == bone) {
            Some(slot) => slot.1 += w,
            None => merged.push((bone, w)),
        }
    }
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    merged.truncate(4);
    let total: f32 = merged.iter().map(|m| m.1).sum();
    let (mut joints, mut weights) = ([0u16; 4], [0.0f32; 4]);
    if total <= 0.0 {
        weights[0] = 1.0;
        return (joints, weights);
    }
    for (i, (bone, w)) in merged.into_iter().enumerate() {
        if let (Some(j), Some(slot)) = (joints.get_mut(i), weights.get_mut(i)) {
            *j = u16::from(bone.0);
            *slot = w / total;
        }
    }
    (joints, weights)
}

/// Where a fitted pose holds a panel vertex: `(c, v, (edge a, edge b) points of its row, where its
/// edges' bones carry it)`.
type Place<'a> = Box<dyn Fn(f32, f32, (Vec3, Vec3), Vec3) -> Vec3 + 'a>;

/// An edge resampled into rows: point, segment, fraction along it.
type Rows = Vec<(Vec3, usize, f32)>;

/// A pose a panel's skin weights are fitted to (see [`fit_weights`]).
struct Fold<'a> {
    /// The panel's two edges in this pose (as `ruled_panel`'s `a` and `b`, point for point).
    a: Vec<Vec3>,
    b: Vec<Vec3>,
    /// Where the panel in this pose holds the vertex at (`c`, `v`), from its row's edge points
    /// and where its edges' bones alone (the weights before fitting) carry it.
    place: Place<'a>,
    /// Each candidate bone's skin transform in this pose (posed model · bind model⁻¹), in the
    /// order of the panel's candidate bones.
    moves: Vec<Affine3A>,
    /// How much this pose counts against the others.
    weight: f32,
}

/// The poses a panel's weights are fitted to, and the bones they may use.
struct Fit<'a> {
    bones: Vec<BoneId>,
    poses: Vec<Fold<'a>>,
}

/// Rounds of neighbour smoothing over a fitted panel's weights.
const SMOOTH_ROUNDS: usize = 12;

/// A ruled grid between two edges with matching ends, rows evenly spaced along each, appended to
/// `sheet`. Each vertex is weighted between the bones carrying its row on either edge, by its
/// place across (`c`). `sag(c, v, pa, pb)` offsets interior points (scallops, billow);
/// `rib_thickness(c, v)` is the shading thickness.
///
/// With `fit`, the weights are then refitted so its posed bones carry each vertex near its place
/// in each posed panel, and smoothed over the grid (each vertex fitted alone picks its own mix of
/// bones, which frays the folded edges). The leading edge and the body line keep their carriers.
fn ruled_panel(
    sheet: &mut Sheet,
    edge_a: Edge,
    edge_b: Edge,
    (rows, cols): (usize, usize),
    sag: impl Fn(f32, f32, Vec3, Vec3) -> Vec3,
    rib_thickness: impl Fn(f32, f32) -> f32,
    fit: Option<&Fit>,
) {
    let ra = resample(edge_a.points, rows);
    let rb = resample(edge_b.points, rows);
    if ra.len() != rows + 1 || rb.len() != rows + 1 {
        return;
    }
    let posed: Vec<(Rows, Rows)> = fit.map_or_else(Vec::new, |f| f.poses.iter().map(|p| (resample(&p.a, rows), resample(&p.b, rows))).collect());
    // Direction along a resampled edge at row `k`.
    let along = |r: &[(Vec3, usize, f32)], k: usize| match (r.get((k + 1).min(rows)), r.get(k.saturating_sub(1))) {
        (Some(next), Some(prev)) => next.0 - prev.0,
        _ => Vec3::ZERO,
    };
    let base = sheet.positions.len().clamp_u32();
    // Per vertex, row-major: dense weights over the fit's bones, and whether they stay put.
    let mut fitted: Vec<(Vec<f32>, bool)> = Vec::new();
    for (row, (&(pa, sa, fa), &(pb, sb, fb))) in ra.iter().zip(&rb).enumerate() {
        let v = row.to_f32() / rows.to_f32();
        let (next_a, next_b) = (along(&ra, row), along(&rb, row));
        let (wa, wb) = (edge_a.weights(sa, fa), edge_b.weights(sb, fb));
        for col in 0..=cols {
            let c = col.to_f32() / cols.to_f32();
            let pos = pa.lerp(pb, c) + sag(c, v, pa, pb);
            let start = wa.iter().map(|&(j, w)| (j, w * (1.0 - c))).chain(wb.iter().map(|&(j, w)| (j, w * c)));
            let (joints, weights) = match fit {
                Some(f) => {
                    let carried_by = dense_weights(&f.bones, start.clone());
                    let targets: Vec<(Vec3, &Fold)> = posed
                        .iter()
                        .zip(&f.poses)
                        .filter_map(|((qa, qb), pose)| {
                            let (qa, qb) = (qa.get(row)?.0, qb.get(row)?.0);
                            let carried: Vec3 = pose.moves.iter().zip(&carried_by).map(|(m, w)| m.transform_point3(pos) * *w).sum();
                            Some(((pose.place)(c, v, (qa, qb), carried), pose))
                        })
                        .collect();
                    // The leading edge rides its bones; the body line (`b`'s first span) the body.
                    let fixed = col == 0 || (col == cols && sb == 0);
                    let dense = if fixed { dense_weights(&f.bones, start) } else { fit_weights(pos, &targets, &f.bones, start) };
                    fitted.push((dense, fixed));
                    ([0; 4], [1.0, 0.0, 0.0, 0.0])
                }
                None => skin(start),
            };
            sheet.positions.push(pos);
            sheet.joints.push(joints);
            sheet.weights.push(weights);
            sheet.uvs.push([c, v]);
            sheet.dirs.push(next_a.lerp(next_b, c).normalize_or(Vec3::X));
            sheet.thickness.push(rib_thickness(c, v).clamp(0.0, 1.0));
        }
    }
    if let Some(f) = fit {
        smooth_weights(&mut fitted, rows + 1, cols + 1);
        let skinned = fitted.iter().map(|(w, _)| skin(f.bones.iter().copied().zip(w.iter().copied())));
        let start = base.idx();
        for (i, (joints, weights)) in skinned.enumerate() {
            if let (Some(j), Some(w)) = (sheet.joints.get_mut(start + i), sheet.weights.get_mut(start + i)) {
                (*j, *w) = (joints, weights);
            }
        }
    }
    grid_indices(sheet, base, rows.clamp_u32(), cols.clamp_u32());
}

/// `start` weights as one weight per bone of `bones` (others dropped), summing to one.
fn dense_weights(bones: &[BoneId], start: impl IntoIterator<Item = (BoneId, f32)>) -> Vec<f32> {
    let mut dense = vec![0.0f32; bones.len()];
    for (bone, w) in start {
        if let Some(slot) = bones.iter().position(|b| *b == bone).and_then(|i| dense.get_mut(i)) {
            *slot += w;
        }
    }
    let total: f32 = dense.iter().sum();
    if total > 0.0 {
        for w in &mut dense {
            *w /= total;
        }
    } else if let Some(first) = dense.first_mut() {
        *first = 1.0;
    }
    dense
}

/// Averages each free vertex's weights with its grid neighbours' (`rows` × `cols` vertices,
/// row-major), [`SMOOTH_ROUNDS`] times.
fn smooth_weights(weights: &mut [(Vec<f32>, bool)], rows: usize, cols: usize) {
    for _ in 0..SMOOTH_ROUNDS {
        let before: Vec<Vec<f32>> = weights.iter().map(|(w, _)| w.clone()).collect();
        for (i, (w, fixed)) in weights.iter_mut().enumerate() {
            if *fixed {
                continue;
            }
            let (row, col) = (i / cols, i % cols);
            let neighbours = [
                row.checked_sub(1).map(|r| r * cols + col),
                (row + 1 < rows).then(|| (row + 1) * cols + col),
                col.checked_sub(1).map(|c| row * cols + c),
                (col + 1 < cols).then(|| row * cols + col + 1),
            ];
            let near: Vec<&Vec<f32>> = neighbours.into_iter().flatten().filter_map(|n| before.get(n)).collect();
            let count = near.len().to_f32();
            for (k, slot) in w.iter_mut().enumerate() {
                let mean = near.iter().filter_map(|n| n.get(k)).sum::<f32>() / count.max(1.0);
                *slot = 0.5f32.mul_add(*slot, 0.5 * mean);
            }
        }
    }
}

/// Skin weights over `bones` for a vertex at `p` (bind pose) that carry it as near each pose's
/// target as they can, staying near the `start` weights: minimises
/// Σₖ αₖ |Σᵢ wᵢ Mₖᵢ p − targetₖ|² + λ |w − w₀|² over weights that are non-negative and sum to one
/// (projected gradient on the simplex). Linear blending is exact at the bind pose whatever the
/// weights, so this only shapes the other poses.
///
/// Membranes need it because no bone carries a membrane's middle: weighted to the body it stays
/// where the spread wing held it, out in the air; weighted to the wing it swings up over the back
/// as the wing plane turns down the flank.
fn fit_weights(p: Vec3, targets: &[(Vec3, &Fold)], bones: &[BoneId], start: impl IntoIterator<Item = (BoneId, f32)>) -> Vec<f32> {
    let w0 = dense_weights(bones, start);
    // Per pose: where each bone alone would carry the vertex, relative to the target.
    let d: Vec<(f32, Vec<Vec3>)> = targets
        .iter()
        .map(|(target, pose)| (pose.weight, pose.moves.iter().map(|m| m.transform_point3(p) - *target).collect()))
        .collect();
    let reach: f32 = d.iter().map(|(a, d)| a * d.iter().map(|d| d.length_squared()).sum::<f32>()).sum::<f32>().max(1e-8);
    // Keep near the start weights at about a tenth of the reach's scale.
    let lambda = 0.01 * reach;
    let step = 0.5 / (reach + lambda);
    let mut w = w0.clone();
    for _ in 0..300 {
        let grad: Vec<f32> = (0..w.len())
            .map(|i| {
                d.iter()
                    .map(|(alpha, d)| {
                        let r: Vec3 = w.iter().zip(d).map(|(w, d)| *d * *w).sum();
                        alpha * r.dot(d.get(i).copied().unwrap_or_default())
                    })
                    .sum::<f32>()
            })
            .collect();
        let next: Vec<f32> = w.iter().zip(&w0).zip(&grad).map(|((w, w0), g)| w - step * 2.0 * lambda.mul_add(w - w0, *g)).collect();
        w = project_simplex(&next);
    }
    w
}

/// The point of the probability simplex nearest `v` (Duchi et al. 2008).
fn project_simplex(v: &[f32]) -> Vec<f32> {
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| b.total_cmp(a));
    let mut sum = 0.0;
    let mut theta = 0.0;
    for (i, x) in sorted.iter().enumerate() {
        sum += x;
        let t = (sum - 1.0) / (i + 1).to_f32();
        if x - t > 0.0 {
            theta = t;
        }
    }
    v.iter().map(|x| (x - theta).max(0.0)).collect()
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

/// Membrane wing segment lengths, metres.
struct MembraneLens {
    humerus: f32,
    forearm: f32,
    hand: f32,
    /// One per finger, root to tip.
    fingers: Vec<f32>,
}

impl MembraneLens {
    /// Bat/dragon: long forearm, short metacarpals, the leading finger (digit III) longest.
    /// Pterosaur (`count` 1): one huge fourth finger on a long metacarpal.
    fn new(span: f32, count: usize) -> Self {
        let (humerus, forearm, hand, fingers) = if count == 1 {
            (0.14, 0.22, 0.12, vec![0.52])
        } else {
            (0.16, 0.27, 0.05, (0..count).map(|i| 0.55 * 0.18f32.mul_add(-i.to_f32(), 1.0)).collect())
        };
        Self { humerus: humerus * span, forearm: forearm * span, hand: hand * span, fingers: fingers.into_iter().map(|f| f * span).collect() }
    }
}

/// A membrane wing's bone frames in one pose: shoulder, elbow, wrist, hand end; and per finger
/// its root, knuckle and tip. Y runs along each bone, Z is the wing plane's normal.
struct MembraneFrames {
    arm: [Transform; 4],
    fingers: Vec<[Transform; 3]>,
    /// The wing plane's normal (up from the spread wing's upper face).
    normal: Vec3,
}

impl MembraneFrames {
    /// Forward kinematics of `pose` (`sweep` already including any extra sweep).
    fn new(pose: &WingPose, ax: &Axes, lens: &MembraneLens) -> Result<Self, AnatomyError> {
        let side = ax.out.x.signum();
        let plane = Quat::from_axis_angle(ax.fwd, pose.raise.to_radians() * side);
        let normal = plane * ax.up;
        // In the plane: `out` at 0°, back (-fwd) at 90°.
        let dir = |deg: f32| {
            let (s, c) = deg.to_radians().sin_cos();
            plane * (ax.out * c - ax.fwd * s)
        };
        let humerus = pose.sweep;
        let forearm = humerus - pose.elbow;
        let hand = forearm + pose.wrist;
        let arm = fixed(frames_along(ax.root, &[(dir(humerus), lens.humerus), (dir(forearm), lens.forearm), (dir(hand), lens.hand)], normal))?;
        let wrist = arm[3].translation;
        let count = lens.fingers.len();
        // The leading finger sits a little behind the hand's line; the rest fan back by `spread`.
        // A pterosaur's lone wing finger turns back off the hand by a fifth of `spread` (20° spread,
        // in line with the forearm folded).
        let lead = if count == 1 { pose.spread * 0.19 } else { 2.0 };
        let fingers = lens
            .fingers
            .iter()
            .enumerate()
            .map(|(i, len)| {
                let fan = if count > 1 { pose.spread * i.to_f32() / (count - 1).to_f32() } else { 0.0 };
                let a = hand + lead + fan;
                fixed(frames_along(wrist, &[(dir(a), len * 0.55), (dir(a + pose.curl), len * 0.45)], normal))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { arm, fingers, normal })
    }
}

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
    let lens = MembraneLens::new(span, count);
    let arm_region = b.region(RegionKind::Wing, cx.side)?;
    let sheet_region = b.region(RegionKind::WingMembrane, cx.side)?;

    // Built spread (the bind pose); `sweep_deg` turns the whole spread wing back.
    let open = WingPose { sweep: membrane.open.sweep + wing.sweep_deg, ..membrane.open };
    let spread = MembraneFrames::new(&open, ax, &lens)?;
    let arm = spread.arm;
    let arm_bones: [BoneId; 3] = fixed(chain_bones(b, at.bone, "wing", &arm, arm_region)?)?;
    let wrist = arm[3].translation;
    let mut finger_bones: Vec<[BoneId; 2]> = Vec::with_capacity(count);
    for (i, f) in spread.fingers.iter().enumerate() {
        finger_bones.push(fixed(chain_bones(b, Some(arm_bones[2]), &format!("wing.finger{i}"), f, arm_region)?)?);
    }
    let (Some(&first), Some(&first_bones), Some(&tip_frames), Some(&tip_bones)) =
        (spread.fingers.first(), finger_bones.first(), spread.fingers.last(), finger_bones.last())
    else {
        return Err(AnatomyError::Internal("membrane wing without fingers"));
    };
    let radius = wing.radius;
    tubes(b, &arm_bones, &arm, radius, radius * 0.55, arm_region, radius * 1.4);
    for (f, bones) in spread.fingers.iter().zip(&finger_bones) {
        tubes(b, bones, f, radius * 0.42, radius * 0.12, arm_region, radius * 0.2);
    }
    if membrane.thumb {
        let hand_dir = (wrist - arm[2].translation).normalize_or(ax.out);
        let thumb_dir = (Quat::from_axis_angle(spread.normal, 50f32.to_radians() * ax.out.x.signum()) * hand_dir + spread.normal * 0.18).normalize();
        let thumb = Transform::from_translation(wrist).with_rotation(frame_y_z(thumb_dir, spread.normal));
        b.hard_part(arm_bones[2], thumb, HardParams { kind: HardKind::Claw, size: span * 0.05, curve: 0.5, twist: 0.0, sclera: 0.0 }, arm_region);
    }

    // Membrane panels between neighbouring fingers (dactylopatagium), scalloped between tips and
    // bowed out of the plane by `billow`.
    let mut sheet = Sheet { kind: SheetKind::Membrane, region: sheet_region, lod: LodLevel::Far, ..default() };
    let (scallop, billow, normal) = (membrane.scallop, membrane.billow, spread.normal);
    for ([fa, fb], [ba, bb]) in spread.fingers.array_windows().zip(finger_bones.array_windows()) {
        let pa = fa.map(|t| t.translation);
        let pb = fb.map(|t| t.translation);
        let (ca, cb) = (ba.map(Carry::Bone), bb.map(Carry::Bone));
        let gap = pa[2].distance(pb[2]);
        ruled_panel(
            &mut sheet,
            Edge { points: &pa, carry: &ca },
            Edge { points: &pb, carry: &cb },
            (14, 8),
            |c, v, pa, pb| {
                let toward_wrist = (wrist - (pa + pb) * 0.5).normalize_or(Vec3::ZERO);
                let bow = (PI * c).sin();
                toward_wrist * scallop * gap * bow * v.powi(3) + normal * billow * pa.distance(pb) * bow
            },
            |c, v| (PI * c).sin().max(0.0).powf(0.6).mul_add(-0.85, 1.0) * 0.3f32.mul_add(-v, 1.0),
            None,
        );
    }

    // Main membrane (plagiopatagium): arm + last finger in front; behind, the body line from the
    // shoulder to the trailing attachment, then the free trailing edge out to the finger tip.
    let tip = tip_frames[2].translation;
    let lead = [arm[0].translation, arm[1].translation, arm[2].translation, wrist, tip_frames[1].translation, tip];
    let lead_carry = [arm_bones[0], arm_bones[1], arm_bones[2], tip_bones[0], tip_bones[1]].map(Carry::Bone);
    let (trail_point, trail_bone) = trailing_point(b, membrane.trailing, ax, at, span, wing.chord);
    let root_bone = at.bone.unwrap_or(arm_bones[0]);
    let trail_bone = trail_bone.unwrap_or(root_bone);
    let trail_len = trail_point.distance(tip);
    let back = [ax.root, trail_point, tip];
    let back_carry = [Carry::Span(root_bone, trail_bone), Carry::Span(trail_bone, tip_bones[1])];
    // The half-folded and folded wing, which the main membrane's weights are fitted to.
    let closed = MembraneFrames::new(&membrane.closed, ax, &lens)?;
    let half = MembraneFrames::new(&membrane.half, ax, &lens)?;
    let body = b.parent_axis();
    let mut bones = vec![root_bone];
    if trail_bone != root_bone {
        bones.push(trail_bone);
    }
    let fits = MainFit { spread: &spread, body: &body, root: ax.root, trail: trail_point, body_bones: bones.len(), billow, gap: wing.radius * 2.0 };
    bones.extend(arm_bones.iter().chain(&tip_bones));
    let fit = Fit { bones, poses: vec![fits.pose(&closed, 1.0, true)?, fits.pose(&half, 0.5, false)?] };
    ruled_panel(
        &mut sheet,
        Edge { points: &lead, carry: &lead_carry },
        Edge { points: &back, carry: &back_carry },
        (18, 10),
        |c, v, pa, pb| {
            // The free trailing edge (far half of `back`) bows in toward the arm.
            let inward = (pa - pb).normalize_or(Vec3::ZERO);
            let scallop = inward * scallop * trail_len * 0.35 * c * (PI * v).sin() * ((v - 0.35) / 0.65).clamp(0.0, 1.0);
            scallop + normal * billow * pa.distance(pb) * (PI * c).sin() * (PI * v).sin().max(0.0).sqrt()
        },
        |c, v| 0.1f32.mul_add(1.0 - v, f32::mul_add((1.0 - c).powi(3), 0.8, 0.12)),
        Some(&fit),
    );
    b.sheet(sheet);

    // Folded: the same chain in the closed pose, through the half pose.
    let posed = |frames: &MembraneFrames| {
        let mut posed: Vec<(BoneId, Transform)> = arm_bones.iter().copied().zip(frames.arm).collect();
        for (bones, fingers) in finger_bones.iter().zip(&frames.fingers) {
            posed.extend(bones.iter().copied().zip(fingers.iter().copied()));
        }
        posed
    };
    b.wing_rig(wing.fold, &posed(&closed), Some(&posed(&half)));
    Ok(end_slots(first_bones[1], first[2]))
}

/// What the main membrane's fitted poses share: the spread frames, the body, the body line
/// (`root` to `trail`) and the panel's bones (`body_bones` body bones, then the arm and the last
/// finger).
struct MainFit<'a> {
    spread: &'a MembraneFrames,
    body: &'a [AxisSample],
    root: Vec3,
    trail: Vec3,
    body_bones: usize,
    billow: f32,
    /// Clearance between the folded membrane and the skin, metres.
    gap: f32,
}

impl<'a> MainFit<'a> {
    /// The main membrane in the `posed` frames. `flat`: laid on the flank where its edges carry it
    /// (the folded wing; a ruled panel between the folded zig-zag arm and the body line twists
    /// over itself), rather than between the posed edges, bowed (the half-folded, raised wing).
    fn pose(&self, posed: &MembraneFrames, weight: f32, flat: bool) -> Result<Fold<'a>, AnatomyError> {
        let (Some(tip), Some(spread_tip)) = (posed.fingers.last(), self.spread.fingers.last()) else {
            return Err(AnatomyError::Internal("membrane wing without fingers"));
        };
        let skin_of = |bind: &Transform, now: &Transform| now.compute_affine() * bind.compute_affine().inverse();
        let mut moves = vec![Affine3A::IDENTITY; self.body_bones];
        moves.extend(self.spread.arm.iter().zip(&posed.arm).map(|(s, p)| skin_of(s, p)));
        moves.extend(spread_tip.iter().zip(tip).map(|(s, p)| skin_of(s, p)));
        let (normal, body, gap, billow) = (posed.normal, self.body, self.gap, self.billow);
        Ok(Fold {
            a: vec![posed.arm[0].translation, posed.arm[1].translation, posed.arm[2].translation, posed.arm[3].translation, tip[1].translation, tip[2].translation],
            b: vec![self.root, self.trail, tip[2].translation],
            place: Box::new(move |c, v, (pa, pb), carried| {
                if flat {
                    // Off the arm (`c` 0), onto the flank.
                    let s = (c / 0.35).clamp(0.0, 1.0);
                    return carried.lerp(onto(body, carried, gap), s * s * 2.0f32.mul_add(-s, 3.0));
                }
                let bow = normal * billow.max(0.04) * pa.distance(pb) * (PI * c).sin() * (PI * v).sin().max(0.0).sqrt();
                outside(body, pa.lerp(pb, c) + bow, gap)
            }),
            moves,
            weight,
        })
    }
}

/// `p`, pushed out of the body whose axis is `axis` to `gap` beyond its skin (its nearest
/// cross-section, an ellipse, grown by a twelfth for the muscle and fat over it); unchanged if
/// already outside.
fn outside(axis: &[AxisSample], p: Vec3, gap: f32) -> Vec3 {
    let Some(s) = axis.iter().min_by(|a, b| a.centre().distance_squared(p).total_cmp(&b.centre().distance_squared(p))) else { return p };
    let local = s.model.rotation.inverse() * (p - s.centre());
    let (rx, rz) = (s.radii.x.mul_add(1.08, gap), s.radii.y.mul_add(1.08, gap));
    let e = (local.x / rx).hypot(local.z / rz);
    if e >= 1.0 || e < 1e-4 {
        return p;
    }
    let scale = 1.0 / e;
    s.centre() + s.model.rotation * Vec3::new(local.x * scale, local.y, local.z * scale)
}

/// `p` moved along its nearest cross-section's radius to `gap` beyond the skin (as [`outside`]),
/// from inside or out: laid on the body.
fn onto(axis: &[AxisSample], p: Vec3, gap: f32) -> Vec3 {
    let Some(s) = axis.iter().min_by(|a, b| a.centre().distance_squared(p).total_cmp(&b.centre().distance_squared(p))) else { return p };
    let local = s.model.rotation.inverse() * (p - s.centre());
    let (rx, rz) = (s.radii.x.mul_add(1.08, gap), s.radii.y.mul_add(1.08, gap));
    let e = (local.x / rx).hypot(local.z / rz);
    if e < 1e-4 {
        return p;
    }
    s.centre() + s.model.rotation * Vec3::new(local.x / e, local.y, local.z / e)
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
    b.wing_rig(fold, &folded, None);
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
    b.wing_rig(wing.fold, &[(hinge, folded)], None);
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
            for s in &a.sheets {
                let finite = s.positions.iter().all(|p| p.is_finite())
                    && s.thickness.iter().all(|t| t.is_finite())
                    && s.dirs.iter().all(|d| d.is_finite())
                    && s.uvs.iter().flatten().all(|u| u.is_finite());
                assert!(finite, "{name}: a {:?} sheet has a non-finite attribute", s.kind);
            }
            // Left and right wings mirror each other.
            let xs: Vec<f32> = a.sheets.iter().map(|s| s.positions.iter().map(|p| p.x).sum::<f32>() / s.positions.len().to_f32()).collect();
            assert!(xs.iter().any(|x| *x > 0.05) && xs.iter().any(|x| *x < -0.05), "{name}: sheets on one side only {xs:?}");
        }
    }

    /// Folded membrane wings lie along the flank: no part of the membrane stands far above the
    /// back or out from the side (the free trailing edge once rode the finger tip up into a sail).
    #[test]
    fn folded_membranes_lie_along_the_flank() {
        for (name, src) in [
            ("dragon", include_str!("../../assets/mobs/dragon.body.ron")),
            ("wyvern", include_str!("../../assets/mobs/wyvern.body.ron")),
            ("bat", include_str!("../../assets/mobs/bat.body.ron")),
        ] {
            let a = build(src);
            let model = a.skeleton.model_from(&a.idle_pose());
            let skin: Vec<Mat4> = model.iter().zip(&a.skeleton.inverse_bind).map(|(m, inv)| Mat4::from(*m) * *inv).collect();
            // The shoulders: where the wings' first bones start.
            let roots: Vec<Vec3> = a.wings.iter().filter_map(|w| w.bones.first()).filter_map(|(bone, _)| model.get(bone.index())).map(|m| m.translation.into()).collect();
            let top = roots.iter().map(|r| r.y).fold(f32::MIN, f32::max);
            let wide = roots.iter().map(|r| r.x.abs()).fold(0.0, f32::max);
            for s in a.sheets.iter().filter(|s| s.kind == SheetKind::Membrane) {
                let span = s.positions.iter().map(|p| p.x.abs()).fold(0.0, f32::max);
                for ((p, j), w) in s.positions.iter().zip(&s.joints).zip(&s.weights) {
                    let posed: Vec3 = j.iter().zip(w).map(|(j, w)| skin[j.idx()].transform_point3(*p) * *w).sum();
                    assert!(posed.y < 0.15f32.mul_add(span, top), "{name}: folded membrane at {posed} rises above the shoulders ({top})");
                    assert!(posed.x.abs() < 0.35f32.mul_add(span, wide), "{name}: folded membrane at {posed} stands out from the side");
                }
            }
        }
    }

    /// Half-folded membrane wings are held up: no wing bone comes near the ground (the old fold
    /// swung the fingers down through it), and the blend is continuous through the half pose.
    #[test]
    fn half_folded_wings_stay_off_the_ground() {
        for (name, src) in [
            ("dragon", include_str!("../../assets/mobs/dragon.body.ron")),
            ("wyvern", include_str!("../../assets/mobs/wyvern.body.ron")),
            ("bat", include_str!("../../assets/mobs/bat.body.ron")),
        ] {
            let a = build(src);
            let rest: Vec<Transform> = a.skeleton.bones.iter().map(|b| b.rest).collect();
            let at = |fold: f32| {
                let mut pose = rest.clone();
                for w in &a.wings {
                    w.apply(fold, &mut pose);
                }
                a.skeleton.model_from(&pose)
            };
            let wing_bones: Vec<usize> = a.wings.iter().flat_map(|w| w.bones.iter().map(|(b, _)| b.index())).collect();
            let ground = at(0.0).iter().map(|m| m.translation.y).fold(f32::MAX, f32::min);
            let height = at(0.0).iter().map(|m| m.translation.y).fold(f32::MIN, f32::max) - ground;
            for fold in [0.25, 0.5, 0.75] {
                let model = at(fold);
                for &i in &wing_bones {
                    let (m, bone) = (model[i], &a.skeleton.bones[i]);
                    let tip = m.transform_point3(Vec3::Y * bone.length);
                    assert!(tip.y > 0.1f32.mul_add(height, ground), "{name}: fold {fold}, a wing bone reaches {} (ground {ground})", tip.y);
                }
            }
            let (below, above) = (at(0.4999), at(0.5001));
            for &i in &wing_bones {
                assert!(below[i].translation.distance(above[i].translation) < 1e-3, "{name}: the fold jumps at its half pose");
            }
        }
    }

    /// Every joint turns less than half a turn between the default open and closed poses, so the
    /// fold blends the short way round at each joint.
    #[test]
    fn membrane_joints_fold_the_short_way() {
        let (o, h, c) = (WingPose::OPEN, WingPose::HALF, WingPose::CLOSED);
        for [from, to] in [[o, h], [h, c]] {
            for (joint, a, b) in [
                ("sweep", from.sweep, to.sweep),
                ("raise", from.raise, to.raise),
                ("elbow", from.elbow, to.elbow),
                ("wrist", from.wrist, to.wrist),
                ("curl", from.curl, to.curl),
                ("spread", from.spread, to.spread),
            ] {
                assert!((b - a).abs() < 175.0, "{joint}: {a} → {b}");
            }
        }
    }
}


