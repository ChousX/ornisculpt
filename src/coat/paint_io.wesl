// The main pass's vertex output: Bevy's `VertexOutput` plus the coat markings' colour
// (`COAT_PAINT`: linear RGB, and in w how much it replaces the coat's own colours). The fragment
// stage hands Bevy its own struct back through `bevy_output`.

import bevy_pbr::render::forward_io::VertexOutput;

struct CoatOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @if(VERTEX_UVS_A)
    @location(2) uv: vec2<f32>,
    @if(VERTEX_UVS_B)
    @location(3) uv_b: vec2<f32>,
    @if(VERTEX_TANGENTS)
    @location(4) world_tangent: vec4<f32>,
    @if(VERTEX_COLORS)
    @location(5) color: vec4<f32>,
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    @location(6) @interpolate(flat) instance_index: u32,
    @if(VISIBILITY_RANGE_DITHER)
    @location(7) @interpolate(flat) visibility_range_dither: i32,
    @if(COAT_PAINT)
    @location(8) paint: vec4<f32>,
}

fn bevy_output(o: CoatOutput) -> VertexOutput {
    var out: VertexOutput;
    out.position = o.position;
    out.world_position = o.world_position;
    out.world_normal = o.world_normal;
@if(VERTEX_UVS_A)
    out.uv = o.uv;
@if(VERTEX_UVS_B)
    out.uv_b = o.uv_b;
@if(VERTEX_TANGENTS)
    out.world_tangent = o.world_tangent;
@if(VERTEX_COLORS)
    out.color = o.color;
@if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = o.instance_index;
@if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = o.visibility_range_dither;
    return out;
}

// The markings' colour (w = how much of it shows), zero without any.
fn paint_of(o: CoatOutput) -> vec4<f32> {
    var paint = vec4<f32>(0.0);
@if(COAT_PAINT)
    paint = o.paint;
    return paint;
}
