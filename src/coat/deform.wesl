// Mob vertex deformation, shared by the main pass (`coat_vertex.wesl`) and the depth, normal,
// motion-vector and shadow passes (`coat_prepass.wesl`), so every pass sees the same surface.
// - Hero meshes take Bevy's joint matrices (SKINNED); crowd meshes the crowd bone buffer at the
//   slot in the mesh tag (COAT_CROWD). Both rotate each vertex about its precomputed centre of
//   rotation (COAT_COR), or blend dual quaternions where a mesh has none (`dqs.wesl`): limbs keep
//   their volume at bent joints, where averaging matrices shrinks them.
// - Fur shells (COAT_SHELL) are pushed out along the normal by uv.x · fur length, drooping a little.
// - Muscles (COAT_MUSCLE) bulge along the normal as they shorten: the distance from the skinned
//   vertex to its muscle's insertion (carried by the joint across) against the rest distance,
//   over the muscle's length.
// - `previous`: last frame's surface, for motion vectors. Hero joints have last frame's matrices;
//   the crowd bone buffer holds only the current pose, so crowd meshes move only with their
//   instance transform.
//
// Inputs use the coat's own defines (COAT_UV, COAT_TANGENT, COAT_COLOR: the mesh has them), set in
// `CoatExt::specialize` with the vertex layout, because the prepass leaves out attributes it
// doesn't itself need.

import bevy_pbr::render::mesh_functions;
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
    @if(COAT_UV)
    @location(2) uv: vec2<f32>,
    @if(COAT_TANGENT)
    @location(4) tangent: vec4<f32>,
    @if(COAT_COLOR)
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

// The vertex in mesh space before skinning: shells pushed out, crowd skin inflated.
fn rest_position(v: CoatVertex) -> vec3<f32> {
    var position = v.position;
@if(COAT_SHELL && COAT_UV) {
    let h = v.uv.x;
    position += v.normal * coat.fur.x * h + vec3<f32>(0.0, -1.0, 0.0) * coat.fur.x * h * h * 0.35;
}
@if(COAT_SKIN && COAT_CROWD && COAT_UV) {
    // Crowd tiers have no shells or cards: carry their volume in the skin.
    position += v.normal * (v.uv.x * coat.fur.x * 0.8 + v.uv.y * coat.feathers.x * 0.2);
}
    return position;
}

// The deformed vertex in world space (this frame, or the last one when `previous`).
fn deform(v: CoatVertex, previous: bool) -> dqs::Skinned {
    let position = rest_position(v);
    var instance_from_local = mesh_functions::get_world_from_local(v.instance_index);
    if previous {
        instance_from_local = mesh_functions::get_previous_world_from_local(v.instance_index);
    }
    var tangent = vec3<f32>(1.0, 0.0, 0.0);
@if(COAT_TANGENT) {
    tangent = v.tangent.xyz;
}
    // The centre of rotation (the vertex itself when the mesh has none: no effect).
    var cor = position;
@if(COAT_COR) {
    cor = v.cor;
}
    // Rigid by default; skinned paths replace it.
    var s: dqs::Skinned;
    s.position = (instance_from_local * vec4<f32>(position, 1.0)).xyz;
    s.normal = normalize((instance_from_local * vec4<f32>(v.normal, 0.0)).xyz);
    s.tangent = normalize((instance_from_local * vec4<f32>(tangent, 0.0)).xyz);
@if(COAT_CROWD) {
    // Crowd bones are mob-local skin matrices.
    let base = mesh_functions::get_tag(v.instance_index) * MAX_CROWD_BONES;
    let j = v.joint_indices + vec4<u32>(base);
    let c = dqs::blend(array<mat4x4<f32>, 4>(crowd_bones[j.x], crowd_bones[j.y], crowd_bones[j.z], crowd_bones[j.w]), v.joint_weights, position, v.normal, tangent, cor);
    s.position = (instance_from_local * vec4<f32>(c.position, 1.0)).xyz;
    s.normal = normalize((instance_from_local * vec4<f32>(c.normal, 0.0)).xyz);
    s.tangent = normalize((instance_from_local * vec4<f32>(c.tangent, 0.0)).xyz);
}
@if(SKINNED && !COAT_CROWD) {
    // Bevy's joint matrices are world-space skin matrices.
    s = dqs::blend(hero_joints(v.joint_indices, v.instance_index, previous), v.joint_weights, position, v.normal, tangent, cor);
}
@if(COAT_MUSCLE) {
    s.position += muscle_offset(v.muscle, bitcast<f32>(v.muscle_joint.y), position, s.position, s.normal, muscle_joint(v, instance_from_local, previous));
}
    return s;
}

// One of Bevy's joint matrices for a hero vertex: this frame's, or the last frame's. Last frame's
// are only bound with motion vectors (any mention of them needs the binding, used or not).
@if(SKINNED)
fn joint_matrix(index: u32, instance_index: u32, previous: bool) -> mat4x4<f32> {
@if(SKINS_USE_UNIFORM_BUFFERS) {
@if(MOTION_VECTOR_PREPASS) {
    if previous {
        return skinning::prev_joint_matrices.data[index];
    }
}
    return skinning::joint_matrices.data[index];
}
@if(!SKINS_USE_UNIFORM_BUFFERS) {
    let i = mesh[instance_index].current_skin_index + index;
@if(MOTION_VECTOR_PREPASS) {
    if previous {
        return skinning::prev_joint_matrices[i];
    }
}
    return skinning::joint_matrices[i];
}
}

// The four joints' skin matrices for a hero vertex.
@if(SKINNED)
fn hero_joints(indexes: vec4<u32>, instance_index: u32, previous: bool) -> array<mat4x4<f32>, 4> {
    return array<mat4x4<f32>, 4>(
        joint_matrix(indexes.x, instance_index, previous),
        joint_matrix(indexes.y, instance_index, previous),
        joint_matrix(indexes.z, instance_index, previous),
        joint_matrix(indexes.w, instance_index, previous),
    );
}

// World-from-rest matrix of the joint carrying the vertex's muscle insertion.
@if(COAT_MUSCLE)
fn muscle_joint(v: CoatVertex, instance_from_local: mat4x4<f32>, previous: bool) -> mat4x4<f32> {
@if(COAT_CROWD)
    return instance_from_local * crowd_bones[mesh_functions::get_tag(v.instance_index) * MAX_CROWD_BONES + v.muscle_joint.x];
@if(!COAT_CROWD && SKINNED)
    return joint_matrix(v.muscle_joint.x, v.instance_index, previous);
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
