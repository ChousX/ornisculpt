//! Contact between a body's limbs, and between its limbs and the body.
//!
//! Limbs are meshed apart (an arm hanging by a thigh never webs onto it), so where a pose brings
//! one into another their surfaces simply pass through each other: an orc's forearms sink into
//! his thighs. [`ContactSet`] stands in for each limb and the body with a few rounded cones
//! (proxies) taken from the field's own primitives at rest and fitted to the real surface. The
//! hero vertex shader (`deform.wesl`, `COAT_CONTACT`) pushes skin that ends up inside another
//! group's proxies out to the middle of the contact, so the two surfaces press flat against each
//! other; [`ContactSet::contacts`] measures the same overlap on the CPU, for viability checks and
//! the editor.
//!
//! Groups: 0 is the body (every primitive not on a limb), then each limb meshed apart in part
//! order, up to [`CONTACT_GROUPS`] in all. A vertex is pushed out of the groups in its mask
//! ([`contact_masks`], packed in the high half of `ATTRIBUTE_MOB_DATA`'s second word): every group
//! but its own and those of the limbs that attach where it lies, since a limb's root passes into
//! the body there by design. A mask of 0 (claws, eyes, wing sheets, garments, crowd meshes) is
//! left alone.

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;

use super::body::{BodyField, BodyFieldOptions, Sheet, limb_of};
use super::{BodyMeshData, MobVertexData};
use crate::num::Idx;
use crate::anatomy::{Anatomy, AnatomyError, BodyPlan, BoneId, LimbRole, LodLevel, PartAssets, PartKind, PartLodRules, PrimLayer, PrimOp, PrimShape};

/// Groups a mask can name (16 bits): the body and up to 15 limbs. Limbs past them aren't tested.
pub const CONTACT_GROUPS: usize = 16;
/// Proxies kept, the largest first (the shader loops over all of them for every hero vertex).
pub const MAX_PROXIES: usize = 96;
/// Proxies thinner than this are left out (fingers, toes, tail tips).
const MIN_RADIUS: f32 = 0.008;
/// Depths shallower than this aren't reported (the fitted proxies are good to about that).
pub const REPORT_DEPTH: f32 = 0.008;

/// A rounded cone standing in for part of a group's surface, in the rest (bind) pose.
///
/// It runs from `a` (radius `r0`) to `b` (radius `r1`), its section an ellipse reaching
/// `radius / squash` along `cross` (a torso wider than it is deep).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactProxy {
    pub bone: BoneId,
    pub group: u8,
    pub a: Vec3,
    pub b: Vec3,
    pub r0: f32,
    pub r1: f32,
    /// Unit, perpendicular to the axis.
    pub cross: Vec3,
    /// How much nearer the surface lies along `cross` than across it (1 = round).
    pub squash: f32,
}

impl ContactProxy {
    /// Signed distance from `p` and the outward direction there, the proxy posed by `skin`
    /// (model-space rest → model-space posed). Mirrors `contact_distance` in `deform.wesl`.
    #[must_use]
    pub fn distance(&self, skin: &Affine3A, p: Vec3) -> (f32, Vec3) {
        let scale = Vec3::from(skin.matrix3.x_axis).length();
        let a = skin.transform_point3(self.a);
        let b = skin.transform_point3(self.b);
        let cross = skin.transform_vector3(self.cross).normalize_or(Vec3::X);
        round_cone(p, a, b, self.r0 * scale, self.r1 * scale, cross, self.squash)
    }
}

/// Distance from `p` to the rounded cone `a`–`b` with radii `r0`, `r1`, its section narrowed by
/// `squash` along `cross`, and the outward direction. Space is stretched by `squash` along
/// `cross`, which makes the section round, and the distance scaled back by the stretch along the
/// direction found.
#[allow(clippy::many_single_char_names)]
fn round_cone(p: Vec3, a: Vec3, b: Vec3, r0: f32, r1: f32, cross: Vec3, squash: f32) -> (f32, Vec3) {
    let k = squash.max(1e-3);
    let stretch = |v: Vec3| v + cross * ((k - 1.0) * v.dot(cross));
    let q = stretch(p - a);
    let ba = b - a;
    let l2 = ba.length_squared().max(1e-10);
    let t = (q.dot(ba) / l2).clamp(0.0, 1.0);
    let off = q - ba * t;
    let n = off.normalize_or(Vec3::Y);
    // The rounded cone's distance (Quilez), on the stretched point.
    let d = round_cone_sdf(q, ba, r0, r1);
    let along = n.dot(cross);
    let unstretch = (k - 1.0).mul_add(along * along, 1.0);
    // Back in real space the outward direction squeezes along `cross` again.
    let dir = (n + cross * ((k - 1.0) * along)).normalize_or(n);
    (d / unstretch, dir)
}

/// Quilez's rounded cone from the origin to `ba`, radii `r1` at the origin and `r2` at `ba`.
fn round_cone_sdf(p: Vec3, ba: Vec3, r1: f32, r2: f32) -> f32 {
    let l2 = ba.length_squared();
    if l2 < 1e-10 {
        return p.length() - r1.max(r2);
    }
    let rr = r1 - r2;
    let a2 = rr.mul_add(-rr, l2);
    let il2 = 1.0 / l2;
    let y = p.dot(ba);
    let z = y - l2;
    let x2 = (p * l2 - ba * y).length_squared();
    let y2 = y * y * l2;
    let z2 = z * z * l2;
    let k = rr.signum() * rr * rr * x2;
    if z.signum() * a2 * z2 > k {
        return (x2 + z2).sqrt().mul_add(il2, -r2);
    }
    if y.signum() * a2 * y2 < k {
        return (x2 + y2).sqrt().mul_add(il2, -r1);
    }
    y.mul_add(rr, (x2 * a2 * il2).sqrt()).mul_add(il2, -r1)
}

/// Where, in a pose, one group's surface sinks into another's.
#[derive(Clone, Debug, PartialEq)]
pub struct Contact {
    /// The sinking group's limb (a part index), `None` for the body.
    pub limb: Option<u16>,
    /// The group it sinks into.
    pub into: Option<u16>,
    /// How deep, at the deepest sample, metres.
    pub depth: f32,
    /// The deepest sample (posed, model space).
    pub at: Vec3,
    /// Samples inside.
    pub samples: usize,
}

/// One point on a proxy's surface at rest, for measuring contacts, and the groups it may sink
/// into (as a vertex mask).
#[derive(Clone, Copy, Debug)]
struct Sample {
    proxy: u16,
    rest: Vec3,
    mask: u16,
}

/// A body's contact proxies and groups.
#[derive(Clone, Debug, Default)]
pub struct ContactSet {
    pub proxies: Vec<ContactProxy>,
    /// The limb of every group past the body's (group `i + 1`), as part indices.
    pub limbs: Vec<u16>,
    samples: Vec<Sample>,
}

/// The limbs given contact groups, in part order (group `i + 1` is `limbs[i]`).
#[must_use]
pub fn contact_limbs(anatomy: &Anatomy) -> Vec<u16> {
    (0u16..).zip(&anatomy.parts).filter(|(_, p)| p.apart).map(|(i, _)| i).take(CONTACT_GROUPS - 1).collect()
}

/// The group of `part`: its limb's, or the body's (0). `None` for a limb past the groups.
fn group_of(limbs: &[u16], anatomy: &Anatomy, part: u16) -> Option<u8> {
    limb_of(anatomy, part).map_or(Some(0), |limb| limbs.iter().position(|l| *l == limb).and_then(|i| u8::try_from(i + 1).ok()))
}

/// The groups a point at `p` (rest pose, on `group`'s surface) may be pushed out of: all but its
/// own, and but the body and any limb attaching at `p`.
fn mask_at(field: &BodyField, limbs: &[u16], group: u8, p: Vec3A) -> u16 {
    let all = u16::MAX >> (16 - (limbs.len() + 1).min(CONTACT_GROUPS));
    let mut out = all & !(1 << group);
    for (i, limb) in limbs.iter().enumerate() {
        if field.attach_weight(*limb, p) > 0.0 {
            out &= !(1 << (i + 1));
            out &= !1;
        }
    }
    out
}

/// A rounded cone fitted round a primitive, in the field's model space: `(a, b, r0, r1, cross,
/// squash)`.
fn proxy_shape(to_local: &Affine3A, shape: &PrimShape) -> Option<(Vec3, Vec3, f32, f32, Vec3, f32)> {
    let model = to_local.inverse();
    let column = |axis: Vec3| Vec3::from(model.matrix3 * Vec3A::from(axis));
    match shape {
        PrimShape::RoundCone { r0, r1, len } => {
            let (sx, sz) = (column(Vec3::X), column(Vec3::Z));
            let (wide, narrow) = if sx.length() >= sz.length() { (sx, sz) } else { (sz, sx) };
            let a = model.transform_point3(Vec3::ZERO);
            let b = model.transform_point3(Vec3::Y * *len);
            let scale = wide.length();
            Some((a, b, r0 * scale, r1 * scale, narrow.normalize_or(Vec3::Z), scale / narrow.length().max(1e-6)))
        }
        PrimShape::Sphere { r } => {
            let c = model.transform_point3(Vec3::ZERO);
            let scale = column(Vec3::X).length();
            Some((c, c, r * scale, r * scale, Vec3::X, 1.0))
        }
        PrimShape::Ellipsoid { radii } => capsule(&model, *radii),
        PrimShape::RoundBox { half, round } => capsule(&model, *half + Vec3::splat(*round)),
        PrimShape::Grid { .. } => None,
    }
}

/// A capsule along the longest of the half extents `h` (local axes), as wide as the middle one and
/// squashed to the shortest.
fn capsule(model: &Affine3A, h: Vec3) -> Option<(Vec3, Vec3, f32, f32, Vec3, f32)> {
    let axes = [Vec3::X, Vec3::Y, Vec3::Z];
    let mut sized: Vec<(Vec3, f32)> = axes.iter().zip(h.to_array()).map(|(ax, e)| (Vec3::from(model.matrix3 * Vec3A::from(*ax)) * e, e)).collect();
    sized.sort_by(|a, b| b.0.length().total_cmp(&a.0.length()));
    let [(long, _), (mid, _), (short, _)] = [*sized.first()?, *sized.get(1)?, *sized.get(2)?];
    let c = model.transform_point3(Vec3::ZERO);
    let r = mid.length();
    let reach = (long.length() - r).max(0.0);
    let dir = long.normalize_or(Vec3::Y);
    Some((c - dir * reach, c + dir * reach, r, r, short.normalize_or(Vec3::Z), r / short.length().max(1e-6)))
}

/// Points round a proxy's surface: `rings` rings of `around` points between its ends.
fn surface_points(proxy: &ContactProxy, rings: u8, around: u8) -> Vec<Vec3> {
    let axis = (proxy.b - proxy.a).normalize_or(Vec3::Y);
    let wide = proxy.cross.cross(axis).normalize_or(Vec3::X);
    let mut out = Vec::new();
    for ring in 0..rings {
        let t = (f32::from(ring) + 0.5) / f32::from(rings);
        let centre = proxy.a.lerp(proxy.b, t);
        let radius = (proxy.r1 - proxy.r0).mul_add(t, proxy.r0);
        for step in 0..around {
            let angle = f32::from(step) / f32::from(around) * std::f32::consts::TAU;
            out.push(centre + wide * (angle.cos() * radius) + proxy.cross * (angle.sin() * radius / proxy.squash.max(1e-3)));
        }
    }
    out
}

impl ContactSet {
    /// The proxies of `anatomy`, fitted to its hero surface at rest.
    #[must_use]
    pub fn new(anatomy: &Anatomy) -> Self {
        let opts = BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false };
        let field = BodyField::new(anatomy, &opts);
        let limbs = contact_limbs(anatomy);
        let mut proxies: Vec<ContactProxy> = field
            .prims
            .iter()
            .filter(|p| p.op == PrimOp::Union && p.layer == PrimLayer::Base)
            .filter_map(|p| {
                let group = group_of(&limbs, anatomy, p.part)?;
                let (a, b, r0, r1, cross, squash) = proxy_shape(&p.to_local, &p.shape)?;
                (r0.max(r1) >= MIN_RADIUS).then_some(ContactProxy { bone: p.bone, group, a, b, r0, r1, cross, squash })
            })
            .collect();
        // The largest first: they decide most contacts, and the shader's loop is capped.
        let size = |p: &ContactProxy| p.r0.max(p.r1) * (p.b - p.a).length().max(p.r0.max(p.r1));
        proxies.sort_by(|x, y| size(y).total_cmp(&size(x)));
        proxies.truncate(MAX_PROXIES);
        // Fit each to its own group's surface: the median distance from points on the proxy to
        // the sheet (blends, muscles and fat stand out of the bare cones).
        let view = field.view(field.bounds);
        for p in &mut proxies {
            let sheet = match p.group {
                0 => Sheet::Body,
                g => limbs.get(usize::from(g) - 1).map_or(Sheet::Body, |l| Sheet::Limb(*l)),
            };
            let mut d: Vec<f32> = surface_points(p, 3, 8).iter().map(|q| view.sheet_distance(sheet, Vec3A::from(*q))).collect();
            d.sort_by(f32::total_cmp);
            let median = d.get(d.len() / 2).copied().unwrap_or(0.0);
            let r = p.r0.max(p.r1);
            let grow = (-median).clamp(-0.4 * r, 0.6 * r);
            p.r0 = (p.r0 + grow).max(0.0);
            p.r1 = (p.r1 + grow).max(0.0);
        }
        let samples = (0u16..)
            .zip(&proxies)
            .flat_map(|(i, p)| surface_points(p, 4, 10).into_iter().map(move |q| (i, p.group, q)))
            .map(|(proxy, group, rest)| Sample { proxy, rest, mask: mask_at(&field, &limbs, group, Vec3A::from(rest)) })
            .collect();
        Self { proxies, limbs, samples }
    }

    /// The limb of group `g` (`None` for the body).
    #[must_use]
    pub fn limb(&self, g: u8) -> Option<u16> {
        if g == 0 { None } else { self.limbs.get(usize::from(g) - 1).copied() }
    }

    /// Skin matrices (rest model → posed model) for bone model transforms `model`.
    fn skins(anatomy: &Anatomy, model: &[Affine3A]) -> Vec<Affine3A> {
        model
            .iter()
            .zip(&anatomy.skeleton.inverse_bind)
            .map(|(m, inv)| *m * Affine3A::from_mat4(*inv))
            .collect()
    }

    /// The deepest point `p` (posed) sinks into the groups in `mask`, and the way out, under the
    /// skin matrices `skins`. Mirrors the shader.
    #[must_use]
    pub fn sink(&self, skins: &[Affine3A], mask: u16, p: Vec3) -> Option<(f32, Vec3, u8)> {
        let mut best: Option<(f32, Vec3, u8)> = None;
        for proxy in self.proxies.iter().filter(|q| mask & (1 << q.group) != 0) {
            let Some(skin) = skins.get(proxy.bone.index()) else { continue };
            // Far from this one: skip the rest (as the shader does).
            let (start, end) = (skin.transform_point3(proxy.a), skin.transform_point3(proxy.b));
            let reach = proxy.r0.max(proxy.r1) * Vec3::from(skin.matrix3.x_axis).length();
            if p.distance((start + end) * 0.5) > start.distance(end).mul_add(0.5, reach) {
                continue;
            }
            let (depth, out) = proxy.distance(skin, p);
            if depth < best.map_or(0.0, |b| b.0) {
                best = Some((depth, out, proxy.group));
            }
        }
        best
    }

    /// Where, in the local pose `pose` (one transform per bone), each group sinks into another,
    /// deepest first; shallower than [`REPORT_DEPTH`] left out.
    #[must_use]
    pub fn contacts(&self, anatomy: &Anatomy, pose: &[Transform]) -> Vec<Contact> {
        let skins = Self::skins(anatomy, &anatomy.skeleton.model_from(pose));
        let mut found: Vec<((u8, u8), Contact)> = Vec::new();
        for s in &self.samples {
            let Some(proxy) = self.proxies.get(usize::from(s.proxy)) else { continue };
            let Some(skin) = skins.get(proxy.bone.index()) else { continue };
            let at = skin.transform_point3(s.rest);
            let Some((d, _, into)) = self.sink(&skins, s.mask, at) else { continue };
            if -d < REPORT_DEPTH {
                continue;
            }
            let key = (proxy.group, into);
            match found.iter_mut().find(|(k, _)| *k == key) {
                Some((_, c)) => {
                    c.samples += 1;
                    if -d > c.depth {
                        c.depth = -d;
                        c.at = at;
                    }
                }
                None => found.push((key, Contact { limb: self.limb(proxy.group), into: self.limb(into), depth: -d, at, samples: 1 })),
            }
        }
        let mut out: Vec<Contact> = found.into_iter().map(|(_, c)| c).collect();
        out.sort_by(|a, b| b.depth.total_cmp(&a.depth));
        out
    }

    /// The proxies as the shader reads them (one dummy, far away, when there are none: a storage
    /// buffer can't be empty).
    #[must_use]
    pub fn gpu(&self) -> Vec<ContactProxyGpu> {
        if self.proxies.is_empty() {
            return vec![ContactProxyGpu { a: [1e6, 1e6, 1e6, 0.0], b: [1e6, 1e6, 1e6, 0.0], cross: [1.0, 0.0, 0.0, 1.0], ids: [0, 31, 0, 0] }];
        }
        self.proxies
            .iter()
            .map(|p| ContactProxyGpu {
                a: p.a.extend(p.r0).to_array(),
                b: p.b.extend(p.r1).to_array(),
                cross: p.cross.extend(p.squash).to_array(),
                ids: [u32::from(p.bone.0), u32::from(p.group), 0, 0],
            })
            .collect()
    }
}

impl ContactSet {
    /// A group's name for messages: its limb's path, or "the body".
    #[must_use]
    pub fn name(&self, anatomy: &Anatomy, limb: Option<u16>) -> String {
        limb.and_then(|l| anatomy.parts.get(l.idx())).map_or_else(|| "the body".to_owned(), |p| p.path.to_string())
    }
}

/// Contacts [`clear_plan`] leaves: shallower than this, metres (the skin presses them flat).
pub const CLEAR_DEPTH: f32 = 0.012;
/// Degrees [`clear_plan`] tries turning a limb by at each step (either way for a swing, outward
/// for a splay), and steps per limb.
const CLEAR_TURNS: [f32; 3] = [4.0, 8.0, 16.0];
const CLEAR_STEPS: u8 = 12;

/// One change [`clear_plan`] made to a limb, in degrees.
#[derive(Clone, Debug, PartialEq)]
pub struct ClearEdit {
    /// The limb's node in the plan (mirrored copies share it), e.g. `torso/arm`.
    pub path: String,
    /// `splay_deg` (outward from the body) or `swing` (its joints' fore-and-aft swing).
    pub field: &'static str,
    pub from: f32,
    pub to: f32,
}

/// What [`clear_plan`] did.
#[derive(Clone, Debug)]
pub struct Cleared {
    pub plan: BodyPlan,
    pub edits: Vec<ClearEdit>,
    /// The deepest contact in the idle pose before and after, metres.
    pub before: f32,
    pub after: f32,
}

/// The plan path of a built part: mirror and repeat suffixes dropped from every segment.
fn plan_path(built: &str) -> String {
    built
        .split('/')
        .map(|seg| {
            let seg = seg.strip_suffix(".l").or_else(|| seg.strip_suffix(".r")).unwrap_or(seg);
            match seg.rsplit_once('.') {
                Some((base, n)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => base,
                _ => seg,
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Sets a limb node's `field` (see [`ClearEdit::field`]) to `value`; its value before.
fn set_limb(plan: &mut BodyPlan, path: &str, field: &str, value: Option<f32>) -> Option<f32> {
    let mut before = None;
    plan.visit_mut(|p, n| {
        if p.to_string() != path {
            return;
        }
        if let PartKind::Limb(l) = &mut n.kind {
            let slot = match field {
                "splay_deg" => Some(&mut l.splay_deg),
                _ => l.joints.as_mut().map(|j| &mut j.swing),
            };
            if let Some(slot) = slot {
                before = Some(*slot);
                if let Some(v) = value {
                    *slot = v;
                }
            }
        }
    });
    before
}

/// Turns limbs that sink deep into another limb or the body in the idle pose out of it.
///
/// Deeper than [`CLEAR_DEPTH`], each limb, the deepest first, turns a few degrees at a time by its
/// `splay_deg` or its joints' `swing`, whichever helps most, until it clears or stops helping. An
/// arm moves rather than the leg it sinks into, and a leg only for the body or another leg.
///
/// # Errors
///
/// When the plan doesn't build.
pub fn clear_plan(plan: &BodyPlan, rules: &PartLodRules, assets: &PartAssets) -> Result<Cleared, AnatomyError> {
    let measure = |plan: &BodyPlan| -> Result<(Anatomy, Vec<Contact>), AnatomyError> {
        let anatomy = Anatomy::from_plan(plan, rules, assets)?;
        let contacts = ContactSet::new(&anatomy).contacts(&anatomy, &anatomy.idle_pose());
        Ok((anatomy, contacts))
    };
    let deepest = |contacts: &[Contact]| contacts.first().map_or(0.0, |c| c.depth);
    let (mut anatomy, mut contacts) = measure(plan)?;
    let before = deepest(&contacts);
    let mut plan = plan.clone();
    let mut edits: Vec<ClearEdit> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    loop {
        let locomotor = |a: &Anatomy, l: u16| a.parts.get(l.idx()).is_some_and(|p| p.role == Some(LimbRole::Locomotor));
        // The deepest contact left with a limb to move: an arm before the leg it lies in.
        let mover = contacts.iter().filter(|c| c.depth > CLEAR_DEPTH).find_map(|c| {
            let limbs: Vec<u16> = [c.limb, c.into].into_iter().flatten().collect();
            // A leg moves only for the body or another leg.
            let free = limbs.iter().any(|l| !locomotor(&anatomy, *l));
            let options = limbs.into_iter().filter(|l| !free || !locomotor(&anatomy, *l));
            options.map(|l| (l, anatomy.parts.get(l.idx()).map(|p| plan_path(&p.path.to_string())))).find_map(|(l, path)| path.filter(|p| !done.contains(p)).map(|p| (l, p)))
        });
        let Some((limb, path)) = mover else { break };
        done.push(path.clone());
        // How deep this limb is in anything, or anything in it.
        let depth_of = |a: &Anatomy, cs: &[Contact]| {
            let name = a.parts.get(limb.idx()).map(|p| plan_path(&p.path.to_string()));
            cs.iter()
                .filter(|c| [c.limb, c.into].into_iter().flatten().any(|l| a.parts.get(l.idx()).map(|p| plan_path(&p.path.to_string())) == name))
                .map(|c| c.depth)
                .fold(0.0, f32::max)
        };
        let mut current = depth_of(&anatomy, &contacts);
        for _ in 0..CLEAR_STEPS {
            if current <= CLEAR_DEPTH {
                break;
            }
            let mut best: Option<(f32, BodyPlan, &'static str, f32, f32)> = None;
            let tries = CLEAR_TURNS.iter().flat_map(|t| [("splay_deg", *t), ("swing", *t), ("swing", -*t)]);
            for (field, step) in tries {
                let mut trial = plan.clone();
                let Some(was) = set_limb(&mut trial, &path, field, None) else { continue };
                set_limb(&mut trial, &path, field, Some(was + step));
                let Ok((a, cs)) = measure(&trial) else { continue };
                let d = depth_of(&a, &cs);
                if d < best.as_ref().map_or(current, |b| b.0) {
                    best = Some((d, trial, field, was, was + step));
                }
            }
            let Some((d, trial, field, was, now)) = best else { break };
            plan = trial;
            current = d;
            match edits.iter_mut().find(|e| e.path == path && e.field == field) {
                Some(e) => e.to = now,
                None => edits.push(ClearEdit { path: path.clone(), field, from: was, to: now }),
            }
        }
        (anatomy, contacts) = measure(&plan)?;
    }
    Ok(Cleared { plan, edits, before, after: deepest(&contacts) })
}

/// [`ContactProxy`] as `deform.wesl` reads it: `a` and `b` with their radii in w, `cross` with the
/// squash, then the bone and group.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ContactProxyGpu {
    pub a: [f32; 4],
    pub b: [f32; 4],
    pub cross: [f32; 4],
    pub ids: [u32; 4],
}

/// Writes each skin vertex's contact mask (the groups it may be pushed out of).
///
/// The mask goes in the high half of the packed vertex data's second word. `field` is the one the
/// mesh was built from, its vertices still in that field's pose (attachment zones move with it).
pub fn contact_masks(body: &mut BodyMeshData, field: &BodyField, anatomy: &Anatomy) {
    let limbs = contact_limbs(anatomy);
    let skin = body.skin_vertex_mask();
    let positions = body.positions.clone();
    for (i, data) in body.mob_data.iter_mut().enumerate() {
        if !skin.get(i).copied().unwrap_or(false) {
            continue;
        }
        let (Some(p), Some(bone)) = (positions.get(i), anatomy.skeleton.get(MobVertexData::unpack(*data).primary_bone)) else { continue };
        let Some(group) = group_of(&limbs, anatomy, bone.part) else { continue };
        let mask = mask_at(field, &limbs, group, Vec3A::from(*p));
        data[1] = (data[1] & 0xffff) | (u32::from(mask) << 16);
    }
}

/// The contact mask packed in vertex data (see [`contact_masks`]).
#[must_use]
pub const fn contact_mask(data: [u32; 2]) -> u16 {
    #[allow(clippy::cast_possible_truncation, clippy::as_conversions)]
    let m = (data[1] >> 16) as u16;
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orc's plan as it was before its arms were cleared (hanging inside his thighs), without
    /// his sculpted head (its SDF takes minutes in a test build).
    fn orc_plan(edit: impl Fn(&str) -> String) -> BodyPlan {
        let src = include_str!("../../assets/mobs/orc.body.ron").replace("plan: Arm, swing: -8.0", "plan: Arm, swing: 8.0");
        let mut plan: BodyPlan = ron::from_str(&edit(&src)).unwrap();
        plan.visit_mut(|_, n| {
            if let PartKind::Head(h) = &mut n.kind {
                h.sculpt = None;
            }
        });
        plan
    }

    fn orc(edit: impl Fn(&str) -> String) -> Anatomy {
        Anatomy::from_plan(&orc_plan(edit), &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    /// A rounded cone's distance: on its axis, off its side, past its ends, squashed.
    #[test]
    fn round_cone_distances() {
        let (a, b) = (Vec3::ZERO, Vec3::Y);
        let d = |p: Vec3| round_cone(p, a, b, 0.2, 0.1, Vec3::Z, 1.0).0;
        assert!((d(Vec3::new(0.5, 0.0, 0.0)) - 0.3).abs() < 1e-3, "{}", d(Vec3::new(0.5, 0.0, 0.0)));
        assert!((d(Vec3::new(0.0, 1.5, 0.0)) - 0.4).abs() < 1e-3);
        assert!((d(Vec3::new(0.0, 0.5, 0.0)) + 0.15).abs() < 0.01, "{}", d(Vec3::new(0.0, 0.5, 0.0)));
        // Squashed twice along Z: the surface is half as far that way.
        let s = |p: Vec3| round_cone(p, a, b, 0.2, 0.2, Vec3::Z, 2.0).0;
        assert!(s(Vec3::new(0.0, 0.5, 0.12)) > 0.0 && s(Vec3::new(0.18, 0.5, 0.0)) < 0.0);
    }

    /// The orc stands with his forearms in his thighs: the check finds the arms sinking into the
    /// legs, and nothing sinking into the limb it attaches to.
    #[test]
    fn the_orc_sinks_his_hands_into_his_thighs() {
        let anatomy = orc(str::to_owned);
        let set = ContactSet::new(&anatomy);
        assert!(!set.proxies.is_empty() && set.proxies.len() <= MAX_PROXIES);
        let contacts = set.contacts(&anatomy, &anatomy.idle_pose());
        let name = |l: Option<u16>| set.name(&anatomy, l);
        let found: Vec<String> = contacts.iter().map(|c| format!("{} into {} {:.3}", name(c.limb), name(c.into), c.depth)).collect();
        println!("{found:#?}");
        assert!(
            contacts.iter().any(|c| name(c.limb).contains("arm") && name(c.into).contains("leg") || name(c.limb).contains("leg") && name(c.into).contains("arm")),
            "{found:?}"
        );
    }

    /// The hero mesh carries masks: limb skin may be pushed out of the body and the other limbs,
    /// claws and eyes out of nothing.
    #[test]
    fn hero_skin_carries_contact_masks() {
        use crate::surface::{AdaptiveContour, SurfaceSettings, mesh_lod};
        let anatomy = orc(str::to_owned);
        let settings = SurfaceSettings::default();
        let body = mesh_lod(&AdaptiveContour, &anatomy, LodLevel::Hero, settings.get(LodLevel::Hero), 0).unwrap();
        let skin = body.skin_vertex_mask();
        let masked = body.mob_data.iter().zip(&skin).filter(|(d, s)| **s && contact_mask(**d) != 0).count();
        let skin_count = skin.iter().filter(|s| **s).count();
        let off_skin = body.mob_data.iter().zip(&skin).filter(|(d, s)| !**s && contact_mask(**d) != 0).count();
        println!("{masked} of {skin_count} skin vertices masked, {off_skin} off the skin");
        assert!(masked > skin_count / 2, "{masked} of {skin_count}");
        assert_eq!(off_skin, 0);
        // Posed at idle, skinned as the GPU does (linearly here), many forearm vertices sink.
        let set = ContactSet::new(&anatomy);
        let skins = ContactSet::skins(&anatomy, &anatomy.skeleton.model_from(&anatomy.idle_pose()));
        let sunk = (0..body.positions.len())
            .filter(|&i| {
                let (Some(p), Some(j), Some(w), Some(d)) = (body.positions.get(i), body.joints.get(i), body.weights.get(i), body.mob_data.get(i)) else { return false };
                let posed: Vec3 = j.iter().zip(w).filter_map(|(j, w)| skins.get(usize::from(*j)).map(|m| m.transform_point3(*p) * *w)).sum();
                set.sink(&skins, contact_mask(*d), posed).is_some_and(|(d, _, _)| d < -0.01)
            })
            .count();
        println!("{sunk} vertices sink over 1 cm at idle");
        assert!(sunk > 100, "{sunk}");
    }

    /// Cleared, the orc's arms splay out of his thighs and nothing is left deep.
    #[test]
    fn clearing_turns_the_orcs_arms_out_of_his_thighs() {
        let plan = orc_plan(str::to_owned);
        let cleared = clear_plan(&plan, &PartLodRules::default(), &PartAssets::default()).unwrap();
        println!("{:.3} -> {:.3}: {:?}", cleared.before, cleared.after, cleared.edits);
        assert!(cleared.before > 0.05 && cleared.after < cleared.before * 0.5, "{} -> {}", cleared.before, cleared.after);
        assert!(cleared.edits.iter().any(|e| e.path == "torso/arm"), "{:?}", cleared.edits);
        assert!(!cleared.edits.iter().any(|e| e.path == "torso/leg"), "the legs stay: {:?}", cleared.edits);
    }

    /// Spread apart, the same orc's arms clear his thighs.
    #[test]
    fn arms_held_out_clear_the_thighs() {
        let anatomy = orc(|s| s.replace("splay_deg: 12.0", "splay_deg: 40.0"));
        let set = ContactSet::new(&anatomy);
        let contacts = set.contacts(&anatomy, &anatomy.idle_pose());
        let name = |l: Option<u16>| set.name(&anatomy, l);
        assert!(
            !contacts.iter().any(|c| name(c.limb).contains("arm") && name(c.into).contains("leg")),
            "{:?}",
            contacts.iter().map(|c| format!("{} into {} {:.3}", name(c.limb), name(c.into), c.depth)).collect::<Vec<_>>()
        );
    }
}
