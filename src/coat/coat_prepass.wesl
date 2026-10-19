// Mob vertex shader for the depth, normal and motion-vector prepass and for shadow maps: the same
// surface as the main pass (`deform.wesl`), so shadows, SSAO, contact shadows and TAA follow the
// skinned, flexed mesh rather than Bevy's linear skinning of the rest mesh. Mirrors Bevy's
// `prepass.wesl` outputs (the fragment stage is still Bevy's).

import bevy_pbr::{
    prepass::io::VertexOutput,
    render::{mesh_functions, view_transformations::position_world_to_clip},
};
import package::coat::deform::{CoatVertex, deform};

@vertex
fn vertex(v: CoatVertex) -> VertexOutput {
    var out: VertexOutput;
    let s = deform(v, false);
    out.world_position = vec4<f32>(s.position, 1.0);
    out.position = position_world_to_clip(s.position);
@if(UNCLIPPED_DEPTH_ORTHO_EMULATION) {
    out.unclipped_depth = out.position.z;
    // Clamp depth to avoid clipping (directional shadow casters behind the near plane).
    out.position.z = min(out.position.z, 1.0);
}
@if(VERTEX_UVS_A)
    out.uv = v.uv;
@if(NORMAL_PREPASS_OR_DEFERRED_PREPASS) {
    out.world_normal = s.normal;
@if(VERTEX_TANGENTS)
    out.world_tangent = vec4<f32>(s.tangent, v.tangent.w);
}
@if(VERTEX_COLORS)
    out.color = v.color;
@if(MOTION_VECTOR_PREPASS)
    out.previous_world_position = vec4<f32>(deform(v, true).position, 1.0);
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = v.instance_index;
@if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        v.instance_index, mesh_functions::get_world_from_local(v.instance_index)[3]);
    return out;
}
