//! Test helpers: sample bodies, and walking them through every pose stage.

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;

use super::Gait;
use super::stages::{Frame, Sim, leg_length};
use crate::anatomy::{Anatomy, BodyPlan, Form, PartAssets, PartLodRules, apply_form, build_anatomy};
use crate::num::ToF32;

/// The body a `*.body.ron` source builds.
pub fn sample_body(src: &str) -> Anatomy {
    edited_body(src, |_| {})
}

/// The body a `*.body.ron` source builds after `edit` changes its plan.
pub fn edited_body(src: &str, edit: impl FnOnce(&mut BodyPlan)) -> Anatomy {
    let mut plan: BodyPlan = ron::from_str(src).unwrap();
    edit(&mut plan);
    let live = apply_form(&plan, &Form::default()).unwrap();
    build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
}

/// Walks `a` straight ahead at 60 fps, `frames` frames at each speed (leg lengths per second),
/// calling `each(sim, local pose, root, frame)` after every frame.
pub fn walk(a: &Anatomy, speeds: &[f32], frames: usize, mut each: impl FnMut(&Sim, &[Transform], Affine3A, usize)) {
    let gait = Gait::for_legs(a.legs.len());
    let leg = leg_length(a);
    let mut root = Affine3A::IDENTITY;
    let mut sim = Sim::new(a, root);
    let dt = 1.0 / 60.0;
    for &speed in speeds {
        sim.loco.velocity = Vec3::Z * speed * leg;
        sim.loco.prev_velocity = sim.loco.velocity;
        for i in 0..frames {
            root.translation += Vec3A::from(sim.loco.velocity * dt);
            let f = Frame { a, gait: &gait, root, dt, time: i.to_f32() * dt, leg };
            let local = sim.frame(&f, None).to_vec();
            each(&sim, &local, root, i);
        }
    }
}
