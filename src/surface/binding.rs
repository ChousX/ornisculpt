//! Geodesic voxel binding (Dionne & de Lasa, "Geodesic Voxel Binding for Production Character
//! Meshes", SCA 2013; Maya's default bind): skin weights from distances measured *through the
//! body*, so parts that sit close together but don't connect inside (a hand hanging by a thigh)
//! don't pull on each other the way straight-line distance makes them.
//!
//! 1. Voxelize the field (in the pose it was built in): interior, boundary (the surface crosses
//!    the cell) or exterior; each cell remembers its nearest primitive's bone and limb.
//! 2. Per bone, Dijkstra over non-exterior cells from the cells its segment passes through, with
//!    Euclidean step costs and a penalty on boundary cells (paths prefer the inside).
//! 3. Per vertex, the distance to a bone is a nearby cell's distance plus the offset to it;
//!    weights are `d^-falloff`, the four nearest bones, normalized.
//!
//! Voxels can't separate gaps finer than a cell (fingers, ~3 mm apart), which the literature
//! notes as the method's failure mode. Following the cut-graph remedy (e.g. "A Geodesic
//! Cut-Cell Prior for Neural Skinning"), edges between cells of different limbs, or of bones not
//! in line with each other (neighbouring fingers), are cut.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use bevy::math::Vec3A;
use bevy::prelude::*;
use bevy::shape::Aabb3d;

use super::{BodyField, BodyMeshData};
use crate::anatomy::{Anatomy, BoneId};

/// How binding weights are computed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BindSettings {
    /// Cell size, metres (scaled up for big bodies with the mesh voxels).
    pub voxel: f32,
    /// Weight falloff `λ` in `w = d^-λ`: higher is more rigid, lower blends wider.
    pub falloff: f32,
    /// Step cost multiplier into boundary cells (> 1 keeps paths inside the body).
    pub boundary_penalty: f32,
    /// How far each bone's distances spread, in multiples of its length (at least `min_reach`).
    pub reach: f32,
    /// Smallest spread, metres.
    pub min_reach: f32,
}

impl Default for BindSettings {
    fn default() -> Self {
        Self { voxel: 0.012, falloff: 3.0, boundary_penalty: 1.5, reach: 3.0, min_reach: 0.12 }
    }
}

const EXTERIOR: u8 = 0;
const BOUNDARY: u8 = 1;
const INTERIOR: u8 = 2;
const NO_BONE: u8 = u8::MAX;
/// Keep at most this many cells; bigger bodies get coarser cells.
const MAX_CELLS: usize = 1_500_000;

/// Per-cell nearest bones by geodesic distance, ready to weight vertices.
pub struct VoxelBinding {
    origin: Vec3A,
    h: f32,
    dims: UVec3,
    kind: Vec<u8>,
    /// The four nearest bones per cell (`NO_BONE` = empty), by geodesic distance.
    near: Vec<[(u8, f32); 4]>,
    /// Each cell's nearest primitive's bone (`NO_BONE` outside).
    owner: Vec<u8>,
    parent: Vec<Option<u8>>,
    falloff: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct Step(f32, u32);

impl Eq for Step {}

impl Ord for Step {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}

impl PartialOrd for Step {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl VoxelBinding {
    pub fn new(field: &BodyField, anatomy: &Anatomy, settings: &BindSettings) -> Self {
        use super::Field;
        let bounds = field.bounds();
        let size = Vec3::from(bounds.max - bounds.min);
        let mut h = settings.voxel.max(1e-3);
        let cells = |h: f32| ((size / h).ceil().as_uvec3() + UVec3::ONE).element_product() as usize;
        while cells(h) > MAX_CELLS {
            h *= 1.25;
        }
        let dims = (size / h).ceil().as_uvec3() + UVec3::ONE;
        let origin = bounds.min;
        let n = dims.element_product() as usize;
        let at = |i: UVec3| origin + Vec3A::from(i.as_vec3() * h);

        // 1. Classify cells, block by block (each block evaluates only the primitives near it).
        let mut kind = vec![EXTERIOR; n];
        let mut bone = vec![NO_BONE; n];
        let mut limb = vec![u16::MAX; n];
        const B: u32 = 8;
        let shell = h * 0.87;
        for bz in (0..dims.z).step_by(B as usize) {
            for by in (0..dims.y).step_by(B as usize) {
                for bx in (0..dims.x).step_by(B as usize) {
                    let lo = UVec3::new(bx, by, bz);
                    let hi = (lo + UVec3::splat(B)).min(dims);
                    let region = Aabb3d { min: at(lo) - Vec3A::splat(shell), max: at(hi) + Vec3A::splat(shell) };
                    let view = field.view(region);
                    if view.is_empty() {
                        continue;
                    }
                    for z in lo.z..hi.z {
                        for y in lo.y..hi.y {
                            for x in lo.x..hi.x {
                                let i = (x + dims.x * (y + dims.y * z)) as usize;
                                let (d, near) = field.probe(&view, at(UVec3::new(x, y, z)));
                                if d > shell {
                                    continue;
                                }
                                kind[i] = if d <= 0.0 { INTERIOR } else { BOUNDARY };
                                if let Some((b, l)) = near {
                                    bone[i] = b.0;
                                    limb[i] = l.unwrap_or(u16::MAX);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Cells joined unless they belong to different limbs or bones out of line.
        let joined = |a: usize, b: usize| -> bool {
            let (la, lb) = (limb[a], limb[b]);
            if la != u16::MAX && lb != u16::MAX && la != lb {
                return false;
            }
            let (ba, bb) = (bone[a], bone[b]);
            ba == NO_BONE || bb == NO_BONE || ba == bb || field.in_line(BoneId(ba), BoneId(bb))
        };
        let index = |c: IVec3| -> Option<usize> {
            (c.cmpge(IVec3::ZERO).all() && c.cmplt(dims.as_ivec3()).all()).then(|| (c.x as u32 + dims.x * (c.y as u32 + dims.y * c.z as u32)) as usize)
        };
        let cell_of = |p: Vec3A| -> IVec3 { ((p - origin) / h).round().as_ivec3() };
        let offsets: Vec<(IVec3, f32)> = (-1..=1)
            .flat_map(|z| (-1..=1).flat_map(move |y| (-1..=1).map(move |x| IVec3::new(x, y, z))))
            .filter(|o| *o != IVec3::ZERO)
            .map(|o| (o, o.as_vec3().length() * h))
            .collect();

        // 2. Per bone, a bounded Dijkstra from the cells along its segment.
        let mut near = vec![[(NO_BONE, f32::INFINITY); 4]; n];
        let mut dist = vec![f32::INFINITY; n];
        let mut touched: Vec<usize> = Vec::new();
        let mut heap = BinaryHeap::new();
        let model = field.bone_model();
        for (bi, def) in anatomy.skeleton.bones.iter().enumerate() {
            let Some(m) = model.get(bi) else { continue };
            let reach = (def.length * settings.reach).max(settings.min_reach);
            let head = m.translation;
            let tail = m.transform_point3a(Vec3A::Y * def.length);
            let steps = ((def.length / (h * 0.5)).ceil() as usize).max(1);
            let mut seeded = false;
            for s in 0..=steps {
                let p = head.lerp(tail, s as f32 / steps as f32);
                if let Some(i) = index(cell_of(p)).filter(|&i| kind[i] != EXTERIOR && dist[i] > 0.0) {
                    dist[i] = 0.0;
                    touched.push(i);
                    heap.push(Step(0.0, i as u32));
                    seeded = true;
                }
            }
            // A bone outside the volume (thin parts): seed from the cells it owns.
            if !seeded {
                for i in (0..n).filter(|&i| bone[i] == bi as u8 && kind[i] != EXTERIOR) {
                    dist[i] = 0.0;
                    touched.push(i);
                    heap.push(Step(0.0, i as u32));
                }
            }
            while let Some(Step(d, i)) = heap.pop() {
                let i = i as usize;
                if d > dist[i] || d > reach {
                    continue;
                }
                let c = IVec3::new((i as u32 % dims.x) as i32, ((i as u32 / dims.x) % dims.y) as i32, (i as u32 / (dims.x * dims.y)) as i32);
                for (o, len) in &offsets {
                    let Some(j) = index(c + *o) else { continue };
                    // A bone's distance spreads only over cells of bones in line with it: through
                    // the palm into its own finger, never on into the next finger.
                    let owner = bone[j];
                    let ours = owner == NO_BONE || owner == bi as u8 || field.in_line(BoneId(bi as u8), BoneId(owner));
                    if kind[j] == EXTERIOR || !ours || !joined(i, j) {
                        continue;
                    }
                    let step = if kind[j] == BOUNDARY { len * settings.boundary_penalty } else { *len };
                    let nd = d + step;
                    if nd < dist[j] {
                        if dist[j].is_infinite() {
                            touched.push(j);
                        }
                        dist[j] = nd;
                        heap.push(Step(nd, j as u32));
                    }
                }
            }
            for &i in &touched {
                let d = dist[i];
                let slot = &mut near[i];
                if d < slot[3].1 {
                    slot[3] = (bi as u8, d);
                    slot.sort_by(|a, b| a.1.total_cmp(&b.1));
                }
                dist[i] = f32::INFINITY;
            }
            touched.clear();
        }
        let parent = anatomy.skeleton.bones.iter().map(|b| b.parent.map(|p| p.0)).collect();
        Self { origin, h, dims, kind, near, owner: bone, parent, falloff: settings.falloff }
    }

    /// Replaces each vertex's joints and weights with the geodesic binding (positions in the pose
    /// the field was built in).
    pub fn apply(&self, data: &mut BodyMeshData) {
        let dims = self.dims.as_ivec3();
        let eps = self.h * 0.5;
        for v in 0..data.positions.len() {
            let p = Vec3A::from(data.positions[v]);
            // The bone the vertex's own surface belongs to (its nearest primitive's).
            let home = data.joints[v][0] as u8;
            let c = ((p - self.origin) / self.h).round().as_ivec3();
            let mut best: [(u8, f32); 8] = [(NO_BONE, f32::INFINITY); 8];
            for z in -1..=1 {
                for y in -1..=1 {
                    for x in -1..=1 {
                        let q = c + IVec3::new(x, y, z);
                        if !(q.cmpge(IVec3::ZERO).all() && q.cmplt(dims).all()) {
                            continue;
                        }
                        let i = (q.x + dims.x * (q.y + dims.y * q.z)) as usize;
                        // Only cells of this part of the body: not the next finger across a gap.
                        let owner = self.owner[i];
                        if self.kind[i] == EXTERIOR || owner != NO_BONE && !self.in_line(home, owner) {
                            continue;
                        }
                        let offset = p.distance(self.origin + Vec3A::from(q.as_vec3() * self.h));
                        for &(b, d) in &self.near[i] {
                            if b == NO_BONE {
                                continue;
                            }
                            let d = d + offset;
                            match best.iter_mut().find(|s| s.0 == b) {
                                Some(s) => s.1 = s.1.min(d),
                                None => {
                                    if let Some(worst) = best.iter_mut().max_by(|a, b| a.1.total_cmp(&b.1)).filter(|w| d < w.1) {
                                        *worst = (b, d);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            best.sort_by(|a, b| a.1.total_cmp(&b.1));
            if best[0].0 == NO_BONE {
                continue;
            }
            let mut joints = [best[0].0 as u16; 4];
            let mut weights = [0.0f32; 4];
            for k in 0..4 {
                if best[k].0 != NO_BONE {
                    joints[k] = best[k].0 as u16;
                    weights[k] = (best[k].1 + eps).powf(-self.falloff);
                }
            }
            let sum: f32 = weights.iter().sum();
            for w in &mut weights {
                *w /= sum;
                if *w < 0.01 {
                    *w = 0.0;
                }
            }
            let sum: f32 = weights.iter().sum();
            weights.iter_mut().for_each(|w| *w /= sum);
            data.joints[v] = joints;
            data.weights[v] = weights;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::anatomy::{BodyPlan, Form, LimbRole, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::surface::{ProceduralHardParts, SurfaceSettings, mesh_lod};

    /// On the human mesh: no hand vertex leans on a leg, no finger on its neighbour, weights sum to 1.
    #[test]
    fn hands_and_fingers_keep_to_themselves() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/biped.body.ron")).unwrap();
        let live = apply_form(&plan, &Form::default()).unwrap();
        let a = build_anatomy(&live, &PartLodRules::default(), &PartAssets::default()).unwrap();
        let settings = SurfaceSettings::default();
        let m = mesh_lod(&crate::surface::AdaptiveContour, &ProceduralHardParts, &a, LodLevel::Hero, settings.get(LodLevel::Hero), 1).unwrap();
        let bones = &a.skeleton.bones;
        let legs: Vec<u16> = a.legs.iter().flat_map(|l| l.chain.iter().map(|b| b.0 as u16)).collect();
        let arm_parts: Vec<usize> = a.parts.iter().enumerate().filter(|(_, p)| p.role == Some(LimbRole::Manipulator)).map(|(i, _)| i).collect();
        let in_hand = |b: u16| {
            let part = bones[b as usize].part as usize;
            arm_parts.contains(&part) && b as u8 == a.parts[part].bones.end - 1 || a.parts[part].parent.is_some_and(|p| arm_parts.contains(&(p as usize)))
        };
        // Which finger a digit bone belongs to: its `digitN` name prefix.
        let finger = |b: u16| bones[b as usize].name.split('.').next().filter(|n| n.starts_with("digit")).map(str::to_owned);
        let (mut hand_verts, mut bad_leg, mut bad_finger) = (0, 0, 0);
        let mut by_seg: std::collections::BTreeMap<String, u32> = Default::default();
        for (j, w) in m.joints.iter().zip(&m.weights) {
            let sum: f32 = w.iter().sum();
            assert!((sum - 1.0).abs() < 1e-3, "weights sum to {sum}");
            let used: Vec<u16> = j.iter().zip(w).filter(|(_, w)| **w > 0.05).map(|(j, _)| *j).collect();
            if !in_hand(j[0]) {
                continue;
            }
            hand_verts += 1;
            if used.iter().any(|b| legs.contains(b)) {
                bad_leg += 1;
            }
            if let Some(f) = finger(j[0]) {
                if used.iter().filter_map(|b| finger(*b)).any(|g| g != f) {
                    bad_finger += 1;
                    let seg = bones[j[0] as usize].name.rsplit('.').next().unwrap_or("").to_owned();
                    *by_seg.entry(seg).or_insert(0) += 1;
                }
            }
        }
        println!("{hand_verts} hand vertices: {bad_leg} lean on a leg, {bad_finger} on another finger");
        println!("by segment of the vertex's main bone: {by_seg:?}");
        assert!(hand_verts > 100);
        assert_eq!(bad_leg, 0);
        // The knuckle webbing between fingers moves with both neighbours; past the first segment a
        // finger is its own.
        let beyond_knuckles: u32 = by_seg.iter().filter(|(s, _)| s.as_str() != "0").map(|(_, n)| n).sum();
        assert_eq!(beyond_knuckles, 0, "finger vertices past the knuckles lean on a neighbour: {by_seg:?}");
        assert!(bad_finger * 5 <= hand_verts, "{bad_finger} of {hand_verts} hand vertices share fingers");
    }
}

impl VoxelBinding {
    /// Same bone, or one a near ancestor of the other (as `BodyField::in_line`).
    fn in_line(&self, a: u8, b: u8) -> bool {
        let up = |from: u8, to: u8| {
            let mut cur = Some(from);
            for _ in 0..=2 {
                match cur {
                    Some(c) if c == to => return true,
                    Some(c) => cur = self.parent.get(c as usize).copied().flatten(),
                    None => return false,
                }
            }
            false
        };
        up(a, b) || up(b, a)
    }
}
