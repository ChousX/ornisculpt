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

## Bench version 5 baseline (2026-10-29)

Version 5 benches the newer models: `man`+skin_fair, `woman`+skin_woman, `horse`+bay,
`ox`+hereford, `spider`+wolf_spider, `darner`+green_darner; the woman and horse walk; the crowd is
men, women, oxen and wolf spiders, each in its own coat (a quarter of the builds plain). `gen`
reads the sculpted head from `assets/parts` (`common::part_assets`). Same machine, on mains.
Results: `bench/results/v5-base-{1,2}` (tree at dec0a6d).

| Scenario | Case | Metric | Value (mean of 2) | Noise (spread of 2) |
|---|---|---|---|---|
| gen | man / woman / horse+bay / ox+hereford / spider / darner | total | 2280 / 2822 / 1448 / 1579 / 507 / 143 ms | 2–8 % |
| gen | man / woman | anatomy stage | ~1.45 / 1.5 s: the head's SDF conversion, paid once per session in the app (`gen` builds fresh part assets each run) | |
| hero | man full / head / eye | frame | 13.6 / 18.1 / 17.7 ms | ≤ 3 % |
| hero | woman full / head / eye | frame | 14.3 / 20.4 / 12.9 ms | ≤ 2.5 % |
| hero | horse+bay full / head / eye | frame | 13.8 / 11.8 / 48.9 ms | ≤ 2 % |
| hero | ox+hereford full / head / eye | frame | 15.2 / 14.4 / 22.0 ms | ≤ 2 % |
| hero | spider full / head / eye | frame | 14.2 / 80.8 / 96.0 ms | ≤ 1 % |
| hero | darner full / head / eye | frame | 13.0 / 13.6 / 13.3 ms | ≤ 6 % |
| motion | woman / horse+bay walking | frame | 13.7 / 13.7 ms | ≤ 3.5 % |
| crowd | 4000 cold start | total / p50 / p95 / frames > 33 ms / > 100 ms | 24.4–28.6 s / 22.6–23.5 ms / 54–73 ms / 192–289 / 12–16 | 17 % on the total |
| crowd | 4000 steady | frame | 62.8–68.8 ms | 9 % |
| (all) | | peak RSS | ≤ 1.09 GB (crowd), ≤ 0.64 GB (hero) | |

The crowd's steady state is GPU-bound: over 10 s of it the main thread used 2.6 % of a core and
the render thread 3.9 %. Turning off muscle flex (`ORNISCULPT_NO_MUSCLE`) or the crowd tiers'
deformation (centres of rotation and muscles) each moved it by under 3 %. Rendering at 960×540
also changes the tiers (they go by on-screen size), so it can't split pixel from vertex cost.
### Profiling with RenderDoc (tir, 2026-10-29)

`renderdoccmd capture` with this build's layer (the one registered in `/etc` points at a store
path that is gone) and `ORNISCULPT_CAPTURE` saves one measured frame, headless:

```text
R=$(dirname $(dirname $(readlink -f $(which renderdoccmd))))
WGPU_DEBUG=1 VK_IMPLICIT_LAYER_PATH=$R/share/vulkan/implicit_layer.d ORNISCULPT_CAPTURE=crowd4000:600 \
  renderdoccmd capture -w -c /tmp/crowd ./target/release/examples/bench crowd --out /tmp/x
qrenderdoc -platform offscreen --python script.py   # the `renderdoc` module, for scripted replay
```

`WGPU_DEBUG=1` names the pipelines. Don't `LD_PRELOAD` librenderdoc: its `dlopen` hook then hides
`libvulkan` from wgpu ("Unable to find a GPU"). On this tiler, per-draw GPU durations are
meaningless (draws run in bins at the end of the pass), and Bevy's GPU-built indirect draws get no
pipeline statistics; index and instance counts per draw are reliable.

Crowd steady frame (v5, frame 600): 9.9 M indices × instances in the main pass over 1630
instances; 10.8 M in shadow cascades 2 and 3, all from the 32 hero-tier mobs (crowd tiers cast
no shadows). Experiments: `max_hero = 0` 69 → 63 ms. The near tier is the cost: `near_px` 160 /
80 / 40 gives 1573 / 2436 / 3421 near mobs at 48.2 / ~68 / 93.5 ms, about 24.5 µs per near mob.
Halving the near mesh (simplify ratio 0.22 → 0.11, man 5046 → 2519 triangles, ox 8766 → 4443)
took the crowd to 54 ms (−21 %) at the same tier counts, but the crowd image moved 5.4 % of pixels
(33.7 dB). The ox's coat markings, painted per vertex, change shape at the near tier's top size.
Landed at 0.15 instead, with the markings kept (below).

Sequential runs drift a few percent as the laptop warms over an hour of benching; for small
effects, alternate the two builds (`ab-A-*`, `ab-B-*` below) with an unaffected case as control.

## Steam Deck, bench version 5 baseline (2026-10-31)

Steam Deck OLED (Galileo: Zen 2, 4 cores and 8 threads; RDNA 2 Van Gogh under RADV, Mesa
26.2.0-devel), SteamOS 3.9.5, Desktop mode, on mains. toth built it and `tools/deck-bench.sh`
ran it: the binary carries nix's glibc, and Vulkan and the driver are SteamOS's. The target is the
same 1920×1080 as everywhere else, although the Deck's screen is 1280×800 (0.49 of the pixels).
Results: `bench/results/deck-v5-base-{1,2}`, tree at 93fea4f. That tree includes the fur-shell
and near-mesh changes made after tir's v5 baseline, so compare against tir's change rows, not its
baseline table.

| Scenario | Case | Metric | Value (mean of 2) | Noise (spread of 2) |
|---|---|---|---|---|
| gen | man / woman / horse+bay / ox+hereford / spider / darner | total | 11828 / 14643 / 7808 / 6800 / 2014 / 411 ms | ≤ 1 % (darner 6 %) |
| hero | man full / head / eye | frame | 16.0 / 28.6 / 24.9 ms | ≤ 1 % |
| hero | woman full / head / eye | frame | 16.4 / 31.0 / 23.1 ms | ≤ 1.2 % |
| hero | horse+bay full / head / eye | frame | 18.1 / 15.1 / 48.3 ms | < 1 % |
| hero | ox+hereford full / head / eye | frame | 19.0 / 20.8 / 47.0 ms | < 1 % |
| hero | spider full / head / eye | frame | 18.9 / 63.0 / 67.9 ms | < 1 % |
| hero | darner full / head / eye | frame | 12.6 / 17.1 / 17.2 ms | ≤ 2.4 % |
| motion | woman / horse+bay walking | frame | 16.0 / 18.0 ms | < 1 % |
| crowd | 4000 cold start | total / p50 / p95 / frames > 33 ms / > 100 ms | 125–128 s / 105–106 ms / 179–182 ms / 1210–1242 / 680–681 | 2 % on the total |
| crowd | 4000 steady | frame (main opaque pass) | 62.7–63.2 ms (23.8 ms) | 1 % |
| (all) | | peak RSS | ≤ 0.92 GB (crowd), ≤ 0.52 GB (hero) | |

Against tir, same models:
- **Full-body hero frames and walks** are 1.2 to 1.3 times tir's.
- **Bare-skin close-ups** are 1.6 to 1.8 times (man head 28.6 ms against 18.1).
- **Fur close-ups** vary: the spider's eye matches tir after cfdd3b3 (67.5 ms), the horse's is 1.4
  times and the ox's 2.6 times.
- **The crowd's steady frame** matches tir's (63 ms against about 61 ms after 4ed7a52).
- **Generation** is where the Deck falls behind, at 4.7 to 5.6 times tir's totals. By stage, for
  the man:
  - annotate, muscle flex and the motion bake run at tir's speed;
  - meshing is 3.3 times, about what six worker threads at roughly half tir's per-core speed
    predict;
  - the head's SDF conversion (the anatomy stage) is 5.4 times and centres of rotation 9.5
    times, both well beyond that. Those two need a profile on x86. `mul_add` isn't the cause:
    compiler_builtins' `fmaf` dispatches to the Deck's hardware FMA.
    *(Half right. The dispatch does reach the hardware FMA, but it is still a call per
    `mul_add`, and the call and its setup were the cost, along with `expf` and `floorf`. See the
    rerun at 3717f25 in the Changes table: both stages dropped by half or more.)*
- **The crowd's cold start** is 125 to 128 s with about 680 frames over 100 ms (tir: 24 to 29 s
  and 12 to 16), because generation shares 8 threads with the main thread.

Against a 60 fps budget (16.7 ms), a single hero fits at full body and walking. Close-ups don't,
and fur at the eye framing is far over. At the Deck's own 1280×800 the pixel-bound cases would
cost less, but that hasn't been measured, because the resolution is fixed within a bench version.
4000 mobs is a stress case, not a Deck target.

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
| 2026-11-02 | Cloth merged into the crowd skin mesh: **measured, not kept** | crowd dressed (`examples/crowd 4000 ORNISCULPT_OUTFITS=1`, frame 5000; toth 3 alternated pairs, Deck 2 pairs + a plain control each) | frame time | toth: plain 15.45 ms, dressed 16.25 (16.37 / 15.80 / 16.58), dressed + merged 16.84 (17.07 / 15.94 / 17.50) — the merge loses 0.70 / 0.14 / 0.92 ms in the three pairs. Deck: plain ~41.3 ms, dressed ~43.0, dressed + merged ~43.4, inside the spread | near-tier shots of a dressed man identical to the pixel; hero 71 dB and far 93 dB against a 74 dB run-to-run noise | Dressing 4000 mobs costs 0.8 ms (toth) to 2 ms (Deck), not the 8–12 ms `types.md` recorded from tir on 2026-10-18, and merging recovers none of it: crowd mobs are instanced, so a garment is an extra entity (extraction, an instance slot), not an extra draw — the whole v5 main pass runs in 1630 instances. The merge only adds a cloth branch to a crowd-skin fragment shader already doing fur, feathers and scales. Reverted. Deck caveat: at frame 3000 two of eighteen bodies were still generating (~3640 mobs up), so its figures are ±2 ms; the dressed/plain gap and the before/after non-difference both clear that. Logs in the session scratchpad, no results directories |
| 2026-11-01 | Steam Deck rerun after the centres-of-rotation and part-SDF work (3717f25, i.e. c31efed + 53a2223 + 3717f25) | all (Steam Deck, bench v5, 1 run vs `deck-v5-base-{1,2}`; `tools/deck-bench.sh all`, plugged in) | gen stages and totals; crowd cold | centres of rotation: man 2415 → 1106 ms (−54 %), woman 4913 → 2220 (−55 %), horse 5385 → 2286 (−57 %), ox 4218 → 1807 (−57 %), spider 498 → 232 (−53 %), darner 94 → 44 (−53 %); anatomy (the head's SDF): man 7775 → 1007 ms, woman 7827 → 1014 (−87 %); totals man 11881 → 3642 ms (−69 %), woman 14662 → 5167 (−65 %), horse 7826 → 4709 (−40 %), ox 6815 → 4383 (−36 %), spider 2017 → 1707 (−15 %), darner 422 → 342 (−19 %). Crowd cold start 128.1 → 77.9 s (−39 %), p50 106 → 91 ms, p95 182 → 162, frames > 33 ms 1242 → 836, > 100 ms 681 → 376. Hero, motion and the crowd's steady frame unchanged (every p50 within 3 %, all GPU-bound); peak RSS +2 to +10 % | head and eye close-ups differ as on toth (man head 3.7 % pixels, 31.5 dB; woman 11 %, 26.7 dB — the new grid plus hair-card jitter, checked by eye on toth at 53a2223); crowd image 3.0 %, 35.4 dB, because more mobs stand by the capture frame | The 5× generation gap against tir's v5 totals is now 1.6 to 3.4× (man 1.6, woman 1.8, ox 2.8, horse 3.25) — tir would gain from these changes too, so a tir rerun is owed before calling that the core-speed gap. What is left on the Deck is meshing (1.4 to 2.5 s a mob) and the centres, still a quarter to a half of each total. The contacts stage reads +19 to +27 % (man 3.4 → 4.0 ms, horse 5.4 → 6.8), 1 ms absolute, cause unlooked-at; the cold phase's worst frame rose 1.85 → 2.68 s (+44 %, noise 11 %) while every other cold metric fell. Results `deck-3717f25-1` |
| 2026-11-01 | Centres of rotation: no `floor` call in the exponential, and a candidate triangle skips the bone pairs it has no weight on (3717f25) | gen (toth, bench v5; c31efed ×2 as base, 2 new runs; `ornisculpt-deck` worktree) | stage.centres_of_rotation_ms; total_ms | man 826 → 439 / 445 ms (−47 %), woman 1719 → 896 / 895 (−48 %), horse 1849 → 910 / 914 (−51 %), ox 1444 → 719 / 720 (−50 %), spider 183 → 100 / 100 (−45 %), darner 39 → 21 / 21 (−45 %). Against ac9923d (both centres changes and the SDF one): centres −48 to −56 %; totals man 4428 → 1652 ms (−63 %), woman 5693 → 2400 (−58 %), horse 3213 → 2041 (−37 %), ox 2828 → 1920 (−32 %), spider 882 → 771 (−13 %), darner 209 → 189 (−10 %) | gen writes none; brute-force check unchanged (1.33 mm) | `floorf` was 6 % of a whole-generation profile after c31efed (`f32::floor` is a `libm` call on x86 without SSE4.1); the test's centres 0.32 → 0.23 s. After this, `group_centres` is still 47 % of the bench's generation CPU on toth, the field's `sdf::shape` and `Acc::gather` 26 %, the head's new conversion 9 %. Results `3717f25-{1,2}`; profile `perf record -e cpu-clock:u` of `bench gen` |
| 2026-11-01 | Centres of rotation: a cheap exponential (`exp_neg`) and plain arithmetic in the similarity loop (c31efed) | gen (toth, bench v5; base ac9923d ×2 on a quiet machine, `base-ac9923d-{3,4}`; 2 new runs; `ornisculpt-deck` worktree) | stage.centres_of_rotation_ms; total_ms | man 924 → 808 / 826 ms (−11 %), woman 1911 → 1681 / 1719 (−11 %), horse 2062 → 1842 / 1849 (−11 %), ox 1610 → 1431 / 1444 (−11 %), spider 197 → 182 / 183 (−8 %), darner 41 → 39 / 39 (−5 %); totals −2 to −8 % (the spider's mesh stage +7 % in the first run only) | gen writes none; the brute-force check (`grouped_centres_match_brute_force`) 1.10 → 1.33 mm | Profiled first on the hero biped test (`perf`, flat): `expf` 27 % and `fmaf` 9 % of the test's samples, the loop itself 20 %. The test's centres went 0.46 → 0.32 s (−30 %), the bench stage only −11 %: the new exponential's `floor` was itself a `libm` call (`floorf`, 6 % of a whole-generation profile), fixed in 3717f25 below. The first two baseline runs (`base-ac9923d-{1,2}`) ran under other builds and read 20 % slow everywhere; discarded. Results `c31efed-{1,2}` |
| 2026-11-01 | Part SDFs: an exact band round every triangle plus jump flooding, pseudo-normal signs, in place of `mesh_to_sdf`'s BVH query (53a2223) | gen (toth, bench v5; c31efed ×2 as base, 2 new runs; `ornisculpt-deck` worktree, quiet machine) | stage.anatomy_ms; total_ms; mem.peak_rss_mb | man anatomy 2812 → 472 / 472 ms (−83 %), total 4355 → 2006 / 2020 (−54 %); woman anatomy 2847 → 475 / 472, total 5550 → 3177 / 3171 (−43 %); horse, ox, spider, darner (no sculpted part) within noise (±1 % to −6 %; the darner's 0.11 → 0.12 ms anatomy trips the gate); peak RSS +4 to +8 % (about 30 MB: the flood's per-cell arrays) | gen writes none; head close-ups of the man, woman, gnome, goblin and orc before and after are the same to the eye (PSNR 36–39 dB, woman 29 dB from hair-card jitter) | Profiled first (`perf`, flat): 87 % of the conversion was the BVH traversal's box distances, not triangle tests. The new grid agrees with the old to under a millimetre at all but 88 of 1.75 M cells; at those and at the 109 cells whose signs differ an exhaustive search sides with the new one (the old sign was the nearest face's own normal). Conversion alone 2.7 → 0.53 s on 16 threads (band 10 ms, flood 450 ms); the flood's time is in gathering 26 neighbours per cell per pass, not in triangle tests. Hero tris move by a handful (man 74634 → 74632). Results `53a2223-{1,2}` |
| 2026-10-31 | Crowd layers: a graph's idles and actions baked per body and blended as up to three masked layers; mobs sit on the game's foot hits (822df11..d990c46) | crowd (5 pairs, before/after binaries alternated, one pair in reverse order, toth otherwise idle) | steady p50 / p95; cold p50 / total | 25.7 → 25.9 ms / 26.4 → 26.7 ms (within noise); 19.6 → 22.2 ms (+13 %) / 45.2 → 46.6 s (+3 %) | crowd image: bodies attach in a different order | The bench's crowd has no graphs, so this is the new instance (96 B) and pass on the old workload: steady, GPU-bound, unchanged. The cold median rose in every pair; bisected to the first commit (a binary at 822df11: 21.8 vs 19.6 ms), not the ground fit; reading layers in place in the shader instead of copying the instance didn't move it (22.2 ms). Unexplained: the standing path does less GPU work than before and the CPU side adds only an empty query and a 96-byte compare per mob. Results `cm-ab-{before,after}-{1..5}`, `cm-ab-a-6`, `cm-ab-after-7`. With the standard graph on all 4000 (`examples/crowd` `ORNISCULPT_MOTION=1`, 4000 frames): 14.4–15.6 ms against 15.8–16.7 ms plain in the same session (uncapped, GPU-bound), 3968 vs 3167 slots uploaded per frame (372 vs 297 KB) |
| 2026-10-31 | Hard parts: no region of their own, bone cap 64 → 128, `HardShape` parameters, procedural unicorn horn and orc tusks (b48fb6b..2f6a603) | gen (toth, bench v5; 3 baseline runs vs 2 new, interleaved base / new / base / new, with four other agents building on the machine) | total | man 4689 / 6121 / 9911 → 4653 / 4502 ms, woman 11651 / 5529 / 14398 → 6847 / 6093, horse 6771 / 3324 / 7318 → 3499 / 3299, ox 3986 / 3289 / 2864 → 2828 / 2848, spider 1193 / 898 / 993 → 1633 / 1051, darner 708 / 231 / 719 → 375 / 261 | hard-part meshes of every plan without a `shape` are bit-identical (`default_shapes_build_as_before`, 132 fingerprints); unicorn and orc changed on purpose (not bench mobs) | The machine's noise (20 to 120 % across the baselines) swamps any effect; none of the six bench mobs has a ridged or ringed part, so the change is a per-vertex branch on a plain horn. `compare` passes every gate on the second new run; the first failed the spider's stage gates (+64 %) during a load spike and nothing else. Results `hard-parts-{base-1,2,3,new-1,2}` |
| 2026-10-29 | Near meshes at 0.15 of their triangles, simplified keeping the coat's markings | crowd (2 runs vs early-shell-1,2) | steady p50 | 72.4 / 71.6 → 61.4 / 60.9 ms (−15 %); gen totals and cold start within the v5 baselines' spread | crowd 5.7 % pixels, 32.4 dB (look change) | Near triangles −32 % (man 5046 → 3439, ox 8766 → 6015). Crowd levels hand meshoptimizer the markings baked on the full mesh (`simplify_keeping`), so the ox's socks, face and shoulder band keep sharper edges than at 0.22. Results `near015-{1,2}` |
| 2026-10-29 | Part SDFs: builds asking at once share one conversion (5869fca) | (unit timing) | 4 concurrent head SDFs | 7.4 / 8.4 → 2.0 / 2.3 s | grids unchanged | `concurrent_head_sdf_timing` (ignored test). A crowd's first men and women each converted the head side by side; crowd cold total moved 28.6 / 24.4 → 23.0 / 22.3 s with the next change, inside its 17 % noise |
| 2026-10-29 | Fur shells: single strands take the 2×2×2 cell search, exactly (cfdd3b3) | hero (v5, 2 runs vs v5-base-1,2) | p50 | spider eye 96.3 → 67.9 / 67.5 ms, spider head 80.8 → 62.5 / 62.2, horse eye 49.0 → 34.7 / 34.5, ox eye 22.2 → 17.7 / 18.1; full-body cases within noise | within the baselines' spread (bare man identical) | Results `fast-strand-{1,2}`. For radius < 0.5 cell only the 8 nearest cells can hold a point that close |
| 2026-10-29 | Fur shells: the strand test runs before the lighting setup (f8a37eb) | hero (3 interleaved rounds) | p50, B/A per round | spider eye −5.9 / −6.5 / −5.6 %, horse eye −7.1 / −10.5 / −3.7 %, ox eye −3.4 / −10.7 / −2.5 %, spider head −3.3 / −4.2 / −0.9 % | ≤ 0.28 % pixels, ≥ 42.6 dB (the unchanged darner differs as much) | Results `ab-{A,B}-{1,2,3}`; the darner control moved ±8 %, as did every case near 13 ms |
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
| 2026-10-31 | The field keeps any number of limbs apart: the per-point limb list grows past the 12 it holds inline (eed18a4..dd2cbea; the shader's 12 is a per-cell cap) | gen (toth, bench v5; base e1f355d vs after built as separate binaries from the same target, run interleaved base/after/base/after, then after/base once; two other agents building on the machine) | stage.mesh_ms; stage.centres_of_rotation_ms; total | mesh: man 676 → 668 / 662 ms, woman 904 → 911 / 955, horse 1034 → 1042 / 1074, ox 1118 → 1134 / 1131, spider 618 → 605 / 622, darner 168 → 157 / 152 (−10 to +6 %, within the run-to-run spread but for the woman's second run); centres of rotation +11 to +14 % on every body (man 791 → 892); totals man 4045 → 4304 / 4326, woman 5141 → 5505 / 5597, horse 2889 → 3125 / 3164, ox 2593 → 2787 / 2784, spider 866 → 871 / 888, darner 213 → 206 / 201 | hero/near/far vertex and triangle counts identical; `mesh_fingerprints` (ignored probe, 25 plans × 3 levels) bit-identical for every plan but the new centipede | The field change (2c9f55a) leaves the mesh stage where it was, as the in-process `mesh_times` probe said (horse 877 → 875 ms, spider 515 → 501, lobster 335 → 331), but the same binary runs centres of rotation 11–14 % and the man's and woman's head SDF conversion 6 % slower, stages that never evaluate the field. Bisected: a binary at eed18a4 (the anatomy change alone) matches the base on every stage (±1 %); 2c9f55a carries the whole shift, and running after-then-base gives the same split, so it is the binary, not the run order or load. Read as code layout (release builds use 16 codegen units and no LTO; the hot loops in `surface/cor.rs` and the SDF conversion moved): to confirm, build base and after with `codegen-units = 1` or thin LTO, which also serves the Deck. A constant of 48 instead would have cost 6 to 30 % of hero meshing (zeroed arrays per point). Results `limbs-ab-{base,after}-{1,2,3}`, `limbs-ab-{eed18a4,2c9f55a}`; `limbs-{base,after}-{1,2}` are the earlier runs under heavier load |
| 2026-10-31 | Gnome and goblin sculpted heads (`parts/head_gnome.gltf`, `parts/head_goblin.gltf`: the MakeHuman head warped by `tools/gen_head_gnome.py` and `gen_head_goblin.py`) in place of their procedural faces; the goblin's neck a little thicker under it | not run | — | — | — | Assets only, no engine change; no bench case loads the gnome or the goblin (cases are man, woman, horse, ox, spider, darner, and the crowd's man/woman/ox/spider), so a run would measure only toth's noise with four other agents building. Judged on headless viewer sheets, `target/shots/faces/final-{gnome,goblin}-before-after.png` |
