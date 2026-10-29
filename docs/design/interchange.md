# Interchange: creatures, parts and clothes from artists' own tools

Status: **proposal, being built on branch `interchange`** (2026-10-29). Behind the `interchange`
cargo feature.

Artists work in the tools they prefer (Blender, Maya, ZBrush and a DCC for retopology) and bring
the result into the game. The rule: **everything a player can make in the game must also be
possible in those standalone tools.** The in-game creator, when it comes, is one more front end
over the same files, never a place with powers of its own.

## How others do it

- **Dreams** (Media Molecule) built its whole campaign with the exact tools players got
  ([PS Blog](https://blog.playstation.com/?p=201280),
  [Game Informer](https://gameinformer.com/feature/2019/04/10/charting-the-frontiers-of-imagination)).
- **Factorio**'s base game is itself a mod on the public API ([wiki](https://wiki.factorio.com/Mods)).
- **The Sims** CAS: artists author parts and morphs in their DCC, players compose within them.

The common rule: one data format, no back door. Here the format is glTF 2.0 with Ornisculpt's data
in `extras`, which every DCC that reads glTF keeps as custom properties.

## The contract

- A creature is still a procedural **body plan** (`BodyPlan`). A DCC edits it; it doesn't replace
  it with a free-form rig. Motion, LOD, crowds, coats and clothes keep working on every import.
- **Round trip is the test.** Every sample plan, exported and imported unchanged, must come back
  equal (`round_trip_every_sample_plan`). Anything in a plan is therefore reachable from a DCC.
- RON stays the canonical text form. `*.body.glb` loads directly as a `BodyPlan` (hot reload works
  on save from the DCC), and the `convert` example writes RON from it and back.
- Units are metres, Y up, +Z forward: glTF's own convention, which Blender's importer and exporter
  convert to and from its Z up.

## `*.body.glb`

The scene has three kinds of node.

1. **The root** `ornisculpt`: `extras.ornisculpt = { format: 1, tone, coat? }`. The plan's body
   tone, and the coat when one was exported with the body (so coats round-trip too).
2. **Part nodes**, one empty per *authored* part, nested as the part tree, named by the part's
   name. Each carries `extras.ornisculpt.part`: the `PartNode` without `children` (its kind and
   params, `attach`, `offset`, `mirror`, `repeat`, `detail`, `tone`, `stamps`, `density`), and
   `extras.ornisculpt.frame`: the node's model-space transform as exported (a column-major 4×4).
   The node sits at the part's **frame**: its attach point on the parent, times its offset. That
   is the frame every part kind builds from (`at.model * cx.offset`).
   - Empties, not bones: not every part has a bone of its own (a hard part rides on its parent's),
     and an empty moves the same way in every DCC; bones in Blender only move in edit mode, and
     its importer re-orients them.
   - Mirrored and repeated parts export their first copy only (`.l`, `.0`): the logical one. The
     reference mesh shows the others.
3. **Reference content**, marked `extras.ornisculpt.reference = true` and ignored on import: the
   skeleton (one joint node per bone, rest pose) and the hero-LOD skin, skinned to it. Artists
   model clothes and parts against it, and can pose it to check them.

`part` may also be a string, JSON or RON (as in a `.body.ron`), for tools whose custom
properties only hold text. Export drops `null`s (an absent `Option` reads back as `None`), since
some DCCs can't store them.

### Import

- The part tree is the nesting of nodes with `extras.ornisculpt.part`. Other nodes are walked
  through (so the artist may group parts under empties of their own) and ignored.
- A node whose model-space transform still equals its `frame` keeps its `attach` and `offset`
  exactly. That is what makes the round trip exact.
- A **moved** node keeps its attach slot, and its offset absorbs the move:
  `offset = attach⁻¹ × node`, with `attach` taken from the anatomy as rebuilt so far (parents
  first, since moving a parent moves its children's attach points). For an `Along` or `Surface`
  attach the part first slides: the node's position is projected onto the parent's axis for a new
  `t` (and `around_deg` for `Surface`), so a part dragged over the skin stays on it with a small
  offset. A `Series` slides its `from` and `to` together, as the editor does.
- Renaming a node renames the part. A node deleted removes the part, a node duplicated (with its
  extras) adds one: Blender's Shift+D on an empty with custom properties is "add a part".
- Not yet (v2): edits to the reference skeleton's bones (segment lengths into limb and chain
  ratios, bone rolls into rest angles).

## Parts (phase 3)

A mesh node with `extras.ornisculpt.part` whose `kind` is `Sculpt` or `Hard` with `Mesh`, and no
mesh path, is an **authored part modelled in place** on the reference body.

- Its parent is the part node it is nested under, or else the part owning the body surface
  nearest the mesh's centroid.
- Its attach follows the import rules above, from the mesh node's transform.
- Its mesh is baked into the authored frame (origin at the attach point, `Orient::Slot` or
  `Model`), welded at UV seams and hard edges (a sculpt's SDF sign needs a closed surface), and
  written into the plan's parts file as `#MeshN/Primitive0`.
- `blend`, `resolution`, `region`, `own_bone` and `orient` come from the extras, with the
  defaults of `SculptParams` when absent.

## Garments (phase 4)

A mesh node with `extras.ornisculpt.garment` is a garment **modelled on the reference body in its
real shape**. Import is the inverse of the fit (`clothing::fit_outfit`). For each vertex:

- the nearest axis sample of the garment's `on` part gives `t`, normalised into its span for `y`;
- the part's dorsal and side directions give the angle around it (mirrored on mirrored parts);
- the distance above the body's surface gives `r - 1`.

`on` and `span` are inferred when absent (the part most vertices are nearest, and the range of `t`
they cover). Vertices past the span become the hang or the end cap, as `y < 0` does in wrap space.
`ease`, `reach`, `hang`, `color` and `roughness` come from the extras. The result is a wrap-space
mesh and an `Outfit` entry. Wrap space is relative to the body, so a garment modelled on the woman
fits the orc and the thick woman too.

## Blender add-on (phase 5)

Optional: plain glTF with hand-typed custom properties works the same. The add-on
(`tools/blender/ornisculpt/`) generates its panels from a JSON schema of the Rust types (the
`schema` example dumps it from reflection), and adds *Mark as part*, *Mark as garment*, *New part
here* and export on save, so a running `editor` or `viewer` hot-reloads each save.

## Artist checklist

- [ ] Export a body to model against: `cargo run --release --example export --features interchange -- mobs/woman.body.ron out.body.glb`.
- [ ] Import it (Blender: File → Import → glTF; custom properties come in as the node's properties).
- [ ] Move, rotate or scale part empties to place parts; edit their `ornisculpt` properties to
      change them; duplicate an empty to add a part; delete one to remove it.
- [ ] Leave the reference skeleton and skin alone: they are rebuilt from the plan.
- [ ] Export as glTF binary with custom properties included (Blender: *Include → Custom
      Properties*), as `name.body.glb` under `assets/`.
