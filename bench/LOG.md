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
| 2026-10-19 | Bench harness fixes (v1 → v2) | all | | | | Offscreen 1920×1080 target; GPU wait per frame (v1 leaked ~0.6 MB/frame and measured submission, not rendering); memory metrics; per-image noise |
| 2026-10-19 | Meshing independent of thread count (`mesher.rs` joins chunks in order; `octree.rs` corner cache per job) | gen | total | within noise | geometry unchanged at a fixed thread count | Bug: hero meshes, and the feather cards placed on them, changed with the worker count. Test `meshing_is_independent_of_thread_count` |
| 2026-10-19 | M1: prepass/shadow vertex stage deforms like the main pass (`deform.wesl`, `coat_prepass.wesl`) | hero, motion | frame | within noise (all cases) | fur/scale close-ups 0.3–2.9 % pixels (shadow detail) | Hero motion vectors use last frame's joints; crowd uses only last frame's instance transform |
| 2026-10-19 | M2: waiting mobs keep their body key; mesh conversion in the generation task; worker-thread budget (`GenBudget::reserved_threads` = 2, split between bodies actually running) | crowd cold | p50 / p95 / frames > 100 ms / total | 91 → 29 ms / 262 → 50 ms / 163 → 5 / 45.5 → 59 s | crowd image changed (bodies attach in a different order) | `reserved_threads` = 0: 51 s total, p95 71 ms, 40 frames > 100 ms. Worst frame (1.4–1.6 s) was renderer start-up, a pipeline compile in the first second (Tracy); excluded from v3 by the pre-roll |
| 2026-10-19 | M2 budget split by bodies actually running (async pool has 3 threads here) | crowd cold (v2) | total | 82 → 59 s | | Was dividing by `max_in_flight` = 4 while only 3 run |
| 2026-10-19 | Centres of rotation grouped by bone set: each group gathers its candidate triangles once into a dense array and scores all members (chunks of 128) | gen | CoR stage / total | −48 to −57 % / −10 to −20 % (biped 1199 → 982 ms, horse 2452 → 1964 ms) | unchanged (within noise) | Equal to the brute-force formula within 2.2 mm (test `grouped_centres_match_brute_force`) |
| 2026-10-19 | (v3, all of the above vs the v3 baseline) | crowd cold | total / p50 / p95 / max / frames > 100 ms | 45.4 → 34.8 s / 93 → 29 ms / 282 → 49 ms / 532 → 303 ms / 175 → 3 | crowd image differs (attach order); all others within noise | Hero, motion and crowd steady frame times within noise. `compare` now gates only medians, means and totals |
