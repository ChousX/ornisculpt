# Roadmap

Status: **proposal** (2026-10-31, at 3735bf1). The order work should go in from here, drawn from
the limits the design docs record as open.

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
   - `types.md` has grown a long list of v2 notes. They should be folded into its sections.
   - There is no README. It needs the module map, the feature flags, the consumers, and links to
     these docs.
2. **Bad assets fail fast.**
   - A RON parse error leaves a headless shot waiting out its timeout (`realism.md`, limit 8). It
     should exit with the error through `BodyFailed`.
   - A marking or covering `Part(...)` selector that matches nothing should warn at load. Today
     renaming a part breaks its coat silently.
3. **A release point.** A `v0.2.0` tag on main, so the consumers can name a known-good rev.

## Phase 1: the fantasy pass's engine limits

1. **Bone and region caps.**
   - The caps are `MAX_BONES` and `Region::MAX`, 64 each (`anatomy/skeleton.rs`,
     `anatomy/region.rs`).
   - Hard parts stop taking a region each. They share their part's region and carry a hard-part
     index. Most of the dragon's region budget goes back.
   - Then either raise the bone cap or fold two-segment toes into one bone. Raising it means
     checking the crowd's 32-bone skeleton and the preview shader.
2. **Hard-part shapes.**
   - Horns, tusks and spikes take a base-radius ratio, taper, curl and ridges, not a fixed
     fraction of their size.
   - The unicorn's spiral horn and the orc's tusks would no longer need authored meshes.
   - These extend the one table of horn and plate sizes that the mesh and the preview share
     (fe4a031).
3. **Mirrored copies that differ** (a lobster's crusher and cutter).
   - `BodyPlan::expand_symmetry` already makes each copy its own node, and a suffixed
     `FormPatch` reaches one copy only.
   - Plans need per-side overrides on a mirrored part built on that.
4. **Later:** keeping more than 12 limbs apart (`LIMB_SLOTS`, `surface/body.rs` and
   `sdf_field.wesl`), when a centipede needs it.

Field and primitive changes carry over to ornisculpter's preview shaders (`--features preview`).

## Phase 2: game-readiness

domain-expance holds every creature at `ForceTier(Hero)`, so that feet follow the ground. That
gives up the crowd tiers, which are where the frame budget goes furthest.

1. **Crowd tiers on uneven ground.** Baked clips assume flat ground. A per-mob root tilt and
   height from the game's `FootGround` probes, plus a cheap foot offset in the blend, would let
   distant creatures leave the hero tier.
2. **Crowd baking of graph cycles** (`motion-api.md` §7, build step 5).
   - Crowd tiers bake only `Gait` modes and the idle (`bake_motion`).
   - Bake the idles and actions a body's graph uses, and blend up to three masked layers in the
     compute pass, so crowd mobs can play idles and actions too.
   - Add an `eat` action to `motion::library` for grazing.
3. **Measure on the target device.** Done on 2026-10-31: the bench v5 baseline for the Steam
   Deck is in `bench/LOG.md`, and `tools/deck-bench.sh` reruns it. What it found:
   - generation is about 5 times tir's, with centres of rotation and the head's SDF
     conversion furthest behind, so the crowd's cold start takes two minutes;
   - fur close-ups are over a 60 fps budget.
   Those two come before more shader work. The contacts loop and SSAO are still to measure.
4. **Crowd cost.**
   - Merge the cloth submesh into the skin mesh. A dressed mob costs an extra draw entity (23 to
     27 ms against 15 ms plain).
   - Find the near tier's remaining per-mob cost (`bench/LOG.md`, v5 RenderDoc notes).
5. **Long sessions.**
   - Free a body's space in the crowd clip atlas when the body goes.
   - Keep a disk cache of generated bodies by `BodyKey`. A crowd's cold start (20 to 30 s for
     4000) and the first head SDF (about 1.5 s) are then paid once per install.
6. **Motion gaps:** `Reach` arm IK, fingers closing on a grip, `BoneScales` and custom gaits in
   crowd tiers, and crowd shadows. The game's needs set the order.

## Phase 3: realism, in `realism.md`'s order

1. **Faces.**
   - Warp the MakeHuman head for the gnome and goblin, as `tools/gen_head_orc.py` did for the orc.
   - Then choose between authored faces through `PartKind::Sculpt` and a richer procedural face.
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
