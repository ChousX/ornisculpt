//! Surface building blocks emitted by parts. Consumed by the `surface` module's SDF.

use std::sync::Arc;

use bevy::prelude::*;

use super::{BoneId, HardParams, LodLevel, RegionId, SdfGrid, TriMesh};

/// One SDF primitive, owned by a bone. Its owning bone is also its skin-weight source.
#[derive(Clone, Debug, PartialEq)]
pub struct Primitive {
    pub bone: BoneId,
    pub shape: PrimShape,
    /// Relative to the owning bone.
    pub local: Transform,
    /// Smooth-min radius against everything already in the field.
    pub blend: f32,
    /// If set, `blend` only applies near this point (primitive-local) and fades to a hard union
    /// at `reach`. Keeps limbs from webbing onto bodies they hang beside.
    pub blend_anchor: Option<(Vec3, f32)>,
    pub op: PrimOp,
    pub layer: PrimLayer,
    pub region: RegionId,
    /// Coarsest level this primitive appears in (also gated by its part's LOD).
    pub lod: LodLevel,
    /// How [`measure_mass`](super::measure_mass) weighs it where it overlaps its neighbours.
    pub weigh: Weigh,
    /// Soft-tissue tuning from its stamp; `None` for a part's own shapes (only stamps become soft
    /// tissue: a part's built-in bulge, a torso's belly, is body).
    pub soft: Option<super::SoftTune>,
}

/// Overlap correction for weighing a primitive (the smooth union counts every volume once; the
/// mass sums primitives).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Weigh {
    /// Its whole volume.
    #[default]
    Whole,
    /// A round cone continuing the previous piece of the same section: the sphere where they
    /// join (its start cap and the previous piece's end cap) is already weighed.
    Continues,
    /// Only this fraction of its volume lies outside the shapes it is blended onto.
    Fraction(f32),
}

/// Shapes are aligned to local +Y where they have an axis.
#[derive(Clone, Debug, PartialEq)]
pub enum PrimShape {
    Sphere { r: f32 },
    Ellipsoid { radii: Vec3 },
    RoundCone { r0: f32, r1: f32, len: f32 },
    /// Rounded box (snouts, hooves, blocky masses).
    RoundBox { half: Vec3, round: f32 },
    /// Authored shape sampled from a grid; `mirror_x` samples it reflected (right-side copies).
    Grid { grid: Arc<SdfGrid>, mirror_x: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrimOp {
    Union,
    Subtract,
    /// Pushes the surface out by `height` (in, if negative) inside an [`PrimShape::Ellipsoid`]
    /// footprint, fading to nothing at its rim: `d − height·(1 − |q/radii|²)²`. Stamps use it: a
    /// smooth union with a shape lying nearly flush with the surface inflates the whole footprint
    /// by a quarter of the blend radius and leaves a ledge round its rim.
    Displace { height: f32 },
}

/// Which tone layer a primitive belongs to (scaled by [`Tone`](super::Tone)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimLayer {
    Base,
    Muscle,
    Landmark,
    Fat,
}

/// A non-SDF mesh piece (claw, horn, beak, tooth, eye), 100% weighted to its bone.
#[derive(Clone, Debug, PartialEq)]
pub struct HardPart {
    pub bone: BoneId,
    pub local: Transform,
    pub params: HardParams,
    pub region: RegionId,
    /// Resolved mesh for `HardKind::Mesh`.
    pub mesh: Option<Arc<TriMesh>>,
    /// Right-side copy: geometry must be reflected across local X.
    pub mirror: bool,
}
