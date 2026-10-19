# Performance and hero-look plan (proposal)

Status: **proposal**. Built on the research report
[`reports/Creature anatomy references and shaders.md`](../../reports/Creature%20anatomy%20references%20and%20shaders.md)
(notes in `research_notes/Creature anatomy references and shaders/`) and on baseline numbers
measured on 2026-10-19.

Rule: **no optimisation or look change lands without a before/after run of the fixed benchmark
below.** Look changes carry a frame-time budget; optimisations carry an image-diff budget.

## 0. Baseline today (2026-10-19, this machine)

Machine: 12-core ARM laptop, Qualcomm GPU (`msm_dpu` display, Adreno-class integrated, tiled), 15 GB RAM,
Wayland. Bevy 0.20.0. All runs `--release`.

| Measurement | Value | Source |
|---|---|---|
| Hero mesh, biped, 8 mm voxels | 1.03 s, 57k tris | `surface::lod::hero_timing` (ignored test) |
| Adaptive contour, biped | 0.48 s, 112k tris | `surface::octree::tests::adaptive_mesh_is_closed…` |
| Centres of rotation, 28.6k verts | 0.39 s | `surface::cor::tests::centres_lie_inside_the_body` |
| Crowd 4000, `ORNISCULPT_COATS=1`, steady | ~23 ms/frame (32 hero / 1880 near / 2088 far) | `examples/crowd.rs` log |
| Crowd 4000, during generation | 100–550 ms single frames, ~50 s until all 48 bodies cached | same |

The hitches are the most visible problem. 48 bodies at ~1 s each with 4 in flight should not stall
the main thread at all.

## 1. The benchmark (fixed, versioned)

Status: **built** (`examples/bench.rs`; generation stages timed by `mob::generate_body`). Version 2
baseline recorded (`bench/LOG.md`; goldens in `bench/golden/`). One binary with a CPU-only mode. Every parameter is a constant in that file,
and the set is versioned as `BENCH_VERSION`. Changing any of them bumps the version and needs a
new baseline, so numbers are only ever compared within one version.

```text
cargo run --release --example bench -- <gen|hero|motion|crowd|all> [--out DIR] [--allow-battery]
cargo run --release --example bench -- compare BASE[,BASE...] NEW [--accept]
```

### 1.1 Global determinism

- Every case renders into a fixed **1920×1080 offscreen image**, whatever size the window manager
  gives the window (a tiling compositor ignored the requested size). The window shows a copy of
  the image, except while measuring; a second camera's passes would otherwise mix into the
  per-pass GPU times, which Bevy keys by pass name only.
- **Pre-roll:** 60 empty-scene frames before the first case (v3), so renderer start-up (a
  1.5 s pipeline compile on this driver) isn't counted as the first case's or the crowd's cold
  start. Mob pipelines still compile when the first bodies appear; that is part of `cold`.
- **Each frame waits for the GPU** (`RenderDevice::poll(wait_indefinitely)` in
  `RenderSystems::Cleanup`). Offscreen frames have no swapchain to throttle the CPU, so
  submissions queued without bound: about 0.6 MB per frame, 8 GB within minutes. Frame times
  then measured submission, not rendering (`BENCH_VERSION` 1 results are invalid for that
  reason). Frame time is now the full frame, CPU and GPU, with no queueing ahead.
- `TimeUpdateStrategy::ManualDuration(1/60 s)`: animation advances one tick per frame whatever the
  frame rate. Virtual time is **paused until bodies are ready**, so frame N of a case renders the
  same pose on every run.
- Fixed `MobSeed`s, plans, coats and forms (listed per scenario). Default `QualityProfile` and
  `GenBudget`.
- No input systems; cameras are scripted.
- Each report records: git SHA + dirty flag, `BENCH_VERSION`, machine model, CPU count, GPU
  adapter name, driver and backend (`RenderAdapterInfo`), mains power, and an xxh3 hash of every
  input asset. `compare` flags changed inputs and machines.
- Memory: each case records peak RSS (`mem.peak_rss_mb`, `VmHWM`) and the least system memory
  available while measuring (`mem.min_available_mb`). On this unified-memory GPU, driver
  allocations may show only in the latter.
- `--no-gpu-timing` leaves out the timestamp queries.
- Runs on battery are refused (`--allow-battery` overrides; the report records it).

### 1.2 Scenarios

| Id | Arg | Workload | Measured | Frames |
|---|---|---|---|---|
| **G** | `gen` | `generate_body` on one thread for the 6 `MOBS`: `biped`, `quadruped`+furry, `horse`+furry, `bird`+bird, `dragon`+chimera, `beetle`+lizard | Median per stage (`anatomy`, `mesh` = 3 LODs, `annotate`, `centres_of_rotation`, `muscle_flex`, `bake_motion`, `coat_meshes`); total median and min; verts/tris per LOD; mesh and coat-mesh KB | 1 warm-up + 5 runs |
| **H1** | `hero` | Each `MOBS` entry alone, `ForceTier(Hero)`, standing (idle). FOV 30°. Framings: **full** (fits body radius), **head** (3× head-bone length, 0.2–2 m, from the eye side), **eye** (5× eye size, ≥ 4 cm) | Frame time p50/p95/p99/max/mean; per-pass GPU medians; one PNG per framing | 120 warm-up (60 between framings), 600 measured |
| **H2** | `motion` | `dragon`+chimera, `quadruped`+furry walking a 2.5 m circle; camera orbiting at 0.3 rad/s | Same stats; PNGs at measured frames 150/300/450 | 120 warm-up, 600 measured |
| **C1** | `crowd` (phase 1) | 4000 mobs exactly as `examples/crowd.rs` with `ORNISCULPT_COATS=1`; camera yaw 0.6, pitch 0.5, 30 m, FOV 45°. Mobs stand still while generating | `cold.total_s` (until every mob is attached and nothing is in flight; cap 180 s), `cold.*_ms` stats, frames > 33 ms / > 100 ms, bodies | until done |
| **C2** | `crowd` (phase 2) | Same scene with wandering on | `steady.*_ms` stats, GPU passes, tier counts, uploaded slots per frame; one PNG | 120 warm-up, 1200 measured |

Not covered yet: outfits in G (garment glTFs need the asset server); a "bent joints" H1 variant
(add it with M1).

### 1.3 Timing sources

- **CPU:** frame time is real wall time between `First` runs, because `FrameTimeDiagnosticsPlugin`
  reports the manual 1/60 s under `ManualDuration`. Each generation stage runs inside
  `info_span!("generate", stage)`, so Tracy (`bevy.tracy.enable` in `devenv.nix`, cargo feature
  `trace_tracy`) shows the same stages the report times. The report is the record; Tracy is for
  finding causes, such as the main-thread vs render-thread split.
- **GPU:** `RenderDiagnosticsPlugin` provides `render/<pass>/elapsed_gpu` when the adapter has
  timestamp queries (Vulkan/DX12). Their medians go in as `gpu.<pass>_ms`. If none appear on the
  Adreno driver, frame time is what's left; H1 with one mob should be GPU-bound.
- **Statistics:** warm-up frames are dropped. `compare` takes several baseline directories and
  uses their spread as the noise floor. A time metric (`*_ms`, `*_s`) counts as changed only
  past max(3 %, 2 × noise).

### 1.4 Output and comparison

- Results go to `bench/results/<sha>[-dirty]/` (git-ignored): one `<scenario>.ron` per scenario
  and one PNG per shot.
- `bench compare BASE[,BASE...] NEW` is built into the same binary, so no Python is needed. It
  prints per-metric deltas, diffs same-named PNGs, and exits non-zero on any gate failure: a time
  regression, a bench-version mismatch, or an image that fails the image gate.
  - Image gate, for optimisations: at most 0.5 % of pixels with any channel off by more than
    8/255, and PSNR ≥ 40 dB. With several baselines, each image's gate widens to twice the
    pixels the baselines themselves differ by (and 3 dB below their PSNR): fur and scale
    close-ups aren't bit-exact from run to run.
  - A look change is expected to change images. Accept it on purpose with `--accept`, which copies
    the new PNGs to `bench/golden/`. Passing `bench/golden` as a base compares images against the
    accepted look.
- `bench/LOG.md`: one row per landed change (SHA, scenario, metric before → after, image delta,
  note). This is the running history.

### 1.5 Budgets on this machine (to confirm after M0)

- H1 full body: ≤ 16.7 ms GPU at hero quality (60 fps). Head/eye framings: ≤ 33 ms in "photo"
  quality (§5), no limit for accumulated stills.
- C2: p95 ≤ 25 ms with coats (today about 23 ms p50).
- C1: no frame > 50 ms while generating; time to complete ≤ today's value.

## 2. Milestones

Each milestone is a series of small commits. Each commit is benched against the previous one and
logged.

### M0: Harness and baseline (do first)
Status: **done** (bench v2 baseline in `bench/LOG.md`). GPU timestamps work on the Adreno driver but
per-pass values look wrong for some cases; frame time is the primary metric.

1. ~~`examples/bench.rs` with the scenarios above, plus spans on the generation stages.~~ Done.
2. ~~`compare` (built into the bench), `bench/LOG.md`, `.gitignore` for results.~~ Done.
3. On mains power: three baseline runs, which give the noise floor. Accept the goldens. Confirm whether GPU
   timestamps work.

Also re-run the hitch investigation (M2.1) under Tracy here, since every later decision depends on it.

### M1: Correctness before quality (blocks TAA, SSAO, contact shadows, DoF)
Status: **1 done** (`src/coat/deform.wesl` shared by `coat_vertex.wesl` and `coat_prepass.wesl`; no
measurable cost). Still open: the shells' prepass needs the strand discard before a depth
prepass (TAA, SSAO) is turned on; crowd motion vectors ignore bone motion. 2 not started.

1. **Deforming prepass.** `CoatExt::specialize` (`src/coat/material.rs:83`) drops CoR, muscle and
   crowd deformation for any `*prepass_pipeline`, so depth, normals, motion vectors and **shadows**
   use Bevy's plain linear skinning of the rest mesh.
   - Add `prepass_vertex_shader()` sharing the deformation code with `coat_vertex.wesl`.
   - Give the prepass the same vertex layout.
   - For motion vectors, keep the previous frame's bones (double-buffer the crowd bone buffer;
     hero tier uses Bevy's previous skin) and previous muscle amplitudes.
   - Test: H1 with `B` held (joints bent), shadow and SSAO visibly hug the bulged muscle. Add an
     H1 variant "bent", with SSAO on, to the goldens. Check the prepass extra cost in H1/C2;
     expect shadow-pass time to rise slightly.
2. **Shader-def trap.** Clearcoat, transmission and anisotropy only compile when the Rust base
   `StandardMaterial` has them non-zero (`body_materials`, `src/mob/generate.rs:236`).
   - Today only wings write `diffuse_transmission` in WGSL and only wings set the base value, so
     nothing is broken yet.
   - Before any skin/ear/wet feature, give the layers that need them a small base value (e.g.
     `1e-3`) at hero tier only. This adds pipeline variants, so measure the C2 effect.

### M2: CPU and generation optimisations
Status: **1 done, 2 partly done.** Key caching, mesh conversion in the task, a worker-thread budget
and grouped centres of rotation: crowd cold start 45.4 → 34.8 s, p95 frames 282 → 49 ms, frames
over 100 ms 175 → 3 (`bench/LOG.md`, v3). Tracy showed the old 1.5 s spike was renderer start-up
(excluded by the v3 pre-roll). Found on the way: meshing depended on the thread count (fixed).
Next: meshing is now the largest stage (~65–80 % of generation); its three LODs still run one
after another.

Hypotheses, ranked. Each one is confirmed by a Tracy capture first, then fixed and measured on
C1/G.

1. **Main-thread hitches during generation (C1).** Suspects, to tell apart with spans:
   - **`start_generation` re-hashes waiting mobs every frame.** Mobs blocked by
     `max_in_flight` keep `GenRequest`. Each frame they clone plan, coat and outfit, call
     `referenced_assets`, and `ron::to_string` the plan, settings, coat and outfit to build
     `body_key`. With 4000 mobs this could be the hitch.
     Fix: compute the key once per mob and cache it in a component; invalidate on `Form`, coat or
     outfit change.
   - **`finish_generation` converts meshes on the main thread.** `to_bevy_meshes` /
     `to_crowd_meshes` run for 3 LODs plus coat meshes. Move the conversion into the task and hand
     over finished `Mesh`es.
   - **Pipeline specialisation and shader compilation** the first time a layer/crowd/layout
     combination appears. Mitigate by pre-warming, i.e. spawning one hidden mob per layer at
     startup, and check whether Bevy 0.20 compiles pipelines asynchronously.
   - **Render-world upload of large meshes**, e.g. 12 fur shells copying the skin.
   - **Task pool starving the main thread** while 4 generation tasks run. Try `max_in_flight` =
     cores − 4, or give generation a smaller pool.
2. **Generation wall time (G).**
   - `mesh_all_lods` meshes 3 LODs one after another (`src/surface/lod.rs:152`). Run them in
     parallel (task-pool `scope`); expected speed-up 1.5–2× per body.
   - `centres_of_rotation` (0.39 s): parallelise per vertex.
   - `muscle_flex`: 12 smoothing passes; check its cost and use a parallel or sparse version if
     it is large.
   - Run the same LOD-parallel treatment on `build_coat_meshes`.
3. **Fur shell memory and upload.** Shells are N full copies of the skin mesh per LOD. Draw one
   skin-derived mesh N times with an instance index (or vertex pulling by `instance_index`) and
   offset by shell index in the vertex shader. This cuts mesh bytes about 12×, plus generation
   time and C1 upload. Record mesh bytes in G.
4. **Crowd steady state (C2).** Profile first. Candidates are `choose_tiers` (iterates every mob
   every frame and clones `Projection` per camera) and the per-frame instance uploads. Fix them
   only if they show up in Tracy.

### M3: GPU cost of the current look
1. Per-pass GPU times on H1 furry/bird and C2. The suspect is shell overdraw: 12 shells × a
   `cellular()` 3×3×3 Worley evaluation per fragment, with `discard`, on a tiled GPU where discard
   defeats early depth/HSR.
2. Options, measured one at a time:
   - Shell count from projected fur length, aiming for 1–2 px between shells (research §fur).
   - A cheap early reject in the shell fragment before the full strand function.
   - `cellular_fast` for outer shells.
   - Alpha-to-coverage with MSAA 4× (tiled GPUs resolve MSAA cheaply) compared against alpha test.
3. Gate: images pass the image gate (§1.4) at H1 full-body framing. Close-up framings
   may change only if a look change pays for it.

### M4: Hero render profile (camera/post, no shader changes)
A `RenderQuality::{Crowd, Hero, Photo}` resource, applied to the camera, in the order below. Each
step is a separate bench row with its own H1/H2 frame-time cost.

1. AA. **Hero**: TAA + `Msaa::Off` + hashed alpha shells (motion vectors need M1). **Fallback**:
   MSAA 4× + alpha-to-coverage. Compare both in H2; on a tiled Adreno MSAA may win on cost.
2. `ScreenSpaceAmbientOcclusion` (High; Ultra in Photo) and `ContactShadows`.
3. Shadows: one tight cascade around the hero mob, `DirectionalLightShadowMap { size: 4096 }`, and
   PCSS (`experimental_pbr_pcss`) in Photo.
4. Lighting rig: key directional, a `RectLight` soft fill (`area_light_luts`), a rim spot, and
   `GeneratedEnvironmentMapLight` from a studio HDRI (`hdr` feature, CC0 HDRI from Poly Haven).
5. Post: AgX or TonyMcMapface, `Bloom::NATURAL` (low), `DepthOfField` Bokeh auto-focused on the
   head anchor (Photo only), and `ContrastAdaptiveSharpening` after TAA.
6. New cargo features: `experimental_pbr_pcss`, `area_light_luts`, `bluenoise_texture`, `hdr`,
   `pan_orbit_camera`. Check the build-time effect.

### M5: Surface shading (inside `coat_fragment.wesl`)
In order of payoff per cost. Each step gets H1 head and eye framings as goldens and an H1 GPU
delta.

1. **Specular AA** (Filament normal-variance roughness) and fold faded `detail()` bump energy into
   roughness. This should also *lower* the H2 shimmer; measure the temporal difference between
   consecutive H2 frames at a frozen pose.
2. **Analytic eye**: cornea refraction onto the iris plane, pupil shapes (round/slit/horizontal),
   limbal ring, lid occlusion, wet highlight, optional tapetum eyeshine. Replaces the plain `Hard`
   eye material.
3. **Scales**: per-scale normal tilt and colour jitter, F2−F1 crevice into
   `diffuse_occlusion`/`specular_occlusion`.
4. **Per-vertex bakes from the SDF** during generation: curvature, AO and thickness, in spare
   attributes. They feed a skin curvature-wrap term (Bevy has no SSS) and membrane thickness.
   Their generation cost is measured in G.
5. **Wet surfaces**: clearcoat on nose, lips and eyes (needs M1.2), plus wet darkening.
6. **Membranes**: Beer–Lambert backlit colour, veins as extra thickness, `TransmittedShadowReceiver`.
7. **Thin-film iridescence** for insect wings, beetle elytra and bird feathers.

### M6: Fur and feathers
1. Strand styling in the shell shader: taper, clumping, curl, and comb/gravity/wind by `h²`; root
   AO by shell height; Kajiya-Kay/Scheuermann two-lobe specular; backscatter rim. Comb direction
   comes from a per-vertex tangent set by the anatomy builder.
2. Silhouette fins: a CPU fin mesh from the skin edge list, faded by `1 − |N·V|`.
3. Short coat (horse): `anisotropy_strength` along the comb tangent instead of shells. This saves
   all shell cost on that coat, so G, H1 and C2 should improve.
4. Hero-only ribbon guard hairs, 2–8k instanced per mob, roots resampled on the skinned mesh.
5. Procedural feather cards: vane, rachis and barbs anti-aliased with `fwidth`, barb-aligned
   anisotropic sheen, and a down layer.

### M7: Photo mode
Freeze `Time<Virtual>`, render at 2–4× resolution, and accumulate 16–64 frames with Halton
`TemporalJitter` offsets in linear HDR. Check first whether `Screenshot` captures before or after
tonemapping on HDR image targets. It runs as a scenario (`P1`) that produces the eye/head stills,
judged by eye with timing recorded but not gated.

## 3. Working loop for every change

1. Branch. Run `bench all` on the parent commit if no result exists for it.
2. Make one change. `cargo test --release`, then run the scenarios that change can affect (at
   least the ones its milestone names).
3. Run `bench compare`. Optimisation: the target metric improves beyond noise, no other
   metric regresses beyond noise, and images pass the image gate (§1.4). Look change: images
   change where intended and the frame-time cost stays within the §1.5 budget.
4. Add a row to `bench/LOG.md`; accept goldens if the look changed on purpose.
5. Commit with the bench delta in the message body.

## 4. Open questions to settle in M0
- Does the Adreno Vulkan driver expose `TIMESTAMP_QUERY` (and inside-pass queries) through wgpu?
- What actually causes the C1 hitches (Tracy)? That decides M2's order.
- Is H1 GPU-bound with one mob (it should be), so it can measure shader changes on their own?
