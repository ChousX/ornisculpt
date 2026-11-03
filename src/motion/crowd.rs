//! Crowd-tier animation: advance the gait phase and pick which baked frames to blend. Only the
//! small per-slot blend instruction is uploaded, and only when it changed; the blend itself runs
//! in a compute pass (see `crate::mob::crowd_gpu`).
//!
//! A crowd mob with a [`MobMotion`](super::MobMotion) also plays its graph's idles and actions:
//! the first time a body meets a graph, the cycles the graph's states reference and that fit the
//! body are baked ([`bake_cycle`]) on the compute pool and appended to the clip atlas, one clip
//! and one mask row (the crowd bones the clip moves) per layer and cycle. Each frame the
//! runtime's playing cycles become up to two layers over the gait, chosen by weight. Until the
//! bake lands (or for cycles that don't fit), the mob plays its gait alone, as a mob without a
//! graph does. The state machine itself runs on the CPU for every tier, so events fire as usual.

use std::ops::Range;
use std::sync::Arc;

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task, futures::check_ready};
use bevy::utils::Parallel;

use super::bake::{BAKE_FRAMES, bake_cycle, idle_frame, standing_frame};
use super::cycle::Target;
use super::cycle::Cycle;
#[cfg(test)]
use super::cycle::CycleKind;
use super::graph::{CPlay, ComposedGraph, LayerMode, MotionGraph};
use super::ground::CrowdGround;
use super::eval::MotionRig;
use super::player::MotionRuntime;
use super::stages::leg_length;
use super::bake::bake_motion;
use super::{BakedMotion, Gait, GaitClock, Locomotion, MobGait};
use crate::anatomy::Anatomy;
use crate::mob::{
    CROWD_LAYERS, ClipBlock, ClipLayout, CrowdInstance, CrowdLayer, CrowdSkin, CrowdSlot, CrowdUploaded, CrowdUploads, GeneratedBody, remap,
    LAYER_ADDITIVE, MobBody, MobSkeleton, ROW_MATRICES,
};
use crate::num::{Clamp, Saturate, ToF32};

/// Bakes running at once (generation shares the pool).
const MAX_BAKES: usize = 2;

/// A graph's idle or action baked for one body, in the crowd atlas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrowdClip {
    /// Atlas index of frame 0's bone 0; frame `f` is at `base + f · bones`.
    pub base: u32,
    /// Atlas index of the mask row: 1 for the slot entries the clip moves, else 0.
    pub mask: u32,
    pub loops: bool,
    pub additive: bool,
}

/// The idles and actions one composed graph plays on one body, baked: indexed by layer and cycle.
#[derive(Debug, Default)]
pub struct GraphClips {
    cycles: usize,
    clips: Vec<Option<CrowdClip>>,
}

impl GraphClips {
    /// The clip for `cycle` playing on `layer`, if it was baked.
    #[must_use]
    pub fn get(&self, layer: usize, cycle: usize) -> Option<CrowdClip> {
        self.clips.get(layer * self.cycles + cycle).copied().flatten()
    }

    /// How many clips were baked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.clips.iter().flatten().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

type GraphKey = Vec<AssetId<MotionGraph>>;

/// One body's bakes: per graph set, the clips and the atlas block holding them.
type Bakes = Vec<(GraphKey, Arc<GraphClips>, Range<u32>)>;

/// What a bake task returns: the matrices to append and, per (layer, cycle), offsets into them.
struct Baked {
    data: Vec<[f32; 16]>,
    cycles: usize,
    /// `(frame 0 offset, mask row offset, loops, additive)` per layer × cycle.
    clips: Vec<Option<(u32, u32, bool, bool)>>,
}

/// Baked graph cycles per body and graph set, and the bakes in flight.
///
/// Each entry owns a block of the clip atlas, so an entry that goes — its graphs reloaded, or its
/// body dropped — gives that block back (`CrowdSkin::release_range`).
#[derive(Resource, Default)]
pub struct CrowdCycles {
    baked: HashMap<AssetId<GeneratedBody>, Bakes>,
    in_flight: Vec<(AssetId<GeneratedBody>, GraphKey, u32, Task<Baked>)>,
    /// Bumped when the graphs change: results from before are dropped.
    epoch: u32,
}

impl CrowdCycles {
    /// The clips of graph set `key` on `body`, once baked.
    #[must_use]
    pub fn get(&self, body: AssetId<GeneratedBody>, key: &[AssetId<MotionGraph>]) -> Option<&Arc<GraphClips>> {
        self.baked.get(&body)?.iter().find(|(k, _, _)| k == key).map(|(_, c, _)| c)
    }

    fn known(&self, body: AssetId<GeneratedBody>, key: &[AssetId<MotionGraph>]) -> bool {
        self.get(body, key).is_some() || self.in_flight.iter().any(|(b, k, _, _)| *b == body && k == key)
    }

    /// Drops every bake of `body` (or of all bodies), freeing the atlas blocks they hold.
    fn forget(&mut self, body: Option<AssetId<GeneratedBody>>, skin: &mut CrowdSkin) {
        let gone = match body {
            Some(id) => self.baked.remove(&id).into_iter().collect(),
            None => self.baked.drain().map(|(_, v)| v).collect::<Vec<_>>(),
        };
        for (_, _, block) in gone.into_iter().flatten() {
            skin.release_range(block.start, block.end - block.start);
        }
    }
}

/// Custom gaits baked per (body, gait), and the bakes in flight.
///
/// A body's own gait is baked into the atlas when it is generated, but a mob with a [`MobGait`]
/// of its own walks to something else. Those are baked on the compute pool, as the graph cycles
/// are, and the mob plays its own block instead of its body's. Until the bake lands it plays
/// its body's default gait, which is what it did before.
#[derive(Resource, Default)]
pub struct CrowdGaits {
    baked: HashMap<(AssetId<GeneratedBody>, AssetId<Gait>), BakedGait>,
    in_flight: Vec<(AssetId<GeneratedBody>, AssetId<Gait>, Task<BakedMotion>)>,
}

/// One body's custom gait in the atlas.
struct BakedGait {
    layout: ClipLayout,
    motion: Arc<BakedMotion>,
    /// The block of the atlas it holds, given back when it goes.
    block: Range<u32>,
}

impl CrowdGaits {
    /// The clips of `gait` on `body`, once baked.
    #[must_use]
    pub fn get(&self, body: AssetId<GeneratedBody>, gait: AssetId<Gait>) -> Option<(ClipLayout, &Arc<BakedMotion>)> {
        let baked = self.baked.get(&(body, gait))?;
        Some((baked.layout, &baked.motion))
    }

    /// The atlas block a crowd mob plays: its own gait's once that is baked, else its body's
    /// own (which `register_clips` put there when the body was generated).
    #[must_use]
    pub fn block_for<'a>(
        &'a self,
        skin: &CrowdSkin,
        body: AssetId<GeneratedBody>,
        gait: Option<AssetId<Gait>>,
        default: &'a Arc<BakedMotion>,
    ) -> Option<(ClipLayout, &'a Arc<BakedMotion>)> {
        gait.and_then(|g| self.get(body, g)).or_else(|| Some((skin.layout(body)?, default)))
    }

    /// Records a baked gait, for tests of the lookup.
    #[cfg(test)]
    fn insert_for_test(&mut self, body: AssetId<GeneratedBody>, gait: AssetId<Gait>, layout: ClipLayout, motion: Arc<BakedMotion>, block: Range<u32>) {
        self.baked.insert((body, gait), BakedGait { layout, motion, block });
    }

    fn known(&self, body: AssetId<GeneratedBody>, gait: AssetId<Gait>) -> bool {
        self.baked.contains_key(&(body, gait)) || self.in_flight.iter().any(|(b, g, _)| *b == body && *g == gait)
    }

    /// Drops every bake of `body`, or of `gait`, freeing the atlas blocks they hold.
    fn forget(&mut self, of: impl Fn(AssetId<GeneratedBody>, AssetId<Gait>) -> bool, skin: &mut CrowdSkin) {
        self.baked.retain(|(body, gait), baked| {
            let keep = !of(*body, *gait);
            if !keep {
                skin.release_range(baked.block.start, baked.block.end - baked.block.start);
            }
            keep
        });
    }
}

/// Bakes the gait of every crowd mob whose body hasn't met it yet, and lands finished ones in
/// the atlas. A mob without a [`MobGait`] needs nothing: its body's own gait is already there.
#[allow(clippy::type_complexity)]
pub(super) fn bake_crowd_gaits(
    mut crowd: ResMut<CrowdGaits>,
    mut gait_events: MessageReader<AssetEvent<Gait>>,
    mut body_events: MessageReader<AssetEvent<GeneratedBody>>,
    bodies: Res<Assets<GeneratedBody>>,
    gaits: Res<Assets<Gait>>,
    mut skin: ResMut<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
    mobs: Query<(&MobBody, &MobGait), (With<CrowdSlot>, Without<MobSkeleton>)>,
) {
    // A gait that reloaded, or a body that has gone, takes its bakes with it.
    let reloaded: Vec<AssetId<Gait>> = gait_events
        .read()
        .filter_map(|e| match e {
            AssetEvent::Modified { id } | AssetEvent::Removed { id } => Some(*id),
            _ => None,
        })
        .collect();
    if !reloaded.is_empty() {
        crowd.forget(|_, gait| reloaded.contains(&gait), &mut skin);
    }
    let gone: Vec<AssetId<GeneratedBody>> = body_events
        .read()
        .filter_map(|e| match e {
            AssetEvent::Removed { id } => Some(*id),
            _ => None,
        })
        .collect();
    if !gone.is_empty() {
        crowd.forget(|body, _| gone.contains(&body), &mut skin);
    }
    let crowd = &mut *crowd;
    // Finished bakes.
    let mut i = 0;
    while i < crowd.in_flight.len() {
        let Some((_, _, task)) = crowd.in_flight.get_mut(i) else { break };
        let Some(motion) = check_ready(task) else {
            i += 1;
            continue;
        };
        let (body, gait, _) = crowd.in_flight.swap_remove(i);
        // A body that went while this baked would hold a block nothing reads.
        let Some(b) = bodies.get(body) else { continue };
        let block = ClipBlock::new(&b.anatomy, &motion);
        let base = skin.append(block.data.clone(), &mut uploads);
        debug!("crowd: baked {} gait modes for body {body:?}", motion.clips.len());
        crowd.baked.insert(
            (body, gait),
            BakedGait { layout: block.layout_at(base), motion: Arc::new(motion), block: base..base + block.len() },
        );
    }
    // New bakes.
    for (body, mob_gait) in &mobs {
        if crowd.in_flight.len() >= MAX_BAKES {
            break;
        }
        let (id, gait_id) = (body.0.id(), mob_gait.0.id());
        if crowd.known(id, gait_id) {
            continue;
        }
        let (Some(b), Some(gait)) = (bodies.get(id), gaits.get(gait_id)) else { continue };
        let (anatomy, gait) = (b.anatomy.clone(), gait.clone());
        let task = AsyncComputeTaskPool::get().spawn(async move { bake_motion(&anatomy, &gait) });
        crowd.in_flight.push((id, gait_id, task));
    }
}

/// A mask row for the atlas: `driven` as 1s and 0s, padded to whole matrices.
fn mask_row(driven: &[bool]) -> impl Iterator<Item = [f32; 16]> + '_ {
    (0..ROW_MATRICES).map(move |m| std::array::from_fn(|i| f32::from(u8::from(driven.get(m * 16 + i).copied().unwrap_or(false)))))
}

/// The crowd slot entries that hang from a leg (the chain and whatever grows from it).
fn leg_entries(anatomy: &Anatomy) -> Vec<bool> {
    let skeleton = &anatomy.skeleton;
    let mut on_leg = vec![false; skeleton.len()];
    for (i, bone) in skeleton.bones.iter().enumerate() {
        let in_chain = anatomy.legs.iter().any(|l| l.chain.iter().any(|b| b.index() == i));
        let parent_on = bone.parent.and_then(|p| on_leg.get(p.index()).copied()).unwrap_or(false);
        if let Some(slot) = on_leg.get_mut(i) {
            *slot = in_chain || parent_on;
        }
    }
    skeleton.crowd.bones.iter().map(|b| on_leg.get(b.index()).copied().unwrap_or(false)).collect()
}

/// Bakes every (layer, cycle) a graph's states play on `anatomy`, where the cycle fits and isn't
/// a gait, into one block of atlas matrices.
///
/// A clip's mask row marks the entries it moves against the standing pose. A cycle that drives
/// neither the root nor the legs leaves the legs out of its row, so an upper-body action laid
/// over a walk lets the legs keep walking (a baked frame's legs would otherwise freeze under the
/// pitched spine). Additive layers are stored as their difference from the standing pose over
/// the idle, so the pass's `frame − idle` is that difference.
fn bake_graph(anatomy: &Anatomy, rig: &MotionRig, graph: &ComposedGraph, cycles: &[Option<Arc<Cycle>>]) -> Baked {
    let n = graph.cycles.len();
    let mut out = Baked { data: Vec::new(), cycles: n, clips: vec![None; graph.layers.len() * n] };
    let idle = idle_frame(anatomy);
    let standing = standing_frame(anatomy);
    let legs = leg_entries(anatomy);
    for (li, layer) in graph.layers.iter().enumerate() {
        let mask = rig.mask(&layer.mask);
        let mut wanted: Vec<usize> = Vec::new();
        for state in &layer.states {
            match &state.play {
                CPlay::Cycle(c) => wanted.push(*c),
                CPlay::Blend { points, .. } => wanted.extend(points.iter().map(|p| p.1)),
                CPlay::Empty | CPlay::Locomotion(_) => {}
            }
        }
        wanted.sort_unstable();
        wanted.dedup();
        for c in wanted {
            let Some(cycle) = cycles.get(c).and_then(Option::as_deref) else { continue };
            if !cycle.fits.check(&rig.traits).is_empty() {
                continue;
            }
            let Some(clip) = bake_cycle(anatomy, rig, cycle, &mask) else { continue };
            let additive = layer.mode == LayerMode::Additive;
            let moves_legs = cycle.tracks.iter().any(|t| matches!(t.target, Target::Root | Target::Legs(_)));
            let mut driven = clip.driven(&standing);
            if !moves_legs && !additive {
                for (d, on_leg) in driven.iter_mut().zip(&legs) {
                    *d &= !on_leg;
                }
            }
            let base = out.data.len().clamp_u32();
            for frame in &clip.frames {
                if additive {
                    out.data.extend(frame.iter().zip(&standing).zip(&idle).map(|((f, s), i)| {
                        let mut m = [0.0; 16];
                        for ((d, f), (s, i)) in m.iter_mut().zip(f).zip(s.iter().zip(i)) {
                            *d = f - s + i;
                        }
                        m
                    }));
                } else {
                    out.data.extend_from_slice(frame);
                }
            }
            let row = out.data.len().clamp_u32();
            out.data.extend(mask_row(&driven));
            if let Some(slot) = out.clips.get_mut(li * n + c) {
                *slot = Some((base, row, cycle.kind.loops(), additive));
            }
        }
    }
    out
}

/// Holes as a share of the atlas, in percent, at or above which [`compact_atlas`] runs.
///
/// Not a guess: the probe (`mob::crowd::tests::clip_atlas_fragmentation`) reports the residual
/// hole share for a session at each threshold, and the cost of compacting is one re-upload of
/// every block that moves. See the roadmap's Phase 2.5 entry for the figures.
const COMPACT_AT_PERCENT: u32 = 10;

/// Closes the clip atlas's holes by moving live blocks down, and remaps every atlas offset held
/// outside [`CrowdSkin`].
///
/// **The referrers, which is the whole difficulty.** A block's atlas offset is cached in five
/// places, and a compaction that misses one plays the wrong clips:
///
/// 1. `CrowdSkin::atlas` — a `ClipLayout` per body (`base`, and `legs` inside the block).
///    Remapped by `compact` itself.
/// 2. `CrowdSkin::chunks` — the CPU copy, keyed by base and kept sorted. Likewise.
/// 3. `CrowdSkin::free_clips` — the holes; emptied by definition.
/// 4. [`CrowdGaits`] — `BakedGait::layout` and `BakedGait::block`, for a mob whose gait is its
///    own. Remapped here.
/// 5. [`CrowdCycles`] — each bake's `Range<u32>` **and the absolute `base`/`mask` inside its
///    `Arc<GraphClips>`**, which were folded in when the bake landed (`base + frame0`). The
///    `Arc` is rebuilt here with the offsets shifted.
///
/// The per-mob `CrowdInstance`s are deliberately *not* in that list: `crowd_animate` rebuilds
/// each one from its layout every frame and uploads it when it differs from `CrowdUploaded`, so
/// they correct themselves the same frame. That is why this runs before it in the chain.
pub fn compact_atlas(
    mut skin: ResMut<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
    mut gaits: ResMut<CrowdGaits>,
    mut cycles: ResMut<CrowdCycles>,
) {
    let (atlas, holes) = skin.atlas_usage();
    if atlas == 0 || holes.saturating_mul(100) < atlas.saturating_mul(COMPACT_AT_PERCENT) {
        return;
    }
    let moves = skin.compact(&mut uploads);
    if moves.is_empty() {
        return;
    }
    debug!("crowd: compacted the clip atlas, {} blocks moved, now {} matrices", moves.len(), skin.atlas_usage().0);
    for baked in gaits.baked.values_mut() {
        let Some(to) = remap(&moves, baked.block.start) else { continue };
        let (delta, len) = (baked.block.start.saturating_sub(to), baked.block.end.saturating_sub(baked.block.start));
        baked.layout.base = baked.layout.base.saturating_sub(delta);
        baked.layout.legs = baked.layout.legs.saturating_sub(delta);
        baked.block = to..to + len;
    }
    for bakes in cycles.baked.values_mut() {
        for (_, clips, block) in bakes.iter_mut() {
            let Some(to) = remap(&moves, block.start) else { continue };
            let (delta, len) = (block.start.saturating_sub(to), block.end.saturating_sub(block.start));
            let shifted = clips
                .clips
                .iter()
                .map(|c| c.map(|c| CrowdClip { base: c.base.saturating_sub(delta), mask: c.mask.saturating_sub(delta), ..c }))
                .collect();
            *clips = Arc::new(GraphClips { cycles: clips.cycles, clips: shifted });
            *block = to..to + len;
        }
    }
}

/// Starts bakes for crowd mobs whose body hasn't met their graph yet, and lands finished ones in
/// the atlas.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn bake_graph_clips(
    mut crowd: ResMut<CrowdCycles>,
    mut graph_events: MessageReader<AssetEvent<MotionGraph>>,
    mut body_events: MessageReader<AssetEvent<GeneratedBody>>,
    bodies: Res<Assets<GeneratedBody>>,
    cycles: Res<Assets<Cycle>>,
    mut skin: ResMut<CrowdSkin>,
    mut uploads: ResMut<CrowdUploads>,
    mobs: Query<(&MobBody, &MotionRuntime), (With<CrowdSlot>, Without<MobSkeleton>)>,
) {
    if graph_events.read().any(|e| {
        matches!(e, AssetEvent::Added { .. } | AssetEvent::Modified { .. } | AssetEvent::Removed { .. } | AssetEvent::LoadedWithDependencies { .. })
    }) {
        crowd.forget(None, &mut skin);
        crowd.epoch = crowd.epoch.wrapping_add(1);
    }
    // A body that has gone takes its bakes with it, atlas blocks and all.
    for ev in body_events.read() {
        if let AssetEvent::Removed { id } = ev {
            crowd.forget(Some(*id), &mut skin);
        }
    }
    let crowd = &mut *crowd;
    // Finished bakes.
    let mut i = 0;
    while i < crowd.in_flight.len() {
        let Some((_, _, _, task)) = crowd.in_flight.get_mut(i) else { break };
        let Some(baked) = check_ready(task) else {
            i += 1;
            continue;
        };
        let (body, key, epoch, _) = crowd.in_flight.swap_remove(i);
        // Stale graphs, or a body that went while this baked: appending either would hold a
        // block of the atlas that nothing would ever read.
        if epoch != crowd.epoch || !bodies.contains(body) {
            continue;
        }
        let len = baked.data.len().clamp_u32();
        let base = skin.append(Arc::new(baked.data), &mut uploads);
        let clips = baked
            .clips
            .iter()
            .map(|c| c.map(|(frame0, mask, loops, additive)| CrowdClip { base: base + frame0, mask: base + mask, loops, additive }))
            .collect();
        let clips = Arc::new(GraphClips { cycles: baked.cycles, clips });
        debug!("crowd: baked {} graph cycles for body {body:?}", clips.len());
        crowd.baked.entry(body).or_default().push((key, clips, base..base + len));
    }
    // New bakes.
    for (body, rt) in &mobs {
        if crowd.in_flight.len() >= MAX_BAKES {
            break;
        }
        let Some((key, graph)) = rt.composed() else { continue };
        let Some(rig) = rt.rig() else { continue };
        let id = body.0.id();
        if crowd.known(id, key) {
            continue;
        }
        let Some(b) = bodies.get(id) else { continue };
        // Every cycle in hand (asset cycles still loading bring us back next frame).
        let list: Option<Vec<Option<Arc<Cycle>>>> = graph
            .cycles
            .iter()
            .map(|src| match src {
                super::graph::CycleSource::Inline(c) => Some(Some(c.clone())),
                super::graph::CycleSource::Asset(h) => cycles.get(h).map(|c| Some(Arc::new(c.clone()))),
            })
            .collect();
        let Some(list) = list else { continue };
        let (anatomy, rig, graph) = (b.anatomy.clone(), rig.clone(), graph.clone());
        let task = AsyncComputeTaskPool::get().spawn(async move { bake_graph(&anatomy, &rig, &graph, &list) });
        crowd.in_flight.push((id, key.to_vec(), crowd.epoch, task));
    }
}

/// The frame pair of a clip at time `t` (0..1) and frame B's share.
fn frame_pair(clip: CrowdClip, bones: u32, t: f32) -> (u32, u32, f32) {
    let n = BAKE_FRAMES.clamp_u32();
    let (f0, f1, tt) = if clip.loops {
        let at = t.rem_euclid(1.0) * n.to_f32();
        let f0 = at.floor().sat_u32() % n;
        (f0, (f0 + 1) % n, at.fract())
    } else {
        let at = t.clamp(0.0, 1.0) * (n - 1).to_f32();
        let f0 = at.floor().sat_u32().min(n - 1);
        (f0, (f0 + 1).min(n - 1), at.fract())
    };
    (clip.base + f0 * bones, clip.base + f1 * bones, tt)
}

/// One graph layer's playing cycles, gathered: the layer, its summed weight, and its heaviest
/// cycle as `(weight, cycle, time)`.
type Playing = (usize, f32, Option<(f32, usize, f32)>);

/// The graph layers a crowd mob shows this frame: per graph layer its heaviest playing cycle
/// with a clip, at the layer's summed weight. When more play than fit, overriding layers come
/// before additive ones (an action before breathing), then the heavier, then the higher.
fn graph_layers(rt: &MotionRuntime, graph: &ComposedGraph, clips: &GraphClips, bones: u32, instance: &mut CrowdInstance) {
    let mut layers: Vec<(f32, CrowdLayer)> = Vec::new();
    let mut current: Option<Playing> = None;
    let finish = |current: Option<Playing>, layers: &mut Vec<(f32, CrowdLayer)>| {
        let Some((li, total, Some((_, c, t)))) = current else { return };
        let (Some(clip), Some(layer)) = (clips.get(li, c), graph.layers.get(li)) else { return };
        let weight = (total * layer.weight).clamp(0.0, 1.0);
        if weight <= 0.0 {
            return;
        }
        let (a, b, tt) = frame_pair(clip, bones, t);
        let mask = if clip.additive { clip.mask | LAYER_ADDITIVE } else { clip.mask };
        layers.push((weight, CrowdLayer::new(a, b, tt, weight, mask)));
    };
    for (li, c, t, w) in rt.timed_actives() {
        if clips.get(li, c).is_none() {
            continue;
        }
        match &mut current {
            Some((l, total, best)) if *l == li => {
                *total += w;
                if best.is_none_or(|(bw, _, _)| w > bw) {
                    *best = Some((w, c, t));
                }
            }
            _ => {
                finish(current.take(), &mut layers);
                current = Some((li, w, Some((w, c, t))));
            }
        }
    }
    finish(current, &mut layers);
    // Room for the heaviest, in layer order.
    let room = CROWD_LAYERS - 1;
    if layers.len() > room {
        let mut order: Vec<usize> = (0..layers.len()).collect();
        let rank = |i: usize| layers.get(i).map_or((false, 0.0, 0), |l| (l.1.mask & LAYER_ADDITIVE == 0, l.0, i));
        order.sort_by(|a, b| {
            let (oa, wa, ia) = rank(*a);
            let (ob, wb, ib) = rank(*b);
            ob.cmp(&oa).then(wb.total_cmp(&wa)).then(ib.cmp(&ia))
        });
        order.truncate(room);
        order.sort_unstable();
        let kept: Vec<(f32, CrowdLayer)> = order.iter().filter_map(|i| layers.get(*i).copied()).collect();
        layers = kept;
    }
    for (_, layer) in layers {
        instance.push(layer);
    }
}

/// Non-hero mobs: queue changed blend instructions for the phase the clock systems advanced.
#[allow(clippy::type_complexity)]
pub(super) fn crowd_animate(
    bodies: Res<Assets<GeneratedBody>>,
    skin: Res<CrowdSkin>,
    crowd: Res<CrowdCycles>,
    custom: Res<CrowdGaits>,
    mut uploads: ResMut<CrowdUploads>,
    mut mobs: Query<(&MobBody, &CrowdSlot, &Locomotion, &GaitClock, Option<&MobGait>, Option<&MotionRuntime>, Option<&CrowdGround>, &mut CrowdUploaded), Without<MobSkeleton>>,
    mut changed: Local<Parallel<Vec<(u32, CrowdInstance)>>>,
) {
    mobs.par_iter_mut().for_each(|(body, slot, loco, clock, gait, rt, ground, mut uploaded)| {
        let Some(b) = bodies.get(&body.0) else { return };
        // A mob with a gait of its own plays that gait's block, once it is baked; until then,
        // and without one, its body's own.
        let Some((layout, motion)) = custom.block_for(&skin, body.0.id(), gait.map(|g| g.0.id()), &b.motion) else { return };
        let leg = leg_length(&b.anatomy);
        let idle = 1.0 - (loco.velocity.length() / leg / 0.3).clamp(0.0, 1.0);
        let mut instance = layout.instance(motion, clock.mode.min(motion.clips.len().saturating_sub(1)), clock.phase, idle);
        if let Some(rt) = rt
            && let Some((key, graph)) = rt.composed()
            && let Some(clips) = crowd.get(body.0.id(), key)
        {
            graph_layers(rt, graph, clips, layout.bones, &mut instance);
        }
        if let Some(g) = ground.filter(|g| !g.is_flat()) {
            instance.ground(g.lift, g.tilt, &g.feet);
        }
        if instance != uploaded.0 {
            uploaded.0 = instance;
            changed.borrow_local_mut().push((slot.0, instance));
        }
    });
    changed.drain_into(&mut uploads.instances);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::build_src;
    use crate::motion::{compose, idle_frame, library};

    /// Every bake holds a block of the clip atlas, and the graphs reloading drops every bake.
    /// Those blocks have to go back, or editing a graph in a running game leaks the whole baked
    /// set each time it is saved.
    #[test]
    fn forgotten_bakes_give_their_atlas_blocks_back() {
        let mut skin = CrowdSkin::for_test();
        let mut uploads = CrowdUploads::default();
        let mut crowd = CrowdCycles::default();
        let body = |n: u128| AssetId::<GeneratedBody>::Uuid { uuid: bevy::asset::uuid::Uuid::from_u128(n) };
        let mut bake = |to: AssetId<GeneratedBody>, len: usize, crowd: &mut CrowdCycles| {
            let base = skin.append(Arc::new(vec![[0.0; 16]; len]), &mut uploads);
            crowd.baked.entry(to).or_default().push((Vec::new(), Arc::new(GraphClips::default()), base..base + len.clamp_u32()));
        };
        bake(body(1), 8, &mut crowd);
        bake(body(2), 4, &mut crowd);
        bake(body(1), 6, &mut crowd);
        assert_eq!(skin.atlas_usage(), (18, 0));

        // One body goes: its two blocks go, the other body's stays.
        crowd.forget(Some(body(1)), &mut skin);
        assert_eq!(skin.atlas_usage(), (12, 8), "the tail block shortened the atlas, the first is a hole");
        assert!(crowd.baked.get(&body(1)).is_none() && crowd.baked.contains_key(&body(2)));
        // The graphs reload: everything goes and the atlas is empty again.
        crowd.forget(None, &mut skin);
        assert_eq!(skin.atlas_usage(), (0, 0));
        assert!(crowd.baked.is_empty());
    }

    #[test]
    fn frame_pairs_loop_or_stop() {
        let clip = CrowdClip { base: 100, mask: 0, loops: true, additive: false };
        let n = BAKE_FRAMES.clamp_u32();
        assert_eq!(frame_pair(clip, 10, 0.0), (100, 110, 0.0));
        let (a, b, t) = frame_pair(clip, 10, 1.0 - 0.5 / n.to_f32());
        assert_eq!((a, b), (100 + (n - 1) * 10, 100), "the last frame wraps to the first");
        assert!((t - 0.5).abs() < 1e-4);
        let once = CrowdClip { loops: false, ..clip };
        assert_eq!(frame_pair(once, 10, 1.0), (100 + (n - 1) * 10, 100 + (n - 1) * 10, 0.0), "an action holds its end");
        assert_eq!(frame_pair(once, 10, 0.0), (100, 110, 0.0));
    }

    /// The standard graph on a quadruped: every idle and action its states play and that fits
    /// gets a clip and a mask row, gaits and misfits don't, and the block is laid out as the
    /// atlas expects.
    #[test]
    fn the_standard_graph_bakes_for_a_quadruped() {
        let a = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let rig = MotionRig::new(&a);
        let g = library::standard();
        let composed = compose(&[&g]).unwrap();
        let cycles: Vec<Option<Arc<Cycle>>> = composed.cycles.iter().map(|c| match c {
            super::super::graph::CycleSource::Inline(c) => Some(c.clone()),
            super::super::graph::CycleSource::Asset(_) => None,
        }).collect();
        let baked = bake_graph(&a, &rig, &composed, &cycles);
        let name = |c: usize| cycles.get(c).and_then(Option::as_deref).map_or("?", |c| c.name.as_str());
        let stride = idle_frame(&a).len().clamp_u32();
        let mut have: Vec<(&str, &str)> = Vec::new();
        for (li, layer) in composed.layers.iter().enumerate() {
            for c in 0..composed.cycles.len() {
                if let Some((frame0, row, loops, additive)) = baked.clips.get(li * baked.cycles + c).copied().flatten() {
                    have.push((layer.name.as_str(), name(c)));
                    assert_eq!(row, frame0 + BAKE_FRAMES.clamp_u32() * stride, "frames then the row");
                    assert_eq!(additive, layer.mode == LayerMode::Additive);
                    assert_eq!(loops, !matches!(cycles.get(c).and_then(Option::as_deref).map(|c| &c.kind), Some(CycleKind::Action { .. })));
                }
            }
        }
        assert!(have.contains(&("head", "look_around")), "{have:?}");
        assert!(have.contains(&("breath", "breathe")), "{have:?}");
        assert!(have.contains(&("action", "shake")), "{have:?}");
        assert!(!have.iter().any(|(_, n)| *n == "wave" || *n == "flare"), "biped and winged actions don't fit: {have:?}");
        assert!(!have.iter().any(|(_, n)| n.contains("walk") || n.contains("trot")), "gaits aren't baked: {have:?}");
        let per_clip = BAKE_FRAMES.clamp_u32() * stride + ROW_MATRICES.clamp_u32();
        assert_eq!(baked.data.len().clamp_u32(), per_clip * have.len().clamp_u32());
    }
}

#[cfg(test)]
mod blend_probe {
    use super::*;
    use crate::anatomy::build_src;
    use crate::mob::blend_with;
    use crate::num::Idx;
    use crate::motion::{compose, idle_frame, library};

    /// Every matrix the pass would write for a quadruped shaking (and breathing) is finite and
    /// nearly rigid, so the skin's dual-quaternion conversion can take it.
    #[test]
    fn layered_matrices_stay_rigid() {
        let a = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let rig = MotionRig::new(&a);
        let g = library::standard();
        let composed = compose(&[&g]).unwrap();
        let cycles: Vec<Option<Arc<Cycle>>> = composed.cycles.iter().map(|c| match c {
            super::super::graph::CycleSource::Inline(c) => Some(c.clone()),
            super::super::graph::CycleSource::Asset(_) => None,
        }).collect();
        let baked = bake_graph(&a, &rig, &composed, &cycles);
        let idle = idle_frame(&a);
        let stride = idle.len().clamp_u32();
        // The atlas: the idle, then the baked block.
        let atlas: Vec<[f32; 16]> = idle.iter().copied().chain(baked.data.iter().copied()).collect();
        let lookup = |i: u32| atlas.get(i.idx()).copied();
        let find = |layer: &str, cycle: &str| {
            let li = composed.layers.iter().position(|l| l.name == layer).unwrap();
            let c = cycles.iter().position(|c| c.as_deref().is_some_and(|c| c.name == cycle)).unwrap();
            let (f0, row, loops, additive) = baked.clips.get(li * baked.cycles + c).copied().flatten().unwrap();
            CrowdClip { base: stride + f0, mask: stride + row, loops, additive }
        };
        let shake = find("action", "shake");
        let breathe = find("breath", "breathe");
        for t in [0.0, 0.1, 0.3, 0.5, 0.9] {
            let mut inst = CrowdInstance { idle: 0, bones: stride, ..default() };
            let (a1, b1, tt) = frame_pair(breathe, stride, t);
            inst.push(CrowdLayer::new(a1, b1, tt, 1.0, breathe.mask | LAYER_ADDITIVE));
            let (a2, b2, t2) = frame_pair(shake, stride, t);
            inst.push(CrowdLayer::new(a2, b2, t2, 0.7, shake.mask));
            for bone in 0..stride {
                let m = blend_with(&inst, bone, lookup).unwrap();
                assert!(m.is_finite(), "t {t} bone {bone}: {m}");
                let (s, _, _) = m.to_scale_rotation_translation();
                assert!(s.min_element() > 0.7 && s.max_element() < 1.3, "t {t} bone {bone}: scale {s}");
            }
        }
    }
}

#[cfg(test)]
mod gaits {
    use super::*;
    use crate::anatomy::build_src;
    use crate::num::Idx;

    /// A mob's own gait becomes its own block of the atlas, laid out for its own modes — not the
    /// body's default, which is what a crowd mob used to play whatever gait it was given.
    #[test]
    fn a_custom_gait_bakes_into_a_block_of_its_own() {
        let a = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let default = Gait::for_legs(a.legs.len());
        // The same gait with its faster modes taken away: one mode instead of several, and a
        // longer stride, so the block and the walk both differ from the body's.
        let mut slow = default.clone();
        slow.modes.truncate(1);
        if let Some(mode) = slow.modes.first_mut() {
            mode.stride *= 1.5;
        }

        let body = ClipBlock::new(&a, &bake_motion(&a, &default));
        let own = ClipBlock::new(&a, &bake_motion(&a, &slow));
        assert_eq!(own.layout_at(0).modes, 1, "the custom block holds the custom gait's modes");
        assert!(body.layout_at(0).modes > own.layout_at(0).modes, "the default has more modes");
        assert!(own.len() < body.len(), "fewer modes, fewer matrices");

        // Laid out at wherever the atlas puts it: the clips and the leg rows move with the base.
        let (here, there) = (own.layout_at(0), own.layout_at(1000));
        assert_eq!(there.base, 1000);
        assert_eq!(there.legs, here.legs + 1000);
        assert_eq!((there.bones, there.modes), (here.bones, here.modes));
        // And the block really holds a different walk: the first clip's frames differ.
        let frame = |block: &ClipBlock, i: usize| block.data.get(here.bones.idx() + i).copied().unwrap_or_default();
        assert!(
            (0..16).any(|i| (frame(&own, i)[13] - frame(&body, i)[13]).abs() > 1e-4),
            "the custom gait's first frame matches the default's"
        );
    }

    /// Which block a crowd mob plays: its own gait's when that is baked, its body's otherwise —
    /// including while the bake is still running, which is what it used to do always.
    #[test]
    fn a_mob_plays_its_own_gait_only_once_it_is_baked() {
        let a = build_src(include_str!("../../assets/mobs/quadruped.body.ron"));
        let (mut skin, mut uploads) = (CrowdSkin::for_test(), CrowdUploads::default());
        let mut gaits = CrowdGaits::default();
        let id = |n: u128| bevy::asset::uuid::Uuid::from_u128(n);
        let body = AssetId::<GeneratedBody>::Uuid { uuid: id(1) };
        let slow = AssetId::<Gait>::Uuid { uuid: id(2) };
        let default = Arc::new(bake_motion(&a, &Gait::for_legs(a.legs.len())));

        // Nothing in the atlas yet: nothing to play.
        assert!(gaits.block_for(&skin, body, None, &default).is_none());
        // The body's own block lands when it is generated.
        let own_block = ClipBlock::new(&a, &default);
        let base = skin.append(own_block.data.clone(), &mut uploads);
        skin.set_layout(body, own_block.layout_at(base));
        let (layout, motion) = gaits.block_for(&skin, body, None, &default).expect("the body's block");
        assert_eq!(layout.base, base);
        // A mob with a gait that hasn't baked yet still plays the body's.
        let (waiting, _) = gaits.block_for(&skin, body, Some(slow), &default).expect("the body's block");
        assert_eq!(waiting.base, layout.base);
        assert_eq!(motion.clips.len(), default.clips.len());

        // Once it is baked, the mob plays its own block, with its own modes.
        let mut only_walk = Gait::for_legs(a.legs.len());
        only_walk.modes.truncate(1);
        let baked = Arc::new(bake_motion(&a, &only_walk));
        let block = ClipBlock::new(&a, &baked);
        let at = skin.append(block.data.clone(), &mut uploads);
        gaits.insert_for_test(body, slow, block.layout_at(at), baked, at..at + block.len());
        let (own, motion) = gaits.block_for(&skin, body, Some(slow), &default).expect("its own block");
        assert_eq!(own.base, at);
        assert_eq!(own.modes, 1);
        assert_eq!(motion.clips.len(), 1);
        // The gait reloads: its block goes back to the atlas and the mob falls back again.
        gaits.forget(|_, g| g == slow, &mut skin);
        assert_eq!(gaits.block_for(&skin, body, Some(slow), &default).expect("the body's block").0.base, base);
        assert_eq!(skin.atlas_usage(), (base + own_block.len(), 0), "the custom block was the atlas's tail");
    }
}
