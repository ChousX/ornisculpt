# Realism pass: ten creatures, and what limits the modelling

Status: **done on branch `realistic-models`** (2026-10-26). Ten body plans with coats:
ox, horse, man, woman, orc, gnome, goblin, spider, dragonfly (green darner) and lobster. Each was
shot headless with the viewer (`ORNISCULPT_SCREENSHOT`, `ORNISCULPT_CAM`), and every limit below
was found in those shots.

| Creature | Plan | Coat |
|---|---|---|
| Ox (Hereford) | `mobs/ox.body.ron` | `coats/hereford.coat.ron` |
| Horse (bay) | `mobs/horse.body.ron` (reworked) | `coats/bay.coat.ron` |
| Man | `mobs/man.body.ron` | `coats/skin_fair.coat.ron` |
| Woman | `mobs/woman.body.ron` | `coats/skin_woman.coat.ron` |
| Orc | `mobs/orc.body.ron` | `coats/orc.coat.ron` |
| Gnome | `mobs/gnome.body.ron` | `coats/gnome.coat.ron` |
| Goblin | `mobs/goblin.body.ron` | `coats/goblin.coat.ron` |
| Wolf spider | `mobs/spider.body.ron` | `coats/wolf_spider.coat.ron` |
| Dragonfly (green darner) | `mobs/darner.body.ron` | `coats/green_darner.coat.ron` |
| American lobster | `mobs/lobster.body.ron` | `coats/lobster.coat.ron` |

```text
cargo run --release --example viewer -- mobs/ox.body.ron coats/hereford.coat.ron
```

The proportions and their sources are in [`anatomy-reference.md`](anatomy-reference.md).

## What the first shots showed

The existing plans were shot as a baseline: horse, quadruped, biped, curvy, hexapod, beetle and
dragonfly. The anatomy was mostly right. What made them read as clay models was mostly *not* the
body shape:

1. **One colour per body.** The skin was `coat.base` everywhere. A horse had no dark points, no
   dark hooves and no dark muzzle. A cow couldn't have a white face. Plates, claws, horns and teeth
   all shared one ivory, and every eye had the same brown iris. Colour carries more
   identity than shape: a Hereford is its white face, a bay its black legs.
2. **Ends of limbs were blobs.** `LimbEnd::Pincer` was an ellipsoid with no fingers.
   `Hoof { split }` ignored `split`. `Foot` was a flat round box, like a flipper.
3. **Arthropod legs were smooth tubes.** Nothing marked the joints, so legs read as noodles
   rather than an exoskeleton.
4. **Human faces had a hole under each cheekbone.** Nothing filled the face between the
   cheekbones and the jaw (the buccal fat and the masseter), so every face looked gaunt and
   bruised.
5. **No hair.** A horse had no mane or tail hair, and humans were bald.
6. **Hard caps that break realistic bodies:**
   - The hero mesh's 200 k triangle budget is fixed. Voxels scale only linearly with body
     size, so an ox (230 k) failed to generate at all.
   - The field keeps at most 8 limbs apart at a point (`LIMB_SLOTS`). A lobster or a spider has
     11, and the extras melted together.

## What changed

All of these are additive. A coat without markings, or a plan that doesn't use the new fields,
builds exactly as before, except for the foot shape and the two caps.

- **Coat markings** (`src/coat/markings.rs`, `Coat::markings`). Colour laid over the skin and
  hard parts, baked per vertex at generation time into a new `ATTRIBUTE_PAINT` (linear RGB plus
  coverage). It reaches the fragment stage through `paint_io.wesl`. Each marking is a colour
  and a region selector (`Part(...)` paths included), narrowed by any mix of these masks:
  - `facing`: which way the rest-pose surface faces (pale belly, dark back, hairline).
  - `height`: a band of heights, as fractions of the body's height.
  - `along`: position along the part, root to tip (points, socks, antenna tips).
  - `tip`: toward the far end of the vertex's own bone (muzzle ends, lips).
  - `bands`: rings along the part (banded legs, a ringed abdomen).
  - `patches`: value-noise patches (mottling, piebald, blush).

  Fur and scales follow the paint: the shader keeps the secondary and accent colours at their
  brightness relative to the base. Painted fur shells and feather cards copy the paint.
  The coat also gets `hard` (hard-part colour) and `iris`. Their defaults are the old constants.
- **`LimbEnd::Pincer`** is now a chela: a swollen palm plus the fixed and hinged fingers,
  slightly open.
- **`LimbEnd::Hoof { split: true }`** gives two claws with a cleft over a coronet. Every hoof
  now leans its toe forward (wall at about 50°) and is wider at the sole.
- **`LimbEnd::Foot`** is a heel, a high instep falling to the broad ball, then tapering toes.
  This changes the look of `biped`, `curvy` and `thick` too.
- **`ChainParams::segmented`** (0 to 1). Each segment narrows at its joints and swells between
  them, and the joints barely blend: exoskeleton legs, a ringed tail, jointed antennae.
- **`FaceParams::cheeks`** fills the cheek between cheekbone and jaw. The default `0` keeps
  existing faces as they were.
- **Triangle budget.** `mesh_lod` coarsens the voxel to fit (up to three times; triangles go as
  1/voxel²) instead of failing.
- **`LIMB_SLOTS` 8 → 12** in `surface/body.rs` and `sdf_field.wesl`.

Tests: `coat::markings` covers ramps, bands, patch coverage, and a bake of the darner's coat on
its body (blue abdomen, dark stripe and rings).

## What still limits realism, most to least

1. **Faces are a dozen ellipsoids and cones.** The cheeks are fixed, but the midface and mouth
   still push forward like a short muzzle. There are no eyelids, no nasolabial fold, no
   philtrum, no ear detail (ears are flaps), and the lips are two ellipsoids. This is the
   biggest gap for the five humanoids. Two ways forward:
   - An authored face: `PartKind::Sculpt` already blends a glTF as an SDF. A base-mesh face
     could be added that way, with the procedural levers kept for proportions.
   - A richer procedural face, with eyelid shells, a philtrum, a nasolabial crease and
     modelled ear cartilage.
2. **Hair and other long coverings.**
   - Fur is shells with one length per coat (`Coat::fur` takes the first fur covering), so a
     mane, a tail switch, a beard and the short body coat can't differ.
   - Here manes, beards and hair are sculpted masses with paint, which reads like a statue.
     The woman's ponytail is a chain.
   - Next step: per-covering fur parameters, or hair cards grown from a mass, the way feathers
     are cards grown from skin.
3. **The skin shading is flat.**
   - Each vertex already stores a cavity term (`MobVertexData::cavity`), but nothing uses it.
     Darkening creases and armpits with it would cost nothing.
   - There's no subsurface scattering on skin (ears and fingers don't glow), and no
     micro-normal detail (pores, wrinkles).
   - Markings place colour; they don't add texture.
4. **Bodies are chains of round cones.**
   - Torsos and limbs read as mannequins: muscle stamps only raise the surface, and there are
     no tendons, sharp creases or bony edges at the knee, shin or wrist.
   - Hands are a box with tube fingers.
   - The humanoids would gain most from per-limb stamp tables tuned on reference meshes
     (`research_notes/` has female base meshes).
5. **Mirrored parts are identical.** A lobster's crusher and cutter claws differ, but `mirror`
   copies one part exactly. Plans can't vary a mirrored copy, short of authoring both sides
   apart (`Side::Center` on the right needs `around_deg` 270 and isn't tested).
6. **Exoskeletons only approximate.** `segmented` gives rings and joints, but there are no
   overlapping tergites, carapace grooves, spines or setae. A body with more than 12 limbs meshed
   apart (a centipede) will merge again.
7. **Rest poses are authored as angles.** Arms hang in a slight A-pose, and relaxed hands,
   head carriage and weight shift all have to be found by hand. Turning the palms in needs an
   arm offset of 75° about Y.
8. **Authoring friction.**
   - Plans are deep RON. A missing paren fails the asset load, and a headless screenshot run
     then waits out its timeout instead of exiting.
   - Marking `Part(...)` paths break silently when a part is renamed.

## How the shots were taken

Batches of `name|plan|coat|yaw,pitch,dist,x,y,z` lines, one viewer run per shot, about 4 s each
on tir:

```text
ORNISCULPT_SCREENSHOT=out.png ORNISCULPT_GIZMOS=0 ORNISCULPT_CAM=0.85,0.15,5,0,0.8,0 \
  BEVY_ASSET_ROOT=$PWD ./target/release/examples/viewer mobs/ox.body.ron coats/hereford.coat.ron
```
