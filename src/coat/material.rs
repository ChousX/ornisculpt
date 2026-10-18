use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

use super::CoatUniform;
use crate::surface::{ATTRIBUTE_CROWD_JOINT_INDEX, ATTRIBUTE_CROWD_JOINT_WEIGHT};

pub const COAT_VERTEX: &str = "embedded://ornisculpt/coat/coat_vertex.wesl";
pub const COAT_FRAGMENT: &str = "embedded://ornisculpt/coat/coat_fragment.wesl";

/// Every mob surface: StandardMaterial lighting with coat shading on top.
pub type MobMaterial = ExtendedMaterial<StandardMaterial, CoatExt>;

/// Which surface a material draws.
#[repr(u8)]
#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CoatLayer {
    /// The body skin (scales shading, fur/feather undercoat).
    #[default]
    Skin,
    /// Fur shells (uv.x = shell height).
    Shell,
    /// Feather cards (uv = across, along).
    Feather,
    /// Hard parts and eyes: plain colour.
    Hard,
    /// Garments: outfit palette colour (index in vertex colour w).
    Cloth,
    /// Wing sheets: membranes, flight feathers, insect wings (sheet kind in vertex colour w).
    Wing,
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
#[bind_group_data(CoatKey)]
pub struct CoatExt {
    /// Crowd skin matrices (read only when `crowd`).
    #[storage(100, read_only)]
    pub bones: Handle<ShaderBuffer>,
    #[uniform(101)]
    pub coat: CoatUniform,
    pub crowd: bool,
    pub layer: CoatLayer,
}

/// Pipeline specialisation for [`CoatExt`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CoatKey {
    pub crowd: bool,
    pub layer: CoatLayer,
}

impl From<&CoatExt> for CoatKey {
    fn from(m: &CoatExt) -> Self {
        Self { crowd: m.crowd, layer: m.layer }
    }
}

impl MaterialExtension for CoatExt {
    fn vertex_shader() -> ShaderRef {
        COAT_VERTEX.into()
    }

    fn fragment_shader() -> ShaderRef {
        COAT_FRAGMENT.into()
    }

    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let mut defs = Vec::new();
        if key.bind_group_data.crowd {
            defs.push("COAT_CROWD".into());
        }
        match key.bind_group_data.layer {
            CoatLayer::Skin => defs.push("COAT_SKIN".into()),
            CoatLayer::Shell => defs.push("COAT_SHELL".into()),
            CoatLayer::Feather => defs.push("COAT_FEATHER".into()),
            CoatLayer::Hard => defs.push("COAT_HARD".into()),
            CoatLayer::Cloth => defs.push("COAT_CLOTH".into()),
            CoatLayer::Wing => defs.push("COAT_WING".into()),
        }
        descriptor.vertex.shader_defs.extend(defs.iter().cloned());
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.extend(defs);
        }
        if key.bind_group_data.crowd {
            // Crowd meshes carry joints under custom attributes, which Bevy doesn't lay out.
            let mut attributes = vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_NORMAL.at_shader_location(1)];
            if layout.0.contains(Mesh::ATTRIBUTE_UV_0) {
                attributes.push(Mesh::ATTRIBUTE_UV_0.at_shader_location(2));
            }
            if layout.0.contains(Mesh::ATTRIBUTE_TANGENT) {
                attributes.push(Mesh::ATTRIBUTE_TANGENT.at_shader_location(4));
            }
            if layout.0.contains(Mesh::ATTRIBUTE_COLOR) {
                attributes.push(Mesh::ATTRIBUTE_COLOR.at_shader_location(5));
            }
            attributes.push(ATTRIBUTE_CROWD_JOINT_INDEX.at_shader_location(6));
            attributes.push(ATTRIBUTE_CROWD_JOINT_WEIGHT.at_shader_location(7));
            descriptor.vertex.buffers = vec![layout.0.get_layout(&attributes)?];
        }
        Ok(())
    }
}
