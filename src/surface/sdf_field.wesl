// The packed body field (`surface/preview.rs`) and its evaluation, shared by the preview's
// fragment shader (`sdf_preview.wesl`) and the volume bake (`sdf_bake.wesl`). `SDF_GROUP` is the
// bind group the packed words are in (the material's, or the bake's own).
//
// The field mirrors `BodyField::eval`: primitives in field order, smooth union (different limbs
// only meet), smooth subtraction, ellipsoid displacement. Each grid cell lists the primitives that
// reach it, so within a cell its list gives the field (as a meshing chunk does).

@group(constants::SDF_GROUP) @binding(100) var<storage, read> words: array<vec4<f32>>;

const PRIM_WORDS: u32 = 8u;
const HEADER_WORDS: u32 = 5u;
// Bricks per row and per layer of the atlas, along x and y (as `BRICK_ROW` in
// `surface/preview_material.rs`).
const BRICK_ROW: u32 = 16u;
const NO_LIMB: u32 = 0xffffffffu;
const FAR: f32 = 1e9;

fn word_u(i: u32) -> vec4<u32> {
    return bitcast<vec4<u32>>(words[i]);
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return min(a, b);
    }
    let h = clamp(0.5 + 0.5 * (a - b) / k, 0.0, 1.0);
    return a * (1.0 - h) + b * h - k * h * (1.0 - h);
}

fn ellipsoid(p: vec3<f32>, r: vec3<f32>) -> f32 {
    let k0 = length(p / r);
    let k1 = length(p / (r * r));
    if k1 == 0.0 {
        return -min(r.x, min(r.y, r.z));
    }
    return k0 * (k0 - 1.0) / k1;
}

fn round_cone(p: vec3<f32>, r0: f32, r1: f32, len: f32) -> f32 {
    let q = vec2<f32>(length(p.xz), p.y);
    let s = (r0 - r1) / len;
    let c = sqrt(max(1.0 - s * s, 0.0));
    let along = dot(q, vec2<f32>(-s, c));
    if along < 0.0 {
        return length(q) - r0;
    }
    if along > c * len {
        return length(q - vec2<f32>(0.0, len)) - r1;
    }
    return dot(q, vec2<f32>(c, s)) - r0;
}

fn round_box(p: vec3<f32>, half: vec3<f32>, round: f32) -> f32 {
    let q = abs(p) - half + vec3<f32>(round);
    return length(max(q, vec3<f32>(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0) - round;
}

// One elliptic tube of a sweep: radius `scale × r(i)`, local x squashed by `aspect`, centre line
// `drop × radius` toward local +Z.
fn tube(p: vec3<f32>, first: u32, count: u32, scale: f32, aspect: f32, drop: f32) -> f32 {
    let at = vec3<f32>(p.x / aspect, p.y, p.z);
    var best = FAR;
    var prev = words[first];
    var prev_r = prev.w * scale;
    var prev_p = vec3<f32>(prev.x / aspect, prev.y, prev.z + drop * prev_r);
    if count == 1u {
        best = length(at - prev_p) - prev_r;
    }
    for (var i = 1u; i < count; i++) {
        let w = words[first + i];
        let r = w.w * scale;
        let cur = vec3<f32>(w.x / aspect, w.y, w.z + drop * r);
        let seg = cur - prev_p;
        let along = clamp(dot(at - prev_p, seg) / max(dot(seg, seg), 1e-12), 0.0, 1.0);
        let radius = prev_r + (r - prev_r) * along;
        best = min(best, length(at - (prev_p + seg * along)) - radius);
        prev_p = cur;
        prev_r = r;
    }
    return best * min(aspect, 1.0);
}

fn sweep(p: vec3<f32>, first: u32, count: u32, aspect: f32, teardrop: bool, max_r: f32) -> f32 {
    let a = max(aspect, 1e-3);
    let main = tube(p, first, count, 1.0, a, 0.0);
    if !teardrop {
        return main;
    }
    let keel = words[3u].xyz;
    let d = tube(p, first, count, keel.y, a * keel.x / keel.y, keel.z / keel.y);
    return smin(main, d, 0.5 * keel.x * max_r * a);
}

fn grid(p_in: vec3<f32>, at: u32, mirror: bool) -> f32 {
    var p = p_in;
    if mirror {
        p.x = -p.x;
    }
    let head = words[at];
    let dims = word_u(at + 1u).xyz;
    let origin = head.xyz;
    let cell = head.w;
    let hi = vec3<f32>(dims - vec3<u32>(1u));
    let g = (p - origin) / cell;
    let c = clamp(g, vec3<f32>(0.0), hi);
    let outside = length((g - c) * cell);
    let base = min(vec3<u32>(floor(c)), dims - vec3<u32>(2u));
    let f = c - vec3<f32>(base);
    var v: array<f32, 8>;
    for (var k = 0u; k < 8u; k++) {
        let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, (k >> 2u) & 1u) + base;
        let index = o.x + dims.x * (o.y + dims.y * o.z);
        v[k] = words[at + 2u + index / 4u][index % 4u];
    }
    let x00 = mix(v[0], v[1], f.x);
    let x10 = mix(v[2], v[3], f.x);
    let x01 = mix(v[4], v[5], f.x);
    let x11 = mix(v[6], v[7], f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z) + outside;
}

struct Sample {
    d: f32,
    // index of the nearest union primitive, or NO_LIMB
    nearest: u32,
};

// The field at `p` over `count` primitives: from `first` on (`listed` false: the whole field is
// `field(p, 0, primitive count, false)`), or the ones listed from item `first` on (a cell's list:
// `field(p, cell.x, cell.y, true)`).
fn field(p: vec3<f32>, first: u32, count: u32, listed: bool) -> Sample {
    let header = word_u(0u);
    let item_start = header.w;
    let grid_start = word_u(2u).w;
    var d = FAR;
    var have = false;
    var raised = 0.0;
    var best = FAR;
    var nearest = NO_LIMB;
    var nearest_limb = NO_LIMB;
    for (var k = 0u; k < count; k++) {
        var index = first + k;
        if listed {
            index = bitcast<u32>(words[item_start + first + k].x);
        }
        let base = HEADER_WORDS + index * PRIM_WORDS;
        let r0 = words[base];
        let r1 = words[base + 1u];
        let r2 = words[base + 2u];
        let q = vec3<f32>(dot(r0.xyz, p) + r0.w, dot(r1.xyz, p) + r1.w, dot(r2.xyz, p) + r2.w);
        let info = word_u(base + 3u);
        let a = words[base + 4u];
        let blend_w = words[base + 6u];
        let anchor = words[base + 7u];
        var dl = FAR;
        switch info.x {
            case 0u: { dl = length(q) - a.x; }
            case 1u: { dl = ellipsoid(q, a.xyz); }
            case 2u: { dl = round_cone(q, a.x, a.y, a.z); }
            case 3u: { dl = round_box(q, a.xyz, a.w); }
            case 4u: { dl = sweep(q, header.y + bitcast<u32>(a.x), bitcast<u32>(a.y), a.z, a.w > 0.5, words[base + 5u].x); }
            case 5u: { dl = grid(q, grid_start + bitcast<u32>(a.x), a.y > 0.5); }
            default: {}
        }
        let min_scale = blend_w.y;
        var blend = blend_w.x;
        if anchor.w > 0.0 {
            blend *= clamp(1.0 - length(q - anchor.xyz) * min_scale / anchor.w, 0.0, 1.0);
        }
        dl *= min_scale;
        switch info.y {
            case 0u: {
                let apart = nearest != NO_LIMB && info.z != NO_LIMB && nearest_limb != NO_LIMB && info.z != nearest_limb;
                if !have {
                    d = dl;
                    have = true;
                } else if apart {
                    d = min(d, dl);
                } else {
                    d = smin(d, dl, blend);
                }
                if dl < best {
                    best = dl;
                    nearest = index;
                    nearest_limb = info.z;
                }
            }
            case 1u: {
                if have {
                    d = -smin(-d, dl, blend);
                }
            }
            case 2u: {
                if info.x == 1u {
                    let s = 1.0 - dot(q / a.xyz, q / a.xyz);
                    if s > 0.0 {
                        raised += blend_w.z * min_scale * s * s;
                    }
                }
            }
            default: {}
        }
    }
    return Sample(d - raised, nearest);
}

struct Grid {
    min: vec3<f32>,
    cell: f32,
    dims: vec3<u32>,
    cells: u32,
};

fn grid_info() -> Grid {
    let g = words[1u];
    return Grid(g.xyz, g.w, word_u(2u).xyz, word_u(0u).z);
}

fn cell_at(g: Grid, p: vec3<f32>) -> vec3<u32> {
    let c = clamp(floor((p - g.min) / g.cell), vec3<f32>(0.0), vec3<f32>(g.dims - vec3<u32>(1u)));
    return vec3<u32>(c);
}

fn cell_list(g: Grid, c: vec3<u32>) -> vec4<u32> {
    return word_u(g.cells + c.x + g.dims.x * (c.y + g.dims.y * c.z));
}

// Ray parameter where the ray leaves box [lo, hi] (the ray starts inside it).
fn exit_t(o: vec3<f32>, inv: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> f32 {
    let t0 = (lo - o) * inv;
    let t1 = (hi - o) * inv;
    let far = max(t0, t1);
    return min(far.x, min(far.y, far.z));
}

// The brick lattice: `voxels` per brick edge (`samples` = voxels + 1 a side, walls included).
struct Bricks {
    voxels: u32,
    samples: u32,
    first: u32,
    count: u32,
};

fn bricks_info() -> Bricks {
    let b = word_u(4u);
    return Bricks(b.x, b.x + 1u, b.y, b.z);
}

// The first texel of brick `brick` in the atlas.
fn brick_origin(bricks: Bricks, brick: u32) -> vec3<u32> {
    return vec3<u32>(brick % BRICK_ROW, (brick / BRICK_ROW) % BRICK_ROW, brick / (BRICK_ROW * BRICK_ROW)) * bricks.samples;
}

// The cell (x, y, z) of index `i`.
fn cell_coords(g: Grid, i: u32) -> vec3<u32> {
    return vec3<u32>(i % g.dims.x, (i / g.dims.x) % g.dims.y, i / (g.dims.x * g.dims.y));
}
