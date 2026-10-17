use std::f32::consts::{PI, TAU};

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;

use super::{DefaultGaits, ExternalRoot, Foot, FootGround, FootProbes, Gait, MobGait, MobIntent, MotionState, SpringBone, fabrik};
use crate::anatomy::{Anatomy, BoneDef, BoneId};
use crate::mob::{GeneratedBody, MobBody, MobJoint, MobSkeleton};

pub(crate) fn leg_length(a: &Anatomy) -> f32 {
    if a.legs.is_empty() {
        0.5
    } else {
        a.legs.iter().map(|l| l.length).sum::<f32>() / a.legs.len() as f32
    }
}

pub(crate) fn gait_for<'a>(handle: Option<&MobGait>, gaits: &'a Assets<Gait>, defaults: &'a DefaultGaits, a: &Anatomy) -> &'a Gait {
    handle.and_then(|h| gaits.get(&h.0)).unwrap_or_else(|| defaults.for_legs(a.legs.len()))
}

fn yaw_of(q: Quat) -> f32 {
    let f = q * Vec3::Z;
    f.x.atan2(f.z)
}

fn wrap(a: f32) -> f32 {
    (a + PI).rem_euclid(TAU) - PI
}

/// Moves mob roots toward their intent (or reads externally driven roots).
#[allow(clippy::type_complexity)]
pub(super) fn move_roots(
    time: Res<Time>,
    bodies: Res<Assets<GeneratedBody>>,
    gaits: Res<Assets<Gait>>,
    defaults: Res<DefaultGaits>,
    mut mobs: Query<(&mut Transform, &MobIntent, &mut MotionState, &MobBody, Option<&MobGait>, Has<ExternalRoot>)>,
) {
    let dt = time.delta_secs().max(1e-4);
    for (mut tf, intent, mut state, body, gait, external) in &mut mobs {
        let Some(body) = bodies.get(&body.0) else { continue };
        let leg = leg_length(&body.anatomy);
        let gait = gait_for(gait, &gaits, &defaults, &body.anatomy);
        state.prev_velocity = state.velocity;

        if external {
            if let Some((pos, rot)) = state.last_root {
                state.velocity = (tf.translation - pos) / dt;
                state.turn_rate = wrap(yaw_of(tf.rotation) - yaw_of(rot)) / dt;
            }
        } else {
            let want = Vec3::new(intent.velocity.x, 0.0, intent.velocity.z);
            let dv = (want - state.velocity).clamp_length_max(gait.accel * leg * dt);
            state.velocity += dv;
            tf.translation += state.velocity * dt;

            let facing = intent
                .facing
                .map(|d| d.as_vec3())
                .or_else(|| (state.velocity.length() > 0.05 * leg).then_some(state.velocity));
            state.turn_rate = 0.0;
            if let Some(f) = facing {
                let delta = wrap(f.x.atan2(f.z) - yaw_of(tf.rotation));
                let max = gait.turn_rate_deg.to_radians() * dt;
                let step = delta.clamp(-max, max);
                tf.rotation = Quat::from_rotation_y(step) * tf.rotation;
                state.turn_rate = step / dt;
            }
        }
        state.last_root = Some((tf.translation, tf.rotation));
    }
}

pub(crate) fn forward_kinematics(bones: &[BoneDef], local: &[Transform], root: Affine3A) -> Vec<Affine3A> {
    let mut world: Vec<Affine3A> = Vec::with_capacity(bones.len());
    for (i, b) in bones.iter().enumerate() {
        let parent = b.parent.map_or(root, |p| world[p.index()]);
        world.push(parent * local[i].compute_affine());
    }
    world
}

fn rotation(a: &Affine3A) -> Quat {
    a.to_scale_rotation_translation().1
}

/// Resets motion state for a (new) body: feet at their rest spots.
pub(crate) fn reset_state(state: &mut MotionState, a: &Anatomy, root: Affine3A) {
    state.feet = a
        .legs
        .iter()
        .map(|l| {
            let h = root.transform_point3(l.rest_foot);
            Foot { position: h, planted: true, swing_from: h, swing_to: h }
        })
        .collect();
    state.springs = vec![SpringBone::default(); a.springs.iter().map(|c| c.bones.len()).sum()];
    state.look = Quat::IDENTITY;
    state.phase = 0.0;
}

/// Chosen gait mode and gait cycles per second.
pub(crate) fn gait_rate(state: &MotionState, gait: &Gait, leg: f32, stepping: bool) -> (usize, f32) {
    let speed = state.velocity.length() / leg;
    let mode = gait.mode_for(speed).min(gait.modes.len().saturating_sub(1));
    let m = &gait.modes[mode];
    let cycles = if speed > 0.05 { (speed / m.stride).max(0.6) } else if stepping { 1.2 } else { 0.0 };
    (mode, cycles)
}

/// Everything the procedural pose needs besides the motion state.
pub(crate) struct PoseInput<'a> {
    pub anatomy: &'a Anatomy,
    pub gait: &'a Gait,
    pub root: Affine3A,
    /// Ground height used where no hit is given.
    pub ground_y: f32,
    pub hits: &'a [Option<Vec3>],
    pub look_at: Option<Vec3>,
    /// Current per-bone joint scales (from `BoneScales`).
    pub scales: &'a [Vec3],
    pub dt: f32,
    pub time: f32,
}

/// The full procedural pose for one frame: gait → feet → bob → spine → springs → leg IK → look-at.
/// Returns local joint transforms (scale = the given scales).
pub(crate) fn solve_pose(input: &PoseInput, state: &mut MotionState) -> Vec<Transform> {
    let a = input.anatomy;
    let bones = &a.skeleton.bones;
    let (root, dt) = (input.root, input.dt);
    let leg = leg_length(a);
    let homes: Vec<Vec3> = a.legs.iter().map(|l| root.transform_point3(l.rest_foot)).collect();
    let mut local: Vec<Transform> = bones
        .iter()
        .zip(input.scales)
        .map(|(b, s)| Transform { scale: *s, ..b.rest })
        .collect();

    // --- Gait phase -----------------------------------------------------------------------------
    let max_error = state
        .feet
        .iter()
        .zip(&homes)
        .map(|(f, h)| Vec2::new(f.position.x - h.x, f.position.z - h.z).length())
        .fold(0.0, f32::max)
        / leg;
    let speed = state.velocity.length() / leg;
    let stepping = speed > 0.05 || max_error > 0.15 || state.feet.iter().any(|f| !f.planted);
    let (mode_index, cycles) = gait_rate(state, input.gait, leg, stepping);
    state.mode = mode_index;
    let mode = &input.gait.modes[mode_index];
    state.phase = (state.phase + cycles * dt).rem_euclid(1.0);

    // --- Feet: plant in stance, arc in swing --------------------------------------------------
    let (velocity, phase) = (state.velocity, state.phase);
    for (i, l) in a.legs.iter().enumerate() {
        let floor = input.hits.get(i).copied().flatten().map_or(input.ground_y + l.rest_foot.y, |h| h.y);
        let p = (phase + mode.pattern.offset(l, i, &a.legs)).rem_euclid(1.0);
        let foot = &mut state.feet[i];
        if stepping && p >= mode.duty && cycles > 0.0 {
            if foot.planted {
                foot.planted = false;
                foot.swing_from = foot.position;
            }
            let s = (p - mode.duty) / (1.0 - mode.duty);
            let mut to = homes[i] + velocity * (mode.duty / cycles) * 0.5;
            to.y = floor;
            foot.swing_to = to;
            let e = s * s * (3.0 - 2.0 * s);
            foot.position = foot.swing_from.lerp(to, e) + Vec3::Y * mode.lift * leg * (PI * s).sin();
        } else if !foot.planted {
            foot.planted = true;
            foot.position = foot.swing_to;
        }
    }

    // --- Body bob -------------------------------------------------------------------------------
    let bob = mode.bob * leg * (0.5 - 0.5 * (phase * 2.0 * TAU).cos()) * speed.min(1.0);
    for (i, b) in bones.iter().enumerate() {
        if b.parent.is_none() {
            local[i].translation.y -= bob;
        }
    }

    // --- Spine bends into turns -----------------------------------------------------------------
    if !a.spine.is_empty() {
        let bend = (-state.turn_rate * 0.12 / a.spine.len() as f32).clamp(-0.15, 0.15);
        for b in &a.spine {
            local[b.index()].rotation *= Quat::from_rotation_z(bend);
        }
    }

    // --- Springs: tails, ears ---------------------------------------------------------------------
    let accel = root.matrix3.inverse() * Vec3A::from((state.velocity - state.prev_velocity) / dt);
    let mut k = 0;
    for chain in &a.springs {
        let n = chain.bones.len() as f32;
        for (j, bone) in chain.bones.iter().enumerate() {
            let reach = (j + 1) as f32 / n;
            let target = Vec2::new(
                (-accel.z * 0.03).clamp(-0.4, 0.4) + (input.time * 1.7 + j as f32).sin() * 0.04 * reach,
                (state.turn_rate * 0.08 + accel.x * 0.02).clamp(-0.5, 0.5) * reach,
            );
            let s = &mut state.springs[k];
            let acc = (target - s.angle) * 60.0 * chain.stiffness - s.velocity * 12.0 * chain.damping;
            s.velocity += acc * dt;
            s.angle += s.velocity * dt;
            local[bone.index()].rotation *= Quat::from_rotation_x(s.angle.x) * Quat::from_rotation_z(s.angle.y);
            k += 1;
        }
    }

    // --- Leg IK ---------------------------------------------------------------------------------
    let world = forward_kinematics(bones, &local, root);
    for (i, l) in a.legs.iter().enumerate() {
        let chain = &l.chain;
        let last = *chain.last().expect("leg chain");
        let mut points: Vec<Vec3> = chain.iter().map(|b| world[b.index()].translation.into()).collect();
        points.push(world[last.index()].transform_point3(Vec3::Y * bones[last.index()].length));
        fabrik(&mut points, state.feet[i].position, 12, 1e-3);
        let mut parent = bones[chain[0].index()].parent.map_or(rotation(&root), |p| rotation(&world[p.index()]));
        for (j, b) in chain.iter().enumerate() {
            let current = parent * local[b.index()].rotation;
            let want = (points[j + 1] - points[j]).normalize_or(current * Vec3::Y);
            let new = Quat::from_rotation_arc(current * Vec3::Y, want) * current;
            local[b.index()].rotation = parent.inverse() * new;
            parent = new;
        }
    }

    // --- Loose cloth swings with the legs ---------------------------------------------------------
    // (`world` predates leg IK, but IK only turns leg bones, so the parents' frames still hold.)
    for f in &a.cloth_follow {
        let Some(leg) = a.legs.get(f.leg as usize) else { continue };
        let hip = leg.chain[0];
        let swing = local[hip.index()].rotation * bones[hip.index()].rest.rotation.inverse();
        let frame = |b: BoneId| bones[b.index()].parent.map_or(rotation(&root), |p| rotation(&world[p.index()]));
        let (leg_parent, cloth_parent) = (frame(hip), frame(f.root));
        let swing = cloth_parent.inverse() * leg_parent * swing * leg_parent.inverse() * cloth_parent;
        let r = &mut local[f.root.index()].rotation;
        *r = Quat::IDENTITY.slerp(swing, f.weight) * *r;
    }

    // --- Look-at --------------------------------------------------------------------------------
    if let Some(look) = &a.look {
        let head = look.bones.last().map(|(b, _)| *b).unwrap_or(BoneId(0));
        let hw = &world[head.index()];
        let goal = input.look_at.map_or(Quat::IDENTITY, |target| {
            let fwd = rotation(hw) * Vec3::Y;
            let want = (target - Vec3::from(hw.translation)).normalize_or(fwd);
            let full = Quat::from_rotation_arc(fwd, want);
            let (_, angle) = full.to_axis_angle();
            let limit = look.limit_deg.to_radians();
            if angle > limit { Quat::IDENTITY.slerp(full, limit / angle) } else { full }
        });
        state.look = state.look.slerp(goal, 1.0 - (-8.0 * dt).exp());
        let first = look.bones[0].0;
        let mut parent = bones[first.index()].parent.map_or(rotation(&root), |p| rotation(&world[p.index()]));
        for (b, share) in &look.bones {
            let current = parent * local[b.index()].rotation;
            let new = Quat::IDENTITY.slerp(state.look, *share) * current;
            local[b.index()].rotation = parent.inverse() * new;
            parent = new;
        }
    }
    local
}

/// Procedural pose for hero-tier mobs (those with joint entities).
#[allow(clippy::type_complexity)]
pub(super) fn animate(
    time: Res<Time>,
    bodies: Res<Assets<GeneratedBody>>,
    gaits: Res<Assets<Gait>>,
    defaults: Res<DefaultGaits>,
    mut mobs: Query<
        (&Transform, &MobBody, &MobSkeleton, &mut MotionState, &MobIntent, Option<&MobGait>, Option<&FootGround>, &mut FootProbes),
        Without<MobJoint>,
    >,
    mut joints: Query<(&MobJoint, &mut Transform), Without<MobBody>>,
) {
    let dt = time.delta_secs().clamp(1e-4, 0.1);
    for (tf, mob_body, skeleton, mut state, intent, gait, ground, mut probes) in &mut mobs {
        let Some(body) = bodies.get(&mob_body.0) else { continue };
        let a = &body.anatomy;
        if skeleton.0.len() != a.skeleton.len() {
            continue;
        }
        let root = tf.compute_affine();
        if state.body != Some(mob_body.0.id()) {
            state.body = Some(mob_body.0.id());
            reset_state(&mut state, a, root);
        }
        let leg = leg_length(a);
        probes.0.clear();
        probes.0.extend(a.legs.iter().map(|l| root.transform_point3(l.rest_foot) + Vec3::Y * leg));
        let scales: Vec<Vec3> = skeleton.0.iter().map(|e| joints.get(*e).map_or(Vec3::ONE, |(_, t)| t.scale)).collect();
        let input = PoseInput {
            anatomy: a,
            gait: gait_for(gait, &gaits, &defaults, a),
            root,
            ground_y: tf.translation.y,
            hits: ground.map_or(&[][..], |g| &g.0[..]),
            look_at: intent.look_at,
            scales: &scales,
            dt,
            time: time.elapsed_secs(),
        };
        let local = solve_pose(&input, &mut state);
        for (e, l) in skeleton.0.iter().zip(&local) {
            if let Ok((_, mut jt)) = joints.get_mut(*e) {
                jt.translation = l.translation;
                jt.rotation = l.rotation;
            }
        }
    }
}
