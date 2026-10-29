# Interchange: creatures, parts and clothes from artists' own tools

Status: **built on branch `interchange`** (2026-10-29), behind the `interchange` cargo feature:
body plans, authored parts and garments round-trip through glTF and Blender. The in-game creator
(a constrained front end over the same files) is next, as its own plan.

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
  on save from the DCC), and ornisculpter's `convert` writes RON from it.
- The format, the importer, `export_body` and the loader are ornisculpt's (`interchange`
  feature): a game loads `.body.glb` files with them. The tools that use them (`export`,
  `convert`, `templates`, the Blender add-on and its checks) live in **ornisculpter**, the
  authoring app.
- Units are metres, Y up, +Z forward: glTF's own convention, which Blender's importer and exporter
  convert to and from its Z up.

## `*.body.glb`

The scene has three kinds of node.

1. **The root** `ornisculpt`: `extras.ornisculpt = { format: 1, tone, coat? }`. The plan's body
   tone, and the coat when one was exported with the body (so coats round-trip too).
2. **Part nodes**, one empty per *authored* part, nested as the part tree, named by the part's
   name. Each carries `extras.ornisculpt.part`: the `PartNode` without `children` (its kind and
   params, `attach`, `offset`, `mirror`, `repeat`, `detail`, `tone`, `stamps`, `density`), and
   `extras.ornisculpt.frame`: the node's model-space transform as exported (a column-major 4×4),
   and `extras.ornisculpt.order`: its place among its siblings (DCCs reorder them, Blender by
   name, and the build order, so bone order and the body's cache key, depends on it).
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
- Offsets recomputed for a moved part (and its children) are rounded to a micrometre and a
  ten-thousandth of a degree, so float noise from the DCC doesn't show in the plan.
- Not yet (v2): edits to the reference skeleton's bones (segment lengths into limb and chain
  ratios, bone rolls into rest angles).

### Checked with Blender

ornisculpter's `tools/blender/round_trip.py` imports a `.body.glb` into Blender 5.2 headless, optionally moves a
part, and exports it again; `convert` turns the result back into RON. With nothing moved the
woman, and the beast with its sculpted feet and mesh crest, come back identical. With her arm moved 2 cm up, only the arm changes: its `Surface` attach
slides up the torso (`t` 0.9 → 0.94) and its offset takes up the rest (3.5 → 4.2 cm out).

```text
blender -b --python tools/blender/round_trip.py -- woman.body.glb out.body.glb move:arm:0.02
ornisculpter convert out.body.glb /tmp/woman.body.ron
```

## Parts with an authored mesh

A sculpt or a mesh hard part is a **mesh node**: the part node carries its mesh, scaled by the
part's `scale`, in the frame the part builds it in (the attach frame times the offset, or just the
attach position for `Orient::Model`). The artist edits that mesh, or models a new part on the
reference body.

- **Unchanged** (its geometry's fingerprint is still the exported `mesh_print`: distinct positions
  to 10 µm and the triangle count, so a DCC re-ordering or splitting vertices doesn't count), the
  part keeps its mesh path exactly.
- **Edited or new**, the mesh is taken from the file (all its triangle primitives), welded for a
  sculpt (its SDF needs a closed surface), and referred to as `{mesh file}#Mesh{n}/Primitive0`
  with `scale: 1`. The loader serves those as labelled meshes of the `.body.glb` itself;
  `convert` writes them to `<plan>.parts.gltf` beside the RON.
- A new part is a mesh node nested under the part it grows from, with
  `extras.ornisculpt.part = { kind: { Sculpt: {} }, attach: … }` (or `Hard` with `kind: { Mesh: {
  path: "" } }`). It has no frame, so it is placed like a moved part: where its node is.
- A node's scale goes into the part's `scale` (a sculpt's frame scale is replaced by it).

### Names

Blender keeps object names unique across the whole file, so the hind leg's `foot` comes in as
`foot.001`. A node named its part's name plus such a suffix keeps the part's name; any other name
renames the part. A part duplicated in the DCC (Shift+D copies its extras, `order` included) gets
its node's name with `.` made `_` (`horn_001`), since `.1` would read as a copy suffix.

Not yet: heads' sculpts (`Head { sculpt }`) and digit tips export without their meshes.

## Garments

Export fits each garment of an outfit on the **first part it wraps** (its logical copy), in the
rest pose, with hanging cloth straight down from its edge, and writes it as a mesh node under the
root carrying `extras.ornisculpt.garment` (the `Garment`), its `order` in the outfit and the
fitted geometry's `mesh_print`. The artist edits it, or models a new garment on the reference
body in its real shape.

On import:

- **Unchanged** (same fingerprint), a garment comes back exactly as it was, so it still re-fits
  any body, including one whose parts moved in the same file.
- **Edited or new**, each vertex is solved back into wrap space: damped Gauss-Newton for the
  wrap-space point (`y`, angle, `r`) that the fit (`clothing::garment_point`) places where the
  vertex is, started from where the vertex lies along and around the part, and from each span edge
  for cloth hanging past it or capping the part's end. Fitted again on the body it was modelled
  on, every vertex of every sample garment lands within 0.1 mm of where it was (the peasant and
  warrior outfits, hanging skirt and cape included), and the result fits other bodies (the orc).
  A vertex the fit can't reach (inside the body) lands at the nearest place it can, with a
  warning past 5 mm.
- `on` and `span`, when not given, come from where the garment lies: the part most vertices are
  nearest (as a pattern over its copies, `torso/arm`), and the stretch of it they cover.
  `ease`, `reach`, `hang`, `color` and `roughness` come from the extras, with `Garment`'s
  defaults.
- The loader serves the outfit as `<file>.body.glb#outfit`, its new meshes as labelled meshes;
  `convert` writes `<plan>.outfit.ron` and the meshes to `<plan>.parts.gltf`.

Checked through Blender: the woman in the peasant outfit comes back identical, plan and outfit;
with the tunic loosened 5% (`scale:tunic:1.05`), only the tunic changes, to a new wrap-space mesh.

Not yet: a garment brought back keeps only its first piece's shape (a sleeve modelled on the left
arm is mirrored onto the right, as wrap space always does).

## Blender add-on

Optional: plain glTF with hand-typed custom properties works the same. ornisculpter's
`tools/blender/ornisculpt/`
installs as an add-on (zip the folder; it carries a `blender_manifest.toml` and `bl_info`). In
the Object properties it adds:

- **Add Part**: a part from a template under the active part, with what hangs off it (an arm with
  its fingers), named apart from its siblings. The templates (`templates.json`) are one part of
  each kind, role, hard kind or shape lifted from the sample plans, written by `ornisculpter
  templates`; run it after changing the part types or the sample plans.
- **Mark as Part** (Sculpt or Mesh): the selected meshes become parts modelled in place, nested
  under the active part.
- **Mark as Garment**: the selected meshes become garments; with a part active they wrap it,
  otherwise their part and span come from where they lie.

In the Scene properties it sets the `.body.glb` to export to, and **Export on Save** writes it on
every save of the `.blend`, so a running `editor` or `viewer` on that file hot-reloads.

ornisculpter's `tools/blender/test_addon.py` checks it headless: on the exported woman it adds a spike from a
template, marks an ico sphere on her back as a sculpt and a cylinder round her waist as a garment,
and exports; `convert` then reads back the spike, the sculpt (its mesh from the file) and the
garment (on `torso`, its span inferred).

A node may also sit outside the `ornisculpt` root (Blender keeps new objects at the top of the
scene): it is placed relative to the root all the same.

## Artist checklist

- [ ] Export a body to model against: `ornisculpter export mobs/woman.body.ron out.body.glb [coat|-] [outfit]`.
- [ ] Import it (Blender: File → Import → glTF; custom properties come in as the node's properties).
- [ ] Move, rotate or scale part empties to place parts; edit their `ornisculpt` properties to
      change them; duplicate an empty to add a part; delete one to remove it.
- [ ] Model parts as meshes nested under the part they grow from, garments on the body in their
      real shape; mark them with `ornisculpt` properties (copy them from an exported one).
- [ ] Leave the reference skeleton and skin alone: they are rebuilt from the plan.
- [ ] Export as glTF binary with custom properties included (Blender: *Include → Custom
      Properties*), as `name.body.glb` under `assets/`.
