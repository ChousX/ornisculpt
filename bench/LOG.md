# Benchmark log

One row per landed change, measured with `examples/bench.rs` (see
`docs/design/perf-and-look-plan.md` §1). Compare against the parent's results (several runs give
the noise floor):

```text
cargo run --release --example bench -- all --out bench/results/<name>
cargo run --release --example bench -- compare bench/results/<base-1>,<base-2>,<base-3> bench/results/<new>
```

The runs below were measured on the tree as committed together with the chain-section, muzzle
and mixed-coat work. Golden screenshots (about 46 MB) aren't committed: record them locally with
`compare ... --accept` (into `bench/golden/`), then pass `bench/golden` as a base to compare against
the accepted look.

Machine: Lenovo Yoga Slim 7x (Snapdragon X, Adreno X1-85, turnip Vulkan), 12 cores, 15 GB, on
mains power. Times are p50 unless noted. Per-pass GPU times (`gpu.*`) are unreliable on this
driver (a furry full-body main pass reads 0.05 ms); frame time is the primary metric.

## Bench version 4 baseline (2026-10-24)

Version 4 runs headless (no window or winit; see `docs/design/perf-and-look-plan.md` §1.1).
Results: `bench/results/v4-base-{1,2}`.

| Scenario | Case | Metric | Value | Noise (spread of 2) |
|---|---|---|---|---|
| gen | biped / quadruped+fur / horse+fur / bird / dragon / beetle | total | 561 / 638 / 1160 / 493 / 562 / 249 ms | ≤ 3 % |
| hero | biped full / head / eye | frame | 15.3 / 14.7 / 16.4 ms | ≤ 10 % |
| hero | quadruped+fur full / head / eye | frame | 22.0 / 43.1 / 66.2 ms | ≤ 1 % |
| hero | horse+fur full / head / eye | frame | 24.3 / 25.4 / 48.4 ms | ≤ 2 % |
| hero | bird full / head / eye | frame | 15.9 / 17.9 / 17.3 ms | ≤ 10 % |
| hero | dragon full / head / eye | frame | 18.5 / 20.9 / 28.3 ms | ≤ 5 % |
| hero | beetle full / head / eye | frame | 16.1 / 17.8 / 18.4 ms | ≤ 3 % |
| motion | dragon / quadruped+fur walking | frame | 16.0 / 20.2 ms | ≤ 10 % |
| crowd | 4000 cold start | total / p50 / p95 / frames > 33 ms / frames > 100 ms | 31.3 s / 21.0 ms / 44.6 ms / 253 / 3–4 | 1 % / 1.4 % / 1.4 % / 4 % / |
| crowd | 4000 steady | frame | 55.0 ms | 1.7 % |
| (all) | | peak RSS | ≤ 1.09 GB (crowd), ≤ 0.6 GB (hero) | |

Light hero and motion cases (under ~20 ms) move up to 10 % between runs: compare them against
several baselines.

## Bench version 3 baseline (2026-10-19)

Version 3 adds a 60-frame empty-scene pre-roll, so renderer start-up (a 1.5 s pipeline compile)
no longer counts as the crowd's cold start. Same tree as the v2 baseline. Results:
`bench/results/base-{1,2,3}`; v2 results moved to `bench/results/v2/`.

| Scenario | Case | Metric | Value | Noise |
|---|---|---|---|---|
| gen | biped / quadruped+fur / horse+fur / bird / dragon / beetle | total | 1199 / 1234 / 2452 / 856 / 1097 / 430 ms | ≤ 2 % |
| gen | (same) | centres of rotation | 392 / 407 / 938 / 270 / 204 / 78 ms | ≤ 4 % |
| crowd | 4000 cold start | total / p50 / p95 / max / frames > 100 ms | 45.4 s / 93 ms / 282 ms / 532 ms / 175 | 2 % / 5 % / 20 % / 9 % / 13 % |
| crowd | 4000 steady | p50 | 53.2 ms | 1 % |
| hero, motion | | p50 | as v2 below (within noise) | |

## Bench version 2 baseline (2026-10-19)

Three runs of the tree before M1/M2, plus the meshing determinism fix. Results: `bench/results/base-{1,2,3}`
(git-ignored).

| Scenario | Case | Metric | Value | Noise (spread of 3) |
|---|---|---|---|---|
| gen | biped / quadruped+fur / horse+fur / bird / dragon / beetle | total | 1203 / 1240 / 2475 / 853 / 1101 / 434 ms | ≤ 1 % |
| gen | (all) | biggest stages | mesh ~65 %, centres of rotation ~33 % | |
| hero | biped full / head / eye | frame | 12.5 / 12.6 / 13.7 ms | 4–7 % |
| hero | quadruped+fur full / head / eye | frame | 20.4 / 42.2 / 64.6 ms | < 1 % |
| hero | horse+fur full / head / eye | frame | 22.4 / 23.9 / 47.8 ms | ≤ 2.5 % |
| hero | bird, dragon, beetle (all framings) | frame | 14–26 ms | |
| motion | dragon / quadruped+fur walking | frame | 15.1 / 18.8 ms | ≤ 5.5 % |
| crowd | 4000 cold start | total / p50 / p95 / frames > 100 ms | 45.5 s / 91 ms / 262 ms / 163 | 1 % / 9 % / 15 % / 9 % |
| crowd | 4000 steady | frame | 53.5 ms | 1.3 % |
| (all) | | peak RSS | ≤ 1.04 GB (crowd), ≤ 0.55 GB (hero) | |

Close-ups of fur and scales are not bit-exact between runs (up to 0.9 % of pixels); `compare`
widens their image gates accordingly.

## Changes

| Date | Change | Scenario / case | Metric | Before → after | Images | Note |
|---|---|---|---|---|---|---|
| 2026-10-27 | Second realism pass (eyelids, face regions, per-covering fur, shell clumping, cavity + shade shading, stage fill light; commits b584c83..HEAD) | hero | p50 | biped full/head/eye 5.80/5.87/6.15 → 5.83/6.05/6.75 ms; quadruped+fur 7.99/16.6/24.6 → 7.49/16.8/25.3; horse+fur 8.56/9.02/18.4 → 6.46/9.54/18.6; bird 6.35/8.24/8.72 → 6.49/8.03/8.85; dragon 6.77/9.94/20.7 → 6.52/9.87/20.6; beetle 6.04/7.15/7.80 → 6.11/7.21/7.76 | all changed (lighting, lids, pelt) | Measured on toth (NVIDIA, one run each, bench v4). Full-body framings within noise or faster (the pelt drops shells past a dozen pixels of depth: horse −24 %); bare-skin close-ups +3–10 % for the cavity, shade and roughness mottle; furry close-ups +1–6 % after the clump search went to `cellular_fast` |
| 2026-10-19 | Bench harness fixes (v1 → v2) | all | | | | Offscreen 1920×1080 target; GPU wait per frame (v1 leaked ~0.6 MB/frame and measured submission, not rendering); memory metrics; per-image noise |
| 2026-10-19 | Meshing independent of thread count (`mesher.rs` joins chunks in order; `octree.rs` corner cache per job) | gen | total | within noise | geometry unchanged at a fixed thread count | Bug: hero meshes, and the feather cards placed on them, changed with the worker count. Test `meshing_is_independent_of_thread_count` |
| 2026-10-19 | M1: prepass/shadow vertex stage deforms like the main pass (`deform.wesl`, `coat_prepass.wesl`) | hero, motion | frame | within noise (all cases) | fur/scale close-ups 0.3–2.9 % pixels (shadow detail) | Hero motion vectors use last frame's joints; crowd uses only last frame's instance transform |
| 2026-10-19 | M2: waiting mobs keep their body key; mesh conversion in the generation task; worker-thread budget (`GenBudget::reserved_threads` = 2, split between bodies actually running) | crowd cold | p50 / p95 / frames > 100 ms / total | 91 → 29 ms / 262 → 50 ms / 163 → 5 / 45.5 → 59 s | crowd image changed (bodies attach in a different order) | `reserved_threads` = 0: 51 s total, p95 71 ms, 40 frames > 100 ms. Worst frame (1.4–1.6 s) was renderer start-up, a pipeline compile in the first second (Tracy); excluded from v3 by the pre-roll |
| 2026-10-19 | M2 budget split by bodies actually running (async pool has 3 threads here) | crowd cold (v2) | total | 82 → 59 s | | Was dividing by `max_in_flight` = 4 while only 3 run |
| 2026-10-19 | Centres of rotation grouped by bone set: each group gathers its candidate triangles once into a dense array and scores all members (chunks of 128) | gen | CoR stage / total | −48 to −57 % / −10 to −20 % (biped 1199 → 982 ms, horse 2452 → 1964 ms) | unchanged (within noise) | Equal to the brute-force formula within 2.2 mm (test `grouped_centres_match_brute_force`) |
| 2026-10-19 | (v3, all of the above vs the v3 baseline) | crowd cold | total / p50 / p95 / max / frames > 100 ms | 45.4 → 34.8 s / 93 → 29 ms / 282 → 49 ms / 532 → 303 ms / 175 → 3 | crowd image differs (attach order); all others within noise | Hero, motion and crowd steady frame times within noise. `compare` now gates only medians, means and totals |
| 2026-10-19 | Parallel meshing: LODs built at once (near/far beside hero), `skin_and_attrs` and `VoxelBinding::apply` per vertex, binding spread per bone (all through order-preserving `surface::par_map` / bone-order merge) | gen (stage probe, not yet benched) | hero `mesh_lod` biped / horse | 673 → 340 ms / 1206 → 556 ms | meshes identical (determinism test) | Benched 2026-10-20 at 4fe5c3e vs the v3 baselines (`bench/results/head-4fe5c3e`): gen totals −48 to −55 % (biped 1199 → 544 ms, horse 2452 → 1107 ms, beetle 430 → 200 ms); crowd cold total 45.4 → 29.5 s, p50 93 → 29 ms; crowd steady +3 % (54.9 ms); hero full-body frames +3 to +13 % with the look change of 1bbada9 in between (muzzles, horse reshape, mixed coats). Probe: `surface::lod::stage_probe` (ignored test) |
| 2026-10-20 | Editor previews: `LodSpec::deformation` (default on; off skips centres of rotation and muscle flex), `BodyFailed` event, editor preview/refine flow | gen, hero, motion, crowd (vs `head-4fe5c3e`, one run each) | gen total / crowd cold total / crowd steady p50 / hero p50 | −2.5 to +4 % / 29.5 → 29.2 s / 54.9 → 53.8 ms / heavy fur cases ±3 % | biped and bird identical (PSNR ∞); fur close-ups 0.4–1.7 % pixels (run-to-run) | Default path unchanged (one flag check per level). In the hero rerun every light case sat at 16.6–16.7 ms: the window was presented at 60 Hz, so those frame-time deltas (+5 to +16 %) are the display cap, not the change. Editor: preview 82 ms biped / 100 ms furry horse, full 0.57 / 0.52 s |
| 2026-10-24 | Bench headless (v3 → v4): no window or winit; the 1920×1080 target, per-frame GPU wait and everything else as v3 | hero, motion, crowd (2 windowed runs vs 2 headless, alternating, same tree) | crowd cold p50 / p95 / frames > 33 ms; crowd steady p50; hero and motion p50 | 33.3 → 21.1 ms / 53.7 → 44 ms / 451 → ~260; 56.8 → 55.2 ms (within noise); all within noise (light cases move ±10 % run to run in both modes) | fur and scale close-ups 0.3–4.2 % pixels vs windowed, though headless runs agree with each other within the usual 0.7 %; biped and bird identical; crowd 2.5 % (attach order) | The window competed for the GPU even while it showed nothing (its camera was off while measuring, but it was still presented), and capped light cases at 60 Hz before. Cold-start totals unchanged (31.5 → 31.3 s). Results `bench/results/{windowed,headless}-{1,2}` |
