// Mob fragment shader: StandardMaterial lighting with coat shading.
// - COAT_SKIN: base colour; scales (cellular colour + relief) where covered; fur/feather regions
//   get a darker undercoat.
// - COAT_SHELL: fur strands, alpha-tested per shell height.
// - COAT_FEATHER: feather vane shape on a card, alpha-tested.
// - COAT_HARD: plain colour.
// Vertex inputs: colour = rest-pose position (xyz) + scale weight (w); on the skin, uv = (fur,
// feather) weights; world_tangent = coat flow (xyz) + distance along it in metres (w, skin only).
//
// Crowd tiers have no shells or cards, so the skin carries fur and feathers itself: fur as
// combed clumps with a soft tip-coloured rim, feathers as overlapping staggered vanes. Every
// pattern fades to its average colour once it gets smaller than a pixel, so distance never
// shimmers.

import bevy_pbr::render::{
    mesh_view_bindings::view,
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::{VertexOutput, FragmentOutput},
};

struct CoatUniform {
    base: vec4<f32>,
    secondary: vec4<f32>,
    accent: vec4<f32>,
    scale_mask: vec2<u32>,
    fur_mask: vec2<u32>,
    feather_mask: vec2<u32>,
    pad: vec2<u32>,
    scales: vec4<f32>,
    fur: vec4<f32>,
    feathers: vec4<f32>,
    cloth: array<vec4<f32>, 8>,
};

@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> coat: CoatUniform;

fn hash3(p: vec3<f32>) -> vec3<f32> {
    var q = vec3<f32>(
        dot(p, vec3<f32>(127.1, 311.7, 74.7)),
        dot(p, vec3<f32>(269.5, 183.3, 246.1)),
        dot(p, vec3<f32>(113.5, 271.9, 124.6)),
    );
    return fract(sin(q) * 43758.5453);
}

// Cellular noise: (distance to nearest, distance to second nearest, nearest cell's random).
fn cellular(p: vec3<f32>) -> vec3<f32> {
    let cell = floor(p);
    var f1 = 8.0;
    var f2 = 8.0;
    var id = 0.0;
    for (var z = -1; z <= 1; z++) {
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let c = cell + vec3<f32>(f32(x), f32(y), f32(z));
                let r = hash3(c);
                let d = length(c + r - p);
                if d < f1 {
                    f2 = f1;
                    f1 = d;
                    id = r.x;
                } else if d < f2 {
                    f2 = d;
                }
            }
        }
    }
    return vec3<f32>(f1, f2, id);
}

// Cheaper cellular noise for crowds: searches the 2×2×2 cells nearest the point (8 instead of 27).
fn cellular_fast(p: vec3<f32>) -> vec3<f32> {
    let cell = floor(p - 0.5);
    var f1 = 8.0;
    var f2 = 8.0;
    var id = 0.0;
    for (var i = 0; i < 8; i++) {
        let c = cell + vec3<f32>(f32(i & 1), f32((i >> 1) & 1), f32((i >> 2) & 1));
        let r = hash3(c);
        let d = length(c + r - p);
        if d < f1 {
            f2 = f1;
            f1 = d;
            id = r.x;
        } else if d < f2 {
            f2 = d;
        }
    }
    return vec3<f32>(f1, f2, id);
}

fn hash1(p: vec3<f32>) -> f32 {
    return fract(sin(dot(p, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
}

// Smooth 3D value noise in 0..1.
fn value3(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let x00 = mix(hash1(i), hash1(i + vec3<f32>(1.0, 0.0, 0.0)), u.x);
    let x10 = mix(hash1(i + vec3<f32>(0.0, 1.0, 0.0)), hash1(i + vec3<f32>(1.0, 1.0, 0.0)), u.x);
    let x01 = mix(hash1(i + vec3<f32>(0.0, 0.0, 1.0)), hash1(i + vec3<f32>(1.0, 0.0, 1.0)), u.x);
    let x11 = mix(hash1(i + vec3<f32>(0.0, 1.0, 1.0)), hash1(i + vec3<f32>(1.0, 1.0, 1.0)), u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// World size of one pixel at `world` (no derivatives, so it is safe in any control flow).
fn pixel_size(world: vec3<f32>) -> f32 {
    let d = length(world - view.world_position);
    let ortho = view.clip_from_view[3][3] == 1.0;
    let per_metre = select(d, 1.0, ortho);
    return 2.0 * per_metre / (view.clip_from_view[1][1] * view.viewport.w);
}

// How much of a pattern with features of `size` metres survives: 1 while a feature spans a few
// pixels, 0 once it is about a pixel.
fn detail(size: f32, pixel: f32) -> f32 {
    return 1.0 - smoothstep(0.3, 0.9, pixel / max(size, 1e-5));
}

struct FeatherHit {
    // along the visible feather (0 just below the tip above it .. 1 at its own tip),
    // distance from the vane edge (0 edge .. ~0.5 centre), per-feather random
    y: f32,
    edge: f32,
    id: f32,
};

fn feather_cell(rest: vec3<f32>, width: f32, row: f32) -> vec3<f32> {
    return cellular(rest / width + row * vec3<f32>(7.31, 3.17, 5.93));
}

// Overlapping feathers: rows run along the flow (`along` in row units); within a row the
// feathers are 3D Voronoi cells (re-seeded per row, so rows stagger). Each feather's tip is
// rounded: it reaches the end of its row at the centre and stops short at the edges, where the
// next row's feather shows instead.
fn feather_at(rest: vec3<f32>, along: f32, width: f32) -> FeatherHit {
    let row = floor(along);
    let y = fract(along);
    let c = feather_cell(rest, width, row);
    let edge = clamp(c.y - c.x, 0.0, 1.0);
    let tip = 1.0 - 0.45 * pow(1.0 - smoothstep(0.0, 0.45, edge), 2.0);
    if y <= tip {
        return FeatherHit(y / tip, edge, c.z);
    }
    let n = feather_cell(rest, width, row + 1.0);
    return FeatherHit((y - tip) * 0.5, clamp(n.y - n.x, 0.0, 1.0), n.z);
}

// Bump the normal by a height field using screen-space derivatives (no tangents needed).
fn bump(n: vec3<f32>, world: vec3<f32>, height: f32) -> vec3<f32> {
    let dp_dx = dpdx(world);
    let dp_dy = dpdy(world);
    let dh_dx = dpdx(height);
    let dh_dy = dpdy(height);
    let r1 = cross(dp_dy, n);
    let r2 = cross(n, dp_dx);
    let det = dot(dp_dx, r1);
    let grad = sign(det) * (dh_dx * r1 + dh_dy * r2);
    return normalize(abs(det) * n - grad);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr = pbr_input_from_standard_material(in, is_front);
    var color = coat.base.rgb;

    var rest = in.world_position.xyz;
    var scale_w = 0.0;
@if(VERTEX_COLORS) {
    rest = in.color.xyz;
    scale_w = in.color.w;
}
    var weights = vec2<f32>(0.0);
@if(COAT_SKIN && VERTEX_UVS_A) {
    weights = in.uv;
}
    var along = 0.0;
    var T = vec3<f32>(0.0, 0.0, 1.0);
@if(VERTEX_TANGENTS) {
    along = in.world_tangent.w;
    T = normalize(in.world_tangent.xyz - pbr.N * dot(in.world_tangent.xyz, pbr.N));
}
    let B = cross(pbr.N, T);
    var roughness = pbr.material.perceptual_roughness;
    var reflectance = pbr.material.reflectance;

    let pixel = pixel_size(in.world_position.xyz);

@if(COAT_SKIN) {
    // Scales. Hero: computed for every pixel when the coat has any (uniform branch) so the
    // derivatives used for relief stay in uniform control flow. Crowd: no relief, so uncovered
    // pixels skip it and covered ones use the cheaper search.
@if(COAT_CROWD) {
    if scale_w > 0.01 {
        let fine = detail(coat.scales.x, pixel);
        var scaled = mix(coat.base.rgb, coat.secondary.rgb, 0.5 * coat.scales.z) * 0.85;
        if fine > 0.0 {
            let c = cellular_fast(rest / max(coat.scales.x, 1e-4));
            let dome = smoothstep(0.0, 0.35, c.y - c.x);
            let tint = mix(coat.base.rgb, coat.secondary.rgb, c.z * coat.scales.z) * mix(0.6, 1.0, dome);
            scaled = mix(scaled, tint, fine);
        }
        color = mix(color, scaled, scale_w);
    }
}
@if(!COAT_CROWD) {
    if coat.scales.x > 0.0 && any(coat.scale_mask != vec2<u32>(0u)) {
        let c = cellular(rest / max(coat.scales.x, 1e-4));
        let edge = c.y - c.x;
        let dome = smoothstep(0.0, 0.35, edge);
        let fine = detail(coat.scales.x, pixel);
        let height = dome * coat.scales.y * coat.scales.x * 0.3 * scale_w * fine;
        let tint = mix(coat.base.rgb, coat.secondary.rgb, c.z * coat.scales.z) * mix(0.55, 1.0, dome);
        let avg = mix(coat.base.rgb, coat.secondary.rgb, 0.5 * coat.scales.z) * 0.85;
        color = mix(color, mix(avg, tint, fine), scale_w);
        pbr.N = bump(pbr.N, in.world_position.xyz, height);
    }
}

    // Fur and feathers. Hero: a dark undercoat beneath shells and cards.
@if(!COAT_CROWD) {
    color = mix(color, coat.secondary.rgb * 0.7, max(weights.x, weights.y));
}
@if(COAT_CROWD) {
    let ndv = clamp(dot(pbr.N, pbr.V), 0.0, 1.0);
    if weights.x > 0.01 {
        // Clumps a few strands wide, mottled at two scales; strands lie along the flow.
        let clump = max(coat.fur.x * 1.2, 2.0 / max(coat.fur.y, 1.0));
        let fine = detail(clump, pixel);
        var n1 = 0.5;
        var n2 = 0.5;
        var grain = 0.5;
        if fine > 0.0 {
            n1 = value3(rest / clump);
            n2 = value3(rest / (clump * 0.4) + 17.0);
            let g = detail(clump * 0.12, pixel);
            if g > 0.0 {
                grain = mix(0.5, value3(rest / (clump * 0.12)), g);
            }
        }
        let mottle = mix(0.5, n1 * 0.65 + n2 * 0.35, fine);
        var fur = mix(coat.base.rgb, coat.secondary.rgb, (1.0 - mottle) * 0.4);
        fur = mix(fur, coat.accent.rgb, coat.fur.w * 0.2) * mix(0.8, 1.06, mottle);
        // Tips catch light at grazing angles: a soft, tip-coloured fuzz around the outline.
        let rim = pow(1.0 - ndv, 3.0);
        fur = mix(fur, mix(coat.base.rgb, coat.accent.rgb, coat.fur.w) * 1.1, rim * 0.5);
        // Fine strand grain while it is still resolvable.
        fur *= 0.94 + 0.12 * grain;
        let N = normalize(pbr.N + (T * (n2 - 0.5) + B * (n1 - 0.5)) * 0.1 * fine);
        pbr.N = normalize(mix(pbr.N, N, weights.x));
        color = mix(color, fur, weights.x);
        roughness = mix(roughness, 1.0, weights.x);
        reflectance = mix(reflectance, vec3<f32>(0.1), weights.x);
    }
    if weights.y > 0.01 {
        // Visible feather rows are about half a feather long (they overlap by half).
        let row_len = max(coat.feathers.x * 0.5, 1e-3);
        let width = max(coat.feathers.y, 1e-3);
        let fine = detail(min(row_len, width), pixel);
        var f = FeatherHit(0.6, 0.25, 0.5);
        if fine > 0.0 {
            f = feather_at(rest, along / row_len, width);
        }
        var vane = mix(coat.secondary.rgb, coat.base.rgb, smoothstep(0.0, 0.6, f.y));
        vane = mix(vane, coat.accent.rgb, smoothstep(0.8, 1.0, f.y) * coat.feathers.z * 0.6);
        vane *= mix(0.9, 1.08, f.id);
        // Shadow under the feather above, a dark outline where vanes overlap, a lighter shaft.
        let shade = mix(0.7, 1.0, smoothstep(0.0, 0.35, f.y)) * mix(0.82, 1.0, smoothstep(0.0, 0.12, f.edge));
        let shaft = select(1.0, 1.12, f.edge > 0.42);
        let patterned = vane * shade * shaft;
        let avg = mix(mix(coat.secondary.rgb, coat.base.rgb, 0.65), coat.accent.rgb, 0.15 * coat.feathers.z) * 0.85;
        let feather = mix(avg, patterned, fine);
        // Each feather lifts toward its tip.
        let N = normalize(pbr.N + T * (0.5 - f.y) * 0.5 * fine);
        pbr.N = normalize(mix(pbr.N, N, weights.y));
        color = mix(color, feather, weights.y);
        roughness = mix(roughness, 0.75, weights.y);
    }
}
}

@if(COAT_SHELL) {
    var h = 0.0;
@if(VERTEX_UVS_A) {
    h = in.uv.x;
}
    // One strand per cell, thinning to a point at full length.
    let c = cellular(rest * coat.fur.y);
    let radius = 0.5 * coat.fur.z * (1.0 - h);
    if c.x > radius {
        discard;
    }
    let root = mix(0.35, 1.0, h);
    color = mix(coat.base.rgb, coat.accent.rgb, h * coat.fur.w) * root;
}

@if(COAT_FEATHER) {
    var uv = vec2<f32>(0.5, 0.5);
@if(VERTEX_UVS_A) {
    uv = in.uv;
}
    // Vane: widest a third of the way up, rounded tip.
    let half_width = 0.5 * sqrt(max(0.0, 1.0 - pow(abs(uv.y * 1.3 - 0.4) / 0.9, 2.0))) * smoothstep(0.0, 0.08, uv.y);
    let across = abs(uv.x - 0.5);
    if across > half_width {
        discard;
    }
    let barbs = 0.85 + 0.15 * sin((uv.y * 40.0 - across * 25.0) * 3.14159);
    let shaft = select(1.0, 0.6, across < 0.02);
    let body = mix(coat.secondary.rgb, coat.base.rgb, smoothstep(0.0, 0.6, uv.y));
    color = mix(body, coat.accent.rgb, smoothstep(0.8, 1.0, uv.y) * coat.feathers.z) * barbs * shaft;
}

@if(COAT_CLOTH) {
    // Garments: palette colour with a faint weave that fades out before it can shimmer.
    let entry = coat.cloth[min(u32(scale_w + 0.5), 7u)];
    let weave_size = 0.004;
    let fine = detail(weave_size, pixel);
    var weave = 0.5;
    if fine > 0.0 {
        let w = rest / weave_size;
        weave = mix(0.5, 0.5 + 0.25 * sin(w.x * 6.283) * sin(w.y * 6.283 + w.z * 3.0), fine);
    }
    color = entry.rgb * (0.9 + 0.2 * weave);
    roughness = entry.w;
    reflectance = vec3<f32>(0.2);
}

    pbr.material.base_color = vec4<f32>(color, 1.0);
    pbr.material.perceptual_roughness = roughness;
    pbr.material.reflectance = reflectance;
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);
    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
