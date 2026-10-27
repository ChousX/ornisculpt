// Mob vertex shader for every coat layer and tier (main pass). The deformation itself (skinning,
// centres of rotation, fur shells, muscle flex) is `deform.wesl`, shared with the prepass.
// - tangent.w is the distance along the coat flow (metres, baked at generation).
// - Vertex colour = rest-pose position (xyz) + scale weight (w); tangent = coat flow; on the
//   skin, uv = (fur, feather) weights; paint = the coat markings' colour (`paint_io.wesl`).

import bevy_pbr::render::view_transformations::position_world_to_clip;
import package::coat::deform::{CoatVertex, deform};
import package::coat::paint_io::CoatOutput;

@vertex
fn vertex(v: CoatVertex) -> CoatOutput {
    var out: CoatOutput;
    let s = deform(v, false);
    out.world_position = vec4<f32>(s.position, 1.0);
    out.position = position_world_to_clip(s.position);
    out.world_normal = s.normal;
@if(VERTEX_UVS_A)
    out.uv = v.uv;
@if(VERTEX_TANGENTS)
    out.world_tangent = vec4<f32>(s.tangent, v.tangent.w);
@if(VERTEX_COLORS)
    out.color = v.color;
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = v.instance_index;
@if(COAT_PAINT)
    out.paint = v.paint;
    return out;
}
