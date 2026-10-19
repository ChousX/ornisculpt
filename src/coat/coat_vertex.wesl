// Mob vertex shader for every coat layer and tier.
// - Hero meshes take Bevy's joint matrices (SKINNED); crowd meshes the crowd bone buffer at the
//   slot in the mesh tag (COAT_CROWD). Both rotate each vertex about its precomputed centre of
//   rotation (COAT_COR), or blend dual quaternions where a mesh has none (`dqs.wesl`): limbs keep
//   their volume at bent joints, where averaging matrices shrinks them.
// - Fur shells (COAT_SHELL) are pushed out along the normal by uv.x · fur length, drooping a little.
// - tangent.w is the distance along the coat flow (metres, baked at generation); crowd skin is inflated over fur/feathers so silhouettes keep the coat's volume.
// - Vertex colour = rest-pose position (xyz) + scale weight (w); tangent = coat flow; on the
//   skin, uv = (fur, feather) weights.

import bevy_pbr::render::{
    mesh_functions,
    forward_io::VertexOutput,
    view_transformations::position_world_to_clip,
};
@if(SKINNED)
import bevy_pbr::render::skinning;
@if(SKINNED && !SKINS_USE_UNIFORM_BUFFERS)
import bevy_pbr::render::mesh_bindings::mesh;
import package::coat::dqs;

const MAX_CROWD_BONES: u32 = 32u;

struct CoatUniform {
    base: vec4<f32>,
    secondary: vec4<f32>,
    accent: vec4<f32>,
    scale_mask: vec2<u32>,
    fur_mask: vec2<u32>,
    feather_mask: vec2<u32>,
    pad: vec2<u32>,
    scales: vec4<f32>,
    fur: vec4<f32>,
    feathers: vec4<f32>,
    cloth: array<vec4<f32>, 8>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> crowd_bones: array<mat4x4<f32>>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> coat: CoatUniform;

struct CoatVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    @if(VERTEX_TANGENTS)
    @location(4) tangent: vec4<f32>,
    @if(VERTEX_COLORS)
    @location(5) color: vec4<f32>,
    @if(SKINNED || COAT_CROWD)
    @location(6) joint_indices: vec4<u32>,
    @if(SKINNED || COAT_CROWD)
    @location(7) joint_weights: vec4<f32>,
    @if(COAT_COR)
    @location(8) cor: vec3<f32>,
};

@vertex
fn vertex(v: CoatVertex) -> VertexOutput {
    var out: VertexOutput;
    var position = v.position;
@if(COAT_SHELL && VERTEX_UVS_A) {
    let h = v.uv.x;
    position += v.normal * coat.fur.x * h + vec3<f32>(0.0, -1.0, 0.0) * coat.fur.x * h * h * 0.35;
}
@if(COAT_SKIN && COAT_CROWD && VERTEX_UVS_A) {
    // Crowd tiers have no shells or cards: carry their volume in the skin.
    position += v.normal * (v.uv.x * coat.fur.x * 0.8 + v.uv.y * coat.feathers.x * 0.2);
}

    let instance_from_local = mesh_functions::get_world_from_local(v.instance_index);
    var tangent = vec3<f32>(1.0, 0.0, 0.0);
@if(VERTEX_TANGENTS) {
    tangent = v.tangent.xyz;
}
    // The centre of rotation (the vertex itself when the mesh has none: no effect).
    var cor = position;
@if(COAT_COR) {
    cor = v.cor;
}
    // Rigid by default; skinned paths replace these.
    var world = (instance_from_local * vec4<f32>(position, 1.0)).xyz;
    var world_normal = normalize((instance_from_local * vec4<f32>(v.normal, 0.0)).xyz);
    var world_tangent = normalize((instance_from_local * vec4<f32>(tangent, 0.0)).xyz);
@if(COAT_CROWD) {
    // Crowd bones are mob-local skin matrices.
    let base = mesh_functions::get_tag(v.instance_index) * MAX_CROWD_BONES;
    let j = v.joint_indices + vec4<u32>(base);
    let s = dqs::blend(array<mat4x4<f32>, 4>(crowd_bones[j.x], crowd_bones[j.y], crowd_bones[j.z], crowd_bones[j.w]), v.joint_weights, position, v.normal, tangent, cor);
    world = (instance_from_local * vec4<f32>(s.position, 1.0)).xyz;
    world_normal = normalize((instance_from_local * vec4<f32>(s.normal, 0.0)).xyz);
    world_tangent = normalize((instance_from_local * vec4<f32>(s.tangent, 0.0)).xyz);
}
@if(SKINNED && !COAT_CROWD) {
    // Bevy's joint matrices are world-space skin matrices.
    let s = dqs::blend(hero_joints(v.joint_indices, v.instance_index), v.joint_weights, position, v.normal, tangent, cor);
    world = s.position;
    world_normal = s.normal;
    world_tangent = s.tangent;
}

    out.world_position = vec4<f32>(world, 1.0);
    out.position = position_world_to_clip(world);
    out.world_normal = world_normal;
@if(VERTEX_UVS_A)
    out.uv = v.uv;
@if(VERTEX_TANGENTS)
    out.world_tangent = vec4<f32>(world_tangent, v.tangent.w);
@if(VERTEX_COLORS)
    out.color = v.color;
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = v.instance_index;
    return out;
}

// The four joints' skin matrices for a hero vertex, from Bevy's joint buffer.
@if(SKINNED)
fn hero_joints(indexes: vec4<u32>, instance_index: u32) -> array<mat4x4<f32>, 4> {
@if(SKINS_USE_UNIFORM_BUFFERS)
    return array<mat4x4<f32>, 4>(
        skinning::joint_matrices.data[indexes.x],
        skinning::joint_matrices.data[indexes.y],
        skinning::joint_matrices.data[indexes.z],
        skinning::joint_matrices.data[indexes.w],
    );
@if(!SKINS_USE_UNIFORM_BUFFERS)
    let base = mesh[instance_index].current_skin_index;
@if(!SKINS_USE_UNIFORM_BUFFERS)
    return array<mat4x4<f32>, 4>(
        skinning::joint_matrices[base + indexes.x],
        skinning::joint_matrices[base + indexes.y],
        skinning::joint_matrices[base + indexes.z],
        skinning::joint_matrices[base + indexes.w],
    );
}
