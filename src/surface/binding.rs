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

use bevy::math::{Affine3A, Vec3A};
use bevy::prelude::*;
use bevy::shape::Aabb3d;

use super::{BodyField, BodyMeshData};
use crate::anatomy::{Anatomy, BoneId};
use crate::num::{Clamp, Idx, Saturate, ToF32};

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
    grid: Grid,
    kind: Vec<u8>,
    /// The four nearest bones per cell (`NO_BONE` = empty), by geodesic distance.
    near: Vec<[(u8, f32); 4]>,
    /// Each cell's nearest primitive's bone (`NO_BONE` outside).
    owner: Vec<u8>,
    parent: Vec<Option<u8>>,
    /// Per bone: the arm it moves (part index; `None` for every other bone, clavicles included)
    /// and that arm's shoulder joint (position, radius) in the field's pose.
    limb: Vec<Option<(u16, Vec3A, f32)>>,
    falloff: f32,
}

#[derive(Clone, Copy, PartialEq)]
struct Step(f32, usize);

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

/// What classification leaves in each cell.
#[derive(Clone, Copy)]
struct Cell {
    kind: u8,
    /// Nearest primitive's bone (`NO_BONE` outside).
    bone: u8,
    /// Nearest primitive's limb (`u16::MAX` = none).
    limb: u16,
}

/// The 3×3×3 block of cells around `c`, `c` included.
fn neighbourhood(c: IVec3) -> impl Iterator<Item = IVec3> {
    (-1..=1).flat_map(move |z| (-1..=1).flat_map(move |y| (-1..=1).map(move |x| c + IVec3::new(x, y, z))))
}

/// Cells are classified in blocks of this many per side.
const BLOCK: u32 = 8;

/// A regular grid of cubic cells, cell centres at `origin + index * size`.
#[derive(Clone, Copy)]
struct Grid {
    origin: Vec3A,
    size: f32,
    dims: UVec3,
}

impl Grid {
    fn len(&self) -> usize {
        self.dims.element_product().idx()
    }

    /// Linear index of cell `c`, if it lies inside.
    fn index(&self, c: IVec3) -> Option<usize> {
        (c.cmpge(IVec3::ZERO).all() && c.cmplt(self.dims.as_ivec3()).all()).then(|| {
            let c = c.as_uvec3();
            (c.x + self.dims.x * (c.y + self.dims.y * c.z)).idx()
        })
    }

    /// Inverse of [`index`](Self::index).
    fn coords(&self, i: usize) -> IVec3 {
        let (i, d) = (i.clamp_u32(), self.dims);
        UVec3::new(i % d.x, (i / d.x) % d.y, i / (d.x * d.y)).as_ivec3()
    }

    fn centre(&self, c: IVec3) -> Vec3A {
        self.origin + Vec3A::from(c.as_vec3() * self.size)
    }

    /// The cell whose centre is nearest `p` (possibly outside the grid).
    fn cell_of(&self, p: Vec3A) -> IVec3 {
        ((p - self.origin) / self.size).round().as_ivec3()
    }
}

impl VoxelBinding {
    #[must_use]
    pub fn new(field: &BodyField, anatomy: &Anatomy, settings: &BindSettings) -> Self {
        use super::Field;
        let bounds = field.bounds();
        let extent = Vec3::from(bounds.max - bounds.min);
        let dims_for = |size: f32| (extent / size).ceil().as_uvec3() + UVec3::ONE;
        let mut size = settings.voxel.max(1e-3);
        while dims_for(size).element_product().idx() > MAX_CELLS {
            size *= 1.25;
        }
        let grid = Grid { origin: bounds.min, size, dims: dims_for(size) };
        let cells = classify(field, grid);
        let near = spread(field, anatomy, settings, grid, &cells);
        Self {
            grid,
            kind: cells.iter().map(|c| c.kind).collect(),
            near,
            owner: cells.iter().map(|c| c.bone).collect(),
            parent: anatomy.skeleton.bones.iter().map(|b| b.parent.map(|p| p.0)).collect(),
            limb: arm_roots(anatomy, field.bone_model()),
            falloff: settings.falloff,
        }
    }

    /// Replaces each vertex's joints and weights with the geodesic binding (positions in the pose
    /// the field was built in).
    pub fn apply(&self, data: &mut BodyMeshData) {
        // The bone the vertex's own surface belongs to (its nearest primitive's). Vertices bind
        // independently: in parallel.
        let homes: Vec<(Vec3, u8)> = data.positions.iter().zip(&data.joints).map(|(p, j)| (*p, j[0].clamp_u8())).collect();
        let bound = super::par_map(&homes, |&(p, home)| self.bind(Vec3A::from(p), home));
        for (b, (joints, weights)) in bound.into_iter().zip(data.joints.iter_mut().zip(data.weights.iter_mut())) {
            if let Some((j, w)) = b {
                *joints = j;
                *weights = w;
            }
        }
    }

    /// Joints and weights of a vertex at `p` whose surface belongs to bone `home`.
    fn bind(&self, p: Vec3A, home: u8) -> Option<([u16; 4], [f32; 4])> {
        let centre = self.grid.cell_of(p);
        let mut best: [(u8, f32); 8] = [(NO_BONE, f32::INFINITY); 8];
        for q in neighbourhood(centre) {
            let Some(i) = self.grid.index(q) else { continue };
            let (Some(&owner), Some(&kind), Some(near)) = (self.owner.get(i), self.kind.get(i), self.near.get(i)) else {
                continue;
            };
            // Only cells of this part of the body: not the next finger across a gap.
            if kind == EXTERIOR || owner != NO_BONE && !self.in_line(home, owner) {
                continue;
            }
            let offset = p.distance(self.grid.centre(q));
            for &(bone, d) in near.iter().filter(|(bone, _)| *bone != NO_BONE) {
                let d = d + offset;
                match best.iter_mut().find(|s| s.0 == bone) {
                    Some(s) => s.1 = s.1.min(d),
                    None => {
                        if let Some(worst) = best.iter_mut().max_by(|a, b| a.1.total_cmp(&b.1)).filter(|w| d < w.1) {
                            *worst = (bone, d);
                        }
                    }
                }
            }
        }
        best.sort_by(|a, b| a.1.total_cmp(&b.1));
        if best[0].0 == NO_BONE {
            return None;
        }
        let eps = self.grid.size * 0.5;
        let mut joints = [u16::from(best[0].0); 4];
        let mut weights = [0.0f32; 4];
        for ((&(bone, d), joint), weight) in best.iter().zip(&mut joints).zip(&mut weights) {
            if bone == NO_BONE {
                continue;
            }
            *joint = u16::from(bone);
            *weight = (d + eps).powf(-self.falloff);
            // A limb's pull on the torso fades within a short radius of its root joint (as a
            // rigger paints it): distance through the body alone lets a raised arm drag the
            // whole side of the chest.
            if let Some((limb, root, radius)) = self.limb.get(bone.idx()).copied().flatten() {
                let own = self.limb.get(home.idx()).copied().flatten().is_some_and(|h| h.0 == limb);
                if !own {
                    let s = ((p.distance(root) / radius.max(1e-3) - 0.5) / 1.0).clamp(0.0, 1.0);
                    *weight *= f32::mul_add(s * s, -2.0f32.mul_add(-s, 3.0), 1.0);
                }
            }
        }
        let sum: f32 = weights.iter().sum();
        if sum <= 1e-12 {
            return None;
        }
        for w in &mut weights {
            *w /= sum;
            if *w < 0.01 {
                *w = 0.0;
            }
        }
        let sum: f32 = weights.iter().sum();
        for w in &mut weights {
            *w /= sum;
        }
        Some((joints, weights))
    }

    /// Same bone, or one a near ancestor of the other (as `BodyField::in_line`).
    fn in_line(&self, a: u8, b: u8) -> bool {
        let up = |from: u8, to: u8| {
            let mut cur = Some(from);
            for _ in 0..=3 {
                match cur {
                    Some(c) if c == to => return true,
                    Some(c) => cur = self.parent.get(c.idx()).copied().flatten(),
                    None => return false,
                }
            }
            false
        };
        up(a, b) || up(b, a)
    }
}

/// Step 1: classifies cells, block by block (each block evaluates only the primitives near it).
fn classify(field: &BodyField, grid: Grid) -> Vec<Cell> {
    let mut cells = vec![Cell { kind: EXTERIOR, bone: NO_BONE, limb: u16::MAX }; grid.len()];
    let shell = grid.size * 0.87;
    let dims = grid.dims;
    for bz in (0..dims.z).step_by(BLOCK.idx()) {
        for by in (0..dims.y).step_by(BLOCK.idx()) {
            for bx in (0..dims.x).step_by(BLOCK.idx()) {
                let lo = UVec3::new(bx, by, bz);
                let hi = (lo + UVec3::splat(BLOCK)).min(dims);
                let region = Aabb3d {
                    min: grid.centre(lo.as_ivec3()) - Vec3A::splat(shell),
                    max: grid.centre(hi.as_ivec3()) + Vec3A::splat(shell),
                };
                let view = field.view(region);
                if view.is_empty() {
                    continue;
                }
                for z in lo.z..hi.z {
                    for y in lo.y..hi.y {
                        for x in lo.x..hi.x {
                            let c = UVec3::new(x, y, z).as_ivec3();
                            let Some(cell) = grid.index(c).and_then(|i| cells.get_mut(i)) else { continue };
                            let (d, nearest) = field.probe(&view, grid.centre(c));
                            if d > shell {
                                continue;
                            }
                            cell.kind = if d <= 0.0 { INTERIOR } else { BOUNDARY };
                            if let Some((bone, limb)) = nearest {
                                cell.bone = bone.0;
                                cell.limb = limb.unwrap_or(u16::MAX);
                            }
                        }
                    }
                }
            }
        }
    }
    cells
}

/// Step 2: per bone, a bounded Dijkstra from the cells along its segment. Returns each cell's four
/// nearest bones.
fn spread(field: &BodyField, anatomy: &Anatomy, settings: &BindSettings, grid: Grid, cells: &[Cell]) -> Vec<[(u8, f32); 4]> {
    // Cells joined unless they belong to different limbs or bones out of line.
    let joined = |a: &Cell, b: &Cell| -> bool {
        if a.limb != u16::MAX && b.limb != u16::MAX && a.limb != b.limb {
            return false;
        }
        a.bone == NO_BONE || b.bone == NO_BONE || a.bone == b.bone || field.in_line(BoneId(a.bone), BoneId(b.bone))
    };
    let offsets: Vec<(IVec3, f32)> = neighbourhood(IVec3::ZERO)
        .filter(|o| *o != IVec3::ZERO)
        .map(|o| (o, o.as_vec3().length() * grid.size))
        .collect();
    let model = field.bone_model();
    // One bone's Dijkstra, in scratch `dist` (left all infinite again): the cells it reached and
    // their distances, in the order they were first reached.
    let one_bone = |bi: usize, dist: &mut [f32], touched: &mut Vec<usize>, frontier: &mut BinaryHeap<Step>| -> Option<Reached> {
        let (Ok(bone), Some(def), Some(m)) = (u8::try_from(bi), anatomy.skeleton.bones.get(bi), model.get(bi)) else { return None };
        let reach = (def.length * settings.reach).max(settings.min_reach);
        let (head, tail) = (m.translation, m.transform_point3a(Vec3A::Y * def.length));
        let steps = (def.length / (grid.size * 0.5)).ceil().sat_usize().max(1);
        let mut seed = |i: usize, d: &mut f32| {
            *d = 0.0;
            touched.push(i);
            frontier.push(Step(0.0, i));
        };
        let mut seeded = false;
        for s in 0..=steps {
            let Some(i) = grid.index(grid.cell_of(head.lerp(tail, s.to_f32() / steps.to_f32()))) else { continue };
            if let (Some(cell), Some(d)) = (cells.get(i), dist.get_mut(i))
                && cell.kind != EXTERIOR
                && *d > 0.0
            {
                seed(i, d);
                seeded = true;
            }
        }
        // A bone outside the volume (thin parts): seed from the cells it owns.
        if !seeded {
            for (i, (_, d)) in cells.iter().zip(dist.iter_mut()).enumerate().filter(|(_, (c, _))| c.bone == bone && c.kind != EXTERIOR) {
                seed(i, d);
            }
        }
        while let Some(Step(d, i)) = frontier.pop() {
            let Some(from) = cells.get(i) else { continue };
            if dist.get(i).is_none_or(|&best| d > best) || d > reach {
                continue;
            }
            let at = grid.coords(i);
            for (offset, len) in &offsets {
                let Some(j) = grid.index(at + *offset) else { continue };
                let (Some(to), Some(dj)) = (cells.get(j), dist.get_mut(j)) else { continue };
                // A bone's distance spreads only over cells of bones in line with it: through
                // the palm into its own finger, never on into the next finger.
                let ours = to.bone == NO_BONE || to.bone == bone || field.in_line(BoneId(bone), BoneId(to.bone));
                if to.kind == EXTERIOR || !ours || !joined(from, to) {
                    continue;
                }
                let step = if to.kind == BOUNDARY { len * settings.boundary_penalty } else { *len };
                let nd = d + step;
                if nd < *dj {
                    if dj.is_infinite() {
                        touched.push(j);
                    }
                    *dj = nd;
                    frontier.push(Step(nd, j));
                }
            }
        }
        let reached = touched.iter().filter_map(|&i| dist.get(i).map(|&d| (i, d))).collect();
        for &i in touched.iter() {
            if let Some(d) = dist.get_mut(i) {
                *d = f32::INFINITY;
            }
        }
        touched.clear();
        Some((bone, reached))
    };
    // Bones spread independently, so they run in parallel (each thread with its own scratch) and
    // merge in bone order: the same result on any thread count.
    let count = anatomy.skeleton.bones.len();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = super::worker_threads().min(count).max(1);
    let mut per_bone: Vec<(usize, Reached)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (one_bone, next) = (&one_bone, &next);
                scope.spawn(move || {
                    let (mut dist, mut touched, mut frontier) = (vec![f32::INFINITY; grid.len()], Vec::new(), BinaryHeap::new());
                    let mut out = Vec::new();
                    loop {
                        let bi = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if bi >= count {
                            break out;
                        }
                        if let Some(r) = one_bone(bi, &mut dist, &mut touched, &mut frontier) {
                            out.push((bi, r));
                        }
                    }
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
    });
    per_bone.sort_unstable_by_key(|(bi, _)| *bi);
    nearest_four(per_bone.into_iter().map(|(_, r)| r), grid.len())
}

/// A bone and the cells its spread reached, with their distances (in the order first reached).
type Reached = (u8, Vec<(usize, f32)>);

/// Each cell's four nearest bones, from every bone's spread in bone order (ties keep the earlier
/// bone).
fn nearest_four(per_bone: impl Iterator<Item = Reached>, cells: usize) -> Vec<[(u8, f32); 4]> {
    let mut near = vec![[(NO_BONE, f32::INFINITY); 4]; cells];
    for (bone, reached) in per_bone {
        for (i, d) in reached {
            let Some(slot) = near.get_mut(i) else { continue };
            if d < slot[3].1 {
                slot[3] = (bone, d);
                slot.sort_by(|a, b| a.1.total_cmp(&b.1));
            }
        }
    }
    near
}

/// Per bone: the arm it moves and that arm's shoulder joint, for fading limb weight out over the
/// torso (see [`VoxelBinding::limb`]).
fn arm_roots(anatomy: &Anatomy, model: &[Affine3A]) -> Vec<Option<(u16, Vec3A, f32)>> {
    let limb_of = |part: u16| -> Option<u16> {
        let p = anatomy.parts.get(part.idx())?;
        if p.role.is_some() {
            return Some(part);
        }
        p.parent.filter(|q| anatomy.parts.get(q.idx()).is_some_and(|q| q.role.is_some()))
    };
    let clavicle = |bone: u8| anatomy.clavicles.iter().any(|c| c.bone.0 == bone);
    let bones = &anatomy.skeleton.bones;
    bones
        .iter()
        .enumerate()
        .map(|(bi, def)| {
            if u8::try_from(bi).is_ok_and(clavicle) {
                return None;
            }
            let limb = limb_of(def.part)?;
            // Arms only: they swing far past where they rest. Legs' weights over the hips are fine as
            // distance gives them, and big hips need them wide.
            let arm = anatomy.parts.get(limb.idx())?;
            if arm.role != Some(crate::anatomy::LimbRole::Manipulator) {
                return None;
            }
            let root = arm.bones.clone().find(|r| !clavicle(*r))?;
            let at = model.get(root.idx())?.translation;
            Some((limb, at, bones.get(root.idx())?.length * 0.35))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::anatomy::{BodyPlan, Form, LimbRole, LodLevel, PartAssets, PartLodRules, apply_form, build_anatomy};
    use crate::num::Idx;
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
        let legs: Vec<u16> = a.legs.iter().flat_map(|l| l.chain.iter().map(|b| u16::from(b.0))).collect();
        let arm_parts: Vec<usize> = a.parts.iter().enumerate().filter(|(_, p)| p.role == Some(LimbRole::Manipulator)).map(|(i, _)| i).collect();
        let in_hand = |b: u16| {
            let part = bones[b.idx()].part.idx();
            arm_parts.contains(&part) && b == u16::from(a.parts[part].bones.end - 1) || a.parts[part].parent.is_some_and(|p| arm_parts.contains(&p.idx()))
        };
        // Which finger a digit bone belongs to: its `digitN` name prefix.
        let finger = |b: u16| bones[b.idx()].name.split('.').next().filter(|n| n.starts_with("digit")).map(str::to_owned);
        let (mut hand_verts, mut bad_leg, mut bad_finger) = (0, 0, 0);
        let mut by_seg: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::default();
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
            if let Some(f) = finger(j[0])
                && used.iter().filter_map(|b| finger(*b)).any(|g| g != f) {
                    bad_finger += 1;
                    let seg = bones[j[0].idx()].name.rsplit('.').next().unwrap_or("").to_owned();
                    *by_seg.entry(seg).or_insert(0) += 1;
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
