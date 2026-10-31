// Crowd blend: one invocation per (slot, bone). Starts from the body's idle pose and lays up to
// three layers of baked frame pairs from the clip atlas over it, each by its weight and mask row,
// then sits the pose on the ground (root lift and tilt, a vertical offset down each leg), into
// the crowd bone buffer that the coat's vertex stage (deform.wesl) reads.

struct CrowdLayer {
    frame_a: u32,
    frame_b: u32,
    // Mask row index, MASK_ALL for every bone; ADDITIVE flag in the top bit.
    mask: u32,
    // (t, weight) as unorm16 pairs.
    blend: u32,
};

struct CrowdInstance {
    idle: u32,
    bones: u32,
    // Leg rows (leg index, then fraction down the leg), MASK_ALL for none.
    legs: u32,
    lift: f32,
    tilt: vec4<f32>,
    // Eight foot offsets as unorm16 pairs over FOOT_RANGE metres.
    feet: vec4<u32>,
    layers: array<CrowdLayer, 3>,
};

const MASK_ALL: u32 = 0x7fffffffu;
const ADDITIVE: u32 = 0x80000000u;
const FOOT_RANGE: f32 = 8.0;

// Matrices per mob slot (`CROWD_SLOT` in `anatomy`): crowd bones, then soft-tissue regions; and
// the matrices a per-bone row of floats takes (`ROW_MATRICES`).
@if(SOFT_TISSUE)
const CROWD_SLOT: u32 = 40u;
@if(SOFT_TISSUE)
const ROW_MATRICES: u32 = 3u;
@if(!SOFT_TISSUE)
const CROWD_SLOT: u32 = 32u;
@if(!SOFT_TISSUE)
const ROW_MATRICES: u32 = 2u;

@group(0) @binding(0) var<storage, read> clips: array<mat4x4<f32>>;
@group(0) @binding(1) var<storage, read> instances: array<CrowdInstance>;
@group(0) @binding(2) var<storage, read_write> bones: array<mat4x4<f32>>;

// Entry `i` of a per-bone row stored as consecutive matrices from `row`.
fn row_value(row: u32, i: u32) -> f32 {
    return clips[row + i / 16u][(i % 16u) / 4u][i % 4u];
}

fn rotation(q: vec4<f32>) -> mat4x4<f32> {
    let x2 = q.x + q.x;
    let y2 = q.y + q.y;
    let z2 = q.z + q.z;
    let xx = q.x * x2;
    let xy = q.x * y2;
    let xz = q.x * z2;
    let yy = q.y * y2;
    let yz = q.y * z2;
    let zz = q.z * z2;
    let wx = q.w * x2;
    let wy = q.w * y2;
    let wz = q.w * z2;
    return mat4x4<f32>(
        vec4<f32>(1.0 - (yy + zz), xy + wz, xz - wy, 0.0),
        vec4<f32>(xy - wz, 1.0 - (xx + zz), yz + wx, 0.0),
        vec4<f32>(xz + wy, yz - wx, 1.0 - (xx + yy), 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x / CROWD_SLOT;
    let bone = id.x % CROWD_SLOT;
    if slot >= arrayLength(&instances) || id.x >= arrayLength(&bones) {
        return;
    }
    var inst = instances[slot];
    if bone >= inst.bones {
        return;
    }
    let idle = clips[inst.idle + bone];
    var m = idle;
    for (var l = 0u; l < 3u; l++) {
        let layer = inst.layers[l];
        let tw = unpack2x16unorm(layer.blend);
        var w = tw.y;
        if w <= 0.0 {
            continue;
        }
        let mask = layer.mask & MASK_ALL;
        if mask != MASK_ALL {
            w *= row_value(mask, bone);
        }
        if w <= 0.0 {
            continue;
        }
        let f = clips[layer.frame_a + bone] * (1.0 - tw.x) + clips[layer.frame_b + bone] * tw.x;
        if (layer.mask & ADDITIVE) != 0u {
            m += (f - idle) * w;
        } else {
            m = m * (1.0 - w) + f * w;
        }
    }
    // The ground: the pose tilted about the mob's origin and raised, then each leg's bones moved
    // by their foot's offset, more the further down the leg.
    if inst.lift != 0.0 || inst.tilt.w != 1.0 {
        m = rotation(inst.tilt) * m;
        m[3].y += inst.lift;
    }
    if inst.legs != MASK_ALL {
        let leg = row_value(inst.legs, bone);
        if leg >= 0.0 {
            let li = u32(leg + 0.5);
            let pair = unpack2x16unorm(inst.feet[li / 2u]);
            let offset = (select(pair.x, pair.y, (li & 1u) == 1u) - 0.5) * FOOT_RANGE;
            m[3].y += offset * row_value(inst.legs + ROW_MATRICES, bone);
        }
    }
    bones[id.x] = m;
}
