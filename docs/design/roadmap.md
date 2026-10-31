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
   - `types.md` has grown a long list of v2 notes. They should be folded into its sections.
     Still open.
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
   - Still open: `LimbEnd::Pincer` has no shape parameters, so the cutter differs only in size.
     The crusher's molars could now be blunt with `HardShape`.
4. **Later:** keeping more than 12 limbs apart (`LIMB_SLOTS`, `surface/body.rs` and
   `sdf_field.wesl`), when a centipede needs it.

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
7. **Bugs the work above turned up.**
   - The crowd's cold-start median rose 13 % with the crowd layers (822df11), and the steady
     frame didn't move. The cause isn't known; a Tracy trace of the cold phase is next.
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
