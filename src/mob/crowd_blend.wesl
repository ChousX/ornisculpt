// Crowd blend: one invocation per (slot, bone). Blends two baked frames and the idle pose from the
// clip atlas into the crowd bone buffer that crowd_skin.wesl reads.

struct CrowdInstance {
    frame_a: u32,
    frame_b: u32,
    idle: u32,
    bones: u32,
    weight_a: f32,
    weight_b: f32,
    weight_idle: f32,
    pad: u32,
};

// Matrices per mob slot (`CROWD_SLOT` in `anatomy`): crowd bones, then soft-tissue regions.
@if(SOFT_TISSUE)
const CROWD_SLOT: u32 = 40u;
@if(!SOFT_TISSUE)
const CROWD_SLOT: u32 = 32u;

@group(0) @binding(0) var<storage, read> clips: array<mat4x4<f32>>;
@group(0) @binding(1) var<storage, read> instances: array<CrowdInstance>;
@group(0) @binding(2) var<storage, read_write> bones: array<mat4x4<f32>>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let slot = id.x / CROWD_SLOT;
    let bone = id.x % CROWD_SLOT;
    if slot >= arrayLength(&instances) || id.x >= arrayLength(&bones) {
        return;
    }
    let inst = instances[slot];
    if bone >= inst.bones {
        return;
    }
    bones[id.x] = clips[inst.frame_a + bone] * inst.weight_a
        + clips[inst.frame_b + bone] * inst.weight_b
        + clips[inst.idle + bone] * inst.weight_idle;
}
