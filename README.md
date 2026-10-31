# ornisculpt

Procedural creatures for [Bevy](https://bevy.org) 0.20. A creature is a *body plan* (a tree of
parts: torso, limbs, head, tail, wings, horns) that the crate turns into a skeleton, a signed
distance field, skinned meshes at three levels of detail, a coat (colours, markings, scales, fur,
feathers, hair cards), clothing and equipment, and procedural motion that walks it on its own
feet. Plans, coats, outfits and gaits are RON files; a game spawns a mob from a plan handle and
gets a generated body a few frames later, shared by every mob with the same plan and form.

```rust
commands.spawn((Mob::new(asset_server.load("mobs/ox.body.ron")), MobCoat(asset_server.load("coats/hereford.coat.ron"))));
```

The sample plans in `assets/mobs` go from a beetle and a dragonfly through an ox, a horse, a
spider and a man to a dragon.

## Module map

| Module | What it holds |
| --- | --- |
| `anatomy` | Body plans and their parts, individual `Form` (levers such as girth and limb length), symmetry expansion, the skeleton and regions the rest of the crate addresses, viability checks, and genomes (`genetics` feature). |
| `surface` | The body field (primitives and combinators), the mesher, per-LOD settings, centres of rotation, muscle flex, contacts, and the GPU field preview (`preview` feature, `*.wesl`). |
| `coat` | Coats: colours, markings, coverings, fur shells, feather and hair cards, and the skin material and its shaders. |
| `clothing` | Garments wrapped onto body regions, outfits, and the skin they hide. |
| `equip` | Tools held in a mob's sockets. |
| `motion` | Locomotion, gaits, foot planting and IK, cycles, the motion graph and the built-in cycle library, hand poses, and motion baked for crowds. |
| `mob` | The ECS side: spawning, async generation and its cache, LOD tiers, crowd skinning on the GPU, runtime levers. |
| `soft` | Soft tissue on springs (`soft-tissue` feature). |
| `interchange` | Plans, parts and garments to and from glTF (`interchange` feature). |
| `ron_loader` | The one asset loader behind every `*.ron` asset. |
| `num`, `random` | Checked numeric conversions, and the deterministic noise generation uses. |

## Cargo features

All off by default.

| Feature | Adds |
| --- | --- |
| `joint-limits` | Motion holds every joint a body declares limits for within them, as the last pose stage. |
| `soft-tissue` | Fat pads and muscle bellies that jiggle, sag and tense; hero mobs run the springs, crowd tiers play them baked. |
| `genetics` | Creatures as DNA (`anatomy::genome`): body, coat and form in one genome, through the `genetics` crate. |
| `preview` | The live GPU preview of a body while it is edited (`surface::SdfPreview`); for authoring tools, games leave it off. |
| `interchange` | Body plans, authored parts and garments to and from glTF, so artists can work in their own tools. |
| `web` | The browser build (`wasm32-unknown-unknown`, WebGPU); built and served by `tools/web.sh`. |

## Examples

All take `--release`; a debug build is slow to generate bodies. Every example that shows a window
also runs headless: set `ORNISCULPT_SCREENSHOT=out.png` and it renders to an image, saves it, and
exits. `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` places the camera. A plan, coat or outfit that
fails to load exits the headless run non-zero with the error.

| Example | Run | Shows |
| --- | --- | --- |
| `viewer` | `cargo run --release --example viewer [mobs/plan.body.ron] [coats/look.coat.ron] [outfits/wear.outfit.ron]` | One mob, with LOD, tone and rig debugging; hot-reloads its assets. |
| `walk` | `cargo run --release --example walk` | Several bodies walking in their coats. |
| `motion` | `cargo run --release --example motion` | The motion API: every body on the standard graph. |
| `hands` | `cargo run --release --example hands` | Hand poses on a standing woman. |
| `hold` | `cargo run --release --example hold [count]` | Held tools in every socket, at hero and crowd tiers. |
| `mobs` | `cargo run --release --example mobs [count]` | Shared bodies, tiers by on-screen size, runtime levers. |
| `crowd` | `cargo run --release --example crowd [count]` | Thousands of wandering mobs across several plans. |
| `fields` | `cargo run --release --example fields [mobs/plan.body.ron]` | The field combinators and the sweep SDF, voxelised. |
| `viability` | `cargo run --release --example viability` | The built-in checks over every sample plan, headless. |
| `bench` | `cargo run --release --example bench -- <gen\|hero\|motion\|crowd\|all> [--out DIR]` | The fixed benchmark; results are logged in `bench/LOG.md`. |

Tests: `cargo test --release --lib`. Lints are strict (clippy pedantic and nursery, no `unwrap`,
`expect`, indexing, panics or `as`), so run `cargo clippy --release --all-targets` with each
feature a change touches.

## Used by

- ornisculpter: the editor, which authors plans, coats
  and outfits over the `preview` and `interchange` features.
- domain-expance: a game of grazing creatures bred from genomes (`genetics`).
- ornithochory: plants that bend round mobs.

All three follow `main`.

## Design docs

The design lives in `docs/design/`:

- [types.md](docs/design/types.md): the types and traits, the crate's reference.
- [roadmap.md](docs/design/roadmap.md): the order work goes in from here.
- [realism.md](docs/design/realism.md): the creature passes, and the engine limits they found.
- [perf-and-look-plan.md](docs/design/perf-and-look-plan.md): the benchmark and the look work.
- [motion-api.md](docs/design/motion-api.md): the motion authoring API.
- [contacts.md](docs/design/contacts.md): limbs that press instead of passing through.
- [wings.md](docs/design/wings.md): wings.
- [authored-parts-and-clothes.md](docs/design/authored-parts-and-clothes.md): hand-authored
  parts and garments.
- [interchange.md](docs/design/interchange.md): glTF in and out.
- [anatomy-reference.md](docs/design/anatomy-reference.md): real proportions behind the sample
  plans.
