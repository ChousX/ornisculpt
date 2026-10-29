//! Parts with an authored mesh (sculpts and mesh hard parts) as glTF mesh nodes.

use bevy::prelude::*;
use xxhash_rust::xxh3::xxh3_64;

use crate::anatomy::{HardKind, Orient, PartKind, TriMesh};

/// The authored mesh of a part kind that has one: its asset path, orientation and scale.
#[must_use]
pub const fn authored(kind: &PartKind) -> Option<(&str, Orient, f32)> {
    match kind {
        PartKind::Sculpt(s) => Some((s.mesh.as_str(), s.orient, s.scale)),
        PartKind::Hard(h) => match &h.kind {
            HardKind::Mesh { path, orient, scale } => Some((path.as_str(), *orient, *scale)),
            _ => None,
        },
        _ => None,
    }
}

/// Points a part kind's authored mesh at `path`, at scale 1. `false` for kinds without one.
pub fn set_authored(kind: &mut PartKind, path: String) -> bool {
    match kind {
        PartKind::Sculpt(s) => {
            s.mesh = path;
            s.scale = 1.0;
            true
        }
        PartKind::Hard(h) => match &mut h.kind {
            HardKind::Mesh { path: p, scale, .. } => {
                *p = path;
                *scale = 1.0;
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// Multiplies a part kind's authored mesh scale by `k`. `false` for kinds without one.
pub fn scale_authored(kind: &mut PartKind, k: f32) -> bool {
    match kind {
        PartKind::Sculpt(s) => {
            s.scale *= k;
            true
        }
        PartKind::Hard(h) => match &mut h.kind {
            HardKind::Mesh { scale, .. } => {
                *scale *= k;
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether a part kind's mesh becomes an SDF (and so needs a closed, welded surface).
#[must_use]
pub const fn is_sculpt(kind: &PartKind) -> bool {
    matches!(kind, PartKind::Sculpt(_))
}

/// The frame a part's offset is applied in, from where it attaches: the attach frame, or (an
/// authored mesh with [`Orient::Model`]) just its position, as the part builds.
#[must_use]
pub const fn base_frame(kind: &PartKind, attach: Transform) -> Transform {
    match authored(kind) {
        Some((_, Orient::Model, _)) => Transform::from_translation(attach.translation),
        _ => attach,
    }
}

/// A mesh's geometry, independent of vertex order and of vertices split at seams (which DCCs
/// change on export): its distinct positions to 10 µm, and its triangle count.
#[must_use]
pub fn fingerprint(positions: &[Vec3], triangles: usize) -> u64 {
    let mut cells: Vec<IVec3> = positions.iter().map(|p| (*p * 1e5).round().as_ivec3()).collect();
    cells.sort_unstable_by_key(|c| (c.x, c.y, c.z));
    cells.dedup();
    let mut bytes: Vec<u8> = Vec::with_capacity(cells.len() * 12 + 8);
    for c in &cells {
        for v in c.to_array() {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&u64::try_from(triangles).unwrap_or(u64::MAX).to_le_bytes());
    xxh3_64(&bytes)
}

/// `mesh` with vertices at the same position (to a micrometre) merged and the triangles that
/// collapse dropped: a sculpt's SDF sign needs a closed surface, which seams in the UVs or
/// normals break.
#[must_use]
pub fn weld(mesh: &TriMesh) -> TriMesh {
    let mut cells: std::collections::HashMap<IVec3, u32> = std::collections::HashMap::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let remap: Vec<u32> = mesh
        .positions
        .iter()
        .map(|p| {
            *cells.entry((*p * 1e6).round().as_ivec3()).or_insert_with(|| {
                positions.push(*p);
                u32::try_from(positions.len() - 1).unwrap_or(u32::MAX)
            })
        })
        .collect();
    let at = |i: u32| usize::try_from(i).ok().and_then(|i| remap.get(i)).copied();
    let indices: Vec<u32> = mesh
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|t| {
            let [a, b, c] = [at(t[0])?, at(t[1])?, at(t[2])?];
            (a != b && b != c && a != c).then_some([a, b, c])
        })
        .flatten()
        .collect();
    TriMesh::new(positions, Vec::new(), indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit quad as two triangles, split along the diagonal into 6 vertices, as a seam would.
    fn split_quad() -> TriMesh {
        let p = [Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y];
        TriMesh::new(p.to_vec(), Vec::new(), vec![0, 1, 2, 3, 4, 5])
    }

    #[test]
    fn weld_merges_seams() {
        let w = weld(&split_quad());
        assert_eq!(w.positions.len(), 4);
        assert_eq!(w.indices.len(), 6);
    }

    #[test]
    fn fingerprint_ignores_order_and_splits() {
        let split = split_quad();
        let welded = weld(&split);
        assert_eq!(fingerprint(&split.positions, 2), fingerprint(&welded.positions, 2));
        let mut moved = welded.positions.clone();
        moved[0].z += 0.01;
        assert_ne!(fingerprint(&moved, 2), fingerprint(&welded.positions, 2));
    }
}
