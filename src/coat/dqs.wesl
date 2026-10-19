// Dual quaternion skinning (Kavan, Collins, Žára & O'Sullivan 2008), scale-aware: each joint's
// skin matrix splits into rotation, translation and the stretch left over (Rᵀ · linear); stretches
// blend linearly, the rigid parts as dual quaternions. Linear blending averages matrices and
// shrinks limbs at bent and twisted joints; this keeps their volume.
//
// Mirrors `src/coat/dqs.rs`, which is the tested reference.

struct Skinned {
    position: vec3<f32>,
    normal: vec3<f32>,
    tangent: vec3<f32>,
};

fn quat_mul(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(b.xyz * a.w + a.xyz * b.w + cross(a.xyz, b.xyz), a.w * b.w - dot(a.xyz, b.xyz));
}

fn quat_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    return v + 2.0 * cross(q.xyz, cross(q.xyz, v) + q.w * v);
}

// Rotation matrix (columns) to quaternion (x, y, z, w).
fn quat_from_rotation(m: mat3x3<f32>) -> vec4<f32> {
    let trace = m[0].x + m[1].y + m[2].z;
    if trace > 0.0 {
        let s = sqrt(trace + 1.0) * 2.0;
        return vec4<f32>((m[1].z - m[2].y) / s, (m[2].x - m[0].z) / s, (m[0].y - m[1].x) / s, 0.25 * s);
    }
    if m[0].x > m[1].y && m[0].x > m[2].z {
        let s = sqrt(1.0 + m[0].x - m[1].y - m[2].z) * 2.0;
        return vec4<f32>(0.25 * s, (m[1].x + m[0].y) / s, (m[2].x + m[0].z) / s, (m[1].z - m[2].y) / s);
    }
    if m[1].y > m[2].z {
        let s = sqrt(1.0 + m[1].y - m[0].x - m[2].z) * 2.0;
        return vec4<f32>((m[1].x + m[0].y) / s, 0.25 * s, (m[2].y + m[1].z) / s, (m[2].x - m[0].z) / s);
    }
    let s = sqrt(1.0 + m[2].z - m[0].x - m[1].y) * 2.0;
    return vec4<f32>((m[2].x + m[0].z) / s, (m[2].y + m[1].z) / s, 0.25 * s, (m[0].y - m[1].x) / s);
}

fn inverse_transpose(m: mat3x3<f32>) -> mat3x3<f32> {
    let x = cross(m[1], m[2]);
    let y = cross(m[2], m[0]);
    let z = cross(m[0], m[1]);
    let det = dot(m[2], z);
    return mat3x3<f32>(x / det, y / det, z / det);
}

// Skins a point, normal and tangent by four joints' skin matrices.
fn skin(joints: array<mat4x4<f32>, 4>, weights: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, tangent: vec3<f32>) -> Skinned {
    var real = vec4<f32>(0.0);
    var dual = vec4<f32>(0.0);
    var stretch = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    var first = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    var have_first = false;
    for (var i = 0u; i < 4u; i++) {
        let w = weights[i];
        if w <= 0.0 {
            continue;
        }
        let m = joints[i];
        let linear = mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
        // Gram–Schmidt on the columns: the nearest rotation for a rotation · scale.
        let c0 = normalize(linear[0]);
        let c1 = normalize(linear[1] - c0 * dot(linear[1], c0));
        let r = mat3x3<f32>(c0, c1, cross(c0, c1));
        var q = normalize(quat_from_rotation(r));
        if !have_first {
            first = q;
            have_first = true;
        } else if dot(q, first) < 0.0 {
            // Same hemisphere as the first joint, or the blend takes the long way round.
            q = -q;
        }
        real += q * w;
        dual += quat_mul(vec4<f32>(m[3].xyz, 0.0), q) * (0.5 * w);
        stretch += (transpose(r) * linear) * w;
    }
    let len = max(length(real), 1e-8);
    real /= len;
    dual /= len;
    let translation = quat_mul(dual, vec4<f32>(-real.xyz, real.w)).xyz * 2.0;
    let fix = inverse_transpose(stretch);
    var out: Skinned;
    out.position = quat_rotate(real, stretch * position) + translation;
    out.normal = normalize(quat_rotate(real, fix * normal));
    out.tangent = normalize(quat_rotate(real, stretch * tangent));
    return out;
}

// Linear blend skinning (averaged matrices), for comparison (`ORNISCULPT_LINEAR_SKIN`).
fn skin_linear(joints: array<mat4x4<f32>, 4>, weights: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, tangent: vec3<f32>) -> Skinned {
    let m = joints[0] * weights.x + joints[1] * weights.y + joints[2] * weights.z + joints[3] * weights.w;
    let linear = mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
    var out: Skinned;
    out.position = (m * vec4<f32>(position, 1.0)).xyz;
    out.normal = normalize(inverse_transpose(linear) * normal);
    out.tangent = normalize(linear * tangent);
    return out;
}

// Optimized-centre skinning (Le & Hodgins 2016): the blended rotation turns the vertex about its
// centre of rotation, which linear blending carries: R · S · (p − cor) + LBS(cor). Keeps the
// volume dual quaternions keep, without their bulge at ball joints.
fn skin_cor(joints: array<mat4x4<f32>, 4>, weights: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, tangent: vec3<f32>, cor: vec3<f32>) -> Skinned {
    var q = vec4<f32>(0.0);
    var first = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    var have_first = false;
    var stretch = mat3x3<f32>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    var lbs = mat4x4<f32>(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    for (var i = 0u; i < 4u; i++) {
        let w = weights[i];
        if w <= 0.0 {
            continue;
        }
        let m = joints[i];
        let linear = mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
        let c0 = normalize(linear[0]);
        let c1 = normalize(linear[1] - c0 * dot(linear[1], c0));
        let r = mat3x3<f32>(c0, c1, cross(c0, c1));
        var qi = normalize(quat_from_rotation(r));
        if !have_first {
            first = qi;
            have_first = true;
        } else if dot(qi, first) < 0.0 {
            qi = -qi;
        }
        q += qi * w;
        stretch += (transpose(r) * linear) * w;
        lbs += m * w;
    }
    q = normalize(q);
    let centre = (lbs * vec4<f32>(cor, 1.0)).xyz;
    var out: Skinned;
    out.position = quat_rotate(q, stretch * (position - cor)) + centre;
    out.normal = normalize(quat_rotate(q, inverse_transpose(stretch) * normal));
    out.tangent = normalize(quat_rotate(q, stretch * tangent));
    return out;
}

// The skinning every mob vertex uses: about its centre of rotation where the mesh has them
// (COAT_COR), dual quaternions otherwise, linear blending if COAT_LINEAR_SKIN.
fn blend(joints: array<mat4x4<f32>, 4>, weights: vec4<f32>, position: vec3<f32>, normal: vec3<f32>, tangent: vec3<f32>, cor: vec3<f32>) -> Skinned {
@if(COAT_LINEAR_SKIN)
    return skin_linear(joints, weights, position, normal, tangent);
@if(!COAT_LINEAR_SKIN && COAT_COR)
    return skin_cor(joints, weights, position, normal, tangent, cor);
@if(!COAT_LINEAR_SKIN && !COAT_COR)
    return skin(joints, weights, position, normal, tangent);
}
