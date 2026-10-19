//! [`PartBuild`] implementations: the geometry of every part kind.
//!
//! Conventions (logical model frame): Y up, mob faces +Z, left = +X. Bones extend along local
//! +Y. Segmented parts keep local -Z toward the dorsal (top) side.

use std::f32::consts::FRAC_PI_2;

use bevy::prelude::*;
use smallvec::SmallVec;

use super::builder::{AxisSample, SkullFrame, frame_y_z};
use super::{
    AnatomyBuilder, AnatomyError, AttachPoint, AttachSlot, BoneId, BuildCx, ChainParams, ChainPart,
    ChainRole, CrossSection, DigitParams, HardParams, HeadParams, LimbEnd, LimbParams, LimbRole,
    HardKind, LookDef, MassParams, MassShape, Orient, PartBuild, PartSlots, PrimLayer, PrimOp, PrimShape, Profile,
    SculptParams,
    RegionId, RegionKind, Side, SocketKind, SpringChainDef, Stance, TorsoParams,
};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Piecewise-linear lookup in `(t, value)` keys; `default` if empty.
fn curve_at(keys: &[(f32, f32)], t: f32, default: f32) -> f32 {
    match keys {
        [] => default,
        [(_, v)] => *v,
        _ => {
            if t <= keys[0].0 {
                return keys[0].1;
            }
            for w in keys.windows(2) {
                let ((t0, v0), (t1, v1)) = (w[0], w[1]);
                if t <= t1 {
                    let k = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                    return v0 + (v1 - v0) * k;
                }
            }
            keys[keys.len() - 1].1
        }
    }
}

fn radius_at(p: &Profile, t: f32) -> f32 {
    (curve_at(&p.curve, t, 1.0) * p.scale).max(1e-3)
}

/// Non-uniform scale (local X = width, local Z = height) for a cross-section.
fn cross_scale(c: CrossSection) -> Vec3 {
    let aspect = match c {
        CrossSection::Round => 1.0,
        CrossSection::Ellipse { aspect } | CrossSection::Flat { aspect } | CrossSection::Teardrop { aspect } => {
            aspect.max(0.05)
        }
    };
    let s = aspect.sqrt();
    Vec3::new(s, 1.0, 1.0 / s)
}

fn with_offset(at: &AttachPoint, cx: &BuildCx) -> Transform {
    at.model * cx.offset
}

fn moved(t: Transform, local: Vec3) -> Transform {
    Transform { translation: t.translation + t.rotation * local, ..t }
}

fn suffixed(base: &str, cx: &BuildCx) -> String {
    match cx.side {
        super::Side::Left => format!("{base}.l"),
        super::Side::Right => format!("{base}.r"),
        super::Side::Center => base.to_owned(),
    }
}

/// `n + 1` frames along a chain: `frames[i]` starts segment `i`, the last is the tip.
/// Segment `i` is rotated by `bend(i)` (local) relative to the previous segment.
fn chain_frames(start: Transform, lens: &[f32], bend: impl Fn(usize) -> Quat) -> Vec<Transform> {
    let n = lens.len();
    let mut frames = Vec::with_capacity(n + 1);
    let mut f = Transform { rotation: start.rotation * bend(0), ..start };
    for i in 0..n {
        frames.push(f);
        let tip = f.translation + f.rotation * Vec3::Y * lens[i];
        let rot = if i + 1 < n { f.rotation * bend(i + 1) } else { f.rotation };
        f = Transform::from_translation(tip).with_rotation(rot);
    }
    frames.push(f);
    frames
}

/// Emits one bone + one round-cone primitive per segment. Returns bones and axis samples.
fn emit_chain(
    b: &mut AnatomyBuilder,
    parent: Option<BoneId>,
    name: &str,
    frames: &[Transform],
    chain: &ChainParams,
    region: impl Fn(f32) -> RegionId,
) -> Result<(Vec<BoneId>, SmallVec<[AxisSample; 9]>), AnatomyError> {
    let n = frames.len() - 1;
    let mut bones = Vec::with_capacity(n);
    let mut axis = SmallVec::new();
    let scale = cross_scale(chain.radius.cross);
    let mut prev = parent;
    // `t` runs along the chain's length (segments may differ), so radius profiles stay put.
    let mut along = vec![0.0f32; n + 1];
    for i in 0..n {
        along[i + 1] = along[i] + frames[i].translation.distance(frames[i + 1].translation);
    }
    let total = along[n].max(1e-6);
    for i in 0..n {
        let (t0, t1) = (along[i] / total, along[i + 1] / total);
        let len = frames[i].translation.distance(frames[i + 1].translation).max(1e-3);
        let bone = b.bone(prev, &format!("{name}.{i}"), frames[i], len, region(t0))?;
        let (r0, r1) = (radius_at(&chain.radius, t0), radius_at(&chain.radius, t1));
        let blend = if i == 0 { chain.blend } else { 0.5 * r0.min(r1) };
        b.primitive(
            bone,
            PrimShape::RoundCone { r0, r1, len },
            Transform { scale, ..frames[i] },
            blend,
            PrimOp::Union,
            PrimLayer::Base,
            region(t0),
        );
        // Blend only around the segment's own joint, so a limb hanging beside the body (or a
        // tail beside a leg) stays separate along its length.
        b.anchor_last_blend(Vec3::ZERO, 2.0 * r0 + 2.0 * blend);
        axis.push(AxisSample { t: t0, bone, model: frames[i], radii: Vec2::new(scale.x, scale.z) * r0 });
        bones.push(bone);
        prev = Some(bone);
    }
    let last = *bones.last().expect("chain has at least one segment");
    let r_end = radius_at(&chain.radius, 1.0);
    axis.push(AxisSample { t: 1.0, bone: last, model: frames[n], radii: Vec2::new(scale.x, scale.z) * r_end });
    Ok((bones, axis))
}

fn slot(slot: AttachSlot, bone: BoneId, model: Transform) -> (AttachSlot, AttachPoint) {
    (slot, AttachPoint { bone: Some(bone), model })
}

/// Start/End/axis slots of a plain chain.
fn chain_slots(bones: &[BoneId], frames: &[Transform], axis: SmallVec<[AxisSample; 9]>) -> PartSlots {
    let (first, last) = (bones[0], bones[bones.len() - 1]);
    let start = frames[0];
    // Flip about local Z: points back along the chain while keeping the dorsal side (-Z) up.
    let back = Transform { rotation: start.rotation * Quat::from_rotation_z(std::f32::consts::PI), ..start };
    PartSlots {
        fixed: [slot(AttachSlot::Start, first, back), slot(AttachSlot::End, last, frames[frames.len() - 1])]
            .into_iter()
            .collect(),
        axis,
        skull: None,
    }
}

// ---------------------------------------------------------------------------------------------
// Torso
// ---------------------------------------------------------------------------------------------

impl PartBuild for TorsoParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let n = self.chain.segments.max(1) as usize;
        let len = self.chain.length.max(0.01);
        let base = with_offset(&at, cx);
        let points: Vec<Vec3> = (0..=n)
            .map(|i| {
                let t = i as f32 / n as f32;
                base.transform_point(Vec3::new(0.0, curve_at(&self.arch, t, 0.0), -len * 0.5 + t * len))
            })
            .collect();
        let frames: Vec<Transform> = (0..=n)
            .map(|i| {
                let dir = if i < n { points[i + 1] - points[i] } else { points[n] - points[n - 1] };
                Transform::from_translation(points[i]).with_rotation(frame_y_z(dir, base.rotation * Vec3::NEG_Y))
            })
            .collect();

        let rear = b.region(RegionKind::TorsoRear, cx.side)?;
        let mid = b.region(RegionKind::TorsoMid, cx.side)?;
        let front = b.region(RegionKind::TorsoFront, cx.side)?;
        let region = |t: f32| if t < 0.34 { rear } else if t < 0.66 { mid } else { front };
        let (bones, axis) = emit_chain(b, at.bone, "torso", &frames, &self.chain, region)?;
        b.spine(&bones);

        if self.belly > 0.0 {
            let belly = b.region(RegionKind::Belly, cx.side)?;
            let s = axis[n / 2];
            let r = s.min_radius();
            b.primitive(
                bones[n / 2],
                PrimShape::Ellipsoid { radii: Vec3::new(r * 0.85, len / n as f32 * 1.2, r * (0.6 + 0.4 * self.belly)) },
                moved(s.model, Vec3::new(0.0, 0.0, r * 0.35 * self.belly)),
                r * 0.5,
                PrimOp::Union,
                PrimLayer::Fat,
                belly,
            );
        }
        let slots = chain_slots(&bones, &frames, axis);
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
        let n = c.segments.max(1) as usize;
        let lens = c.segment_lengths();
        // Curl is per metre: each joint bends by the curl over the segment before it.
        let frames = chain_frames(with_offset(&at, cx), &lens, |i| {
            if i == 0 { Quat::IDENTITY } else { Quat::from_rotation_x((c.curl * lens[i - 1]).to_radians()) }
        });

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
        let slots = chain_slots(&bones, &frames, axis);
        match self.role {
            ChainRole::Neck => surface_socket(b, &slots, &suffixed("neck", cx), SocketKind::Neck, cx.side, 0.5, 180.0),
            ChainRole::Tail => {
                let size = slots.axis.last().map_or(0.02, |s| s.min_radius() * 2.0);
                b.socket(&suffixed("tail_tip", cx), SocketKind::TailTip, cx.side, bones[n - 1], frames[n], size);
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

impl PartBuild for LimbParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let n = self.chain.segments.max(1) as usize;
        let lens = self.chain.segment_lengths();
        let pos = with_offset(&at, cx).translation;
        let outward = at.model.rotation * Vec3::Y;

        // Initial direction: straight down rotated toward the attach normal by the splay.
        let splay = match self.stance {
            Stance::Sprawl | Stance::Arched if self.role != LimbRole::Manipulator => self.splay_deg.max(85.0),
            _ => self.splay_deg,
        };
        let axis = Vec3::NEG_Y.cross(outward).try_normalize().unwrap_or(Vec3::Z);
        let dir0 = Quat::from_axis_angle(axis, splay.to_radians().min(FRAC_PI_2 * 1.2)) * Vec3::NEG_Y;
        let base = Transform::from_translation(pos)
            .with_rotation(frame_y_z(dir0, Vec3::Z) * cx.offset.rotation);

        let (role, stance) = (self.role, self.stance);
        let frames = chain_frames(base, &lens, |i| {
            let (x, z) = self.bends.get(i).copied().unwrap_or_else(|| limb_bends(role, stance, i));
            Quat::from_rotation_x(x.to_radians()) * Quat::from_rotation_z(z.to_radians())
        });
        let upper = b.region(RegionKind::LimbUpper, cx.side)?;
        let lower = b.region(RegionKind::LimbLower, cx.side)?;
        let end_region = b.region(RegionKind::LimbEnd, cx.side)?;
        // Arms hang from a clavicle reaching in from the shoulder toward the chest, so raises can
        // share their rotation with it (a shoulder lifts; it doesn't fold).
        let clavicle_len = match self.clavicle {
            Some(l) => l,
            None if self.role == LimbRole::Manipulator => lens[0] * 0.45,
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
        if let Some(c) = clavicle {
            b.clavicle(c, bones[0], cx.side);
        }
        let last = bones[n - 1];
        let tip = frames[n];
        let r = radius_at(&self.chain.radius, 1.0);
        // Ground-contact frame at the tip: Y forward (horizontal), Z down.
        let ground = Transform::from_translation(tip.translation).with_rotation(frame_y_z(Vec3::Z, Vec3::NEG_Y));

        let prim = |b: &mut AnatomyBuilder, shape: PrimShape, model: Transform| {
            b.primitive(last, shape, model, r * 0.6, PrimOp::Union, PrimLayer::Base, end_region)
        };
        let end_slot = match self.end {
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
                let m = moved(ground, Vec3::new(0.0, length * 0.5 - r * 0.5, r * 0.2));
                prim(b, PrimShape::RoundBox { half: Vec3::new(r * 1.1, length * 0.5, r * 0.45), round: r * 0.4 }, m);
                moved(ground, Vec3::new(0.0, length - r * 0.5, r * 0.2))
            }
            LimbEnd::Pad => {
                prim(b, PrimShape::Ellipsoid { radii: Vec3::new(r * 1.3, r * 1.4, r * 0.7) }, moved(ground, Vec3::Y * r * 0.4));
                moved(ground, Vec3::new(0.0, r * 1.5, r * 0.2))
            }
            LimbEnd::Hoof { .. } => {
                let down = Transform::from_translation(tip.translation).with_rotation(frame_y_z(Vec3::NEG_Y, Vec3::Z));
                prim(b, PrimShape::RoundCone { r0: r * 1.05, r1: r * 1.3, len: r * 1.2 }, down);
                moved(ground, Vec3::Y * r * 1.3)
            }
            LimbEnd::Pincer { size } => {
                prim(b, PrimShape::Ellipsoid { radii: Vec3::new(size * 0.5, size, size * 0.4) }, moved(tip, Vec3::Y * size));
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
        };

        if matches!(self.role, LimbRole::Manipulator | LimbRole::Both) && self.end.grasps() {
            let size = match self.end {
                LimbEnd::Hand { palm } => palm,
                LimbEnd::Pincer { size } => size,
                _ => r * 2.0,
            };
            // Socket +Y = the held tool's axis: across the fist (forward when the arm hangs);
            // +Z runs down the limb toward the fingers.
            let fist = Quat::from_mat3(&Mat3::from_cols(Vec3::NEG_X, Vec3::Z, Vec3::Y));
            let grip = moved(tip, Vec3::Y * size * 0.5).with_rotation(tip.rotation * fist);
            b.socket(&suffixed("grip", cx), SocketKind::Grip, cx.side, last, grip, size);
        }
        if matches!(self.role, LimbRole::Locomotor | LimbRole::Both) {
            // The IK pole is the rest pose's own first joint direction: knees forward, a
            // quadruped's elbows back, a spider's knees up.
            let (hip, foot) = (frames[0].translation, frames[n].translation);
            let axis = (foot - hip).normalize_or(Vec3::NEG_Y);
            let knee = frames[1.min(n)].translation - hip;
            let pole = (knee - axis * knee.dot(axis)).try_normalize().unwrap_or(Vec3::Z);
            b.leg(bones.iter().copied().collect(), cx.side, self.stance, tip.translation, pole);
        }

        let mut slots = chain_slots(&bones, &frames, axis_samples);
        slots.fixed[1] = slot(AttachSlot::End, last, end_slot);
        Ok(slots)
    }
}

// ---------------------------------------------------------------------------------------------
// Head
// ---------------------------------------------------------------------------------------------

impl PartBuild for HeadParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let base = with_offset(&at, cx);
        let fwd = base.rotation * Vec3::Y;
        let level = Vec3::new(fwd.x, 0.0, fwd.z).try_normalize().unwrap_or(Vec3::Z);
        // Skull frame: Y forward (level), Z down, X left.
        let skull = Transform::from_translation(base.translation).with_rotation(frame_y_z(level, Vec3::NEG_Y));
        let s = self.skull.size; // half extents: x width, y height, z depth
        let skull_region = b.region(RegionKind::Skull, cx.side)?;
        let snout_region = b.region(RegionKind::Snout, cx.side)?;
        let bone = b.bone(at.bone, "skull", skull, s.z * 2.0 + self.snout, skull_region)?;

        b.primitive(
            bone,
            PrimShape::Ellipsoid { radii: Vec3::new(s.x, s.z, s.y) },
            moved(skull, Vec3::Y * s.z * 0.8),
            self.skull.blend,
            PrimOp::Union,
            PrimLayer::Base,
            skull_region,
        );
        let snout_start = s.z * 1.2;
        if self.snout > 0.0 {
            b.primitive(
                bone,
                PrimShape::RoundCone { r0: s.y * 0.55, r1: s.y * 0.35, len: self.snout },
                moved(skull, Vec3::new(0.0, snout_start, s.y * 0.15)),
                s.y * 0.3,
                PrimOp::Union,
                PrimLayer::Base,
                snout_region,
            );
        }
        let front = snout_start + self.snout + s.y * 0.35;

        let mut fixed: SmallVec<[(AttachSlot, AttachPoint); 6]> = SmallVec::new();
        if let Some(j) = &self.jaw {
            let jaw_region = b.region(RegionKind::Jaw, cx.side)?;
            let start = moved(skull, Vec3::new(0.0, s.z * 0.6, s.y * 0.45));
            let jaw = b.bone(Some(bone), "jaw", start, j.length, jaw_region)?;
            let (r0, r1) = (radius_at(&j.radius, 0.0) * 0.8, radius_at(&j.radius, 1.0) * 0.6);
            b.primitive(
                jaw,
                PrimShape::RoundCone { r0, r1, len: j.length },
                Transform { scale: Vec3::new(1.4, 1.0, 0.6), ..start },
                j.blend,
                PrimOp::Union,
                PrimLayer::Base,
                jaw_region,
            );
            fixed.push(slot(AttachSlot::Jaw, jaw, moved(start, Vec3::Y * j.length)));
        }

        // Local placement in the skull frame: (position, +Y direction, +Z hint).
        let place = |p: Vec3, dir: Vec3, hint: Vec3| skull * Transform::from_translation(p).with_rotation(frame_y_z(dir, hint));
        let eye_dir = Vec3::Y.lerp(Vec3::X, self.eye_lateral.clamp(0.0, 1.0));
        let eye = place(Vec3::new(s.x * self.eye_spacing.max(0.2), s.z * 1.3, -s.y * 0.3), eye_dir, Vec3::Z);
        let ear = place(Vec3::new(s.x * 0.6, s.z * 0.5, -s.y * 0.85), Vec3::new(0.5, -0.2, -1.0), Vec3::Y);
        let horn = place(Vec3::new(s.x * 0.45, s.z * 1.1, -s.y * 0.85), Vec3::new(0.3, 0.4, -1.0), Vec3::Y);
        let crown = place(Vec3::new(0.0, s.z * 0.8, -s.y), Vec3::NEG_Z, Vec3::Y);
        let nose = place(Vec3::new(0.0, front, s.y * 0.15), Vec3::Y, Vec3::Z);
        let mouth = place(Vec3::new(0.0, front * 0.9, s.y * 0.5), Vec3::Y, Vec3::Z);
        for (sl, m) in [
            (AttachSlot::Eye, eye),
            (AttachSlot::Ear, ear),
            (AttachSlot::Horn, horn),
            (AttachSlot::Crown, crown),
            (AttachSlot::Nose, nose),
            (AttachSlot::Mouth, mouth),
            (AttachSlot::End, nose),
        ] {
            fixed.push(slot(sl, bone, m));
        }
        b.socket("head", SocketKind::Head, cx.side, bone, crown, s.x * 2.0);
        b.socket("mouth", SocketKind::Mouth, cx.side, bone, mouth, s.y);

        // Look-at: neck bones (root to head) plus the skull, sharing the turn.
        let mut look: Vec<BoneId> = at.bone.map(|p| b.ancestor_bones(p, RegionKind::Neck).into_vec()).unwrap_or_default();
        look.reverse();
        look.push(bone);
        let share = 1.0 / look.len() as f32;
        b.look(LookDef { bones: look.into_iter().map(|b| (b, share)).collect(), limit_deg: 70.0 });

        let axis = [0.0, 1.0]
            .into_iter()
            .map(|t| AxisSample { t, bone, model: moved(skull, Vec3::Y * s.z * 1.6 * t), radii: Vec2::new(s.x, s.y) })
            .collect();
        let skull_frame = SkullFrame { bone, model: moved(skull, Vec3::Y * s.z * 0.8), radii: Vec3::new(s.x, s.z, s.y) };
        Ok(PartSlots { fixed, axis, skull: Some(skull_frame) })
    }
}

// ---------------------------------------------------------------------------------------------
// Digits
// ---------------------------------------------------------------------------------------------

impl PartBuild for DigitParams {
    fn build(&self, b: &mut AnatomyBuilder, at: AttachPoint, cx: &BuildCx) -> Result<PartSlots, AnatomyError> {
        let base = with_offset(&at, cx);
        let n = self.count.max(1) as usize;
        let segs = self.chain.segments.max(1) as usize;
        let lens = self.chain.segment_lengths();
        let spacing = self.chain.radius.scale * 2.4;
        let curl = |s: usize| Quat::from_rotation_x((self.chain.curl * lens[s - 1]).to_radians());
        let region = b.region(RegionKind::Digit, cx.side)?;
        let hard_region = if self.tip.is_some() { Some(b.region(RegionKind::Hard, cx.side)?) } else { None };
        let mut end = None;
        // An opposable first digit (a thumb) roots back toward the wrist on the palm side, shorter
        // and thicker, turned to close across the palm; the rest line up along the end.
        let thumb = self.opposable && n > 1;
        let row = if thumb { n - 1 } else { n };
        let reach: f32 = lens.iter().sum();
        let r = self.chain.radius.scale;
        let thumb_chain = ChainParams { radius: Profile { scale: r * 1.2, ..self.chain.radius.clone() }, ..self.chain.clone() };
        let thumb_lens: Vec<f32> = lens.iter().map(|l| l * 0.75).collect();
        for i in 0..n {
            let is_thumb = thumb && i == 0;
            let j = if thumb { i.saturating_sub(1) } else { i };
            let k = if row > 1 { j as f32 / (row - 1) as f32 - 0.5 } else { 0.0 };
            let start = if is_thumb {
                let edge = spacing * 0.5 * (row - 1) as f32 + r;
                base * Transform::from_translation(Vec3::new(-edge, -reach * 0.6, r * 0.6))
                    .with_rotation(Quat::from_rotation_z(40f32.to_radians()) * Quat::from_rotation_y(70f32.to_radians()))
            } else {
                base * Transform::from_translation(Vec3::X * spacing * k * (row - 1) as f32)
                    .with_rotation(Quat::from_rotation_z((-self.spread_deg * k).to_radians()))
            };
            let (chain, seg_lens) = if is_thumb { (&thumb_chain, &thumb_lens) } else { (&self.chain, &lens) };
            let frames = chain_frames(start, seg_lens, |s| if s == 0 { Quat::IDENTITY } else { curl(s) });
            let name = suffixed(&format!("digit{i}"), cx);
            let (bones, _) = emit_chain(b, at.bone, &name, &frames, chain, |_| region)?;
            let last = bones[segs - 1];
            if let (Some(tip), Some(hr)) = (&self.tip, hard_region) {
                b.hard_part(last, frames[segs], tip.clone(), hr);
            }
            if i == n / 2 {
                end = Some(slot(AttachSlot::End, last, frames[segs]));
            }
        }
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
        Ok(PartSlots {
            fixed: [slot(AttachSlot::End, bone, moved(model, Vec3::Y * s.y))].into_iter().collect(),
            ..default()
        })
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
        b.primitive(
            bone,
            PrimShape::Grid { grid, mirror_x: false },
            Transform { scale: Vec3::splat(self.scale), ..base },
            self.blend,
            PrimOp::Union,
            PrimLayer::Base,
            region,
        );
        // Slots along the part's local +Y, through the middle of its bounds.
        let mid = (lo + hi) * 0.5;
        let radius = ((hi.x - lo.x) + (hi.z - lo.z)) * 0.25;
        let sample = |t: f32| moved(base, Vec3::new(mid.x, lo.y + (hi.y - lo.y) * t, mid.z));
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
