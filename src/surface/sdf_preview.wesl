// SDF preview: draws a body straight from its packed field (`sdf_field.wesl`), so an edit shows on
// the next frame without meshing. Lit like a StandardMaterial and writes its own depth, so it sits
// in the scene like the mesh it stands in for.
//
// Each ray starts where it enters the hull (the cells that may hold surface) and sphere traces the
// distance bricks baked from the field after each change (`sdf_bake.wesl`), read trilinearly: a
// texture fetch a step rather than an evaluation of the primitives (several times dearer on a
// laptop GPU). Cells without a brick hold no surface and are crossed in one step. Details thinner
// than a voxel soften or drop out; the mesh that follows has them. What is there (an eye, a hard
// part, which part) comes from the bricks too: the primitives are never evaluated here, which keeps
// the shader small (evaluating them once at the hit cost more than the rest of the shading, mostly
// by slowing the march around it).

import bevy_pbr::render::{
    mesh_view_bindings::view,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::VertexOutput,
};
import package::surface::sdf_field::{Bricks, Grid, brick_origin, bricks_info, cell_at, cell_list, exit_t, grid_info, part_lit};

struct PreviewUniform {
    model_from_world: mat4x4<f32>,
    world_from_model: mat4x4<f32>,
    // rgb, strength
    highlight: vec4<f32>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> preview: PreviewUniform;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var atlas: texture_3d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var atlas_sampler: sampler;

const MAX_STEPS: u32 = 128u;
// Of the baked distance, per step: a whole step (the trilinear distance only overshoots by
// fractions of a voxel, and nothing thinner than that survives the lattice anyway).
const STEP: f32 = 1.0;

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

// Where `p` falls in the brick `brick` of cell `c`, in atlas texels.
fn brick_texel(g: Grid, b: Bricks, c: vec3<u32>, brick: u32, p: vec3<f32>) -> vec3<f32> {
    let local = clamp((p - (g.min + vec3<f32>(c) * g.cell)) / g.cell, vec3<f32>(0.0), vec3<f32>(1.0)) * f32(b.voxels);
    return vec3<f32>(brick_origin(b, brick)) + local;
}

// The baked values at `p`, in cell `c` whose brick is `brick` (trilinear between lattice points):
// distance, distance to the nearest eye, to the nearest other hard part, and a part (meaningless
// between lattice points: see `brick_part`).
fn brick_sample(g: Grid, b: Bricks, c: vec3<u32>, brick: u32, p: vec3<f32>) -> vec4<f32> {
    let texel = brick_texel(g, b, c, brick, p) + vec3<f32>(0.5);
    return textureSampleLevel(atlas, atlas_sampler, texel / vec3<f32>(textureDimensions(atlas)), 0.0);
}

fn brick_distance(g: Grid, b: Bricks, c: vec3<u32>, brick: u32, p: vec3<f32>) -> f32 {
    return brick_sample(g, b, c, brick, p).x;
}

// The part of the primitive nearest the lattice point closest to `p`.
fn brick_part(g: Grid, b: Bricks, c: vec3<u32>, brick: u32, p: vec3<f32>) -> u32 {
    let texel = vec3<u32>(round(brick_texel(g, b, c, brick, p)));
    return u32(textureLoad(atlas, texel, 0).w + 0.5);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let g = grid_info();
    let lo = g.min;
    let hi = g.min + vec3<f32>(g.dims) * g.cell;
    let camera = (preview.model_from_world * vec4<f32>(view.world_position, 1.0)).xyz;
    let surface = (preview.model_from_world * in.world_position).xyz;
    let dir = normalize(surface - camera);
    let inv = 1.0 / select(dir, vec3<f32>(1e-12), abs(dir) < vec3<f32>(1e-12));
    let t_far = exit_t(camera, inv, lo, hi);
    // From just before the hull face this fragment is on.
    var t = max(dot(surface - camera, dir) - g.cell * 1e-3, 0.0);
    var hit = false;
    var p = camera;
    var list = vec4<u32>(0u);
    let b = bricks_info();
    let voxel = g.cell / f32(b.voxels);
    for (var i = 0u; i < MAX_STEPS; i++) {
        if t > t_far {
            break;
        }
        p = camera + dir * t;
        let c = cell_at(g, p);
        let w = cell_list(g, c);
        if w.z == 0u {
            // No surface in this cell: on to the next.
            let c_lo = g.min + vec3<f32>(c) * g.cell;
            t += max(exit_t(p, inv, c_lo, c_lo + vec3<f32>(g.cell)), 0.0) + g.cell * 1e-3;
            continue;
        }
        // Sphere trace the baked distance (trilinear, so a touch rounder than the field).
        let d = brick_distance(g, b, c, w.z - 1u, p);
        if d < max(2e-4, t * 4e-4) {
            hit = true;
            list = w;
            break;
        }
        t += d * STEP;
    }
    if !hit {
        discard;
    }

    // Normal from the baked distance's gradient: tetrahedron taps half a voxel out, each read from
    // the brick of the cell it lands in (bricks agree on their shared walls, so no seams), or from
    // the hit's own brick, clamped to its walls, where that cell has none (a cell without a brick
    // has no distances, only a flat stand-in). Facing the camera where a sliver too thin for the
    // lattice leaves no gradient.
    let c = cell_at(g, p);
    var n_model = vec3<f32>(0.0);
    for (var k = 0u; k < 4u; k++) {
        let tap = vec3<f32>(f32((k + 1u) & 2u) - 1.0, f32((k >> 1u) & 1u) * 2.0 - 1.0, f32(k & 1u) * 2.0 - 1.0);
        let q = p + tap * voxel * 0.5;
        let cq = cell_at(g, q);
        let brick = cell_list(g, cq).z;
        if brick == 0u {
            n_model += tap * brick_distance(g, b, c, list.z - 1u, q);
        } else {
            n_model += tap * brick_distance(g, b, cq, brick - 1u, q);
        }
    }
    if dot(n_model, n_model) < 1e-12 {
        n_model = -dir;
    }
    let world = (preview.world_from_model * vec4<f32>(p, 1.0)).xyz;
    // Normals go through the inverse transpose of world_from_model.
    let n_world = normalize((transpose(preview.model_from_world) * vec4<f32>(n_model, 0.0)).xyz);

    var vo = in;
    vo.world_position = vec4<f32>(world, 1.0);
    vo.world_normal = n_world;
    let clip = view.clip_from_world * vec4<f32>(world, 1.0);
    let depth = clip.z / clip.w;
    vo.position = vec4<f32>(in.position.xy, depth, in.position.w);
    var pbr = pbr_input_from_standard_material(vo, true);
    pbr.world_position = vec4<f32>(world, 1.0);
    pbr.world_normal = n_world;
    pbr.N = n_world;
    pbr.V = normalize(view.world_position - world);
    pbr.frag_coord = vec4<f32>(in.position.xy, depth, in.position.w);

    // What the surface here is: an eye or a hard part where that is as near as the field (hard
    // parts join it with a hard union, so on one its distance is the field's), skin otherwise; and
    // tinted where its part is selected.
    let at = brick_sample(g, b, c, list.z - 1u, p);
    let near = at.x + voxel * 0.1;
    var color = pbr.material.base_color.rgb;
    if at.y <= near {
        color = vec3<f32>(0.03, 0.02, 0.015);
        pbr.material.perceptual_roughness = 0.1;
    } else if at.z <= near {
        color = vec3<f32>(0.85, 0.8, 0.68);
        pbr.material.perceptual_roughness = 0.4;
    }
    if part_lit(brick_part(g, b, c, list.z - 1u, p)) {
        color = mix(color, preview.highlight.rgb, preview.highlight.w);
    }
    pbr.material.base_color = vec4<f32>(color, pbr.material.base_color.a);

    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr, apply_pbr_lighting(pbr));
    out.depth = depth;
    return out;
}
