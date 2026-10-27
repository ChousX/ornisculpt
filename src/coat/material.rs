use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;

use super::CoatUniform;
use crate::surface::{
    ATTRIBUTE_COR, ATTRIBUTE_CROWD_JOINT_INDEX, ATTRIBUTE_CROWD_JOINT_WEIGHT, ATTRIBUTE_MOB_DATA, ATTRIBUTE_MUSCLE, ATTRIBUTE_MUSCLE_JOINT, ATTRIBUTE_PAINT,
    ATTRIBUTE_SOFT,
};

const COAT_VERTEX: &str = "embedded://ornisculpt/coat/coat_vertex.wesl";
const COAT_FRAGMENT: &str = "embedded://ornisculpt/coat/coat_fragment.wesl";
const COAT_PREPASS: &str = "embedded://ornisculpt/coat/coat_prepass.wesl";

/// Every mob surface: `StandardMaterial` lighting with coat shading on top.
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
    /// Hair cards (uv = across, along).
    Hair,
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

    fn prepass_vertex_shader() -> ShaderRef {
        COAT_PREPASS.into()
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
        // The main pass and the prepass/shadow pass deform alike (`deform.wesl`), so both get the
        // same defines and vertex layout.
        // Meshes with centres of rotation skin about them (`dqs.wesl`); others with dual quaternions.
        let cor = layout.0.contains(ATTRIBUTE_COR);
        if cor {
            defs.push("COAT_COR".into());
        }
        // Meshes with muscle flex bulge their muscles as they shorten (`deform.wesl`).
        // `ORNISCULPT_NO_MUSCLE`: keep muscles still (for comparing).
        let muscle = layout.0.contains(ATTRIBUTE_MUSCLE) && std::env::var_os("ORNISCULPT_NO_MUSCLE").is_none();
        if muscle {
            defs.push("COAT_MUSCLE".into());
        }
        // Meshes with soft-tissue weights jiggle (`soft-tissue` feature). `ORNISCULPT_NO_SOFT`:
        // keep them still (for comparing).
        let soft = layout.0.contains(ATTRIBUTE_SOFT) && std::env::var_os("ORNISCULPT_NO_SOFT").is_none();
        if soft {
            defs.push("COAT_SOFT".into());
        }
        defs.extend(crate::mob::crowd_slot_defs());
        // Optional inputs by what the mesh has: the prepass would otherwise drop the ones it
        // doesn't need itself (shell heights in UV0, for one).
        // Coat markings (`ATTRIBUTE_PAINT`) and the surface data (`ATTRIBUTE_MOB_DATA`: cavity,
        // curvature) ride to the fragment stage in `paint_io.wesl`.
        let inputs: Vec<_> = [
            (Mesh::ATTRIBUTE_UV_0, 2, "COAT_UV"),
            (Mesh::ATTRIBUTE_TANGENT, 4, "COAT_TANGENT"),
            (Mesh::ATTRIBUTE_COLOR, 5, "COAT_COLOR"),
            (ATTRIBUTE_PAINT, 12, "COAT_PAINT"),
            (ATTRIBUTE_MOB_DATA, 13, "COAT_DATA"),
        ]
            .into_iter()
            .filter(|(attribute, ..)| layout.0.contains(*attribute))
            .collect();
        defs.extend(inputs.iter().map(|(_, _, def)| (*def).into()));
        match key.bind_group_data.layer {
            CoatLayer::Skin => defs.push("COAT_SKIN".into()),
            CoatLayer::Shell => defs.push("COAT_SHELL".into()),
            CoatLayer::Feather => defs.push("COAT_FEATHER".into()),
            CoatLayer::Hard => defs.push("COAT_HARD".into()),
            CoatLayer::Cloth => defs.push("COAT_CLOTH".into()),
            CoatLayer::Wing => defs.push("COAT_WING".into()),
            CoatLayer::Hair => defs.push("COAT_HAIR".into()),
        }
        // `ORNISCULPT_LINEAR_SKIN`: average joint matrices instead of dual quaternions (for comparing).
        if std::env::var_os("ORNISCULPT_LINEAR_SKIN").is_some() {
            descriptor.vertex.shader_defs.push("COAT_LINEAR_SKIN".into());
        }
        descriptor.vertex.shader_defs.extend(defs.iter().cloned());
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader_defs.extend(defs);
        }
        // Always our own layout (`deform.wesl` locations): crowd joints, centres of rotation and
        // muscle flex are custom attributes Bevy doesn't lay out, and the prepass would use its own
        // locations otherwise.
        let mut attributes = vec![Mesh::ATTRIBUTE_POSITION.at_shader_location(0), Mesh::ATTRIBUTE_NORMAL.at_shader_location(1)];
        attributes.extend(inputs.into_iter().map(|(attribute, location, _)| attribute.at_shader_location(location)));
        if key.bind_group_data.crowd {
            attributes.push(ATTRIBUTE_CROWD_JOINT_INDEX.at_shader_location(6));
            attributes.push(ATTRIBUTE_CROWD_JOINT_WEIGHT.at_shader_location(7));
        } else if layout.0.contains(Mesh::ATTRIBUTE_JOINT_INDEX) {
            attributes.push(Mesh::ATTRIBUTE_JOINT_INDEX.at_shader_location(6));
            attributes.push(Mesh::ATTRIBUTE_JOINT_WEIGHT.at_shader_location(7));
        }
        if cor {
            attributes.push(ATTRIBUTE_COR.at_shader_location(8));
        }
        if muscle {
            attributes.push(ATTRIBUTE_MUSCLE.at_shader_location(9));
            attributes.push(ATTRIBUTE_MUSCLE_JOINT.at_shader_location(10));
        }
        if soft {
            attributes.push(ATTRIBUTE_SOFT.at_shader_location(11));
        }
        descriptor.vertex.buffers = vec![layout.0.get_layout(&attributes)?];
        Ok(())
    }
}
