// Bakes the packed body field (`sdf_field.wesl`) into the preview's brick atlas: one lattice
// point of one brick per invocation, from the brick's cell list (a run of invocations shares a
// brick, so the loop is mostly uniform). A cell lists everything within `LIST_PAD_VOXELS` of it,
// so the surface of anything else lies at least that far beyond its walls: the value is capped
// there. Runs once per edit.

import package::surface::sdf_field::{bricks_info, brick_origin, cell_coords, cell_list, field, grid_info, word_u};

@group(0) @binding(1) var atlas: texture_storage_3d<rgba16float, write>;

// As `LIST_PAD_VOXELS` in `surface/preview.rs`.
const LIST_PAD_VOXELS: f32 = 2.0;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let b = bricks_info();
    let per_brick = b.samples * b.samples * b.samples;
    let index = id.x + id.y * groups.x * 64u;
    let brick = index / per_brick;
    if brick >= b.count {
        return;
    }
    let local = index % per_brick;
    let at = vec3<u32>(local % b.samples, (local / b.samples) % b.samples, local / (b.samples * b.samples));
    let g = grid_info();
    let c = cell_coords(g, word_u(b.first + brick).x);
    let voxel = g.cell / f32(b.voxels);
    let p = g.min + vec3<f32>(c) * g.cell + vec3<f32>(at) * voxel;
    let walls = min(vec3<f32>(at), vec3<f32>(f32(b.voxels)) - vec3<f32>(at)) * voxel;
    var d = min(walls.x, min(walls.y, walls.z)) + LIST_PAD_VOXELS * voxel;
    let list = cell_list(g, c);
    if list.y > 0u {
        d = min(d, field(p, list.x, list.y, true).d);
    }
    textureStore(atlas, brick_origin(b, brick) + at, vec4<f32>(d, 0.0, 0.0, 0.0));
}
