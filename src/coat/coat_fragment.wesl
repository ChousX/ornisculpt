// Mob fragment shader: StandardMaterial lighting with coat shading.
// - COAT_SKIN: base colour; scales (cellular colour + relief) where covered; fur/feather regions
//   get a darker undercoat.
// - COAT_SHELL: fur strands, alpha-tested per shell height (uv = shell height, fur length).
// - COAT_FEATHER: feather vane shape on a card, alpha-tested.
// - COAT_HARD: plain colour.
// Coat markings (COAT_PAINT, `paint_io.wesl`) replace the base colour where they show.
// Vertex inputs: colour = rest-pose position (xyz) + scale weight (w); on the skin, uv = (fur,
// feather) weights; world_tangent = coat flow (xyz) + distance along it in metres (w, skin only).
//
// Crowd tiers have no shells or cards, so the skin carries fur and feathers itself: fur as
// combed clumps with a soft tip-coloured rim, feathers as overlapping staggered vanes. Every
// pattern fades to its average colour once it gets smaller than a pixel, so distance never
// shimmers.

import bevy_pbr::render::{
    mesh_view_bindings::{view, lights},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
    forward_io::FragmentOutput,
};
import package::coat::paint_io::{CoatOutput, bevy_output, paint_of, surface_of};

struct CoatUniform {
    base: vec4<f32>,
    secondary: vec4<f32>,
    accent: vec4<f32>,
    scale_mask: vec2<u32>,
    fur_mask: vec2<u32>,
    feather_mask: vec2<u32>,
    // flex gain (0 = muscles still), _
    muscle: vec2<f32>,
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
fn fragment(out_v: CoatOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let in = bevy_output(out_v);
    var pbr = pbr_input_from_standard_material(in, is_front);
    // Coat markings replace the base colour where they show; the darker secondary and lighter
    // accent follow them, keeping their brightness relative to the base.
    let paint = paint_of(out_v);
    let lum = vec3<f32>(0.2126, 0.7152, 0.0722);
    let base_lum = max(dot(coat.base.rgb, lum), 1e-3);
    let base = mix(coat.base.rgb, paint.rgb, paint.a);
    let secondary = mix(coat.secondary.rgb, min(paint.rgb * (dot(coat.secondary.rgb, lum) / base_lum), vec3<f32>(1.0)), paint.a);
    let accent = mix(coat.accent.rgb, min(paint.rgb * (dot(coat.accent.rgb, lum) / base_lum), vec3<f32>(1.0)), paint.a);
    var color = base;

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
    var glow = vec3<f32>(0.0);
    var reflectance = pbr.material.reflectance;

    let pixel = pixel_size(in.world_position.xyz);
    let surface = surface_of(out_v);

@if(COAT_SKIN) {
    // Occlusion baked from the field: creases (between the fingers, the lids, the lips) and
    // what the body shades of itself (under the chin, the armpit, between the thighs). Less sky
    // and less highlight reach them; they also darken under direct light, as the light bouncing
    // in them is lost, and bare skin flushes where it folds.
    let cavity = surface.x;
    let shade = surface.y;
    pbr.diffuse_occlusion *= (1.0 - 0.45 * cavity) * (1.0 - 0.5 * shade);
    pbr.specular_occlusion *= (1.0 - 0.6 * cavity) * (1.0 - 0.5 * shade);
    color *= 1.0 - 0.2 * cavity - 0.3 * shade;
    let bare = 1.0 - max(max(weights.x, weights.y), scale_w);
    color *= mix(vec3<f32>(1.0), vec3<f32>(1.0, 0.84, 0.78), cavity * bare * 0.7);
    // Bare skin reflects little (F0 about 0.03), is rougher in its creases, and its sheen is
    // mottled at a few millimetres (oilier and drier patches) while that can be seen.
    reflectance = mix(reflectance, vec3<f32>(0.3), bare);
    roughness = mix(roughness, min(roughness + 0.25 * cavity, 1.0), bare);
@if(!COAT_CROWD) {
    let mottle_size = 0.004;
    let mottle = detail(mottle_size, pixel) * bare;
    if mottle > 0.0 {
        roughness = clamp(roughness + (value3(rest / mottle_size) - 0.5) * 0.2 * mottle, 0.05, 1.0);
    }
}

    // Scales. Hero: computed for every pixel when the coat has any (uniform branch) so the
    // derivatives used for relief stay in uniform control flow. Crowd: no relief, so uncovered
    // pixels skip it and covered ones use the cheaper search.
@if(COAT_CROWD) {
    if scale_w > 0.01 {
        let fine = detail(coat.scales.x, pixel);
        var scaled = mix(base, secondary, 0.5 * coat.scales.z) * 0.85;
        if fine > 0.0 {
            let c = cellular_fast(rest / max(coat.scales.x, 1e-4));
            let dome = smoothstep(0.0, 0.35, c.y - c.x);
            let tint = mix(base, secondary, c.z * coat.scales.z) * mix(0.6, 1.0, dome);
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
        let tint = mix(base, secondary, c.z * coat.scales.z) * mix(0.55, 1.0, dome);
        let avg = mix(base, secondary, 0.5 * coat.scales.z) * 0.85;
        color = mix(color, mix(avg, tint, fine), scale_w);
        pbr.N = bump(pbr.N, in.world_position.xyz, height);
    }
}

    // Fur and feathers. Hero: a dark undercoat beneath shells and cards.
@if(!COAT_CROWD) {
    color = mix(color, secondary * 0.7, max(weights.x, weights.y));
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
        var fur = mix(base, secondary, (1.0 - mottle) * 0.4);
        fur = mix(fur, accent, coat.fur.w * 0.2) * mix(0.8, 1.06, mottle);
        // Tips catch light at grazing angles: a soft, tip-coloured fuzz around the outline.
        let rim = pow(1.0 - ndv, 3.0);
        fur = mix(fur, mix(base, accent, coat.fur.w) * 1.1, rim * 0.5);
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
        var vane = mix(secondary, base, smoothstep(0.0, 0.6, f.y));
        vane = mix(vane, accent, smoothstep(0.8, 1.0, f.y) * coat.feathers.z * 0.6);
        vane *= mix(0.9, 1.08, f.id);
        // Shadow under the feather above, a dark outline where vanes overlap, a lighter shaft.
        let shade = mix(0.7, 1.0, smoothstep(0.0, 0.35, f.y)) * mix(0.82, 1.0, smoothstep(0.0, 0.12, f.edge));
        let shaft = select(1.0, 1.12, f.edge > 0.42);
        let patterned = vane * shade * shaft;
        let avg = mix(mix(secondary, base, 0.65), accent, 0.15 * coat.feathers.z) * 0.85;
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
    // uv = (shell height, the fur's length here in metres).
    var h = 0.0;
    var fur_length = coat.fur.x;
@if(VERTEX_UVS_A) {
    h = in.uv.x;
    fur_length = in.uv.y;
}
    // One strand per cell, thinning to a point at full length. Strands need a few pixels per cell
    // to resolve; finer than that they alias into salt-and-pepper. So the strands gather into
    // clumps as they get small: the cells double (level by level, each level fading into the
    // next with the clumps' area kept) until a clump spans about four pixels. Clumps wider than
    // the fur is long would read as dapple, and a coat only a dozen pixels deep shows its shells
    // as layers of specks, so past either the shells go over to a smooth pelt: the shell halfway
    // up stays whole in the fur's average colour (clear of the skin, which the lowest shells dip
    // under where their simplified source strays), the rest go.
    let cell = 1.0 / max(coat.fur.y, 1.0);
    let radius = 0.5 * coat.fur.z * (1.0 - h);
    let level = max(0.0, log2(4.0 * pixel / cell));
    let top = log2(max(2.0 * fur_length / cell, 1.0));
    var pelt = 0.0;
    if level > top || fur_length < 12.0 * pixel {
        let step = 1.0 / max(coat.scales.w, 1.0);
        if h > 0.5 + 1e-3 || h <= 0.5 - step + 1e-3 {
            discard;
        }
        pelt = 1.0;
    } else {
        let l0 = floor(level);
        let f = level - l0;
        let k0 = exp2(l0);
        let fine = cellular(rest * coat.fur.y / k0).x <= radius * sqrt(1.0 - f);
        var coarse = false;
        if f > 0.0 {
            coarse = cellular_fast(rest * coat.fur.y / (k0 * 2.0)).x <= radius * sqrt(f);
        }
        if !(fine || coarse) {
            discard;
        }
    }
    let root = mix(mix(0.35, 1.0, h), 0.72, pelt);
    color = mix(base, accent, mix(h, 0.5, pelt) * coat.fur.w) * root * (1.0 - 0.3 * surface.y);
    pbr.diffuse_occlusion *= 1.0 - 0.5 * surface.y;
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
    let body = mix(secondary, base, smoothstep(0.0, 0.6, uv.y));
    color = mix(body, accent, smoothstep(0.8, 1.0, uv.y) * coat.feathers.z) * barbs * shaft;
}

@if(COAT_HARD && VERTEX_UVS_A) {
    // Eyes: uv = (cosine from the eye's forward axis, 1 + how much white shows); other hard
    // parts have zero uvs. Iris in base, white in secondary, pupil in accent. With no white the
    // iris fills the eye round a wider pupil (most animals).
    let soft = max(0.015, fwidth(in.uv.x));
    if in.uv.y >= 1.0 {
        let white = in.uv.y - 1.0;
        let c = in.uv.x;
        let iris_edge = mix(-1.0, 0.87, white);
        let iris = smoothstep(iris_edge - soft, iris_edge + soft, c);
        let across = clamp((c - iris_edge) / max(1.0 - iris_edge, 1e-3), 0.0, 1.0);
        // Darker limbal ring at the rim, lighter toward the pupil.
        let iris_color = base * mix(0.55, 1.25, smoothstep(0.0, 0.6, across));
        color = mix(secondary, iris_color, iris);
        let pupil_edge = mix(0.9, 0.984, white);
        color = mix(color, accent, smoothstep(pupil_edge - soft, pupil_edge + soft, c));
        roughness = 0.05;
    }
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

@if(COAT_WING) {
    // Wing sheets; the kind rides in vertex colour w. uv = sheet coordinates, tangent = rib /
    // shaft / wing direction with thickness in w (0 = thinnest).
    let kind = u32(scale_w + 0.5);
    var uv = vec2<f32>(0.5);
@if(VERTEX_UVS_A) {
    uv = in.uv;
}
    var thickness = 0.5;
@if(VERTEX_TANGENTS) {
    thickness = clamp(in.world_tangent.w, 0.0, 1.0);
}
    let ndv = clamp(abs(dot(pbr.N, pbr.V)), 0.0, 1.0);
    var transmit = 0.0;
    var glow_tint = vec3<f32>(0.0);
    if kind == 0u {
        // Membrane (bat, dragon): leathery, veined, glowing warm when backlit. Veins branch out
        // from the finger ribs; fibres run along the panel; thick ribs stay dark.
        let fine = detail(0.01, pixel);
        let warp = value3(vec3<f32>(uv * vec2<f32>(3.0, 7.0), 0.0));
        let vein_line = abs(sin(3.14159 * (uv.x * 6.0 + warp * 1.6)));
        let vein = (1.0 - smoothstep(0.0, 0.08, vein_line)) * (0.4 + 0.6 * uv.y) * fine;
        let fibre = 0.5 + 0.5 * sin(uv.y * 90.0 + uv.x * 12.0 + warp * 4.0);
        let pebble = cellular_fast(rest / 0.006);
        let leather = mix(1.0, smoothstep(0.0, 0.3, pebble.y - pebble.x) * 0.25 + 0.75, fine);
        let skin = mix(secondary, base, 0.35);
        color = skin * leather * mix(1.0, 0.92 + 0.08 * fibre, fine);
        color = mix(color, skin * 0.45, max(vein * 0.7, thickness * 0.55));
        transmit = mix(0.7, 0.05, max(thickness, vein * 0.6));
        glow_tint = mix(base, vec3<f32>(1.0, 0.45, 0.3), 0.5) * (1.0 - thickness) * (1.0 - vein * 0.6);
        roughness = 0.55;
        reflectance = vec3<f32>(0.3);
    } else if kind == 1u {
        // Flight feather: asymmetric vane (narrow leading side), pale shaft, angled barbs, tip
        // band; anisotropic sheen along the shaft below.
        let a = uv.x - 0.5;
        let lead = select(0.5, 0.36, a < 0.0);
        let tip = 1.0 - smoothstep(0.88, 1.0, uv.y);
        let half_width = lead * mix(0.2, 1.0, sqrt(max(tip, 0.0)));
        if abs(a) > half_width || uv.y < 0.02 {
            discard;
        }
        let shaft = 1.0 - smoothstep(0.015, 0.03, abs(a));
        let barbs = 0.9 + 0.1 * sin(uv.y * 70.0 - abs(a) * 40.0) * detail(0.004, pixel);
        var vane = mix(secondary, base, smoothstep(0.0, 0.5, uv.y));
        vane = mix(vane, accent, smoothstep(0.78, 0.97, uv.y) * coat.feathers.z);
        vane *= 0.85 + 0.15 * smoothstep(0.0, 0.25, half_width - abs(a));
        color = mix(vane * barbs, mix(accent, vec3<f32>(0.9), 0.5), shaft * 0.6);
        transmit = 0.3;
        glow_tint = vane * 0.6;
        roughness = 0.65;
    } else if kind == 2u {
        // Insect wing: longitudinal veins from the hinge, cross-vein cells, dark costa and
        // pterostigma, thin-film iridescence; mostly see-through (dithered).
        let long_veins = smoothstep(0.86, 0.96, abs(fract(uv.y * 6.0 + 0.12 * sin(uv.x * 4.0)) - 0.5) * 2.0);
        let cells = cellular_fast(vec3<f32>(uv.x * 22.0, uv.y * 9.0, 0.5));
        let cross_veins = 1.0 - smoothstep(0.0, 0.08, cells.y - cells.x);
        let costa = 1.0 - smoothstep(0.04, 0.08, uv.y);
        let stigma = smoothstep(0.78, 0.8, uv.x) * (1.0 - smoothstep(0.88, 0.9, uv.x)) * (1.0 - smoothstep(0.12, 0.16, uv.y));
        let fine = detail(0.003, pixel);
        let vein = clamp(max(max(long_veins, cross_veins) * fine, max(costa, stigma)), 0.0, 1.0);
        // Thin film: phase from film thickness (varying per cell) and viewing angle.
        let film = 0.35 + 0.4 * cells.z + 0.25 * thickness;
        let phase = film * (1.0 + 0.6 * (1.0 - ndv));
        let irid = 0.5 + 0.5 * cos(6.28318 * (phase * vec3<f32>(1.0, 0.85, 0.7) + vec3<f32>(0.0, 0.33, 0.67)));
        let membrane = mix(accent * 0.6, irid, 0.55 * (0.4 + 0.6 * (1.0 - ndv)));
        color = mix(membrane, secondary * 0.25, vein);
        var alpha = mix(0.22, 1.0, vein);
@if(COAT_CROWD) {
        alpha = 1.0;
}
        // Ordered 4×4 dither: transparency without sorting.
        let px = vec2<u32>(in.position.xy) % vec2<u32>(4u);
        let bayer = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
        if alpha < (bayer[px.y * 4u + px.x] + 0.5) / 16.0 {
            discard;
        }
        transmit = 0.6;
        glow_tint = membrane * 0.4;
        roughness = 0.2;
        reflectance = vec3<f32>(0.5);
    } else {
        // Elytron: a hard, glossy, finely punctured shell.
        let dots = cellular_fast(rest / 0.004);
        let pits = 1.0 - (1.0 - smoothstep(0.0, 0.12, dots.x)) * 0.25 * detail(0.004, pixel);
        color = base * pits;
        roughness = 0.22;
        reflectance = vec3<f32>(0.6);
    }
    pbr.material.diffuse_transmission = transmit;
    // Fast translucency (Barré-Brisebois & Bouchard, GDC 2011): light through a thin surface,
    // brightest looking toward the light, scaled by thinness.
    if lights.n_directional_lights > 0u {
        let light = lights.directional_lights[0];
        let h = normalize(light.direction_to_light + pbr.N * 0.3);
        let through = pow(clamp(dot(pbr.V, -h), 0.0, 1.0), 4.0) * 1.2;
        glow = glow_tint * through * light.color.rgb * view.exposure;
        if kind == 1u {
            // Anisotropic sheen along the shaft (Kajiya–Kay style).
            let hv = normalize(light.direction_to_light + pbr.V);
            let th = dot(T, hv);
            glow += accent * pow(sqrt(max(0.0, 1.0 - th * th)), 80.0) * 0.06 * light.color.rgb * view.exposure;
        }
    }
}

    pbr.material.base_color = vec4<f32>(color, 1.0);
    pbr.material.perceptual_roughness = roughness;
    pbr.material.reflectance = reflectance;
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr);
    out.color = vec4<f32>(out.color.rgb + glow, out.color.a);
    out.color = main_pass_post_lighting_processing(pbr, out.color);
    return out;
}
