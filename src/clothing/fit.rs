//! Wrapping garments onto a built body.

use std::f32::consts::TAU;

use bevy::math::Vec3A;
use bevy::prelude::*;
use smallvec::SmallVec;

use super::{Garment, Hang, MAX_GARMENTS, Outfit};
use crate::num::{Clamp, Idx, Saturate, ToF32};
use crate::anatomy::{
    Anatomy, AxisSample, BoneDef, BoneId, BuiltPart, ClothFollow, LodLevel, MAX_BONES, PartAssets, PartPath, PrimLayer,
    RegionId, SpringChainDef, TriMesh, frame_y_z, mirror_point, path_matches, relative, sample_axis,
};
use crate::surface::{
    BodyField, BodyFieldOptions, BodyMeshData, Field, Influences, MaterialSlot, MobVertexData, SurfaceSettings, Submesh,
    remap_to_crowd, simplify,
};

/// Where a wrap-space direction meets the body on one part.
struct SurfacePoint {
    /// Stored model space.
    position: Vec3,
    /// Outward surface normal, stored model space.
    normal: Vec3,
    info: Influences,
    region: RegionId,
    layer: PrimLayer,
}

/// One garment on one part.
struct Piece<'a> {
    part: &'a BuiltPart,
    garment: &'a Garment,
    palette: u8,
}

impl Piece<'_> {
    fn stored(&self, p: Vec3) -> Vec3 {
        if self.part.mirrored { mirror_point(p) } else { p }
    }

    fn span_t(&self, y: f32) -> f32 {
        let (a, b) = self.garment.span;
        f32::mul_add(b - a, y.clamp(0.0, 1.0), a)
    }

    /// Marches from the part's axis toward `theta` to where the body ends. `y` beyond 0..1 on a
    /// garment without [`Hang`] wraps over the part's end cap instead (`|y| = 1` reaches the tip).
    /// The garment hugs the whole body (blend bulges included) but never sits further out than
    /// its own part's surface plus a bulge allowance: a torso ray running on into an arm root
    /// stops near the torso, and the arm emerges through the garment as through an armhole.
    fn surface(&self, field: &BodyField, y: f32, theta: f32, cap: bool) -> Option<SurfacePoint> {
        let sample = sample_axis(&self.part.axis, self.span_t(y))?;
        let rotation = sample.model.rotation;
        let around = (rotation * Vec3::NEG_Z * theta.cos() + rotation * Vec3::X * theta.sin()).normalize();
        let axis = rotation * Vec3::Y;
        let over = if !cap { 0.0 } else if y < 0.0 { (-y).min(1.0) } else { (y - 1.0).clamp(0.0, 1.0) };
        let tip = if y < 0.0 { -axis } else { axis };
        let phi = over * std::f32::consts::FRAC_PI_2;
        let dir = (around * phi.cos() + tip * phi.sin()).normalize();
        let origin = sample.centre();
        let at = |dist: f32| Vec3A::from(self.stored(origin + dir * dist));
        let estimate = sample.radius_toward(theta);
        let limit = f32::mul_add(estimate, 3.0, 0.05);
        let own = self.part.primitives.start.idx()..self.part.primitives.end.idx();
        let d_full = exit(|dist| field.distance(at(dist)), limit);
        let d_own = exit(|dist| field.distance_among(own.clone(), at(dist)), limit);
        let allowance = f32::mul_add(estimate, self.garment.reach, 0.01);
        let (dist, own_only) = match (d_full, d_own) {
            (Some(full), Some(mine)) if full > mine + allowance => (mine + allowance, true),
            (Some(full), _) => (full, false),
            (None, Some(mine)) => (mine, true),
            (None, None) => (estimate, false),
        };
        let p = at(dist);
        let normal = if own_only {
            let grad = |o: Vec3A| field.distance_among(own.clone(), p + o) - field.distance_among(own.clone(), p - o);
            Vec3A::new(grad(Vec3A::X * 1e-3), grad(Vec3A::Y * 1e-3), grad(Vec3A::Z * 1e-3))
        } else {
            field.gradient(p)
        };
        let normal = normal.normalize_or(Vec3A::from(self.stored(dir) - self.stored(Vec3::ZERO)));
        let info = if own_only { field.surface_info_among(own, p) } else { field.surface_info(p) };
        Some(SurfacePoint { position: p.into(), normal: normal.into(), info: info.influences, region: info.region, layer: info.layer })
    }
}

/// Distance along a ray (parametrised by `distance(d)`) from inside a body to where it exits;
/// `None` if the ray starts outside or never exits within `limit`.
fn exit(distance: impl Fn(f32) -> f32, limit: f32) -> Option<f32> {
    if distance(0.0) >= 0.0 {
        return None;
    }
    let (mut lo, mut d) = (0.0, 0.0);
    loop {
        let v = distance(d);
        if v >= 0.0 {
            break;
        }
        lo = d;
        d += (-v).max(0.002);
        if d > limit {
            return None;
        }
    }
    let mut hi = d;
    for _ in 0..8 {
        let mid = f32::midpoint(lo, hi);
        if distance(mid) < 0.0 { lo = mid } else { hi = mid }
    }
    Some(hi)
}

/// A hanging cloth chain.
struct Chain {
    theta: f32,
    bones: SmallVec<[BoneId; 4]>,
    /// Rest position of the chain root, stored model space.
    root: Vec3,
}

/// Wraps `outfit` onto `anatomy`: adds loose-cloth bones (springs, leg following) to the
/// skeleton and returns the garments as one hero-detail cloth mesh in the dressed skeleton.
pub fn fit_outfit(mut anatomy: Anatomy, outfit: &Outfit, assets: &PartAssets) -> (Anatomy, Option<BodyMeshData>) {
    let field = BodyField::new(&anatomy, &BodyFieldOptions { level: LodLevel::Hero, seed: 0, geometric_folds: false, spread: false });
    let mut out = BodyMeshData::default();
    let mut cloth_bones: Vec<BoneId> = Vec::new();
    let mut new_bones: Vec<BoneDef> = Vec::new();
    let mut springs: Vec<SpringChainDef> = Vec::new();
    let mut follows: Vec<ClothFollow> = Vec::new();
    let model_rest = anatomy.skeleton.rest_model();

    for (gi, garment) in outfit.garments.iter().enumerate() {
        let Some(mesh) = assets.meshes.get(&garment.mesh) else {
            warn!("garment {}: mesh {} not loaded", garment.name, garment.mesh);
            continue;
        };
        let pattern = PartPath(garment.on.clone());
        for part in anatomy.parts.iter().filter(|p| !p.axis.is_empty() && path_matches(&pattern, &p.path)) {
            let piece = Piece { part, garment, palette: gi.min(MAX_GARMENTS - 1).clamp_u8() };
            let chains = garment.hang.and_then(|hang| {
                let rig = hang_rig(&piece, &field, mesh, &hang, &model_rest, &anatomy, &mut new_bones)?;
                for c in &rig {
                    cloth_bones.extend(c.bones.iter().copied());
                    springs.push(SpringChainDef { bones: c.bones.iter().copied().collect(), stiffness: hang.stiffness, damping: hang.damping, gravity: 1.0 });
                }
                if hang.follow_legs > 0.0 {
                    follows.extend(follow_legs(&rig, &anatomy, &model_rest, hang.follow_legs));
                }
                Some(rig)
            });
            wrap_piece(&piece, &field, mesh, chains.as_deref(), &mut out);
        }
    }
    anatomy.skeleton.bones.extend(new_bones);
    anatomy.springs.extend(springs);
    anatomy.cloth_follow.extend(follows);
    if !cloth_bones.is_empty() {
        anatomy.finish_skeleton(&cloth_bones);
    }
    if out.indices.is_empty() {
        return (anatomy, None);
    }
    out.submeshes.push(Submesh { indices: 0..out.indices.len().clamp_u32(), slot: MaterialSlot::Cloth });
    (anatomy, Some(out))
}

/// Hanging edge of a piece: which end of the span, and the drop direction there.
fn hang_edge(piece: &Piece, start: bool) -> Option<(AxisSample, Vec3)> {
    let t = piece.span_t(if start { 0.0 } else { 1.0 });
    let s = sample_axis(&piece.part.axis, t)?;
    let along = s.model.rotation * Vec3::Y * if start { -1.0 } else { 1.0 };
    Some((s, along))
}

fn drop_dir(piece: &Piece, along_logical: Vec3, normal: Vec3, hang: &Hang) -> (Vec3, Vec3) {
    let along = (piece.stored(along_logical) - piece.stored(Vec3::ZERO)).normalize_or(Vec3::NEG_Y);
    let dir = along.lerp(Vec3::NEG_Y, hang.droop.clamp(0.0, 1.0)).normalize_or(Vec3::NEG_Y);
    let radial = (normal - dir * normal.dot(dir)).normalize_or(Vec3::ZERO);
    (dir, radial)
}

fn theta_of(p: Vec3) -> f32 {
    p.x.atan2(-p.z)
}

/// Chain angles: evenly around a full ring, or across the hanging vertices' angular range.
fn chain_angles(mesh: &TriMesh, count: u8) -> Vec<f32> {
    let n = usize::from(count.max(1)).to_f32();
    let mut angles: Vec<f32> = mesh.positions.iter().filter(|p| !(0.0..=1.0).contains(&p.y)).map(|p| theta_of(*p)).collect();
    angles.sort_by(f32::total_cmp);
    let (Some(&lo), Some(&hi)) = (angles.first(), angles.last()) else { return Vec::new() };
    let biggest_gap = angles.array_windows().map(|[a, b]| b - a).fold(TAU - (hi - lo), f32::max);
    let steps = 0..usize::from(count.max(1));
    if biggest_gap < 0.6 {
        return steps.map(|k| k.to_f32() / n * TAU).collect();
    }
    steps.map(|k| lo + (hi - lo) * (k.to_f32() + 0.5) / n).collect()
}

/// Adds the spring-bone chains a hanging piece needs (appended to `new_bones`).
#[allow(clippy::too_many_arguments)]
fn hang_rig(
    piece: &Piece,
    field: &BodyField,
    mesh: &TriMesh,
    hang: &Hang,
    model_rest: &[bevy::math::Affine3A],
    anatomy: &Anatomy,
    new_bones: &mut Vec<BoneDef>,
) -> Option<Vec<Chain>> {
    let first = anatomy.skeleton.len();
    let start = mesh.positions.iter().any(|p| p.y < 0.0);
    let (edge, along) = hang_edge(piece, start)?;
    let segments = usize::from(hang.segments.max(1));
    let angles = chain_angles(mesh, hang.chains);
    if first + new_bones.len() + angles.len() * segments > MAX_BONES {
        warn!("garment {}: not enough bones left for its hanging cloth; it stays rigid", piece.garment.name);
        return None;
    }
    let parent = edge.bone;
    let parent_model = Transform::from_matrix((*model_rest.get(parent.index())?).into());
    let region = anatomy.skeleton.get(parent)?.region;
    let part = anatomy.parts.iter().position(|p| std::ptr::eq(p, piece.part)).unwrap_or(0).clamp_u16();
    let seg_len = hang.length / segments.to_f32();
    let mut chains = Vec::new();
    for theta in angles {
        let surf = piece.surface(field, if start { 0.0 } else { 1.0 }, theta, false)?;
        let (dir, radial) = drop_dir(piece, along, surf.normal, hang);
        let dir = (dir + radial * hang.flare).normalize();
        let root = surf.position + surf.normal * piece.garment.ease;
        let mut bones = SmallVec::new();
        let mut prev: (BoneId, Transform) = (parent, parent_model);
        for j in 0..segments {
            let model = Transform::from_translation(root + dir * seg_len * j.to_f32()).with_rotation(frame_y_z(dir, radial));
            // Fits: the bone budget was checked above.
            let id = BoneId(u8::try_from(first + new_bones.len()).ok()?);
            new_bones.push(BoneDef {
                name: format!("cloth.{}.{}.{j}", piece.garment.name, chains.len()),
                parent: Some(prev.0),
                rest: relative(&prev.1, &model),
                length: seg_len,
                region,
                part,
            });
            bones.push(id);
            prev = (id, model);
        }
        chains.push(Chain { theta, bones, root });
    }
    Some(chains)
}

/// Each chain follows the nearest leg's swing, fading with distance from that hip.
fn follow_legs(chains: &[Chain], anatomy: &Anatomy, model_rest: &[bevy::math::Affine3A], weight: f32) -> Vec<ClothFollow> {
    let hips: Vec<(usize, Vec3)> = anatomy
        .legs
        .iter()
        .enumerate()
        .filter_map(|(i, l)| Some((i, model_rest.get(l.chain.first()?.index())?.translation.into())))
        .collect();
    let spread = hips.iter().map(|(_, a)| hips.iter().map(|(_, b)| a.distance(*b)).fold(0.0, f32::max)).fold(0.1, f32::max);
    chains
        .iter()
        .filter_map(|c| {
            let (leg, d) = hips.iter().map(|(i, h)| (*i, h.distance(c.root))).min_by(|a, b| a.1.total_cmp(&b.1))?;
            let w = weight * (1.0 - d / spread).clamp(0.0, 1.0);
            (w > 0.01).then_some(ClothFollow { root: *c.bones.first()?, leg: leg.clamp_u8(), weight: w })
        })
        .collect()
}

/// Skin influences for a hanging vertex: the two nearest chains, two segments each. `None`
/// without chains.
fn hang_influences(chains: &[Chain], theta: f32, along: f32) -> Option<([u16; 4], [f32; 4])> {
    let (first, last) = (chains.first()?, chains.last()?);
    let n = chains.len();
    let segs = first.bones.len();
    let ring = n > 1 && (last.theta - first.theta) > TAU * (n.to_f32() - 1.5) / n.to_f32();
    let (k0, k1, a) = if n == 1 {
        (0, 0, 0.0)
    } else if ring {
        let x = theta.rem_euclid(TAU) / (TAU / n.to_f32());
        let k0 = x.floor().sat_usize() % n;
        (k0, (k0 + 1) % n, x.fract())
    } else {
        let step = (last.theta - first.theta) / (n - 1).to_f32();
        let x = ((theta - first.theta) / step).clamp(0.0, (n - 1).to_f32());
        let k0 = x.floor().sat_usize().min(n - 2);
        (k0, k0 + 1, x - k0.to_f32())
    };
    let seg_pos = (along * segs.to_f32()).clamp(0.0, segs.to_f32() - 1e-3);
    let (s0, f) = (seg_pos.floor().sat_usize(), seg_pos.fract());
    let s1 = (s0 + 1).min(segs.saturating_sub(1));
    let segment = |chain: usize, seg: usize| chains.get(chain).and_then(|c| c.bones.get(seg)).copied();
    let mut acc: SmallVec<[(BoneId, f32); 4]> = SmallVec::new();
    for (bone, w) in [
        (segment(k0, s0)?, (1.0 - a) * (1.0 - f)),
        (segment(k0, s1)?, (1.0 - a) * f),
        (segment(k1, s0)?, a * (1.0 - f)),
        (segment(k1, s1)?, a * f),
    ] {
        match acc.iter_mut().find(|(b, _)| *b == bone) {
            Some(entry) => entry.1 += w,
            None => acc.push((bone, w)),
        }
    }
    let mut joints = [0u16; 4];
    let mut weights = [0.0; 4];
    for ((joint, weight), (bone, w)) in joints.iter_mut().zip(&mut weights).zip(&acc) {
        *joint = u16::from(bone.0);
        *weight = *w;
    }
    Some((joints, weights))
}

/// Wraps one piece's mesh onto its part and appends it to `out`.
fn wrap_piece(piece: &Piece, field: &BodyField, mesh: &TriMesh, chains: Option<&[Chain]>, out: &mut BodyMeshData) {
    let base = out.positions.len().clamp_u32();
    let g = piece.garment;
    let start = out.positions.len();
    for p in &mesh.positions {
        let theta = theta_of(*p);
        let height = (p.x.hypot(p.z) - 1.0) + g.ease;
        let hanging = !(0.0..=1.0).contains(&p.y);
        let Some(surf) = piece.surface(field, p.y, theta, g.hang.is_none()) else {
            // A part without an axis sample here: collapse the vertex (never happens for parts
            // with an axis, which every matched part has).
            out.positions.push(Vec3::ZERO);
            out.normals.push(Vec3::Y);
            out.joints.push([0; 4]);
            out.weights.push([1.0, 0.0, 0.0, 0.0]);
            out.mob_data.push([0, 0]);
            out.cloth.push(piece.palette);
            continue;
        };
        let at_edge = surf.position + surf.normal * height;
        let skin = (surf.info.bones.map(|b| u16::from(b.0)), surf.info.weights);
        let (position, (joints, weights)) = match (hanging, g.hang, chains) {
            (true, Some(hang), Some(chains)) if !chains.is_empty() => {
                let start_edge = p.y < 0.0;
                let beyond = if start_edge { -p.y } else { p.y - 1.0 };
                let along = hang_edge(piece, start_edge).map_or(Vec3::NEG_Y, |(_, along)| along);
                let (dir, radial) = drop_dir(piece, along, surf.normal, &hang);
                let drop = beyond * hang.length;
                (at_edge + dir * drop + radial * drop * hang.flare, hang_influences(chains, theta, beyond).unwrap_or(skin))
            }
            _ => (at_edge, skin),
        };
        out.positions.push(position);
        out.normals.push(surf.normal);
        out.joints.push(joints);
        out.weights.push(weights);
        out.mob_data.push(
            MobVertexData {
                region: surf.region,
                cavity: 0,
                curvature: 128,
                limb_t: 0,
                primary_bone: BoneId(joints[0].clamp_u8()),
                layer: surf.layer,
            }
            .pack(),
        );
        out.cloth.push(piece.palette);
    }
    // Mirrored parts reflect positions, which flips winding.
    for t in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| base + i);
        out.indices.extend(if piece.part.mirrored { [a, c, b] } else { [a, b, c] });
    }
    recompute_normals(out, start);
}

/// Area-weighted vertex normals for vertices from `start` on (their triangles come last).
fn recompute_normals(out: &mut BodyMeshData, start: usize) {
    let mut acc = vec![Vec3::ZERO; out.positions.len().saturating_sub(start)];
    let first_tri = out.indices.iter().position(|i| i.idx() >= start).unwrap_or(out.indices.len()) / 3 * 3;
    for &[a, b, c] in out.indices.get(first_tri..).unwrap_or_default().as_chunks::<3>().0 {
        let (Some(&pa), Some(&pb), Some(&pc)) = (out.positions.get(a.idx()), out.positions.get(b.idx()), out.positions.get(c.idx())) else {
            continue;
        };
        let n = (pb - pa).cross(pc - pa);
        for v in [a, b, c] {
            if let Some(sum) = v.idx().checked_sub(start).and_then(|k| acc.get_mut(k)) {
                *sum += n;
            }
        }
    }
    for (n, normal) in acc.into_iter().zip(out.normals.iter_mut().skip(start)) {
        *normal = n.normalize_or(*normal);
    }
}

/// Cloth meshes per LOD from the hero fit: simplified and pushed out a little further at coarser
/// levels (their body is coarser too), crowd tiers remapped to the crowd skeleton.
#[must_use]
pub fn garment_lods(hero: &BodyMeshData, anatomy: &Anatomy, settings: &SurfaceSettings) -> [BodyMeshData; 3] {
    LodLevel::ALL.map(|level| {
        let spec = settings.get(level);
        let mut m = if level == LodLevel::Hero {
            hero.clone()
        } else {
            let ratio = if level == LodLevel::Near { 0.5 } else { 0.25 };
            let mut m = simplify(hero, ratio, spec.max_error);
            m.submeshes = vec![Submesh { indices: 0..m.indices.len().clamp_u32(), slot: MaterialSlot::Cloth }];
            let push = spec.mesh.voxel_size * 0.5;
            for (p, n) in m.positions.iter_mut().zip(&m.normals) {
                *p += *n * push;
            }
            m
        };
        if level != LodLevel::Hero {
            remap_to_crowd(&mut m, anatomy);
        }
        m.coverage.clear();
        m.flow.clear();
        m
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::anatomy::{BodyPlan, Form, PartLodRules, apply_form, build_anatomy};

    fn tube() -> TriMesh {
        let (rows, cols) = (8u32, 16u32);
        let mut positions = Vec::new();
        for i in 0..=rows {
            for j in 0..cols {
                let t = j.to_f32() / cols.to_f32() * TAU;
                positions.push(Vec3::new(t.sin(), i.to_f32() / rows.to_f32(), -t.cos()));
            }
        }
        let v = |i: u32, j: u32| i * cols + j % cols;
        let mut indices = Vec::new();
        for i in 0..rows {
            for j in 0..cols {
                indices.extend([v(i, j), v(i + 1, j), v(i, j + 1), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)]);
            }
        }
        TriMesh::new(positions, Vec::new(), indices)
    }

    pub(super) fn biped() -> Anatomy {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap()
    }

    #[test]
    fn sleeves_wrap_both_arms() {
        let anatomy = biped();
        let mut assets = PartAssets::default();
        assets.insert(String::from("tube"), Arc::new(tube()));
        let outfit = Outfit {
            garments: vec![Garment { name: "sleeve".into(), on: "torso/arm".into(), span: (0.0, 0.4), mesh: "tube".into(), ..default() }],
        };
        let arms: Vec<_> = anatomy.parts.iter().filter(|p| p.path.0.contains("arm")).map(|p| (p.path.0.clone(), p.axis.len())).collect();
        let (_, mesh) = fit_outfit(anatomy, &outfit, &assets);
        let mesh = mesh.expect("sleeves");
        let (lo, hi) = mesh.positions.iter().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        println!("arms {arms:?} verts {} bounds {lo} {hi}", mesh.positions.len());
        assert_eq!(mesh.positions.len(), 2 * 9 * 16);
        // One sleeve each side, around shoulder-to-elbow height.
        assert!(lo.x < -0.15 && hi.x > 0.15, "{lo} {hi}");
    }
}
