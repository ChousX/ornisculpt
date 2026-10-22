use std::ops::Range;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::VertexFormat;

use crate::anatomy::{BoneId, PrimLayer, RegionId};
use crate::num::{Clamp, Idx};

/// Per-vertex Ornisculpt data, packed into two `u32`s on the GPU:
/// - x: `region | cavity << 8 | curvature << 16 | limb_t << 24`
/// - y: `primary_bone | layer << 8` (upper 16 bits reserved)
///
/// Bone-local position is not stored; shaders rebuild it from `primary_bone` and the bind pose.
pub const ATTRIBUTE_MOB_DATA: MeshVertexAttribute =
    MeshVertexAttribute::new("Ornisculpt_MobData", 0x0b5c_0001, VertexFormat::Uint32x2);

/// Crowd meshes carry joints under their own names: Bevy treats any mesh with the standard joint
/// attributes as a `SkinnedMesh`, while crowd meshes are skinned by the crowd shader instead.
pub const ATTRIBUTE_CROWD_JOINT_INDEX: MeshVertexAttribute =
    MeshVertexAttribute::new("Ornisculpt_CrowdJointIndex", 0x0b5c_0002, VertexFormat::Uint16x4);
pub const ATTRIBUTE_CROWD_JOINT_WEIGHT: MeshVertexAttribute =
    MeshVertexAttribute::new("Ornisculpt_CrowdJointWeight", 0x0b5c_0003, VertexFormat::Float32x4);
/// Per-vertex centre of rotation ([`BodyMeshData::cor`]).
pub const ATTRIBUTE_COR: MeshVertexAttribute = MeshVertexAttribute::new("Ornisculpt_CentreOfRotation", 0x0b5c_0004, VertexFormat::Float32x3);
/// Muscle flex ([`MuscleFlex`]): rest-pose insertion (xyz) and amplitude (w).
pub const ATTRIBUTE_MUSCLE: MeshVertexAttribute = MeshVertexAttribute::new("Ornisculpt_Muscle", 0x0b5c_0005, VertexFormat::Float32x4);
/// Muscle flex: the joint carrying the insertion (x) and the muscle's length (y, `f32` bits).
pub const ATTRIBUTE_MUSCLE_JOINT: MeshVertexAttribute = MeshVertexAttribute::new("Ornisculpt_MuscleJoint", 0x0b5c_0006, VertexFormat::Uint32x2);

/// Soft tissue ([`SoftVertex`]): two (joints, weight bits) pairs.
pub const ATTRIBUTE_SOFT: MeshVertexAttribute = MeshVertexAttribute::new("Ornisculpt_Soft", 0x0b5c_0007, VertexFormat::Uint32x4);

/// A vertex's share of up to two soft-tissue regions (see `soft::soft_weights`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SoftVertex {
    /// Per region: its soft joint (low 16 bits) and its bone's joint (high 16 bits).
    pub joints: [u32; 2],
    /// 0 = no region.
    pub weights: [f32; 2],
}

/// Per-vertex muscle flex (see [`muscle_flex`](super::muscle_flex)).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MuscleFlex {
    /// The muscle's insertion in the rest pose (model space), carried by `joint`.
    pub insertion: Vec3,
    /// Outward push per unit of contraction (metres); 0 = no muscle under the vertex.
    pub amplitude: f32,
    /// Rest length of the muscle (belly centre to insertion, metres): contraction is the vertex's
    /// shortening towards the insertion over this, so it stays gentle next to the insertion.
    pub length: f32,
    /// Joint carrying the insertion (crowd skeleton index at crowd levels).
    pub joint: u16,
}

/// [`BodyMeshData::cloth`] value of vertices that are not part of a garment.
pub const NOT_CLOTH: u8 = 255;

/// Unpacked form of [`ATTRIBUTE_MOB_DATA`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MobVertexData {
    pub region: RegionId,
    /// Ambient-occlusion-like concavity, 0 = open, 255 = deep crease.
    pub cavity: u8,
    /// Signed curvature remapped to 0..=255 (128 = flat).
    pub curvature: u8,
    /// Position along the owning limb/chain, 0 = root, 255 = tip.
    pub limb_t: u8,
    pub primary_bone: BoneId,
    pub layer: PrimLayer,
}

impl MobVertexData {
    #[must_use]
    pub const fn pack(&self) -> [u32; 2] {
        let layer = match self.layer {
            PrimLayer::Base => 0u8,
            PrimLayer::Muscle => 1,
            PrimLayer::Landmark => 2,
            PrimLayer::Fat => 3,
        };
        [
            u32::from_le_bytes([self.region.0, self.cavity, self.curvature, self.limb_t]),
            u32::from_le_bytes([self.primary_bone.0, layer, 0, 0]),
        ]
    }

    #[must_use]
    pub const fn unpack([x, y]: [u32; 2]) -> Self {
        let [region, cavity, curvature, limb_t] = x.to_le_bytes();
        let [bone, layer, _, _] = y.to_le_bytes();
        Self {
            region: RegionId(region),
            cavity,
            curvature,
            limb_t,
            primary_bone: BoneId(bone),
            layer: match layer {
                1 => PrimLayer::Muscle,
                2 => PrimLayer::Landmark,
                3 => PrimLayer::Fat,
                _ => PrimLayer::Base,
            },
        }
    }
}

/// Which material a range of indices uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MaterialSlot {
    /// SDF body: coat shading.
    Skin,
    /// Claws, horns, teeth, beaks.
    Hard,
    Eye,
    /// Garments: colour from the outfit palette (index in vertex colour w).
    Cloth,
    /// Wing sheets: membranes, flight feathers, insect wings (sheet kind in vertex colour w).
    Wing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submesh {
    pub indices: Range<u32>,
    pub slot: MaterialSlot,
}

/// A finished, skinned mesh for one LOD level, before upload.
///
/// Joint indices refer to the full skeleton at `Hero` and to the crowd skeleton at coarser
/// levels (see [`remap_to_crowd`](super::remap_to_crowd)).
#[derive(Clone, Debug, Default)]
pub struct BodyMeshData {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    /// Matches Bevy's `ATTRIBUTE_JOINT_INDEX` (Uint16x4).
    pub joints: Vec<[u16; 4]>,
    /// Matches Bevy's `ATTRIBUTE_JOINT_WEIGHT` (Float32x4).
    pub weights: Vec<[f32; 4]>,
    /// Packed [`MobVertexData`].
    pub mob_data: Vec<[u32; 2]>,
    /// Optional UVs (fur shell height, feather card coordinates); empty for body meshes.
    pub uvs: Vec<[f32; 2]>,
    /// Coat coverage weights per vertex: (scales, fur, feathers). Empty = bare skin.
    pub coverage: Vec<[f32; 3]>,
    /// Coat flow per vertex: direction (xyz, model space, unit), the way fur lies and feathers
    /// point; w = distance along the flow in metres (skin only). Empty = no coat coverings.
    pub flow: Vec<[f32; 4]>,
    /// Outfit palette index per vertex ([`NOT_CLOTH`] for body vertices). Empty = no garments.
    pub cloth: Vec<u8>,
    /// Optimized centre of rotation per vertex (rest pose; see `surface::centres_of_rotation`).
    /// Empty = skin with dual quaternions instead.
    pub cor: Vec<Vec3>,
    /// Muscle flex per vertex. Empty = no muscles move.
    pub muscle: Vec<MuscleFlex>,
    /// Soft-tissue weights per vertex. Empty = nothing jiggles.
    pub soft: Vec<SoftVertex>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<Submesh>,
}

impl BodyMeshData {
    #[must_use]
    pub const fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    #[must_use]
    pub const fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Builds a Bevy mesh with position, normal, joint index/weight and [`ATTRIBUTE_MOB_DATA`].
    /// Submeshes stay in one index buffer; the material system splits by range.
    #[must_use]
    pub fn to_bevy_mesh(&self) -> Mesh {
        self.build_mesh(false)
    }

    /// Like [`to_bevy_mesh`](Self::to_bevy_mesh) but with joints under the crowd attribute names.
    #[must_use]
    pub fn to_crowd_mesh(&self) -> Mesh {
        self.build_mesh(true)
    }

    fn build_mesh(&self, crowd: bool) -> Mesh {
        let (joints, weights) = if crowd {
            (ATTRIBUTE_CROWD_JOINT_INDEX, ATTRIBUTE_CROWD_JOINT_WEIGHT)
        } else {
            (Mesh::ATTRIBUTE_JOINT_INDEX, Mesh::ATTRIBUTE_JOINT_WEIGHT)
        };
        // Coat inputs: colour = rest-pose position + scale weight; with coverings, tangent = flow.
        // UV0 = layer coordinates on shells and cards, (fur, feather) weights on the skin.
        let n = self.positions.len();
        let cover = |i: usize| self.coverage.get(i).copied().unwrap_or([0.0; 3]);
        let w = |i: usize| match self.cloth.get(i) {
            Some(&c) if c != NOT_CLOTH => f32::from(c),
            _ => cover(i)[0],
        };
        let rest: Vec<[f32; 4]> = self.positions.iter().enumerate().map(|(i, p)| [p.x, p.y, p.z, w(i)]).collect();
        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
            .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals.clone())
            .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, rest)

            .with_inserted_attribute(joints, VertexAttributeValues::Uint16x4(self.joints.clone()))
            .with_inserted_attribute(weights, self.weights.clone())
            .with_inserted_attribute(ATTRIBUTE_MOB_DATA, VertexAttributeValues::Uint32x2(self.mob_data.clone()))
            .with_inserted_indices(Indices::U32(self.indices.clone()));
        // Skin carries coat weights in UV0; other layers their own coordinates.
        let skin = self.submeshes.iter().all(|s| s.slot == MaterialSlot::Skin);
        if self.uvs.len() == n && !(skin && self.coverage.len() == n) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs.clone());
        } else if self.coverage.len() == n {
            let weights: Vec<[f32; 2]> = self.coverage.iter().map(|c| [c[1], c[2]]).collect();
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, weights);
        }
        if self.cor.len() == n {
            mesh.insert_attribute(ATTRIBUTE_COR, self.cor.clone());
        }
        if self.flow.len() == n {
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, self.flow.clone());
        }
        if self.muscle.len() == n && self.muscle.iter().any(|m| m.amplitude > 0.0) {
            let flex: Vec<[f32; 4]> = self.muscle.iter().map(|m| m.insertion.extend(m.amplitude).to_array()).collect();
            let joints: Vec<[u32; 2]> = self.muscle.iter().map(|m| [u32::from(m.joint), m.length.to_bits()]).collect();
            mesh.insert_attribute(ATTRIBUTE_MUSCLE, flex);
            mesh.insert_attribute(ATTRIBUTE_MUSCLE_JOINT, joints);
        }
        if self.soft.len() == n && self.soft.iter().any(|s| s.weights[0] > 0.0) {
            let soft: Vec<[u32; 4]> =
                self.soft.iter().map(|s| [s.joints[0], s.weights[0].to_bits(), s.joints[1], s.weights[1].to_bits()]).collect();
            mesh.insert_attribute(ATTRIBUTE_SOFT, soft);
        }
        mesh
    }

    /// One Bevy mesh per material slot (vertices compacted per slot), for renderers that bind one
    /// material per mesh.
    #[must_use]
    pub fn to_bevy_meshes(&self) -> Vec<(MaterialSlot, Mesh)> {
        self.split_meshes(false)
    }

    /// [`to_bevy_meshes`](Self::to_bevy_meshes) for crowd rendering.
    #[must_use]
    pub fn to_crowd_meshes(&self) -> Vec<(MaterialSlot, Mesh)> {
        self.split_meshes(true)
    }

    fn split_meshes(&self, crowd: bool) -> Vec<(MaterialSlot, Mesh)> {
        let mut slots: Vec<MaterialSlot> = self.submeshes.iter().map(|s| s.slot).collect();
        slots.sort();
        slots.dedup();
        slots
            .into_iter()
            .map(|slot| {
                let mut remap = vec![u32::MAX; self.positions.len()];
                let mut part = Self::default();
                for s in self.submeshes.iter().filter(|s| s.slot == slot) {
                    for tri in self.submesh_indices(s).as_chunks::<3>().0 {
                        // Triangles with a vertex missing an attribute are dropped whole.
                        if !tri.iter().all(|i| self.vertex(i.idx()).is_some()) {
                            continue;
                        }
                        for &i in tri {
                            let i = i.idx();
                            let (Some(r), Some(v)) = (remap.get_mut(i), self.vertex(i)) else { continue };
                            if *r == u32::MAX {
                                *r = part.positions.len().clamp_u32();
                                part.push_vertex(v);
                                if let Some(uv) = self.uvs.get(i) {
                                    part.uvs.push(*uv);
                                }
                                if let Some(c) = self.coverage.get(i) {
                                    part.coverage.push(*c);
                                }
                                if let Some(f) = self.flow.get(i) {
                                    part.flow.push(*f);
                                }
                                if let Some(c) = self.cloth.get(i) {
                                    part.cloth.push(*c);
                                }
                                if let Some(m) = self.muscle.get(i) {
                                    part.muscle.push(*m);
                                }
                                if let Some(m) = self.soft.get(i) {
                                    part.soft.push(*m);
                                }
                            }
                            part.indices.push(*r);
                        }
                    }
                }
                (slot, part.build_mesh(crowd))
            })
            .collect()
    }

    /// The required attributes of vertex `i`, or `None` if any of them is missing.
    #[must_use]
    pub fn vertex(&self, i: usize) -> Option<Vertex> {
        Some(Vertex {
            position: *self.positions.get(i)?,
            normal: *self.normals.get(i)?,
            joints: *self.joints.get(i)?,
            weights: *self.weights.get(i)?,
            mob_data: *self.mob_data.get(i)?,
        })
    }

    /// Appends the required attributes of one vertex.
    pub fn push_vertex(&mut self, v: Vertex) {
        self.positions.push(v.position);
        self.normals.push(v.normal);
        self.joints.push(v.joints);
        self.weights.push(v.weights);
        self.mob_data.push(v.mob_data);
    }

    /// The index buffer slice of a submesh; empty if its range lies outside the buffer.
    #[must_use]
    pub fn submesh_indices(&self, s: &Submesh) -> &[u32] {
        self.indices.get(s.indices.start.idx()..s.indices.end.idx()).unwrap_or_default()
    }

    /// Triangles of every submesh in `slot`.
    pub fn slot_triangles(&self, slot: MaterialSlot) -> impl Iterator<Item = &[u32; 3]> {
        self.submeshes
            .iter()
            .filter(move |s| s.slot == slot)
            .flat_map(|s| self.submesh_indices(s).as_chunks::<3>().0)
    }
}

/// The attributes every [`BodyMeshData`] vertex has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub position: Vec3,
    pub normal: Vec3,
    pub joints: [u16; 4],
    pub weights: [f32; 4],
    pub mob_data: [u32; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mob_data_round_trips() {
        let d = MobVertexData {
            region: RegionId(41),
            cavity: 200,
            curvature: 128,
            limb_t: 255,
            primary_bone: BoneId(63),
            layer: PrimLayer::Landmark,
        };
        assert_eq!(MobVertexData::unpack(d.pack()), d);
    }
}
