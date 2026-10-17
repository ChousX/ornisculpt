// Mob vertex shader for every coat layer and tier.
// - Hero meshes are skinned by Bevy (SKINNED); crowd meshes from the crowd bone buffer at the
//   slot in the mesh tag (COAT_CROWD).
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

    var world_from_local = mesh_functions::get_world_from_local(v.instance_index);
@if(COAT_CROWD) {
    let base = mesh_functions::get_tag(v.instance_index) * MAX_CROWD_BONES;
    let skin = crowd_bones[base + v.joint_indices.x] * v.joint_weights.x
        + crowd_bones[base + v.joint_indices.y] * v.joint_weights.y
        + crowd_bones[base + v.joint_indices.z] * v.joint_weights.z
        + crowd_bones[base + v.joint_indices.w] * v.joint_weights.w;
    world_from_local = world_from_local * skin;
}
@if(SKINNED && !COAT_CROWD) {
    world_from_local = skinning::skin_model(v.joint_indices, v.joint_weights, v.instance_index);
}

    let world = world_from_local * vec4<f32>(position, 1.0);
    out.world_position = world;
    out.position = position_world_to_clip(world.xyz);
    out.world_normal = normalize((world_from_local * vec4<f32>(v.normal, 0.0)).xyz);
@if(VERTEX_UVS_A)
    out.uv = v.uv;
@if(VERTEX_TANGENTS)
    out.world_tangent = vec4<f32>(normalize((world_from_local * vec4<f32>(v.tangent.xyz, 0.0)).xyz), v.tangent.w);
@if(VERTEX_COLORS)
    out.color = v.color;
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = v.instance_index;
    return out;
}
