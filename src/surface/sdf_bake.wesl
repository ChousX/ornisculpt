// Bakes the packed body field (`sdf_field.wesl`) into the preview's brick atlas: one lattice
// point of one listed brick per invocation, from the brick's cell list (a run of invocations
// shares a brick, so the loop is mostly uniform). A cell lists everything within `LIST_PAD_VOXELS`
// of it, so the surface of anything else lies at least that far beyond its walls: values are
// capped there. Runs after each change, for the bricks whose key changed only (`list`).
//
// Per lattice point: the field's distance, the distances to the nearest eye and to the nearest
// other hard part (what the preview colours by), and the part of the nearest primitive (what it
// highlights by).

import package::surface::sdf_field::{PRIM_WORDS, HEADER_WORDS, bricks_info, brick_origin, cell_coords, cell_list, field, grid_info, word_u};

@group(0) @binding(1) var atlas: texture_storage_3d<rgba16float, write>;
// The bricks to bake: atlas slot, cell index.
@group(0) @binding(2) var<storage, read> list: array<vec2<u32>>;

// As `LIST_PAD_VOXELS` in `surface/preview.rs`.
const LIST_PAD_VOXELS: f32 = 2.0;
// As `NO_LIMB`: no primitive.
const NONE: u32 = 0xffffffffu;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let b = bricks_info();
    let per_brick = b.samples * b.samples * b.samples;
    let index = id.x + id.y * groups.x * 64u;
    let brick = index / per_brick;
    if brick >= arrayLength(&list) {
        return;
    }
    let entry = list[brick];
    let local = index % per_brick;
    let at = vec3<u32>(local % b.samples, (local / b.samples) % b.samples, local / (b.samples * b.samples));
    let g = grid_info();
    let c = cell_coords(g, entry.y);
    let voxel = g.cell / f32(b.voxels);
    let p = g.min + vec3<f32>(c) * g.cell + vec3<f32>(at) * voxel;
    let walls = min(vec3<f32>(at), vec3<f32>(f32(b.voxels)) - vec3<f32>(at)) * voxel;
    let cap = min(walls.x, min(walls.y, walls.z)) + LIST_PAD_VOXELS * voxel;
    var out = vec4<f32>(cap, cap, cap, 0.0);
    let cl = cell_list(g, c);
    if cl.y > 0u {
        let s = field(p, cl.x, cl.y, true);
        out = vec4<f32>(min(cap, s.d), min(cap, s.eye), min(cap, s.hard), 0.0);
        if s.nearest != NONE {
            out.w = f32(word_u(HEADER_WORDS + s.nearest * PRIM_WORDS + 6u).w);
        }
    }
    textureStore(atlas, brick_origin(b, entry.x) + at, out);
}
