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

### What a tier costs on the Deck (2026-11-02)

`examples/crowd 4000` (bare mobs, no coats or outfits), one camera, 9000 frames so every body is
generated before the reading, `ORNISCULPT_TIERS=hero_px,near_px,max_hero` sweeping the thresholds.
All runs settle at 12 bodies and the same far count.

| `max_hero` (near_px 80) | hero / near / far | frame | | `near_px` (32 heroes) | hero / near / far | frame |
|---|---|---|---|---|---|---|
| 0 | 0 / 1903 / 2097 | 10.4 ms | | 160 | 32 / 1284 / 2684 | 23.8 ms |
| 8 | 8 / 1895 / 2097 | 13.9 ms | | 80 | 32 / 1871 / 2097 | 26.7 ms |
| 16 | 16 / 1887 / 2097 | 17.1 ms | | 40 | 32 / 2892 / 1076 | 32.0 ms |
| 32 | 32 / 1871 / 2097 | 26.7 ms | | none (hero overflow only) | 32 / 1003 / 2965 | 23.3 ms |
| 64 | 64 / 1839 / 2097 | 43.8 ms | | | | |

Which fits **base under 1 ms + 0.5 ms a hero mob + 5 µs a near mob + under 0.5 µs a far one**:

- **The hero tier is the Deck's crowd frame.** The marginal hero costs 0.44 / 0.40 / 0.60 /
  0.53 ms across the four steps — linear, so it is geometry and the shadow passes, not screen
  size (the biggest mobs are promoted first, and they are no dearer than the rest). At the
  default cap of 32 that is 16.3 of 26.7 ms, 61 % of the frame for 0.8 % of the mobs.
- **The near tier is cheap here**, 4.92 µs a mob from 160 to 80 px and 5.18 µs from 80 to 40 —
  flat in on-screen size, so it is per-mob work, not pixels. tir measured 24.5 µs (v5 notes
  above): the Deck's near tier is five times cheaper than tir's while its hero tier is dearer.
  The near tier is not where this device's frame goes.
- **A 60 fps frame on the Deck** buys about 30 hero mobs, or 3000 near ones, or a mix. Phase 2
  opens by noting domain-expance holds every creature at `ForceTier(Hero)`; that is the budget
  it is spending.
**Where the hero mob's half-millisecond goes** (`ORNISCULPT_SHADOWS=0` stops the key light
casting; same scene and camera):

| | shadows on | shadows off | the shadows |
|---|---|---|---|
| 0 heroes | 10.4 ms | 7.0 ms | 3.4 ms |
| 32 heroes | 26.1 ms | 7.7 ms | 18.4 ms |

- **A hero mob is its shadow.** The 32 heroes' casting is 18.4 − 3.4 = 14.9 ms, **0.47 ms each**;
  everything else about being a hero — the 74 k-triangle mesh, the joint upload, Bevy skinning —
  is (7.7 − 7.0) / 32 = **24 µs each**. Shadow casting is 95 % of the tier's cost, and the
  remaining 24 µs is only five times a near mob's 5 µs, which is what the detail difference
  ought to look like.
- The 3.4 ms with no mob casting is the ground plane's cascades plus the shadow sample every
  fragment of all 4000 mobs pays. Crowd tiers cast nothing (`NotShadowCaster`).
- With shadows off, 32 heroes (7.7 ms) runs *faster* than none (7.0 ms → within the 6.2–8.3
  spread): promoting the nearest mobs out of the crowd shader costs nothing measurable.
**What a coarse mesh costs to cast** (`ORNISCULPT_CROWD_SHADOWS=1` drops the `NotShadowCaster`
the library gives crowd meshes, so all 4000 crowd mobs cast their simplified geometry):

| | not casting | casting | the casting |
|---|---|---|---|
| 4000 crowd mobs, 0 heroes | 10.4 ms | 30.1 ms | 19.7 ms → **4.9 µs a mob** |
| 3968 crowd mobs + 32 heroes | 26.1 ms | 48.8 ms | 22.7 ms → **5.7 µs a mob** |

- A crowd mesh casts for 5 µs, a hero mesh for 470 µs: **90 times cheaper** — about what the
  triangle counts say (3.4 k against 74 k on the man), so the shadow pass is geometry-bound.
- **Crowd shadows** (roadmap item 6) therefore cost about 5 µs a caster: all 4000 is +20 ms and
  out of the question, but the nearest few hundred is 2 to 3 ms. It wants a caster budget, not
  an on/off switch.
- *(Done the same day: hero mobs cast through a coarse proxy, 26.1 → 9.9 ms at 32 heroes and
43.8 → 10.3 at 64. First row of the Changes table.)* **A coarse shadow for a hero** would take
the 32 heroes' 14.9 ms to about 0.2 ms — 56 % of
  this frame. Drawing one needs a trick, though: Bevy has no shadow-only component, and the way
  to fake it — a proxy on a `RenderLayers` layer the camera does not see but the light does,
  which works because casters are gathered on `InheritedVisibility` and filtered by the light's
  mask (`bevy_light-0.20.1/src/lib.rs:342`) — rests on an interaction neither `RenderLayers` nor
  the light types document. The proxy has to
  follow the hero's real pose, so it would be the Near mesh Bevy-skinned by the hero's joints in
  crowd-skeleton order — not the crowd instance, which hero mobs stop uploading
  (`Without<MobSkeleton>`, `motion/crowd.rs:342`) and which carries the baked pose, not the
  played one.

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
| 2026-11-02 | Centres of rotation: `exp_neg` by interpolation in a table, instead of an exponent shift and a polynomial | gen (toth, bench v5, **5** interleaved runs of each build against 269c3c3, machine otherwise idle) | whole-run user CPU; `stage.centres_of_rotation_ms` (mean of 5) | **User CPU 500.1 → 474.4 s (−5.1 %; 493.5–504.5 → 470.0–476.7, the ranges not overlapping).** Centres stage: man 308.7 → 272.2 ms (−11.8 %), woman 597.8 → 526.0 (−12.0 %), horse 599.9 → 526.5 (−12.2 %), ox 479.6 → 420.6 (−12.3 %), spider 73.0 → 64.7 (−11.3 %), darner 16.6 → 14.9 (−10.3 %); summed 2076 → 1825 (−12.1 %). Totals 7991 → 7725 ms (−3.3 %) | gen writes none; brute-force check still **1.3329 mm**, unchanged to four decimals | 2048 samples of `e^-x` over [0, 16] plus one spare, built once per mesh and threaded through `group_centres`, read with linear interpolation: **8e-6 relative** (measured 7.74e-6), against the quartic's 6.4e-6 — and a far shorter dependency chain than an `f32`-to-`i32` round trip, a Horner chain and a `from_bits`. Above x = 16 it returns zero: the histogram of the argument over 215 M scoring visits puts **3.8 % of calls there carrying 1e-9 of the total weight**, which is why the brute-force figure does not move. 8 KB of L1 shared by the workers, read-only. This also settles that the member loop is **latency-bound on the exponential, not memory-bound** — the two leaner data layouts in the row below were worth 2 %, this is worth 12 %. Results `cor-exp-table-run-{1..5}` against `269c3c3-{1..5}` |
| 2026-11-02 | Centres of rotation: the per-triangle pair list drops the stored product, 16 → 12 bytes an entry | gen (toth, bench v5, 4 interleaved runs each of four builds against c79988c, machine otherwise idle) | whole-run user CPU; `stage.centres_of_rotation_ms` (mean of 4) | **Centres stage summed 2117 → 2079 ms (−1.8 %); every run of the new build beat every run of the old on every mob**: man 315.1 → 309.9, woman 612.9 → 599.7, horse 611.8 → 601.3, ox 486.4 → 478.1, spider 74.1 → 73.1, darner 16.9 → 16.6. Whole-run user CPU 503.7 → 502.1 s (−0.3 %, ranges overlapping — the stage is about a third of the run) | gen writes none; **bit-identical** (`pj * pk * vjk` evaluates as `(pj * pk) * vjk`, the same multiply in the same place) | The hoisted list of fa0b724 streams once per member, 2111 members × 8284 candidates for the biggest group, so the record's size is worth as much as its contents: dropping `pj·pk` and recomputing it costs one multiply and saves 4 of 16 bytes. **Two other layouts were measured in the same batch and dropped.** Dense per-triangle weights plus a per-triangle bitmask of the scoring pairs (17 B a triangle against 42) came to 2086 ms, −1.5 % — as good, for more code and four bounds-checked loads a visit instead of one, so not worth it. This says the member loop is **not** memory-bound: tripling the traffic in fa0b724 still won 23 %, and shrinking it back wins 2 %. Results `cor-slim-pairs-{1..4}`, `cor-pair-bitmask-{1..4}` against `c79988c-{1..4}` |
| 2026-11-02 | Centres of rotation: `exp_neg` straight-line, with no early-out for large `x` | gen (toth, bench v5, 4 interleaved runs of each build against 483ee8a, machine otherwise idle) | whole-run user CPU; `stage.centres_of_rotation_ms` (mean of 4) | **User CPU 505.5 → 504.1 s (−0.3 %; 500.5–510.5 → 500.4–505.9, the ranges overlapping).** Centres stage: man 319.1 → 317.3 ms, woman 619.8 → 617.0, horse 617.3 → 617.1, ox 490.6 → 489.2, spider 74.3 → 74.3, darner 16.9 → 16.8; summed 2138 → 2132 (−0.3 %) | gen writes none; brute-force check still 1.3329 mm | **Small enough to be almost nothing, and recorded as such**: it is below every mob's spread except the man's, where all four runs of the new build came out lower. Kept because it is consistent and the function is simpler for it. The guard was `if x > 80.0 { return 0.0 }`, which the probe says fires on **0.30 %** of calls here — it was paying a test on every call to skip work on three in a thousand. The exponent shift saturates to zero by itself once `127 - whole` would go negative, so the guard was not load-bearing; what it did guard was an infinite `x`, which now yields NaN instead of 0 (unreachable from a finite mesh). Results `cor-no-earlyout-{1..4}` |
| 2026-11-02 | Centres of rotation: `exp_neg`'s fraction by the least-relative-error quartic instead of the degree-6 Taylor series | gen (toth, bench v5, 4 interleaved runs of each build against fa0b724; nothing else running on the machine, so these stage times are steady to 0.7 %) | whole-run user CPU; `stage.centres_of_rotation_ms` (mean of 4, min–max) | **User CPU 532.9 → 505.5 s (−5.1 %; 526.4–535.6 → 500.5–510.5).** Centres stage: man 366.6 → 319.1 ms (−13.0 %), woman 714.8 → 619.8 (−13.3 %), horse 709.3 → 617.3 (−13.0 %), ox 562.6 → 490.6 (−12.8 %), spider 83.9 → 74.3 (−11.4 %), darner 18.3 → 16.9 (−7.9 %). Totals 8356 → 8070 ms (−3.4 %) | gen writes none; the brute-force check still reads 1.3329 mm | `perf annotate` put `exp_neg` at **56 % of `group_centres`** and `group_centres` at 47 % of generation CPU, so a quarter of this bench's CPU is inside that one function. Its 2^frac was a degree-6 Taylor series about 0, which is a poor fit at the far end of (−1, 0]: the quartic that minimises relative error over the whole interval is **both shorter and more accurate** — 6.4e-6 measured in f32 against the series' 2.9e-5 — so the dependency chain loses two multiplies for nothing. The test's bound tightens 5e-5 → 1e-5 to say so. Plain multiplies, not `mul_add`, for the reason the file already gives. Results `cor-quartic-exp-{1..4}` against `fa0b724-{1..4}` |
| 2026-11-02 | Centres of rotation: the per-group gather strips each candidate triangle down to the bone pairs it actually weights, so the member loop visits only pairs that score | gen (toth, bench v5, 4 interleaved runs of each build against 39c635c; **other agents were building on toth through most of them**, so the wall-clock stage times carry up to 41 % spread and the process's user CPU is the primary metric here) | whole-run user CPU; `stage.centres_of_rotation_ms` (mean of 4, min–max) | **User CPU over the whole `gen` run 610.9 → 544.7 s (−10.8 %; 607.3–618.2 against 540.7–547.6, the two ranges not overlapping, each build's own spread under 2 %).** Centres stage, mean (min–max): man 535 → 413 ms (−23 %; 456–644 → 374–435), woman 1094 → 777 (−29 %; 1004–1342 → 726–833), horse 1039 → 776 (−25 %; 941–1123 → 721–824), ox 823 → 638 (−23 %; 811–844 → 569–657), spider 118 → 97 (−18 %; 105–127 → 85–106), darner 26.3 → 22.7 (−14 %; 22.0–28.0 → 18.5–24.9). Totals mean 11 026 → 9 530 ms (−14 %) | gen writes none; **the centres are bit-identical** (an FNV hash of every centre matches on biped, quadruped and horse), and the brute-force check still reads 1.3329 mm | `pjk = pj·pk`, the two `p.get()`s feeding it and the `== 0.0` skip ran once per member per candidate per pair, though all three depend only on the triangle and the pair. The gather now builds a per-triangle list of just the pairs with `pjk != 0`, in the same order, so the member loop does the same multiplies in the same order and the result is unchanged. A probe on the biped hero mesh counted **406 M pair visits of which 47 % scored nothing**; `perf annotate` put that line at 11.6 % of `group_centres`. A weight vector has at most 4 bones, so a group has at most 6 pairs — not the 28 the `SmallVec` hints at. **Also tested and dropped: re-gathering per 128-member chunk.** It is real (1.71× the necessary triangle-touches) but the gather is 0.41 % of the scoring work it feeds, and no gather line reached 0.4 % of the function in the profile; the module doc that claimed one gather per group is corrected instead. Results `39c635c-{1..4}` and `cor-sparse-pairs-{1..4}` |
| 2026-11-02 | Crowd shadows on a budget (`QualityProfile::max_casters`), and the behind-camera bug in `screen_px` it turned up | crowd (toth: `bench crowd` in interleaved pairs against f042866, plus `examples/crowd 4000` for the wide-shot numbers) | steady p50; tier counts | **The budget is off by default, so the bench measures the rest of the change: steady p50 12.90 / 12.92 → 13.26 / 13.28 ms (+0.36, both pairs agreeing), cause unlooked-at** — not the `CrowdCastable` marker (dropping it reads the same) and not a per-frame sweep (there isn't one). With casters, the bench's close framing costs **0.6 ms for 16 and 1.3 ms for 64**, while `examples/crowd`'s wide shot costs **0.10 ms for 64, 1.18 for 256 and 5.97 for all 3968**. Tier split 2436 near / 1532 far → 1707 / 2261 | the nearest mobs gain shadows when a profile asks for them; with a camera inside the crowd the hero mobs change (see the note) | A crowd mob's coarse mesh may cast once its on-screen size puts it in the budget, biggest first; coat shells and cards never cast (alpha-tested). **What a caster costs is its size on screen, not its triangles**: 64 of them are 0.1 ms in a wide shot and 1.3 ms up close, so the earlier row's "5 µs a caster" (measured with all 4000 casting, nearly all tiny) understates a near caster five to ten times. Hence off on every profile but `HIGH`. **The bug:** `screen_px` clamped depth to 0.01, so a mob *behind* the camera read as the biggest thing in the scene and took a hero slot — a camera standing in a crowd spent its whole hero budget on mobs at its back, and the first version of the caster budget drew eight shadows nobody could see, which is how it was found. Now anything entirely behind the camera is 0 px. This changes which mobs are heroes in any scene whose camera stands inside the crowd, including the tier-cost sweep rows above: those heroes still cost about what was measured (an off-screen hero still poses, uploads joints and casts its proxy shadow), but the counts there are not all on-screen mobs |
| 2026-11-02 | A body's clip-atlas block is freed when the body goes, and a bake's when its graphs reload (`CrowdSkin::release_range`) | crowd (toth, `bench crowd`, 2 interleaved base/new pairs against d5b9ff6's parent) | cold total; steady p50 | base 30.1 / 31.6 s cold and 12.88 / 14.11 ms steady; new 31.0 / 35.9 s and 13.05 / 14.19 ms | none | **The bench cannot show this change**, and the runs are here only to show it costs nothing: no body is ever dropped in a bench run, so no block is ever released and `append` walks an empty hole list. The two binaries track each other and both drift upward across the pairs, which is toth's usual cold-phase spread. What the change is for is a session that replaces bodies (an editor's previews, a game streaming creatures) and, more sharply, motion-graph reload: that cleared `CrowdCycles::baked` and leaked every atlas block those bakes held, so each save of a graph in a running game leaked the whole baked set. Correctness is pinned by four unit tests (reuse, joining, the atlas shortening at its end, and the bakes) and by `examples/crowd 2000 ORNISCULPT_MOTION=1` rendering as before — a wrong atlas index shows up instantly as garbage skinning |
| 2026-11-02 | Generated bodies kept on disk by `BodyKey` (`BodyMeshCache::set_dir`, `ORNISCULPT_CACHE=<dir>` in the examples) | crowd (toth and Steam Deck, `bench crowd`: cache off, cache filling, then warm — once on toth, twice on the Deck); unit timing | cold start; steady p50; peak RSS; disk | **Steam Deck: cold start 79.2 s off → 1.89 / 1.86 s warm** (the filling run 79.7 s, so writing 190 MB costs nothing measurable). Deck cold frames 1124 → 18 / 17, frames over 33 ms 1015 → 9 / 11, over 100 ms 206 → 3 / 2, worst cold frame 1816 → 614 / 618 ms, cold peak RSS 985 → 808 / 795 MB (−19 %); Deck steady p50 39.89 → 38.73 / 38.63 ms (−3 %). **toth: 37.6 s off → 1.03 s warm** (filling 35.2 s), cold frames 2108 → 14, over 33 ms 119 → 5, worst cold frame 820 → 366 ms, cold peak RSS 1421 → 1155 MB, steady p50 13.60 → 13.60 (filling 13.69). The man alone on toth: **1.57 s to generate, 15.5 ms warm** — anatomy 12.2 ms, cache read 0.77, motion bake 0.51, contacts 1.92; 4 MB on disk, 190 MB for the crowd's 24 bodies | crowd image differs: with a warm cache the whole crowd is standing by the capture frame instead of a part of it | The cached seam is `[BodyMeshData; 3]` plus the coat's `[CoatMeshes; 3]`, written by hand as little-endian arrays (`mob::mesh_cache`), so meshing, garments, annotation, centres of rotation, muscle flex, soft-tissue weights and the coat's cards and shells are all skipped; anatomy, the motion bake and the contact set are rebuilt. `a_cached_body_is_read_back_by_the_next_build` pins that the second build does not mesh and its geometry is byte-identical to the first's; a damaged, truncated, stale-layout or other-`PartLodRules` file is a miss, not a crash (three more tests). Off by default — the crate writes nothing to disk unless an app asks. **Not comparable with the other crowd rows**: a warm cache measures a different workload, so `bench` keeps the cache off unless `ORNISCULPT_CACHE` is set. The Deck is where this was aimed and where it pays most: 42 times off the cold start, and a crowd that is standing in under two seconds instead of a minute and a quarter. Two effects were not expected and hold across both warm runs there: peak RSS is a fifth lower (the field, the octree and the dense meshes are never allocated) and the steady frame is 3 % faster, cause unlooked-at. Results `deck-a431abe-{off,warm-1,warm-2,warm-3}`; toth's in the session scratchpad (`off-1`, `warm-1`, `warm-2`), no results directories |
| 2026-11-02 | A formed plan's hash memoised per (plan asset, `Form`), so a crowd hashes a dozen bodies instead of four thousand | crowd (toth, `bench crowd`, 3 runs after against 5 before); unit timing | `body_key`; cold frames | **`body_key` 62.5 → 6.2 µs a mob**, so the frame that first sees 4000 mobs spends 25 ms on keys instead of 250. The bench's worst cold frame read 1.85 / 1.99 / 2.07 / 2.04 / 2.22 s before and 0.71 / 1.54 / 1.64 s after | none | Hashing a plan means serialising it: 6.5 KB of RON, 56 of the 62 µs, repeated for every mob though a crowd of 4000 holds a dozen distinct (plan, form) pairs. The memo keys on the plan's asset and its form and is dropped when the plan reloads; an edited `LiveBody` is still hashed outright, since it shares its asset and form with a plan it no longer matches. Key derivation is otherwise unchanged, so bodies still share exactly as before. **The crowd bench on toth cannot confirm the frame-level effect**: an unchanged binary gives two modes there (cold p50 9.0–9.3 ms with 12–13 frames over 33 ms, or 14.4–17.8 ms with 146–214), and these runs land in the fast one. The 62.5 → 6.2 µs is a direct measurement and stands on its own |
| 2026-11-02 | Tracy trace of the crowd's cold phase, after the `tracy` feature (`cargo build --features tracy`, `tracy-capture -o run.tracy`, `tracy-csvexport`) | crowd (toth, `bench crowd`, 4 runs of one binary) | cold frames; zone times | The cold phase's worst frame is **2.4 s inside `start_generation`** (mean 1.0 ms over 3019 calls), the frame where every mob is handled before any body exists. Measured directly for the man's plan: `body_key` 65 µs a mob (260 ms for 4000), `apply_form` + `LiveBody` 15 µs, `referenced_assets` 15 µs — about 374 ms of it, the rest on the heavier plans. `body_key` dominates because it RON-serialises the formed plan, the settings, the coat and the outfit for every mob | none | **The 13 % cold-median regression this was meant to explain is not separable on toth**: one unchanged binary gave cold p50 9.3 / 9.1 / 16.6 / 17.8 ms and frames over 33 ms of 12 / 13 / 189 / 214 across four runs. The cold phase here is far too noisy for a 13 % median; it wants the Deck, or many interleaved pairs. Retracted along the way: `bevy`'s `multi_threaded` looked absent from `Cargo.toml` and `AsyncComputeTaskPool::spawn` runs its future inline without it, which would have put generation on the main thread — but `bevy_tasks` has the feature on through its own defaults (`cargo tree -f "{p} [{f}]" -p bevy_tasks`), so that was never the cause and adding it changed nothing. The hotspot stands on its own: hashing a plan by serialising it costs a quarter-second of one frame in a 4000-mob crowd |
| 2026-11-02 | tir rerun of `gen` at 5a5f0ff, settling what the Deck/tir generation gap is now | gen (tir, bench v5, 2 runs against its own `v5-base-{1,2}`, on mains) | stage and total | man 2306 → 1204 / 1218 ms (−47 %), woman 2882 → 1580 / 1560 (−45 %), horse 1462 → 1206 / 1261 (−16 %), ox 1628 → 1196 / 1206 (−26 %), spider 527 → 528 / 511, darner 146 → 158 / 148. By stage: the head's SDF (anatomy) 1501 → 413 ms on the man and 1611 → 419 on the woman (−70 to −74 %), centres of rotation −20 % (man) to −49 % (ox) | gen writes none | **The Deck is now 2.3 to 3.8 times tir's generation, against 4.3 to 5.4 at v5** (man 3.0, woman 3.3, horse 3.8, ox 3.7, spider 3.3, darner 2.3, taking the Deck's `deck-3717f25-1`). The two fixes paid more on x86 than on aarch64, exactly as the libm story predicts: centres fell 48 to 57 % on the Deck and toth but only 20 to 49 % here, and the head's conversion 87 % there against 70 to 74 % here. What is left is close to the core-speed difference. The run also carries everything else since v5-base (near triangles −32 % from the 0.15 simplify ratio, annotate −17 %), and `coat_meshes` reads +12 to +37 % on every plan — small in absolute terms (33 → 43 ms), cause unlooked-at. Results `tir-5a5f0ff-{1,2}` |
| 2026-11-02 | Hero mobs cast through a coarse proxy: their own meshes `NotShadowCaster`, the `Near` mesh skinned by their joints in crowd-skeleton order, kept off camera on render layer 61 | crowd (Steam Deck, `examples/crowd 4000`, bare mobs, one camera, frame 9000) | frame | 32 heroes 26.1 → **9.9 ms** (−62 %), 64 heroes 43.8 → **10.3 ms** (−76 %). A hero's marginal cost is now 12 µs against 0.5 ms, about 40× cheaper, and the whole shadow bill at 32 heroes is 2.2 ms instead of 18.4 | viewer shots of a man, standing and bent 55°: the shadow is in the same place and follows the played pose (59.5 and 54.6 dB whole-frame, the difference confined to the shadow's own pixels) | The proxy is the `Near` geometry (3.4 k triangles against 74 k) Bevy-skinned by the mob's own joints, so the shadow bends with what is being played, not with a baked crowd pose. **It casts while invisible only because `bevy_light` filters shadow casters by the light's `RenderLayers`, which neither `RenderLayers` nor the light types document** — see `README.md` and the warning on `SHADOW_PROXY_LAYER`. `shadow_proxy_layers` puts every shadow-casting light on the layer, so apps change nothing; a camera on layer 61 would draw the proxies. Costs one more mesh per body and one entity per hero |
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
