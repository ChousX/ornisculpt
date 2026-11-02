# Roadmap

Status: **in progress.** Proposed 2026-10-31 at 3735bf1, from the limits the design docs record
as open. Items marked *done* say where they landed.

## Where it stands

- **Built:** anatomy plans, the body field and its meshing, skinning (centres of rotation, DQS
  fallback), coats with markings, fur, feathers and hair cards, wings, the motion runtime (cycles,
  root motion, the built-in library and graphs), baked crowd motion on the GPU, clothing,
  equipment, contacts, soft tissue, genetics, interchange, the browser build and the versioned
  bench.
- **Used by:** domain-expance (a game: grazing creatures bred from genomes), ornithochory (plants
  that bend round mobs) and ornisculpter (the editor). All three follow `main`.
- **How work has gone:** creature passes, each shot headless, each ranking the engine limits it
  met (`realism.md`). The features have outrun the hardening. The next gains are in the caps,
  assets that fail cleanly, cost on the machines games run on, and what domain-expance needs.

## Phase 0: consolidate

1. **Docs that match the code.**
   - *Done* (de7d2da): `types.md`'s v2 notes are folded into
     its sections, each checked against the code at e1f355d, with the subsystem docs linked
     and the measured numbers dated.
   - *Done* (f115db7): `README.md`, with the module map, the feature flags, the consumers and
     links to these docs.
2. **Bad assets fail fast.** *Done* (f115db7).
   - A plan, coat, outfit or part mesh that fails to load fails its mobs through `BodyFailed`,
     and headless shots exit with the error.
   - A selector that matches nothing warns once per body and coat. It warns when the body is
     baked, not at load, because a selector can only be judged against a body.
3. **A release point.** *Done*: `v0.2.0`.

## Phase 1: the fantasy pass's engine limits

1. **Bone and region caps.** *Done* (0dd418d).
   - Hard parts take the region of the skin they grow from, plus a hard-part index for markings.
     The dragon went from 64 regions to 33. `Region::MAX` stays 64.
   - The hero bone cap is 128. It was only a constant: bone ids are bytes, and Bevy skins up to
     256 joints. The crowd's cap stays 32.
2. **Hard-part shapes.** *Done* (0dd418d).
   - `HardShape` gives a base, taper, bend, ridges and rings, and the defaults build every
     existing plan unchanged.
   - The unicorn's horn is procedural now. The orc's tusks are thicker and splay out.
3. **Mirrored copies that differ.** *Done* (e06c604).
   - A mirrored part takes `left` and `right` overrides, applied in `expand_symmetry`.
   - The lobster has a crusher claw and a cutter claw.
   - *Done* (1c092f0): `LimbEnd::Pincer` takes a `shape` (finger length, palm width and
     depth, finger thickness, gape) whose defaults build the old claw. The lobster's cutter has
     long slim fingers on a flat palm; its crusher a swollen palm, short thick fingers and domed
     `HardShape` molars.
4. **More than 12 limbs apart.** *Done* (d78791b).
   - `BodyField::eval` keeps every limb apart: its per-point limb list grows past the 12 it holds
     inline, instead of merging the extras into the last slot. Every other plan's meshes are
     bit-identical (the arithmetic is unchanged; a constant of 48 would have cost 6 to 30 % of
     hero meshing time in zeroed arrays). The preview shader's 12 is now a per-cell cap, which
     the packer checks and `every_mob_fits_the_preview_cells` tests: a lobster's cell reaches 11.
   - The centipede (`mobs/centipede.body.ron`, sixteen pairs, 127 bones, 17 regions) proves it,
     with `share_regions` so a series of legs takes one set of regions per side.
   - Still open: the centipede sits one bone under the hero cap, and its crowd skeleton keeps
     only the first four legs (11 of its 32 bones), so crowd tiers can't play its wave.

Field and primitive changes carry over to ornisculpter's preview shaders (`--features preview`).

## Phase 2: game-readiness

domain-expance holds every creature at `ForceTier(Hero)`, so that feet follow the ground. That
gives up the crowd tiers, which are where the frame budget goes furthest.

1. **Crowd tiers on uneven ground.** *Done* (1cba439).
   - A crowd mob with a `FootGround` sits on the game's foot hits: a lift, a tilt and a per-foot
     offset in the blend. `examples/slope.rs` shows it.
   - Next, domain-expance can drop `ForceTier(Hero)` for distant creatures.
2. **Crowd baking of graph cycles.** *Done* (1cba439).
   - Each body bakes the idles and actions its graph plays, and the blend takes up to three
     masked layers.
   - `eat` is in `motion::library` for quadrupeds that can reach the ground.
   - Still open:
     - an action drops the breathing layer while it plays (three layers in all);
     - crossfades inside a layer show only the heavier cycle;
     - a graph's own gaits aren't baked.
3. **Measure on the target device.** Done on 2026-10-31: the bench v5 baseline for the Steam
   Deck is in `bench/LOG.md`, and `tools/deck-bench.sh` reruns it. What it found:
   - generation is about 5 times tir's, with centres of rotation and the head's SDF
     conversion furthest behind, so the crowd's cold start takes two minutes;
   - fur close-ups are over a 60 fps budget.
   Those two come before more shader work. The contacts loop and SSAO are still to measure.
   - *Done* (c31efed, 3717f25, 53a2223), the two slow stages, profiled on toth (a Zen 2
     like the Deck's APU; `perf record -e cpu-clock:u`, no call graphs under this kernel's
     `perf_event_paranoid`): centres of rotation spent two thirds of their time in `libm`'s
     `expf` and `fmaf` (`f32::mul_add` is a call on x86 without the FMA target feature, as
     `f32::floor` is without SSE4.1; both are one instruction on aarch64, which is why tir
     never showed them); the head's conversion spent 87 % in `mesh_to_sdf`'s BVH
     traversal. A cheap exponential and plain arithmetic in the one, a band-plus-jump-flood
     grid in the other (`anatomy::mesh_sdf`). On toth: centres −48 to −56 %, the head's
     conversion −83 %, the man's generation 4.4 → 1.65 s (rows in `bench/LOG.md`). Still
     open: `group_centres` is still 47 % of generation CPU, the body field 26 %; the other
     stages' `mul_add`s (`surface::body`, `sdf.rs`, `contact.rs`) cost the same way wherever
     they are hot.
   - *Done*: the Deck rerun (`tools/deck-bench.sh all`, results `deck-3717f25-1`). The two
     stages fall as far there as on toth: centres of rotation −53 to −57 %, the head's
     conversion −87 % (7.8 → 1.0 s), the man's generation 11.9 → 3.6 s and the woman's
     14.7 → 5.2 s. **The crowd's cold start is 78 s instead of 128**, its median frame 91 ms
     instead of 106, with 376 frames over 100 ms instead of 681. Hero frames, walks and the
     crowd's steady frame don't move: they are GPU-bound. Against tir's v5 totals generation
     is now 1.6 to 3.4 times rather than 4.7 to 5.6. What remains on the Deck is meshing (1.4 to
     2.5 s a mob) and the centres. Two minutes of cold start is still two minutes at 4000 mobs,
     so item 5's body cache is what makes it cheap.
   - *Done* (2026-11-02): the tir rerun that was owed. tir's own totals fell 13 to 47 % (man
     2306 → 1211 ms, woman 2882 → 1570), so **the Deck is 2.3 to 3.8 times tir's generation
     against 4.3 to 5.4 at v5** — the fixes paid more on x86 than on aarch64, as the libm story
     predicts, and what is left is close to the core-speed difference.
4. **Crowd cost.**
   - *Done* (nothing kept, 2026-11-02): merging the cloth submesh into the skin mesh. The item
     rested on 23 to 27 ms against 15 ms plain, read on tir in October; measured again, dressing
     4000 mobs costs 0.8 ms a frame on toth and about 2 ms on the Deck. Crowd mobs are instanced,
     so a garment is an extra *entity*, not an extra draw, and the merge (built, look-identical,
     measured) gains nothing: −0.1 to −0.9 ms on toth and inside the spread on the Deck. Rows in
     `bench/LOG.md`.
   - *Done* (2026-11-02): what each tier costs on the Deck, by sweeping `ORNISCULPT_TIERS` over
     4000 bare mobs at one camera (table in `bench/LOG.md`). A frame is a base under 1 ms plus
     **0.5 ms a hero mob, 5 µs a near mob and under half a microsecond a far one**. So the near
     tier is not the cost here — tir's 24.5 µs a near mob doesn't carry over, the Deck's is five
     times cheaper — **the hero tier is**: at the default cap of 32 it is 16.3 of the 26.7 ms,
     61 % of the frame for 0.8 % of the mobs, and it grows linearly to 43.8 ms at 64. A 60 fps
     frame on the Deck buys about 30 hero mobs or 3000 crowd ones.
   - *Done* (2026-11-02): that 0.5 ms is **the shadow pass, 95 % of it** (`ORNISCULPT_SHADOWS=0`;
     table in `bench/LOG.md`). The 32 heroes' casting is 14.9 ms, 0.47 ms each; everything else
     about being a hero — the 74 k-triangle mesh, the joint upload, Bevy skinning — is 24 µs a
     mob, only five times a near mob. With shadows off, 32 heroes costs no more than none.
   - *Measured* (2026-11-02): a coarse mesh casts for 5 µs against the hero mesh's 470 µs, 90
     times cheaper and in proportion to the triangles (3.4 k against 74 k). So giving a hero a
     coarse shadow would take the 32 heroes' 14.9 ms to about 0.2 ms, **56 % of the Deck's crowd
     frame**. It also prices item 6: crowd shadows are 5 µs a caster, so the nearest few hundred
     cost 2 to 3 ms and all 4000 cost 20.
   - *Done* (2026-11-02): hero mobs cast through a coarse proxy, taking the Deck's crowd frame
     **26.1 → 9.9 ms at 32 heroes and 43.8 → 10.3 at 64**. A hero's marginal cost is 12 µs where
     it was 0.5 ms. The proxy is the `Near` mesh skinned by the mob's own joints in
     crowd-skeleton order, so the shadow follows the played pose; it is kept off camera on
     `SHADOW_PROXY_LAYER`, which `shadow_proxy_layers` adds to every shadow-casting light. The
     chosen route was the first of the two below, with its warning written into the constant,
     `README.md` and `types.md` §10. What it cost: one more mesh per body, one entity per hero,
     and a dependence on undocumented engine behaviour to watch at each Bevy upgrade.
   - The two routes that were weighed, kept here because the choice may want revisiting:
     - *A coarse proxy* keeps every hero's shadow and takes nearly all the saving, but there is
       no first-party way to draw one. Bevy has `NotShadowCaster` and `NotShadowReceiver` and
       nothing for shadow-only. It would have to lean on `RenderLayers`: casters are gathered on
       `InheritedVisibility` and filtered by the light's own mask
       (`check_dir_light_mesh_visibility`, `bevy_light-0.20.1/src/lib.rs:342`), so a proxy on a
       layer the camera doesn't see but the light does casts without being drawn. That
       interaction is real but **undocumented** — `RenderLayers` documents cameras only, and the
       light types never mention it — so it could go in a release without being called
       breaking, which matters with three consumers on `main`. It also costs an API convention:
       apps would have to put their lights on the proxy's layer, or the plugin would have to.
       The proxy must follow the played pose, so it is the Near mesh Bevy-skinned by the hero's
       own joints in crowd-skeleton order — not the crowd instance, which hero mobs stop
       uploading and which holds the baked pose.
     - *A caster cap* is documented API and no trick: the nearest few heroes cast, the rest take
       `NotShadowCaster`. Four casters is 1.9 ms against 32's 14.9. The cost is that a mob pops
       its shadow as it crosses the threshold.
     Decide between them before writing either.
5. **Long sessions.**
   - Free a body's space in the crowd clip atlas when the body goes.
   - Keep a disk cache of generated bodies by `BodyKey`. A crowd's cold start (78 s for 4000 on
     the Deck) and the first head SDF are then paid once per install.
     - *Done* (2026-11-02), the head SDF half: `PartAssetCache::set_cache_dir` keeps converted
       grids by (mesh hash, resolution). The man's head is 0.5 s to convert and 3 ms to read
       back, 7 MB on disk. Off by default; a damaged or stale file is converted again.
     - Still open, the body half. Caching `GenOutput` whole would mean serde across the
       anatomy graph (`Anatomy` alone has twenty fields of its own types) and a binary codec.
       The narrower seam is `BodyMeshData`: a dozen parallel arrays of plain data, so the
       levels, coat meshes and their centres and weights can be written by hand the way the SDF
       grid is, leaving only the cheap part of anatomy to rebuild (0.1 s once the head's grid is
       cached). That is the next step, and it is where the rest of the 78 s lives.
6. **Motion gaps:** `Reach` arm IK, fingers closing on a grip, `BoneScales` and custom gaits in
   crowd tiers, and crowd shadows. The game's needs set the order.
7. **Bugs the work above turned up.**
   - The crowd's cold-start median rose 13 % with the crowd layers (822df11), and the steady
     frame didn't move. The Tracy trace the item asked for (2026-11-02, `--features tracy`)
     **could not separate it on toth**: one unchanged binary gave cold p50 9.3 to 17.8 ms and
     frames over 33 ms of 12 to 214 over four runs. Retrying it wants the Deck, whose cold
     phase is steadier, or many interleaved pairs. The trace did find a hotspot worth fixing on
     its own: the worst cold frame is 2.4 s in `start_generation`, where every mob computes its
     `BodyKey` before any body exists, and `body_key` RON-serialises the formed plan, settings,
     coat and outfit each time — 65 µs a mob, a quarter of a second for 4000. Hashing the plan
     asset once and mixing in `Form`, the seed and the coat and outfit hashes would remove it.
   - A headless run that exits early sometimes crashes in teardown (`free(): invalid size`, or
     a segfault) after logging its error. This predates f115db7, so scripts should test for a
     non-zero exit rather than 1.

## Phase 3: realism, in `realism.md`'s order

1. **Faces.**
   - *Done* (4833358): the gnome and goblin wear the MakeHuman head warped, through the module
     the orc's tool now shares (`tools/head_warp.py`).
   - Next, choose between authored faces through `PartKind::Sculpt` and a richer procedural face.
     On a sculpted head, `Kind(Face)` covers only the nostrils. A `Face` region over the sculpt's
     front would help either way.
2. **Bodies.**
   - Per-limb muscle and tendon stamp tables, tuned on the reference meshes in `research_notes/`.
   - Real hands.
3. **Skin shading.** A subsurface wrap for ears and fingers, and SSAO if the Deck numbers allow
   it.
4. **Clothing.**
   - Skin hidden under opaque garments (the orc's loincloth).
   - Garments in contacts.
   - Heads and hoods mapped, now that heads have an axis.
5. **Wings.**
   - `WingCoat` colours.
   - The propatagium and uropatagium.
   - Flight as locomotion. Flapping cycles are already in the library.

## Phase 4: front ends

- The in-game creator `interchange.md` names as next: a constrained front end over the same
  files.
- Interchange v2:
  - edits to the reference skeleton;
  - sculpted heads and digit tips exported with their meshes;
  - garments of more than one piece.
- Genetics off its `traits` branch once that lands on master.

## Why this order

- Phases 0 and 1 are short and unblock the creature passes.
- Phase 2 comes before Phase 3 because a game depends on the crate now. Realism that's too slow
  on its hardware gets cut later anyway.
- If a showcase comes first instead, Phases 2 and 3 swap. Their items stand either way.

## Checking each step

- **Benchmarks:** look and perf changes follow `perf-and-look-plan.md`. Each one gets a
  before/after `bench -- compare` and a row in `bench/LOG.md`.
- **Tests:** `cargo test --lib -j 4` on tir, or release on toth for the sculpted-head tests.
- **Creatures:** headless viewer contact sheets before and after.
- **Phase 2:** a domain-expance build against the tag, and the Deck bench.
