# Contacts: limbs that press instead of passing through

Status: **implemented** (`src/surface/contact.rs`, `COAT_CONTACT` in `coat/deform.wesl`, the
`rest_self_intersection` viability check; posing and clearing in ornisculpter's editor).

Limbs are meshed apart, so an arm hanging by a thigh never webs onto it (`surface::body`'s
sheets). The cost: where a pose brings one into another, the two surfaces just cross. The orc
stood with his forearms ten centimetres inside his thighs, fingers poking out of them as lumps.

## Proxies

`ContactSet::new(anatomy)` stands each group in for with rounded cones, in the rest (bind) pose:

- **Groups.** 0 is the body (every primitive not on a limb), then each limb meshed apart in part
  order (`contact_limbs`), up to 16 in all. Hands, toes and hooves count with their limb.
- **Shapes.** From the field's own base-layer primitives: round cones as they are, spheres,
  ellipsoids and rounded boxes as capsules along their longest axis. An elliptical section (a
  torso wider than deep) keeps its aspect as a `squash` across one axis. Grids (sculpted heads),
  stamps and anything under 8 mm (fingers) are left out; the 96 largest are kept.
- **Fitted.** Bare cones sit inside the real surface (blends, muscle and fat stamps stand out of
  them), so each grows or shrinks by the median distance from points on it to its group's sheet.

## Masks

Each hero skin vertex carries the groups it may be pushed out of, in the high half of
`ATTRIBUTE_MOB_DATA`'s second word (`contact_masks`, baked in `mesh_lod` at hero detail): every
group but its own, and but the body and any limb that attaches where the vertex lies (a limb's
root passes into the body there by design; a hip mustn't be pushed out of its own thigh). A mask
of 0 leaves the vertex alone: claws, eyes, wing sheets, garments and crowd meshes.

## The shader

`deform.wesl`'s `pressed`, after skinning, hero tiers only (skin, fur shells and hair cards): the
proxies are posed by their bones' joint matrices; a vertex inside the deepest one in its mask
moves half the depth out, 1.5 mm short of meeting (two coincident surfaces fight for depth), and
its normal turns to face the other surface. Both sides do the same, so they meet and press flat.
`ORNISCULPT_NO_CONTACT=1` turns it off, for comparing.

Moving skin only works for shallow contacts: thighs pressed together, an arm brushing the flank, a
swing in a walk. Pushed out of a deep one, a forearm flattens into a sheet lying on the thigh.

## Deep contacts are poses to fix

- `ContactSet::contacts(anatomy, pose)` measures, per pair of groups, how deep points on one
  group's proxies lie in another's (the same distances the shader uses), deepest first.
- The `rest_self_intersection` viability check warns of any deeper than 2 cm in the idle pose.
- `clear_plan` turns limbs out: the deepest contact first, its limb's `splay_deg` or joints'
  `swing` by 4, 8 or 16 degrees, whichever helps most, until it is under 1.2 cm or stops helping.
  An arm moves rather than the leg it lies in, and a leg only for the body or another leg.

## In the editor (ornisculpter)

- Contacts are marked on the preview, red where deep enough to clear and orange where the skin
  presses them flat, and listed under the status line.
- Ctrl+drag a limb poses it. On its first bone, sideways swings it and up and down splays it; on a
  later bone, up and down bends that joint.
- `C` runs `clear_plan` as one undo step and says what it changed.

## Costs

- Generation: one more stage (`contacts`), a millisecond or two.
- Editor: the proxies and their contacts add 3 to 7 ms to a preview build (about 40 ms).
- Hero vertex shader: a loop over up to 96 proxies per vertex, most skipped by a bounding-sphere
  test. Not measured on the Steam Deck yet.

## Limits

- Crowd tiers don't press: their meshes carry no masks, and the loop would cost them dearly.
- Proxies are approximate. A contact between parts their cones miss (a palm's box, a sculpted
  head) goes unseen, and the fit is about a centimetre.
- Garments don't press yet: the orc's loincloth still shows skin through it where his thighs push
  through it, which is the garment fit, not contacts.
