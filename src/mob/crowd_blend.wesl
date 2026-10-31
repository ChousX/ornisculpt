// Crowd blend: one invocation per (slot, bone). Starts from the body's idle pose and lays up to
// three layers of baked frame pairs from the clip atlas over it, each by its weight and mask row,
// into the crowd bone buffer that the coat's vertex stage (deform.wesl) reads.

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
    pad: vec2<u32>,
    layers: array<CrowdLayer, 3>,
};

const MASK_ALL: u32 = 0x7fffffffu;
const ADDITIVE: u32 = 0x80000000u;

// Matrices per mob slot (`CROWD_SLOT` in `anatomy`): crowd bones, then soft-tissue regions.
@if(SOFT_TISSUE)
const CROWD_SLOT: u32 = 40u;
@if(!SOFT_TISSUE)
const CROWD_SLOT: u32 = 32u;

@group(0) @binding(0) var<storage, read> clips: array<mat4x4<f32>>;
@group(0) @binding(1) var<storage, read> instances: array<CrowdInstance>;
@group(0) @binding(2) var<storage, read_write> bones: array<mat4x4<f32>>;

// Entry `i` of a per-bone row stored as consecutive matrices from `row`.
fn row_value(row: u32, i: u32) -> f32 {
    return clips[row + i / 16u][(i % 16u) / 4u][i % 4u];
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
    bones[id.x] = m;
}
