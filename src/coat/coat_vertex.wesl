// Mob vertex shader for every coat layer and tier.
// - Hero meshes take Bevy's joint matrices (SKINNED); crowd meshes the crowd bone buffer at the
//   slot in the mesh tag (COAT_CROWD). Both rotate each vertex about its precomputed centre of
//   rotation (COAT_COR), or blend dual quaternions where a mesh has none (`dqs.wesl`): limbs keep
//   their volume at bent joints, where averaging matrices shrinks them.
// - Fur shells (COAT_SHELL) are pushed out along the normal by uv.x · fur length, drooping a little.
// - Muscles (COAT_MUSCLE) bulge along the normal as they shorten: the distance from the skinned
//   vertex to its muscle's insertion (carried by the joint across) against the rest distance,
//   over the muscle's length.
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
    // flex gain (0 = muscles still), _
    muscle: vec2<f32>,
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
    @if(COAT_MUSCLE)
    @location(9) muscle: vec4<f32>,
    @if(COAT_MUSCLE)
    @location(10) muscle_joint: vec2<u32>,
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
@if(COAT_MUSCLE) {
    world += muscle_offset(v.muscle, bitcast<f32>(v.muscle_joint.y), position, world, world_normal, muscle_joint(v, instance_from_local));
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

// One joint's skin matrix for a hero vertex.
@if(SKINNED)
fn hero_joint(index: u32, instance_index: u32) -> mat4x4<f32> {
@if(SKINS_USE_UNIFORM_BUFFERS)
    return skinning::joint_matrices.data[index];
@if(!SKINS_USE_UNIFORM_BUFFERS)
    return skinning::joint_matrices[mesh[instance_index].current_skin_index + index];
}

// World-from-rest matrix of the joint carrying the vertex's muscle insertion.
@if(COAT_MUSCLE)
fn muscle_joint(v: CoatVertex, instance_from_local: mat4x4<f32>) -> mat4x4<f32> {
@if(COAT_CROWD)
    return instance_from_local * crowd_bones[mesh_functions::get_tag(v.instance_index) * MAX_CROWD_BONES + v.muscle_joint.x];
@if(!COAT_CROWD && SKINNED)
    return hero_joint(v.muscle_joint.x, v.instance_index);
@if(!COAT_CROWD && !SKINNED)
    return instance_from_local;
}

// Muscle bulge (world space): out along the normal by amplitude · contraction, where contraction
// is how much closer the vertex has come to the insertion than at rest, over the muscle's rest
// length (negative when stretched, which flattens the muscle a little). Dividing by the muscle's
// length rather than the vertex's own distance keeps vertices next to the insertion calm.
@if(COAT_MUSCLE)
fn muscle_offset(muscle: vec4<f32>, muscle_length: f32, rest: vec3<f32>, world: vec3<f32>, world_normal: vec3<f32>, joint: mat4x4<f32>) -> vec3<f32> {
    if muscle.w <= 0.0 || coat.muscle.x <= 0.0 {
        return vec3<f32>(0.0);
    }
    let insertion = (joint * vec4<f32>(muscle.xyz, 1.0)).xyz;
    let scale = length(joint[0].xyz);
    let shortening = distance(rest, muscle.xyz) * scale - distance(world, insertion);
    let contraction = clamp(shortening / max(muscle_length * scale, 1e-4), -0.1, 0.3);
    return world_normal * (muscle.w * contraction * scale * coat.muscle.x);
}
