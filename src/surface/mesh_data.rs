use std::ops::Range;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::VertexFormat;

use crate::anatomy::{BoneId, PrimLayer, RegionId};

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
    pub fn pack(&self) -> [u32; 2] {
        let layer = match self.layer {
            PrimLayer::Base => 0u32,
            PrimLayer::Muscle => 1,
            PrimLayer::Landmark => 2,
            PrimLayer::Fat => 3,
        };
        [
            self.region.0 as u32
                | (self.cavity as u32) << 8
                | (self.curvature as u32) << 16
                | (self.limb_t as u32) << 24,
            self.primary_bone.0 as u32 | layer << 8,
        ]
    }

    pub fn unpack([x, y]: [u32; 2]) -> Self {
        Self {
            region: RegionId(x as u8),
            cavity: (x >> 8) as u8,
            curvature: (x >> 16) as u8,
            limb_t: (x >> 24) as u8,
            primary_bone: BoneId(y as u8),
            layer: match (y >> 8) & 0xff {
                1 => PrimLayer::Muscle,
                2 => PrimLayer::Landmark,
                3 => PrimLayer::Fat,
                _ => PrimLayer::Base,
            },
        }
    }
}

/// Which material a range of indices uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaterialSlot {
    /// SDF body: coat shading.
    Skin,
    /// Claws, horns, teeth, beaks.
    Hard,
    Eye,
    /// Garments: colour from the outfit palette (index in vertex colour w).
    Cloth,
}

#[derive(Clone, Debug, PartialEq)]
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
    pub indices: Vec<u32>,
    pub submeshes: Vec<Submesh>,
}

impl BodyMeshData {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Builds a Bevy mesh with position, normal, joint index/weight and [`ATTRIBUTE_MOB_DATA`].
    /// Submeshes stay in one index buffer; the material system splits by range.
    pub fn to_bevy_mesh(&self) -> Mesh {
        self.build_mesh(false)
    }

    /// Like [`to_bevy_mesh`](Self::to_bevy_mesh) but with joints under the crowd attribute names.
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
            Some(&c) if c != NOT_CLOTH => c as f32,
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
        if self.uvs.len() == n {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs.clone());
        } else if self.coverage.len() == n {
            let weights: Vec<[f32; 2]> = self.coverage.iter().map(|c| [c[1], c[2]]).collect();
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, weights);
        }
        if self.flow.len() == n {
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, self.flow.clone());
        }
        mesh
    }

    /// One Bevy mesh per material slot (vertices compacted per slot), for renderers that bind one
    /// material per mesh.
    pub fn to_bevy_meshes(&self) -> Vec<(MaterialSlot, Mesh)> {
        self.split_meshes(false)
    }

    /// [`to_bevy_meshes`](Self::to_bevy_meshes) for crowd rendering.
    pub fn to_crowd_meshes(&self) -> Vec<(MaterialSlot, Mesh)> {
        self.split_meshes(true)
    }

    fn split_meshes(&self, crowd: bool) -> Vec<(MaterialSlot, Mesh)> {
        let mut slots: Vec<MaterialSlot> = self.submeshes.iter().map(|s| s.slot).collect();
        slots.dedup();
        slots.sort_by_key(|s| *s as u8);
        slots.dedup();
        slots
            .into_iter()
            .map(|slot| {
                let mut remap = vec![u32::MAX; self.positions.len()];
                let mut part = BodyMeshData::default();
                for s in self.submeshes.iter().filter(|s| s.slot == slot) {
                    for &i in &self.indices[s.indices.start as usize..s.indices.end as usize] {
                        let r = &mut remap[i as usize];
                        if *r == u32::MAX {
                            *r = part.positions.len() as u32;
                            let i = i as usize;
                            part.positions.push(self.positions[i]);
                            part.normals.push(self.normals[i]);
                            part.joints.push(self.joints[i]);
                            part.weights.push(self.weights[i]);
                            part.mob_data.push(self.mob_data[i]);
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
                        }
                        part.indices.push(*r);
                    }
                }
                (slot, part.build_mesh(crowd))
            })
            .collect()
    }
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
