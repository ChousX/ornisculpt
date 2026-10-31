// Mob vertex deformation, shared by the main pass (`coat_vertex.wesl`) and the depth, normal,
// motion-vector and shadow passes (`coat_prepass.wesl`), so every pass sees the same surface.
// - Hero meshes take Bevy's joint matrices (SKINNED); crowd meshes the crowd bone buffer at the
//   slot in the mesh tag (COAT_CROWD). Both rotate each vertex about its precomputed centre of
//   rotation (COAT_COR), or blend dual quaternions where a mesh has none (`dqs.wesl`): limbs keep
//   their volume at bent joints, where averaging matrices shrinks them.
// - Fur shells (COAT_SHELL) are pushed out along the normal by uv.x · the fur length baked in uv.y,
//   drooping a little.
// - Muscles (COAT_MUSCLE) bulge along the normal as they shorten: the distance from the skinned
//   vertex to its muscle's insertion (carried by the joint across) against the rest distance,
//   over the muscle's length.
// - Soft tissue (COAT_SOFT) moves with up to two regions' springs: each region is a soft joint (its
//   bone's skin matrix, moved by the spring) beside its bone's, and S·p − B·p is the spring's offset.
// - Contact (COAT_CONTACT, hero only): skin that a pose brings inside another limb or the body is
//   pushed out halfway, so the two surfaces meet and press flat (`surface::contact`). Each vertex's
//   mask (high half of the mob data's second word) names the groups it may be pushed out of.
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

// Matrices per mob slot (`CROWD_SLOT` in `anatomy`): crowd bones, then soft-tissue regions.
@if(SOFT_TISSUE)
const CROWD_SLOT: u32 = 40u;
@if(!SOFT_TISSUE)
const CROWD_SLOT: u32 = 32u;

struct CoatUniform {
    base: vec4<f32>,
    secondary: vec4<f32>,
    accent: vec4<f32>,
    scale_mask: vec2<u32>,
    fur_mask: vec2<u32>,
    feather_mask: vec2<u32>,
    // flex gain (0 = muscles still), soft-tissue gain (0 = still)
    muscle: vec2<f32>,
    scales: vec4<f32>,
    fur: vec4<f32>,
    feathers: vec4<f32>,
    cloth: array<vec4<f32>, 8>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> crowd_bones: array<mat4x4<f32>>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> coat: CoatUniform;

// `surface::ContactProxyGpu`: a rounded cone in the rest pose, from a (radius a.w) to b (radius
// b.w), its section narrowed `cross.w` times along `cross`; then its bone and group.
struct ContactProxy {
    a: vec4<f32>,
    b: vec4<f32>,
    cross: vec4<f32>,
    ids: vec4<u32>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(102) var<storage, read> contact_proxies: array<ContactProxy>;

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
    @if(COAT_SOFT)
    @location(11) soft: vec4<u32>,
    @if(COAT_PAINT)
    @location(12) paint: vec4<f32>,
    // `ATTRIBUTE_MOB_DATA`: region | cavity << 8 | shade << 16 | limb_t << 24, bone | layer << 8
    // (the layer byte is 4 + the hard-part index on a hard part's vertices; see `MobVertexData`).
    @if(COAT_DATA)
    @location(13) data: vec2<u32>,
};

// The vertex in mesh space before skinning: shells pushed out, crowd skin inflated.
fn rest_position(v: CoatVertex) -> vec3<f32> {
    var position = v.position;
@if(COAT_SHELL && COAT_UV) {
    // uv = (shell height, the fur's length here).
    let h = v.uv.x;
    position += v.normal * v.uv.y * h + vec3<f32>(0.0, -1.0, 0.0) * v.uv.y * h * h * 0.35;
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
    let base = mesh_functions::get_tag(v.instance_index) * CROWD_SLOT;
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
@if(COAT_SOFT) {
    s.position += soft_offset(v, position, instance_from_local, previous);
}
@if(COAT_CONTACT && COAT_DATA && SKINNED && !COAT_CROWD) {
    let mask = v.data.y >> 16u;
    if mask != 0u {
        s = pressed(s, mask, v.instance_index, previous);
    }
}
    return s;
}

// How far apart two pressed surfaces stay, metres.
const CONTACT_GAP: f32 = 0.0015;

// Quilez's rounded cone from the origin to `ba`, radii `r1` there and `r2` at `ba`.
fn round_cone_sdf(p: vec3<f32>, ba: vec3<f32>, r1: f32, r2: f32) -> f32 {
    let l2 = dot(ba, ba);
    if l2 < 1e-10 {
        return length(p) - max(r1, r2);
    }
    let rr = r1 - r2;
    let a2 = l2 - rr * rr;
    let il2 = 1.0 / l2;
    let y = dot(p, ba);
    let z = y - l2;
    let xv = p * l2 - ba * y;
    let x2 = dot(xv, xv);
    let y2 = y * y * l2;
    let z2 = z * z * l2;
    let k = sign(rr) * rr * rr * x2;
    if sign(z) * a2 * z2 > k {
        return sqrt(x2 + z2) * il2 - r2;
    }
    if sign(y) * a2 * y2 < k {
        return sqrt(x2 + y2) * il2 - r1;
    }
    return (sqrt(x2 * a2 * il2) + y * rr) * il2 - r1;
}

// The skinned vertex pushed out of the deepest contact proxy in `mask` it lies inside, its normal
// turned to face the other surface (`ContactSet::sink` finds the same proxy).
@if(COAT_CONTACT && SKINNED)
fn pressed(s: dqs::Skinned, mask: u32, instance_index: u32, previous: bool) -> dqs::Skinned {
    var deepest = 0.0;
    var out_dir = vec3<f32>(0.0);
    let count = arrayLength(&contact_proxies);
    for (var i = 0u; i < count; i++) {
        let c = contact_proxies[i];
        if (mask & (1u << c.ids.y)) == 0u {
            continue;
        }
        let m = joint_matrix(c.ids.x, instance_index, previous);
        let scale = length(m[0].xyz);
        let a = (m * vec4<f32>(c.a.xyz, 1.0)).xyz;
        let b = (m * vec4<f32>(c.b.xyz, 1.0)).xyz;
        let r = max(c.a.w, c.b.w) * scale;
        let ba = b - a;
        // Far from this one: skip the rest.
        if distance(s.position, (a + b) * 0.5) > length(ba) * 0.5 + r {
            continue;
        }
        let cross = normalize((m * vec4<f32>(c.cross.xyz, 0.0)).xyz);
        let k = max(c.cross.w, 1e-3);
        let p = s.position - a;
        let q = p + cross * ((k - 1.0) * dot(p, cross));
        let t = clamp(dot(q, ba) / max(dot(ba, ba), 1e-10), 0.0, 1.0);
        let off = q - ba * t;
        let n = select(vec3<f32>(0.0, 1.0, 0.0), normalize(off), dot(off, off) > 1e-12);
        let along = dot(n, cross);
        let d = round_cone_sdf(q, ba, c.a.w * scale, c.b.w * scale) / ((k - 1.0) * along * along + 1.0);
        if d < deepest {
            deepest = d;
            out_dir = normalize(n + cross * ((k - 1.0) * along));
        }
    }
    var out = s;
    if deepest < 0.0 {
        // The two surfaces meet halfway (each is pushed out of the other), CONTACT_GAP short of
        // it so they don't fight for depth. A limb hung deep inside another (a forearm in a
        // thigh) is a pose to fix (`ContactSet::clearance`): this only moves skin.
        out.position += out_dir * (-deepest * 0.5 + CONTACT_GAP * smoothstep(0.0, 0.004, -deepest));
        out.normal = normalize(mix(s.normal, -out_dir, smoothstep(0.0, 0.01, -deepest)));
    }
    return out;
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
    return instance_from_local * crowd_bones[mesh_functions::get_tag(v.instance_index) * CROWD_SLOT + v.muscle_joint.x];
@if(!COAT_CROWD && SKINNED)
    return joint_matrix(v.muscle_joint.x, v.instance_index, previous);
@if(!COAT_CROWD && !SKINNED)
    return instance_from_local;
}

// World-from-rest matrix of joint `index`, as `soft` indexes them (soft joints after the bones).
@if(COAT_SOFT)
fn soft_joint(v: CoatVertex, index: u32, instance_from_local: mat4x4<f32>, previous: bool) -> mat4x4<f32> {
@if(COAT_CROWD)
    return instance_from_local * crowd_bones[mesh_functions::get_tag(v.instance_index) * CROWD_SLOT + index];
@if(!COAT_CROWD && SKINNED)
    return joint_matrix(index, v.instance_index, previous);
@if(!COAT_CROWD && !SKINNED)
    return instance_from_local;
}

// Soft-tissue offset (world space): each region's soft joint less its bone, at the rest position,
// by the vertex's weight. Crowd soft matrices are baked, hero ones follow the springs.
@if(COAT_SOFT)
fn soft_offset(v: CoatVertex, rest: vec3<f32>, instance_from_local: mat4x4<f32>, previous: bool) -> vec3<f32> {
    let p = vec4<f32>(rest, 1.0);
    var offset = vec3<f32>(0.0);
    for (var k = 0u; k < 2u; k++) {
        let w = bitcast<f32>(v.soft[k * 2u + 1u]);
        if w > 0.0 {
            let j = v.soft[k * 2u];
            let soft = soft_joint(v, j & 0xffffu, instance_from_local, previous);
            let bone = soft_joint(v, j >> 16u, instance_from_local, previous);
            offset += ((soft * p).xyz - (bone * p).xyz) * w;
        }
    }
    return offset * coat.muscle.y;
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
