//! The procedural pose, one stage per function, over the motion components. The systems in
//! `pose` run each stage on every mob in parallel; [`Sim`] chains the same stages on plain data
//! for the crowd baker and the tests.
//!
//! Order: choose step → advance clock → rest pose → wings → feet → bob → torso → springs →
//! cycles → leg IK → look-at.

use std::f32::consts::TAU;

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;
use smallvec::SmallVec;

use super::eval::LayerPose;
use super::gait::BodyMotion;
use super::heft::{Heft, HeftResponse};
use super::state::{ActiveStep, Feet, Foot, GaitClock, GaitOverride, Locomotion, LookState, MobPose, SpringBone, SpringState, TorsoState};
use super::{Gait, fabrik, two_bone};
use crate::anatomy::{Anatomy, BoneDef, BoneId, Side};
use crate::num::ToF32;

pub fn leg_length(a: &Anatomy) -> f32 {
    if a.legs.is_empty() {
        0.5
    } else {
        a.legs.iter().map(|l| l.length).sum::<f32>() / a.legs.len().to_f32()
    }
}

#[cfg(test)]
pub fn forward_kinematics(bones: &[BoneDef], local: &[Transform], root: Affine3A) -> Vec<Affine3A> {
    let mut world: Vec<Affine3A> = Vec::with_capacity(bones.len());
    forward_kinematics_into(bones, local, root, &mut world);
    world
}

fn forward_kinematics_into(bones: &[BoneDef], local: &[Transform], root: Affine3A, world: &mut Vec<Affine3A>) {
    world.clear();
    for (b, l) in bones.iter().zip(local) {
        let parent = b.parent.and_then(|p| world.get(p.index()).copied()).unwrap_or(root);
        world.push(parent * l.compute_affine());
    }
}

fn rotation(a: &Affine3A) -> Quat {
    a.to_scale_rotation_translation().1
}

/// What every stage needs to know about the mob this frame.
pub struct Frame<'a> {
    pub a: &'a Anatomy,
    pub gait: &'a Gait,
    /// The mob's root transform.
    pub root: Affine3A,
    pub dt: f32,
    pub time: f32,
    pub leg: f32,
}

impl Frame<'_> {
    /// Speed in leg lengths per second.
    fn speed(&self, loco: &Locomotion) -> f32 {
        loco.velocity.length() / self.leg
    }

    /// Each foot's rest spot, world space.
    fn homes(&self) -> SmallVec<[Vec3; 8]> {
        self.a.legs.iter().map(|l| self.root.transform_point3(l.rest_foot)).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Binding
// ---------------------------------------------------------------------------------------------

/// Fresh state for a (new) body: feet on their rest spots, springs still.
pub fn bind(a: &Anatomy, root: Affine3A, feet: &mut Feet, springs: &mut SpringState, look: &mut LookState, torso: &mut TorsoState, clock: &mut GaitClock) {
    feet.0 = a
        .legs
        .iter()
        .map(|l| {
            let h = root.transform_point3(l.rest_foot);
            Foot { position: h, planted: true, swing_from: h, swing_to: h }
        })
        .collect();
    springs.0 = vec![SpringBone::default(); a.springs.iter().map(|c| c.bones.len()).sum()];
    *look = LookState::default();
    *torso = TorsoState::default();
    clock.phase = 0.0;
}

// ---------------------------------------------------------------------------------------------
// Step and clock (every tier)
// ---------------------------------------------------------------------------------------------

/// Picks the gait mode for the speed and settles the step: the mode's settings (or a playing
/// gait cycle's foot plan) with the weight response applied.
pub fn choose_step(f: &Frame, loco: &Locomotion, heft: &Heft, cycle: &GaitOverride, clock: &mut GaitClock, step: &mut ActiveStep) {
    let gait = f.gait;
    clock.mode = gait.mode_for(f.speed(loco)).min(gait.modes.len().saturating_sub(1));
    let Some(m) = gait.modes.get(clock.mode) else { return };
    let (pattern, duty, lift, bob, stride, body, shape) = match &cycle.0 {
        Some((p, stride)) => (&p.pattern, p.duty, p.lift, p.bob, *stride, p.body, &p.shape),
        None => (&m.pattern, m.duty, m.lift, m.bob, m.stride, m.body, &m.shape),
    };
    let r = &gait.heft;
    let h = heft.heaviness;
    if step.pattern != *pattern {
        step.pattern = pattern.clone();
    }
    if step.shape != *shape {
        step.shape = shape.clone();
    }
    step.base_duty = duty;
    step.duty = (duty + r.duty.sample(h)).clamp(0.2, 0.9);
    step.lift = lift * r.lift.sample(h);
    step.bob = bob;
    step.stride = stride;
    step.body = body;
}

fn cycles_per_second(speed: f32, stride: f32, stepping: bool) -> f32 {
    if speed > 0.05 { (speed / stride).max(0.6) } else if stepping { 1.2 } else { 0.0 }
}

/// Advances the gait phase. `feet` (hero tier) lets the feet keep stepping until they've caught
/// up with the body.
pub fn advance_clock(f: &Frame, loco: &Locomotion, heft: &Heft, step: &ActiveStep, feet: Option<&Feet>, clock: &mut GaitClock) {
    let speed = f.speed(loco);
    let lagging = feet.is_some_and(|feet| {
        let homes = f.homes();
        let error = feet.0.iter().zip(&homes).map(|(p, h)| Vec2::new(p.position.x - h.x, p.position.z - h.z).length()).fold(0.0, f32::max) / f.leg;
        error > 0.15 || feet.0.iter().any(|p| !p.planted)
    });
    clock.stepping = speed > 0.05 || lagging;
    let cadence = f.gait.heft.cadence.sample(heft.heaviness);
    // The same stance distance over a longer stance.
    clock.rate = cycles_per_second(speed, step.stride, clock.stepping) * cadence * step.duty / step.base_duty.max(1e-3);
    clock.phase = clock.rate.mul_add(f.dt, clock.phase).rem_euclid(1.0);
}

// ---------------------------------------------------------------------------------------------
// Pose (hero tier)
// ---------------------------------------------------------------------------------------------

/// Starts the frame's pose from the rest pose with the joints' current scales.
pub fn rest_pose(a: &Anatomy, scales: impl Iterator<Item = Vec3>, pose: &mut MobPose) {
    pose.local.clear();
    pose.local.extend(a.skeleton.bones.iter().zip(scales).map(|(b, s)| Transform { scale: s, ..b.rest }));
    let n = pose.local.len();
    pose.local.extend(a.skeleton.bones.get(n..).unwrap_or_default().iter().map(|b| b.rest));
}

/// Wings blend from spread (bind) toward folded by each wing's fold (a cycle may override it).
pub fn fold_wings(a: &Anatomy, cycles: Option<&LayerPose>, pose: &mut MobPose) {
    for (k, w) in a.wings.iter().enumerate() {
        let (target, weight) = cycles.and_then(|l| l.fold.get(k).copied()).unwrap_or((0.0, 0.0));
        let fold = f32::mul_add(target - w.fold, weight, w.fold);
        for (bone, folded) in &w.bones {
            if let Some(t) = pose.local.get_mut(bone.index()) {
                t.rotation = t.rotation.slerp(*folded, fold);
            }
        }
    }
}

/// Plants feet in their stance and arcs them through the swing. Returns the legs that touched
/// down this frame.
pub fn plant_feet(f: &Frame, loco: &Locomotion, clock: &GaitClock, step: &ActiveStep, hits: &[Option<Vec3>], feet: &mut Feet) -> SmallVec<[usize; 8]> {
    let a = f.a;
    let homes = f.homes();
    let ground_y = f.root.translation.y;
    let mut landed = SmallVec::new();
    for (i, l) in a.legs.iter().enumerate() {
        let floor = hits.get(i).copied().flatten().map_or(ground_y + l.rest_foot.y, |h| h.y);
        let p = step.leg_phase(clock.phase, &a.legs, i);
        let (Some(foot), Some(&home)) = (feet.0.get_mut(i), homes.get(i)) else { continue };
        if clock.stepping && p >= step.duty && clock.rate > 0.0 {
            if foot.planted {
                foot.planted = false;
                foot.swing_from = foot.position;
            }
            let swing = (p - step.duty) / (1.0 - step.duty);
            let mut to = home + loco.velocity * (step.duty / clock.rate) * 0.5;
            to.y = floor;
            foot.swing_to = to;
            let travel = step.shape.travel.sample(swing);
            foot.position = foot.swing_from.lerp(to, travel) + Vec3::Y * step.lift * f.leg * step.shape.arc.sample(swing);
        } else if !foot.planted {
            foot.planted = true;
            foot.position = foot.swing_to;
            landed.push(i);
        }
    }
    landed
}

/// The body bobs with the steps and bends into turns.
pub fn bob_and_bend(f: &Frame, loco: &Locomotion, clock: &GaitClock, step: &ActiveStep, pose: &mut MobPose) {
    let a = f.a;
    let bob = step.bob * f.leg * 0.5f32.mul_add(-(clock.phase * 2.0 * TAU).cos(), 0.5) * f.speed(loco).min(1.0);
    for (b, t) in a.skeleton.bones.iter().zip(&mut pose.local) {
        if b.parent.is_none() {
            t.translation.y -= bob;
        }
    }
    if !a.spine.is_empty() {
        let bend = (-loco.turn_rate * 0.12 / a.spine.len().to_f32()).clamp(-0.15, 0.15);
        for b in &a.spine {
            if let Some(t) = pose.local.get_mut(b.index()) {
                t.rotation *= Quat::from_rotation_z(bend);
            }
        }
    }
}

/// The torso rides the feet: girdles turn, roll and sway over them, the spine bends between,
/// the head steadies, tails swing against the hips.
pub fn ride_torso(f: &Frame, loco: &Locomotion, clock: &GaitClock, step: &ActiveStep, heft: &Heft, state: &mut TorsoState, pose: &mut MobPose) {
    let (a, dt) = (f.a, f.dt);
    let speed = f.speed(loco);
    let level = if speed > 0.05 { step.shape.lively.sample(speed) } else if clock.stepping { 0.3 } else { 0.0 };
    let ease = 1.0 - (-4.0 * dt).exp();
    state.level = (level - state.level).mul_add(ease, state.level);
    let body = state.motion.get_or_insert(step.body);
    body.approach(&step.body, ease);
    let body = *body;
    let Some(torso) = Torso::new(a, step, &body, &f.gait.heft, *heft, f.leg, state.level) else { return };
    let phase = clock.phase;
    let g = torso.at(phase);
    let model = a.skeleton.rest_model();
    let model_rot = |b: BoneId| model.get(b.index()).map_or(Quat::IDENTITY, |m| Quat::from_mat3a(&m.matrix3).normalize());
    // A model-space rotation applied to a bone in its own frame.
    let turn = |local: &mut [Transform], b: BoneId, q: Quat| {
        let m = model_rot(b);
        if let Some(t) = local.get_mut(b.index()) {
            t.rotation *= m.inverse() * q * m;
        }
    };
    let local = &mut pose.local;
    let n = a.spine.len();
    // `Torso::new` checked the spine has bones.
    let Some(&hips) = a.spine.first() else { return };
    // The first spine bone is the root: the hips' turn moves the whole body; the rest of the
    // spine carries the difference to the shoulders.
    turn(local, hips, g.hips());
    if n > 1 {
        let share = 1.0 / (n - 1).to_f32();
        let bend = Quat::from_rotation_y((g.chest_yaw - g.hip_yaw) * share) * Quat::from_rotation_z((g.chest_roll - g.hip_roll) * share);
        for b in a.spine.iter().skip(1) {
            turn(local, *b, bend);
        }
    }
    if let Some(t) = local.get_mut(hips.index()) {
        t.translation += Vec3::new(g.sway, -g.lift, 0.0);
    }
    // The head holds steadier than the shoulders, and nods with the fore feet.
    if let Some(neck) = a.skeleton.find("neck.0").or_else(|| a.skeleton.find("skull")) {
        let steady = Quat::IDENTITY.slerp(g.chest().inverse(), body.steady);
        turn(local, neck, steady * Quat::from_axis_angle(Vec3::NEG_X, g.nod));
    }
    // Tails swing against the hips, the tip trailing.
    let (lag, swing) = torso.tail();
    for chain in &a.springs {
        let Some(first) = chain.bones.first() else { continue };
        if a.skeleton.get(*first).is_none_or(|b| !b.name.starts_with("tail")) {
            continue;
        }
        let n = chain.bones.len().to_f32();
        for (j, b) in chain.bones.iter().enumerate() {
            let late = torso.at(f32::mul_add(lag, -(j + 1).to_f32(), phase));
            turn(local, *b, Quat::from_rotation_y(-late.hip_yaw * swing * 2.0 / n));
        }
    }
}

/// Spring chains (tails, ears) lag behind acceleration and turns.
pub fn swing_springs(f: &Frame, loco: &Locomotion, springs: &mut SpringState, pose: &mut MobPose) {
    let dt = f.dt;
    let accel = f.root.matrix3.inverse() * Vec3A::from((loco.velocity - loco.prev_velocity) / dt);
    let mut k = 0;
    for chain in &f.a.springs {
        let n = chain.bones.len().to_f32();
        for (j, bone) in chain.bones.iter().enumerate() {
            let reach = (j + 1).to_f32() / n;
            let target = Vec2::new(
                (f.time.mul_add(1.7, j.to_f32()).sin() * 0.04).mul_add(reach, (-accel.z * 0.03).clamp(-0.4, 0.4)),
                accel.x.mul_add(0.02, loco.turn_rate * 0.08).clamp(-0.5, 0.5) * reach,
            );
            let Some(s) = springs.0.get_mut(k) else { return };
            let acc = (target - s.angle) * 60.0 * chain.stiffness - s.velocity * 12.0 * chain.damping;
            s.velocity += acc * dt;
            s.angle += s.velocity * dt;
            if let Some(t) = pose.local.get_mut(bone.index()) {
                t.rotation *= Quat::from_rotation_x(s.angle.x) * Quat::from_rotation_z(s.angle.y);
            }
            k += 1;
        }
    }
}

/// Playing cycles' rotations and root offset.
pub fn apply_cycles(a: &Anatomy, cycles: &LayerPose, pose: &mut MobPose) {
    let local = &mut pose.local;
    for ((q, touched), t) in cycles.rot.iter().zip(&cycles.touched).zip(local.iter_mut()) {
        if *touched > 0.0 {
            t.rotation *= *q;
        }
    }
    for (b, t) in a.skeleton.bones.iter().zip(local.iter_mut()) {
        if b.parent.is_none() {
            t.translation += cycles.root_offset;
            t.rotation = cycles.root_rot * t.rotation;
        }
    }
}


/// Scapulohumeral rhythm: past ~30°, a third of an arm's turn moves to its clavicle, so the
/// shoulder lifts rather than one joint folding the skin through the whole raise. The arm still
/// points where the animation put it; only the shoulder point moves.
pub fn shoulder_rhythm(a: &Anatomy, pose: &mut MobPose) {
    const FREE: f32 = 30.0;
    const SHARE: f32 = 1.0 / 3.0;
    const MAX: f32 = 45.0;
    for c in &a.clavicles {
        let (Some(def), Some(arm)) = (a.skeleton.get(c.arm), pose.local.get(c.arm.index())) else { continue };
        let (rest, arm) = (def.rest.rotation, arm.rotation);
        // The arm's turn from rest, in the clavicle's frame; the short way round.
        let mut turn = (arm * rest.inverse()).normalize();
        if turn.w < 0.0 {
            turn = -turn;
        }
        let (axis, angle) = turn.to_axis_angle();
        let shared = ((angle.to_degrees() - FREE).max(0.0) * SHARE).min(MAX).to_radians();
        if shared <= 1e-4 {
            continue;
        }
        let girdle = Quat::from_axis_angle(axis, shared);
        if let Some(t) = pose.local.get_mut(c.bone.index()) {
            t.rotation *= girdle;
        }
        if let Some(t) = pose.local.get_mut(c.arm.index()) {
            t.rotation = girdle.inverse() * arm;
        }
    }
}

/// Legs reach their feet (plus any cycle foot offsets), then cycles flex the lower joints, then
/// loose cloth swings with the thighs. Leaves the pre-IK world transforms in `pose.world`.
pub fn solve_legs(f: &Frame, feet: &Feet, cycles: Option<&LayerPose>, pose: &mut MobPose) {
    let (a, root) = (f.a, f.root);
    let bones = &a.skeleton.bones;
    forward_kinematics_into(bones, &pose.local, root, &mut pose.world);
    let (local, world) = (&mut pose.local, &pose.world);
    // World rotation of a bone's parent (the root's for a root bone).
    let parent_rotation = |b: BoneId| bones.get(b.index()).and_then(|d| d.parent).and_then(|p| world.get(p.index())).map_or_else(|| rotation(&root), rotation);
    for (i, l) in a.legs.iter().enumerate() {
        let chain = &l.chain;
        let (Some(&first), Some(&last), Some(foot)) = (chain.first(), chain.last(), feet.0.get(i)) else { continue };
        let (Some(last_world), Some(last_def)) = (world.get(last.index()), bones.get(last.index())) else { continue };
        let mut points: SmallVec<[Vec3; 6]> = chain.iter().filter_map(|b| world.get(b.index())).map(|m| m.translation.into()).collect();
        if points.len() != chain.len() {
            continue;
        }
        points.push(last_world.transform_point3(Vec3::Y * last_def.length));
        // Knees bend toward the leg's pole, never whichever side the solve happens to find.
        // Some "up" keeps the hint well defined when the leg reaches along the pole (a foot
        // swinging forward); it projects away on a vertical leg.
        let knee_hint = rotation(&root) * (l.pole.normalize_or(Vec3::Z) + Vec3::Y * 0.5);
        let target = foot.position + cycles.and_then(|c| c.feet.get(i)).map_or(Vec3::ZERO, |offset| rotation(&root) * *offset);
        if let [p0, p1, lower_points @ ..] = points.as_mut_slice()
            && let Some(&p2) = lower_points.first()
        {
            // Upper two bones solve exactly; any lower segments (a dog's metatarsus, a horse's
            // cannon and pastern) keep their rest offsets and hang from the second joint, so the
            // upper bones aim for the point above the foot.
            let lower: SmallVec<[Vec3; 6]> = lower_points.iter().map(|p| *p - p2).collect();
            let reach = target - lower.last().copied().unwrap_or(Vec3::ZERO);
            let mut three = [*p0, *p1, p2];
            two_bone(&mut three, reach, knee_hint);
            (*p0, *p1) = (three[0], three[1]);
            for (p, offset) in lower_points.iter_mut().zip(&lower) {
                *p = three[2] + *offset;
            }
        } else {
            fabrik(&mut points, target, 32, 1e-3);
        }
        let mut parent = parent_rotation(first);
        for (b, [from, to]) in chain.iter().zip(points.array_windows()) {
            let Some(t) = local.get_mut(b.index()) else { continue };
            let current = parent * t.rotation;
            let want = (*to - *from).normalize_or(current * Vec3::Y);
            let new = Quat::from_rotation_arc(current * Vec3::Y, want) * current;
            t.rotation = parent.inverse() * new;
            parent = new;
        }
        // Flex after IK (fetlocks, hocks, toes curling in the swing).
        if let Some(c) = cycles {
            for b in chain {
                let i = b.index();
                if let (Some(t), Some(&touched), Some(post)) = (local.get_mut(i), c.touched.get(i), c.post.get(i))
                    && touched > 0.0
                {
                    t.rotation *= *post;
                }
            }
        }
    }
    // Loose cloth swings with the legs. (`world` predates leg IK, but IK only turns leg bones, so
    // the parents' frames still hold.)
    for c in &a.cloth_follow {
        let Some(&hip) = a.legs.get(usize::from(c.leg)).and_then(|leg| leg.chain.first()) else { continue };
        let (Some(hip_local), Some(hip_def)) = (local.get(hip.index()), bones.get(hip.index())) else { continue };
        let swing = hip_local.rotation * hip_def.rest.rotation.inverse();
        let (leg_parent, cloth_parent) = (parent_rotation(hip), parent_rotation(c.root));
        let swing = cloth_parent.inverse() * leg_parent * swing * leg_parent.inverse() * cloth_parent;
        if let Some(t) = local.get_mut(c.root.index()) {
            t.rotation = Quat::IDENTITY.slerp(swing, c.weight) * t.rotation;
        }
    }
}

/// The head turns toward `target` (eased, within the body's limit). Needs [`solve_legs`]'s
/// world transforms.
pub fn look_at(f: &Frame, target: Option<Vec3>, look: &mut LookState, pose: &mut MobPose) {
    let (a, root) = (f.a, f.root);
    let Some(def) = &a.look else { return };
    let bones = &a.skeleton.bones;
    let world = &pose.world;
    let head = def.bones.last().map_or(BoneId(0), |(b, _)| *b);
    let Some(hw) = world.get(head.index()) else { return };
    let goal = target.map_or(Quat::IDENTITY, |target| {
        let fwd = rotation(hw) * Vec3::Y;
        let want = (target - Vec3::from(hw.translation)).normalize_or(fwd);
        let full = Quat::from_rotation_arc(fwd, want);
        let (_, angle) = full.to_axis_angle();
        let limit = def.limit_deg.to_radians();
        if angle > limit { Quat::IDENTITY.slerp(full, limit / angle) } else { full }
    });
    look.0 = look.0.slerp(goal, 1.0 - (-8.0 * f.dt).exp());
    let Some(&(first, _)) = def.bones.first() else { return };
    let mut parent = bones.get(first.index()).and_then(|d| d.parent).and_then(|p| world.get(p.index())).map_or_else(|| rotation(&root), rotation);
    for (b, share) in &def.bones {
        let Some(t) = pose.local.get_mut(b.index()) else { continue };
        let current = parent * t.rotation;
        let new = Quat::IDENTITY.slerp(look.0, *share) * current;
        t.rotation = parent.inverse() * new;
        parent = new;
    }
}

// ---------------------------------------------------------------------------------------------
// Torso motion
// ---------------------------------------------------------------------------------------------

/// The torso's motion over the feet at one gait phase (radians, leg-length offsets already
/// scaled to world units).
#[derive(Clone, Copy, Debug, Default)]
struct Girdles {
    hip_yaw: f32,
    hip_roll: f32,
    chest_yaw: f32,
    chest_roll: f32,
    /// Front up.
    pitch: f32,
    /// Hips toward the planted foot (+X = left).
    sway: f32,
    /// Drop of the root that keeps the rocking centred.
    lift: f32,
    nod: f32,
}

impl Girdles {
    fn hips(&self) -> Quat {
        Quat::from_rotation_y(self.hip_yaw) * Quat::from_rotation_z(self.hip_roll) * Quat::from_axis_angle(Vec3::NEG_X, self.pitch)
    }

    fn chest(&self) -> Quat {
        Quat::from_rotation_y(self.chest_yaw) * Quat::from_rotation_z(self.chest_roll) * Quat::from_axis_angle(Vec3::NEG_X, self.pitch)
    }
}

/// Derives girdle motion from where each foot is in its step.
struct Torso<'a> {
    a: &'a Anatomy,
    step: &'a ActiveStep,
    body: BodyMotion,
    response: &'a HeftResponse,
    heft: Heft,
    fore: u8,
    hind: u8,
    upright: bool,
    /// Hip to shoulder distance.
    length: f32,
    leg: f32,
    level: f32,
}

impl<'a> Torso<'a> {
    fn new(a: &'a Anatomy, step: &'a ActiveStep, body: &BodyMotion, response: &'a HeftResponse, heft: Heft, leg: f32, level: f32) -> Option<Self> {
        if a.spine.is_empty() || a.legs.is_empty() || level <= 1e-3 {
            return None;
        }
        let model = a.skeleton.rest_model();
        let upright = a.spine.first().and_then(|b| model.get(b.index())).is_some_and(|m| (m.matrix3 * Vec3A::Y).y > 0.7);
        let fore = a.legs.iter().map(|l| l.pair).min().unwrap_or(0);
        let hind = a.legs.iter().map(|l| l.pair).max().unwrap_or(0);
        let hip = |pair: u8| {
            let hips = a.legs.iter().filter(|l| l.pair == pair).filter_map(|l| model.get(l.chain.first()?.index()));
            let (sum, n) = hips.fold((Vec3::ZERO, 0.0), |(s, n), m| (s + Vec3::from(m.translation), n + 1.0));
            sum / f32::max(n, 1.0)
        };
        let length = (hip(fore) - hip(hind)).length().max(leg * 0.5);
        Some(Self { a, step, body: *body, response, heft, fore, hind, upright, length, leg, level })
    }

    /// Mean over one pair of (side·reach, side·raise, side·load, load, landing); see
    /// [`StepShape`](super::StepShape) and [`HeftResponse::landing`].
    fn pair(&self, phase: f32, pair: u8) -> [f32; 5] {
        let (mut sum, mut n) = ([0.0; 5], 0.0);
        let shape = &self.step.shape;
        for (i, l) in self.a.legs.iter().enumerate().filter(|(_, l)| l.pair == pair) {
            let p = self.step.leg_phase(phase, &self.a.legs, i);
            let t = self.step.shape_time(p);
            let side = match l.side {
                Side::Left => 1.0,
                Side::Right => -1.0,
                Side::Center => 0.0,
            };
            let load = shape.load.sample(t);
            sum[0] = f32::mul_add(side, shape.reach.sample(t), sum[0]);
            sum[1] = f32::mul_add(side, shape.raise.sample(t), sum[1]);
            sum[2] = f32::mul_add(side, load, sum[2]);
            sum[3] += load;
            sum[4] += if p < self.step.duty { self.response.landing.sample(p / self.step.duty) } else { 0.0 };
            n += 1.0;
        }
        sum.map(|s| s / f32::max(n, 1.0))
    }

    fn at(&self, phase: f32) -> Girdles {
        let b = &self.body;
        let k = self.level;
        let h = self.heft.heaviness;
        let waddle = self.response.waddle.sample(h);
        let settle = self.response.settle.sample(h) * self.leg * k;
        let nod = b.nod.to_radians() * (self.heft.head / 0.1).clamp(0.5, 2.0) * k;
        let hind = self.pair(phase, self.hind);
        let mut g = Girdles {
            hip_yaw: -b.yaw.to_radians() * hind[0] * k,
            hip_roll: -b.roll.to_radians() * hind[1] * k * waddle,
            sway: b.sway * self.leg * hind[2] * k * waddle,
            ..default()
        };
        if self.fore == self.hind {
            g.lift = settle * hind[4];
            if self.upright {
                // The chest turns against the hips; arms swing with it.
                g.chest_yaw = -b.counter * g.hip_yaw;
                g.chest_roll = -0.5 * b.counter * g.hip_roll;
            } else {
                // One pair under a level body (birds): it moves as one, the head bobs.
                g.chest_yaw = g.hip_yaw;
                g.chest_roll = g.hip_roll;
                g.nod = -nod * 2.0f32.mul_add(hind[3], -1.0);
            }
        } else {
            let fore = self.pair(phase, self.fore);
            g.chest_yaw = -b.yaw.to_radians() * fore[0] * k;
            g.chest_roll = -b.roll.to_radians() * fore[1] * k * waddle;
            // Walks vault over a stiff leg (highest mid-stance); runs compress (lowest).
            let vault = if self.step.duty >= 0.5 { 1.0 } else { -1.0 };
            let load_shift = fore[3] - hind[3];
            let rise = b.rock * load_shift * self.leg * vault * k;
            // Each girdle sinks as its feet take its share of the weight.
            let (sink_hind, sink_fore) = (settle * hind[4] * 2.0 * (1.0 - self.heft.fore), settle * fore[4] * 2.0 * self.heft.fore);
            g.pitch = ((rise + sink_hind - sink_fore) / self.length).atan();
            g.lift = rise.mul_add(0.5, sink_hind);
            g.nod = -nod * 2.0f32.mul_add(fore[3], -1.0);
        }
        g
    }

    /// Tails: how far each bone trails the hips (cycles), and how much they swing. A heavy tail
    /// lags more and swings less.
    fn tail(&self) -> (f32, f32) {
        let t = self.heft.tail;
        ((0.06 * 4.0f32.mul_add(t, 1.0)).min(0.12), self.body.tail / 5.0f32.mul_add(t, 1.0))
    }
}

// ---------------------------------------------------------------------------------------------
// Offline
// ---------------------------------------------------------------------------------------------

/// Every stage chained on plain data, for the crowd baker and tests (the ECS runs the same
/// stages as separate systems).
#[derive(Default)]
pub struct Sim {
    pub loco: Locomotion,
    pub clock: GaitClock,
    pub step: ActiveStep,
    pub feet: Feet,
    pub springs: SpringState,
    pub look: LookState,
    pub torso: TorsoState,
    pub heft: Heft,
    pub pose: MobPose,
}

impl Sim {
    pub fn new(a: &Anatomy, root: Affine3A) -> Self {
        let mut s = Self { heft: Heft::of(a), ..default() };
        bind(a, root, &mut s.feet, &mut s.springs, &mut s.look, &mut s.torso, &mut s.clock);
        s
    }

    /// One frame at constant velocity; returns the local pose.
    pub fn frame(&mut self, f: &Frame, target: Option<Vec3>) -> &[Transform] {
        let a = f.a;
        choose_step(f, &self.loco, &self.heft, &GaitOverride(None), &mut self.clock, &mut self.step);
        advance_clock(f, &self.loco, &self.heft, &self.step, Some(&self.feet), &mut self.clock);
        rest_pose(a, std::iter::repeat(Vec3::ONE), &mut self.pose);
        fold_wings(a, None, &mut self.pose);
        plant_feet(f, &self.loco, &self.clock, &self.step, &[], &mut self.feet);
        bob_and_bend(f, &self.loco, &self.clock, &self.step, &mut self.pose);
        ride_torso(f, &self.loco, &self.clock, &self.step, &self.heft, &mut self.torso, &mut self.pose);
        swing_springs(f, &self.loco, &mut self.springs, &mut self.pose);
        shoulder_rhythm(a, &mut self.pose);
        solve_legs(f, &self.feet, None, &mut self.pose);
        look_at(f, target, &mut self.look, &mut self.pose);
        &self.pose.local
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::motion::Gait;

    fn biped() -> Anatomy {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    /// Walks a biped and reports the largest frame-to-frame knee jump, relative to the typical
    /// one. A snapping knee (bend flipping or a straightened leg popping back) shows as a spike.
    #[test]
    fn knees_move_smoothly() {
        let a = biped();
        let gait = Gait::for_legs(a.legs.len());
        let leg = leg_length(&a);
        let mut root = Affine3A::IDENTITY;
        let mut sim = Sim::new(&a, root);
        let dt = 1.0 / 60.0;
        let mut knees: Vec<(f32, Vec3, f32, bool)> = Vec::new();
        let mut extra: Vec<(Vec3, Vec3)> = Vec::new();
        for speed in [0.6, 1.2] {
            sim.loco.velocity = Vec3::Z * speed * leg;
            sim.loco.prev_velocity = sim.loco.velocity;
            for i in 0..240 {
                root.translation += Vec3A::from(sim.loco.velocity * dt);
                let f = Frame { a: &a, gait: &gait, root, dt, time: i.to_f32() * dt, leg };
                let local = sim.frame(&f, None).to_vec();
                let world = forward_kinematics(&a.skeleton.bones, &local, root);
                let l = &a.legs[0];
                let knee = Vec3::from(world[l.chain[1].index()].translation) - Vec3::from(root.translation);
                let hip = Vec3::from(world[l.chain[0].index()].translation);
                let chain: f32 = l.chain.iter().map(|b| a.skeleton.get(*b).unwrap().length).sum();
                let reach = hip.distance(sim.feet.0[0].position) / chain;
                knees.push((sim.clock.phase, knee, reach, sim.feet.0[0].planted));
                extra.push((hip - Vec3::from(root.translation), sim.feet.0[0].position - Vec3::from(root.translation)));
            }
        }
        let steps: Vec<f32> = knees.windows(2).map(|w| w[0].1.distance(w[1].1)).collect();
        let mut sorted = steps.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        let (worst, at) = steps.iter().enumerate().map(|(i, s)| (*s, i)).fold((0.0, 0), |a, b| if b.0 > a.0 { b } else { a });
        for k in at.saturating_sub(3)..(at + 4).min(knees.len()) {
            println!("{k}: phase {:.3} knee {:.3} reach {:.3} planted {} hip {:.3} foot {:.3}", knees[k].0, knees[k].1, knees[k].2, knees[k].3, extra[k].0, extra[k].1);
        }
        println!("chain {:?}", a.legs[0].chain.iter().map(|b| (a.skeleton.get(*b).unwrap().name.clone(), a.skeleton.get(*b).unwrap().length)).collect::<Vec<_>>());
        println!("worst knee step {worst:.4} at {at}, median {median:.4}, ratio {:.1}", worst / median);
        assert!(worst < median * 6.0, "knee snaps: {worst} vs median {median}");
    }
}

#[cfg(test)]
mod all_plans {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::motion::Gait;

    /// Largest frame-to-frame step of any leg's first joint, over its median, while walking at
    /// two speeds. A snapping joint shows as a spike far above the median.
    fn worst_joint_step(a: &Anatomy) -> (f32, f32) {
        let gait = Gait::for_legs(a.legs.len());
        let leg = leg_length(a);
        let mut root = Affine3A::IDENTITY;
        let mut sim = Sim::new(a, root);
        let dt = 1.0 / 60.0;
        let mut prev: Option<Vec<Vec3>> = None;
        let mut steps = Vec::new();
        for speed in [0.5, 1.0] {
            sim.loco.velocity = Vec3::Z * speed * leg;
            sim.loco.prev_velocity = sim.loco.velocity;
            for i in 0..240 {
                root.translation += Vec3A::from(sim.loco.velocity * dt);
                let f = Frame { a, gait: &gait, root, dt, time: i.to_f32() * dt, leg };
                let local = sim.frame(&f, None).to_vec();
                let world = forward_kinematics(&a.skeleton.bones, &local, root);
                let joints: Vec<Vec3> = a
                    .legs
                    .iter()
                    .map(|l| Vec3::from(world[l.chain[1.min(l.chain.len() - 1)].index()].translation) - Vec3::from(root.translation))
                    .collect();
                if let Some(p) = &prev {
                    steps.extend(p.iter().zip(&joints).map(|(a, b)| a.distance(*b)));
                }
                prev = Some(joints);
            }
        }
        let mut sorted = steps.clone();
        sorted.sort_by(f32::total_cmp);
        (sorted[sorted.len() - 1], sorted[sorted.len() / 2])
    }

    #[test]
    fn sample_plans_walk_without_snaps() {
        for (name, src) in [
            ("biped", include_str!("../../assets/mobs/biped.body.ron")),
            ("quadruped", include_str!("../../assets/mobs/quadruped.body.ron")),
            ("horse", include_str!("../../assets/mobs/horse.body.ron")),
            ("hexapod", include_str!("../../assets/mobs/hexapod.body.ron")),
        ] {
            let plan: BodyPlan = ron::from_str(src).unwrap();
            let live = apply_form(&plan, &Form::default()).unwrap();
            let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
            let (worst, median) = worst_joint_step(&a);
            println!("{name}: worst {worst:.4} median {median:.4} ratio {:.1}", worst / median);
            assert!(worst < f32::mul_add(median, 8.0, 0.01), "{name}: a leg joint snaps ({worst} vs median {median})");
        }
    }
}

#[cfg(test)]
mod torso {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::motion::Gait;

    /// World yaw and roll (degrees) of a bone, relative to its rest.
    fn yaw_roll(world: &Affine3A, rest: &Affine3A) -> (f32, f32) {
        let q = Quat::from_mat3a(&world.matrix3).normalize() * Quat::from_mat3a(&rest.matrix3).normalize().inverse();
        let (fwd, up) = (q * Vec3::Z, q * Vec3::Y);
        (fwd.x.atan2(fwd.z).to_degrees(), (-up.x).atan2(up.y).to_degrees())
    }

    /// Walks a body and returns the yaw/roll ranges (peak to peak) of hips, chest and head, and
    /// how the hips' yaw follows the hind left foot (negative = turns away from a forward foot).
    fn walk(src: &str) -> [(f32, f32); 3] {
        let plan: BodyPlan = ron::from_str(src).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let gait = Gait::for_legs(a.legs.len());
        let leg = leg_length(&a);
        let rest = a.skeleton.rest_model();
        let mut root = Affine3A::IDENTITY;
        let mut sim = Sim::new(&a, root);
        let dt = 1.0 / 60.0;
        sim.loco.velocity = Vec3::Z * leg;
        sim.loco.prev_velocity = sim.loco.velocity;
        let head = a.skeleton.find("skull").unwrap_or_else(|| *a.spine.last().unwrap());
        let watch = [a.spine[0], *a.spine.last().unwrap(), head];
        let mut ranges = [(f32::MAX, f32::MIN, f32::MAX, f32::MIN); 3];
        for i in 0..360 {
            root.translation += Vec3A::from(sim.loco.velocity * dt);
            let f = Frame { a: &a, gait: &gait, root, dt, time: i.to_f32() * dt, leg };
            let local = sim.frame(&f, None).to_vec();
            if i < 120 {
                continue;
            }
            let world = forward_kinematics(&a.skeleton.bones, &local, root);
            for (r, b) in ranges.iter_mut().zip(watch) {
                let (y, ro) = yaw_roll(&world[b.index()], &rest[b.index()]);
                *r = (r.0.min(y), r.1.max(y), r.2.min(ro), r.3.max(ro));
            }
        }
        ranges.map(|r| (r.1 - r.0, r.3 - r.2))
    }

    #[test]
    fn torso_moves_and_head_steadies() {
        for (name, src) in [
            ("biped", include_str!("../../assets/mobs/biped.body.ron")),
            ("quadruped", include_str!("../../assets/mobs/quadruped.body.ron")),
            ("horse", include_str!("../../assets/mobs/horse.body.ron")),
            ("hexapod", include_str!("../../assets/mobs/hexapod.body.ron")),
            ("bird", include_str!("../../assets/mobs/bird.body.ron")),
        ] {
            let [hips, chest, head] = walk(src);
            println!("{name}: hips yaw {:.1}° roll {:.1}° | chest yaw {:.1}° roll {:.1}° | head yaw {:.1}° roll {:.1}°", hips.0, hips.1, chest.0, chest.1, head.0, head.1);
            assert!(hips.0 > 2.0 && hips.1 > 1.5, "{name}: the hips barely move");
            assert!(head.0 < chest.0.max(hips.0), "{name}: the head swings more than the body");
        }
    }
}

#[cfg(test)]
mod shoulders {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};

    fn human() -> Anatomy {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    /// The arm's direction and its shoulder's height (world), after raising it by `degrees` and
    /// running the rhythm.
    fn raise(a: &Anatomy, degrees: f32) -> (Vec3, Vec3, f32, Quat) {
        let c = a.clavicles[0];
        let mut pose = MobPose::default();
        rest_pose(a, std::iter::repeat(Vec3::ONE), &mut pose);
        // Raise sideways: about the body's forward axis, in the clavicle's frame.
        let model = a.skeleton.rest_model();
        let clav = Quat::from_mat3a(&model[c.bone.index()].matrix3).normalize();
        let out = model[c.arm.index()].translation.x.signum();
        let q = clav.inverse() * Quat::from_rotation_z(out * degrees.to_radians()) * clav;
        pose.local[c.arm.index()].rotation = q * pose.local[c.arm.index()].rotation;
        let before = forward_kinematics(&a.skeleton.bones, &pose.local, Affine3A::IDENTITY);
        shoulder_rhythm(a, &mut pose);
        let after = forward_kinematics(&a.skeleton.bones, &pose.local, Affine3A::IDENTITY);
        let dir = |w: &[Affine3A]| (w[c.arm.index()].matrix3 * Vec3A::Y).normalize();
        let lift = after[c.arm.index()].translation.y - before[c.arm.index()].translation.y;
        (dir(&before).into(), dir(&after).into(), lift, pose.local[c.bone.index()].rotation * a.skeleton.bones[c.bone.index()].rest.rotation.inverse())
    }

    #[test]
    fn arms_hang_from_clavicles() {
        let a = human();
        assert_eq!(a.clavicles.len(), 2);
        for c in &a.clavicles {
            assert_eq!(a.skeleton.bones[c.arm.index()].parent, Some(c.bone));
        }
    }

    #[test]
    fn raised_arms_lift_the_shoulder_and_keep_their_aim() {
        let a = human();
        let (before, after, lift, _) = raise(&a, 120.0);
        println!("120° raise: shoulder lifts {:.1} cm, aim changes {:.3}°", lift * 100.0, before.angle_between(after).to_degrees());
        assert!(after.y > 0.3, "a 120° raise points the arm up: {after}");
        assert!(before.angle_between(after).to_degrees() < 0.1, "the arm keeps pointing where it was put");
        assert!(lift > 0.01, "the shoulder lifts: {lift}");
        let (_, _, small, clav) = raise(&a, 20.0);
        assert!(small.abs() < 1e-5 && clav.angle_between(Quat::IDENTITY) < 1e-4, "walking swings leave the clavicle alone");
    }
}
