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

## Second pass (2026-10-27): faces, hair, fur at a distance, skin shading

Shot the same way (viewer, headless), judged on contact sheets of the humanoids and the animals.
All additive again; plans and coats that don't use the new fields build as before, except that
every face now has lids and a mouth line, and skin shades differently everywhere.

- **Eyelids** (`FaceParams::eyelids`, default on). A thin skin shell round each eyeball with an
  almond opening cut through its front; the eye slot sits 0.15 radii back so the cornea stands
  about level with brow and cheek. The lids blend over 6 mm: a wide blend swells them clear of
  the ball and the eye sinks into a hole.
- **Face regions.** A face is built in `Face` (it was `Snout`, so `Kind(Face)` matched nothing),
  with `Brow` (one ridge over each eye), `Eyelid` and `Lip` for coats to dress. Coats grow
  eyebrows and lashes as short fur and paint lips by region. A thin cut along the lip line gives
  the mouth its seam; the lips stand 5 mm proud of the face instead of 13.
- **Region ownership.** A vertex is owned by the nearest union primitive *whose blend reaches
  the surface there*. By raw distance the lips owned a band 72 mm wide round 41 mm lips, since
  the skin beside a mouth stands clear of the broad midface it blends from.
- **Eyes.** Iris crypts and fibres, a collarette ring, a duller sclera toward the corners. A
  split-mesh bug put the skin's coat weights in an eye's UV0 whenever the coat had coverings, so
  every eye under a furry coat was one iris-coloured ball; fixed (`split_meshes` now sets the
  part's submesh), with a test.
- **Fur per covering, and by facing.** Each fur covering gives its regions their own strand
  length (`Coat::cover_at`; density, thickness, tip and shells still come from the first).
  `Covering::facing` narrows a covering like a marking's facing mask, so a hairline is fur on
  the skull itself. Shells and feather cards grow from the baked per-vertex coverage. Hair,
  beards, manes, tail switches and eyebrows are fur now.
- **Fur shells by distance.** Strands gather into clumps as their cells shrink toward a pixel
  (cells double level by level, each fading into the next with the clumps' area kept, until a
  clump spans about four pixels), and past the point where clumps would be wider than the fur
  is long, or the coat is under a dozen pixels deep, the shells settle into a pelt one shell
  thick in the fur's average colour. The salt-and-pepper that a bay's summer coat showed from
  two metres is gone.
- **Skin shading.** The baked cavity term reaches the shader at last (`COAT_DATA`,
  `paint_io.wesl`), joined by a body-scale `shade` probe (about a fourteenth of the body long)
  in the byte curvature never used. Both scale Bevy's diffuse and specular occlusion and darken
  the albedo a little (occlusion only touches ambient light); creases flush. Bare skin reflects
  at F0 0.03 and its roughness is mottled over a few millimetres close up.
- **Stage.** The examples light with a warm sun, a cool fill from the sun's other side, ambient
  450 and an overcast backdrop: eyes under lids were holes with one shadowed sun.

Things tried and dropped: a screen-space-derivative normal grain for pores (sandpaper at any
amplitude that showed; roughness mottle instead) and long manes as shells (a 7 cm mane on
8 shells is a brush of layers; 3.5 cm for now, cards later).

## Third pass (2026-10-29): dragon, wyvern, unicorn, zebra, orc

Plans and coats only, in three rounds of shots (viewer, headless): no engine changes.

| Creature | Plan | Coat |
|---|---|---|
| Red dragon | `mobs/dragon.body.ron` (reworked) | `coats/dragon_red.coat.ron` |
| Wyvern | `mobs/wyvern.body.ron` (reworked) | `coats/wyvern_green.coat.ron` |
| Unicorn | `mobs/unicorn.body.ron` | `coats/unicorn.coat.ron` |
| Plains zebra | `mobs/zebra.body.ron` | `coats/zebra.coat.ron` |
| Orc | `mobs/orc.body.ron` (tusks, crest) | `coats/orc.coat.ron`, `outfits/orc.outfit.ron` |

- **Dragon:** a deep-chested body on shorter, clawed legs, an S-curved neck, horns swept back
  over cheek horns and fangs, spines down neck, back and tail, a spade at the tail's tip, wings
  half raised (`fold: 0.42`). Coat: crimson scales at 3.4 cm (the lizard's 1.4 cm glittered
  at a distance), a dark back, pale-gold belly scutes as bands under a facing mask, bone-tipped
  horns and claws.
- **Wyvern:** pterosaur wings half spread (`fold: 0.35`), theropod legs with three clawed toes, a
  barbed stinger. Coat: bottle green, dark back, banded tail.
- **Unicorn:** the horse on Arabian lines (dished face, arched neck, bigger eyes) with a spiral
  horn, a rigid mesh from `parts/horns.gltf` (`tools/gen_horns.py`): `HardKind::Horn` is too
  thick for it (base radius 0.22 × length) and has no ridges. Mane, forelock and tail are hair
  cards at 9000 per m² (2600 left gaps).
- **Zebra:** a stockier, shorter-legged horse, upright mane, big ears, a tufted tail. Stripes are
  `bands` markings per part: upright on the torso, rings on legs, neck and face.
- **Orc:** horn-shaped tusks (the `Tusk` kind is thinner than it reads), a stiff black crest
  (fur shells at 4 cm look right for a mohawk), heavier brow, nose and jaw; a hide loincloth,
  belt, bracers and shin wraps.

Shots: `target/shots/fantasy/` (`before_after_*.png`, `new_horses.png`).

What the round found, for the engine:

1. **Fur shells smeared markings** (fixed, 3d62404). The shells grew on
   `simplify(body, 0.2, 0.01)`, which ignored the paint, so a furry zebra's stripes came out as
   grey blotches. The shell source now simplifies with the paint as attributes
   (`simplify_keeping`), and the zebra wears its fur again.
2. **Hero meshes lost fine markings on flat skin** (fixed, aaf5519). The hero level simplified
   to half without the markings, leaving flanks in triangles several centimetres across. The
   `keep` hook runs at every level now; hero triangle counts are unchanged and generation is
   5 to 9 % slower (bench gen on tir).
3. **Bands followed each vertex's bone** (fixed, 03a0053). Skin bound to a leg's first bone took
   the leg's rings, and a belly bound to one bone took one band as a black patch.
   `Bands::axis` counts bands over the body's extent along a model-space direction, and
   `Bands::warp` makes them waver; the zebra's stripes run on over shoulders and haunch.
4. **Budgets.** A dragon with clawed toes, spines and horns hits both caps at 64: bones
   (two-segment toes were 16 too many) and regions (every hard part is a region; the spine series
   had to shrink). Hard parts don't need a region each.
5. **Hard-part shapes are fixed ratios.** Horns, tusks and spikes take one size; their base
   radius is a fixed fraction of it. A slender horn needs an authored mesh.

## What still limits realism, most to least

1. **Faces are a dozen ellipsoids and cones.** Lids, brows, lashes and a mouth line are in, but
   there is no nasolabial fold, no philtrum, no ear detail (ears are flaps), the nose is a cone
   and two wings, and the lips are two ellipsoids. The woman, man and thick woman wear the
   sculpted MakeHuman head, and the orc a warp of it (`tools/gen_head_orc.py`: displacement
   fields round the landmarks, which then come out for the plan); the gnome and goblin are
   still procedural and could be warped the same way. Two ways forward for the rest:
   - An authored face: `PartKind::Sculpt` already blends a glTF as an SDF. A base-mesh face
     could be added that way, with the procedural levers kept for proportions.
   - A richer procedural face, with eyelid shells, a philtrum, a nasolabial crease and
     modelled ear cartilage.
2. **Long hair.** Short hair, beards and eyebrows work as fur shells now, but shells can't do
   a mane, a ponytail or anything past a few centimetres: the layers show. Next step: hair
   cards grown from a mass, the way feathers are cards grown from skin, with anisotropic
   shading. Strand tips still speckle against the sky at close range (the top shells thin to
   points).
3. **The skin shading is still simple.** Cavity and shade darken creases and hollows, but
   there's no subsurface scattering (ears and fingers don't glow), no pores or wrinkles that
   survive a close-up, and markings place colour without texture. Screen-space AO would add
   depth the baked terms miss at joints in motion; it has a frame-time cost to measure on the
   Steam Deck first.
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
