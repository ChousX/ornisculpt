# Wings (proposal)

Status: **implemented** (`src/anatomy/wing.rs`; sheets in `surface::lod`; shading `COAT_WING` in
`coat_fragment.wesl`). Samples: `dragon` (bat-style, 3 fingers), `wyvern` (pterosaur, 1 finger),
`bat`, `bird` (slotted feathered), `beetle` (elytra + hindwings), `dragonfly` (two spread pairs).
Wings come before the motion API, which will drive them through
a `Wings` target (`Fold`, `Spread`, `Flap`, `Sweep`, `Twist`).

Four kinds, from one part type:

| Kind | Real model | Structure | Surface |
|---|---|---|---|
| **Dragon** | bat / pterosaur | arm + 1–4 long fingers, membrane between them back to the body | scaled leading edge, leathery veined membrane |
| **Bat** | bat | arm + fingers III–V spread, thumb claw free | thin translucent membrane, fine fur at the root |
| **Bird** | bird | humerus → ulna → fused hand | primaries, secondaries, tertials and coverts as layered feather cards |
| **Insect** | dragonfly / beetle / bee | no internal bones: a rigid blade on a hinge, 1–2 pairs | veined, translucent, iridescent; optional hardened elytra |

## 1. Anatomy (research)

- **Bat** ([Wikipedia: bat flight](https://en.wikipedia.org/wiki/Bat_flight),
  [Sears et al. 2006](https://pmc.ncbi.nlm.nih.gov/articles/PMC1458926),
  [bat digit morphometrics](https://zoolstud.sinica.edu.tw/Journals/59/59-60.pdf)).
  - Digits III–V carry the wing. Digit III sets the span (hand-wing length) and digit V sets
    the width.
  - The forearm is the size proxy. Aspect ratios run 5–11: open-air fliers are long and
    narrow, gleaners short and broad.
  - Membrane panels:
    - propatagium: in front of the arm;
    - dactylopatagium: between the fingers;
    - plagiopatagium: from the fifth finger back to the body and hind leg;
    - uropatagium: between the legs and tail.
  - The thumb stays free, with a claw.
- **Pterosaur → dragon** ([Elgin et al.](https://bioone.org/journals/acta-palaeontologica-polonica/volume-56/issue-1/app.2009.0145/The-Extent-of-the-Pterosaur-Flight-Membrane/10.4202/app.2009.0145.full),
  [uropatagium](https://www.scielo.br/j/aabc/a/f9YWHKz4HN7BVchqN9fvFtf/?lang=en)).
  - One enormous fourth finger is the spar. The main membrane (brachiopatagium) runs from its
    tip back to the hind limb.
  - A wrist bone, the pteroid, supports a front membrane.
  - Radiating fibres (actinofibrils) stiffen the outer membrane.
  - Classic fantasy dragons are bat-style (3–4 fingers). Pterosaur-style is the 1-finger case
    of the same structure.
- **Bird** ([bird skeleton](https://www.visualdictionaryonline.com/animal-kingdom/birds/bird/skeleton-bird.php),
  [bird anatomy](https://www.batrachos.com/en/avesanatomy/)).
  - Bones: humerus → radius and ulna (the ulna is longer and thicker) → fused carpometacarpus
    with 2–3 reduced digits.
  - Flight feathers:
    - ~10 **primaries** on the hand, longest at the tip;
    - 10–20 **secondaries** on the ulna;
    - **tertials** near the humerus;
    - rows of **coverts** overlapping their bases.
  - Folding is a Z: elbow and wrist close together, so the primaries slide under the
    secondaries.
- **Insect** ([insect wings](https://brisbaneinsects.com/brisbane_insects/InsectWings.htm),
  [ladybird wing folding](https://pmc.ncbi.nlm.nih.gov/articles/PMC5465895),
  [beetle wing review](https://dx.doi.org/10.3390/ijms15046009)).
  - Wings have no bones. Longitudinal veins (costa, subcosta, radius, media, cubitus, anal)
    radiate from the hinge, joined by cross-veins.
  - Beetles have hardened forewings (elytra) covering fan-folded hindwings. Hemiptera have half-
    hardened forewings.
  - At rest the wings fold back over the abdomen.

## 2. Part type

```rust
PartKind::Wing(WingParams)

pub struct WingParams {
    pub kind: WingKind,
    /// Tip-to-root length of one wing when spread, metres.
    pub span: f32,
    /// Width at the root (chord), metres.
    pub chord: f32,
    /// Rest pose: 0 = spread, 1 = fully folded. The motion API overrides it later.
    pub fold: Bounded<0.0, 1.0>,
    /// Angle back from straight out, degrees, when spread (swept vs straight wings).
    pub sweep_deg: f32,
}

pub enum WingKind {
    /// Bat and dragon: arm + `fingers` (1 = pterosaur) carrying a membrane. The membrane's
    /// trailing edge runs from the last finger tip back to `trailing` and scallops between
    /// finger tips by `scallop`.
    Membrane { fingers: Bounded<1, 4>, trailing: Trailing, scallop: f32, thumb: bool,
               surface: MembraneSurface /* Leather | Thin */ },
    /// Bird: arm bones + feather cards generated in anatomical rows.
    Feathered { primaries: u8, secondaries: u8, covert_rows: u8, shape: WingShape },
    /// Insect: a rigid blade on a hinge bone; `pair` 0 = fore, 1 = hind (attach two parts).
    Blade { planform: Planform, veins: u8, elytron: bool },
}

pub enum Trailing { Body(f32 /* torso t */), HindLeg, Tail }
pub enum WingShape { Elliptical, HighSpeed, HighAspect, Slotted }  // songbird, falcon, albatross, eagle
pub enum Planform { Long, Broad, Round }                           // dragonfly, butterfly-ish, bee
```

**Attachment.** Wings attach at a torso `Surface` slot on the dorsal-lateral shoulder,
mirrored. A dragon is six-limbed: four legs plus two wings on the same torso. Insects attach
to the thorax: forewings at t≈0.75 and hindwings at t≈0.45.

## 3. Bones and folding

| Kind | Bones per wing | Crowd skeleton |
|---|---|---|
| Membrane (3 fingers) | humerus, forearm, wrist + 3 × 2 finger bones = 9 | arm 3 kept; fingers fold into the wrist |
| Bird | humerus, ulna, hand = 3 | all 3 kept |
| Insect blade | 1 hinge | kept |

- **Bind pose:** spread. Geometry is generated flat and spread, which is simple and exact.
- **Rest pose:** comes from `fold`, applied as joint rotations in the pose solver and the
  crowd bake:
  - bird: a Z-fold;
  - membrane: the arm tucks and the fingers close together, so the membrane bunches between
    them;
  - insect: the hinge rotates back over the abdomen.
- **Bone budget:** a dragon has 4 legs ×3, torso 4, neck 4, head 2, tail 6 and wings 2 × 9,
  for **46 hero** bones (cap 64) and **32 crowd** bones (fingers folded).

## 4. Geometry

- **Membrane wings:**
  - Arm and finger bones are thin SDF tubes in the body field, blended at the shoulder, with
    knuckle bumps from tone landmarks.
  - The **membrane** is a generated sheet, not an SDF: an SDF can't mesh 2 mm membranes at a
    1–5 cm voxel. The sheet is triangulated between the finger chains and the trailing
    attachment.
  - Scallops are a catenary sag between finger tips.
  - Skin weights are interpolated across the sheet from the two nearest bones, so folding
    bunches it naturally.
  - The sheet uses a new `MaterialSlot::Membrane` and is double-sided.
- **Bird:**
  - Arm bones are an SDF tube, with the covert mass thickening the leading edge.
  - Flight feathers are **long cards**, one per feather, built like today's feather cards
    but placed anatomically:
    - primaries fan from the hand, lengths following `WingShape`;
    - secondaries line the ulna;
    - coverts are shorter rows on top.
  - Each card is skinned rigidly to its bone, so the Z-fold slides the primaries under the
    secondaries.
- **Insect:** a planform mesh (a few dozen triangles) on the hinge bone. Veins live in the
  shader, not the mesh.
- **LOD:**
  - Near: membrane sheets simplified; bird primaries merged into a single wing-outline card per
    tract; insect wings unchanged.
  - Far: one outline mesh per wing (membrane or feather tract), opaque.
  - Folded wings at Far are mostly hidden against the body anyway.

## 5. Shading

New coat-shader layers (`COAT_MEMBRANE`, `COAT_FLIGHT_FEATHER`, `COAT_INSECT_WING`) on the
existing `MobMaterial`, with colours from `Coat` plus a new `WingCoat`.

- **Membranes (bat, dragon):** thin-surface translucency.
  - **Backlighting.** Bevy's `diffuse_transmission` is a flipped Lambertian lobe made for thin
    surfaces like leaves and paper; it's cheap and needs no extra pass
    ([Bevy `StandardMaterial`](https://doc.qu1x.dev/bevy_trackball/bevy/pbr/struct.StandardMaterial.html),
    [transmission example](https://bevy.org/examples/3d-rendering/transmission/)). Set
    ≈0.4–0.6 so the sun glows through the wing.
  - **Fast translucency look.** On top, the Frostbite "fast translucency" term: a view-
    dependent `pow(saturate(dot(V, -(L + N·distortion))), power) · scale`, scaled by a
    thickness factor. It gives the warm glow at grazing angles, with distortion and power as
    material constants
    ([Barré-Brisebois & Bouchard, GDC 2011](https://frostbite.com/frostbite/news/approximating-translucency-for-a-fast-cheap-and-convincing-subsurface-scattering-look),
    [Zucconi's breakdown](https://www.alanzucconi.com/2017/08/30/fast-subsurface-scattering-1/)).
    Thickness is baked per vertex: thick near bones and the leading edge, thin mid-panel. So
    the bones show as dark ribs against the glow, the signature membrane look.
  - **Veins:** a procedural pattern branching from the finger bones, in panel coordinates
    baked on the sheet: darker, thicker, less transmissive.
  - **Dragon:** coat **scales** (the existing cellular shader) on the arm, finger tubes and a
    band along the leading edge. The membrane itself gets a pebbled leather bump and
    radiating actinofibril striations, which fade by pixel footprint like the coat patterns.
- **Bird flight feathers:**
  - **Anisotropic** specular along the barbs, using Bevy's anisotropy with the tangent along
    the vane, since the parallel barbs make highlights stretch and shift
    ([feather CGI patent](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/7427991)).
  - **Sheen** lobe for the soft rim
    ([real-time feather appearance model](https://findanexpert.unimelb.edu.au/scholarlywork/2331275-a-real-time--multiscale-and-procedural-feather-appearance-model)).
  - **Translucency** through the vane when backlit, using the same diffuse transmission.
  - Patterns: tip and bar bands, a dark shaft (rachis), and a slightly separated barb edge;
    alpha-tested at Hero, opaque outline at Far.
- **Insect wings:**
  - Veins in the shader: longitudinal veins from the hinge plus a cross-vein cell network
    (cellular noise, as for scales).
  - **Thin-film iridescence:** hue shifting with view angle and film thickness, in the
    spirit of the Belcour–Barla model, which handles iridescence consistently in RGB
    real-time engines
    ([paper](https://blog.unity.com/es/technology/a-practical-extension-to-microfacet-theory-for-the-modeling-of-varying-iridescence),
    [real-time spatially varying thickness](https://studenttheses.uu.nl/handle/20.500.12932/15359)).
    The cheap form is the standard thin-film approximation: phase from `thickness · cosθ`
    mapped through an RGB interference palette, with thickness varying across cells.
  - Transparency (see question 2).
  - Elytra: an opaque, hard, glossy shell (clearcoat) with the coat's colour and pattern.

## Questions

1. **Flight.** This pass builds wings and their fold/spread poses only. Flapping, gliding and
   flight locomotion (3D movement, takeoff, landing, hover) belong to the motion API. Agreed?
2. **Insect wing transparency.** Real alpha blending needs sorting and costs overdraw. The
   alternative is alpha-to-coverage or dithered at Hero and tinted-opaque at Near/Far.
3. **Dragon default.** Bat-style 3 fingers, or pterosaur 1 finger? Both come from
   `fingers: 1..=4`, so this is only the sample.

## Implementation notes (what changed from the proposal)

- **Decisions:** poses now and flight later; dithered insect wings at Hero and opaque at
  Near/Far; both dragon styles shipped as samples.
- **`WingKind` variants are newtypes** (`Membrane((fingers: 3, ..))`), so their fields can
  default, like other part params.
- **Membrane proportions:**
  - Bat/dragon: humerus 0.16, forearm 0.27, metacarpal 0.05 of the span, with fingers
    0.55 / 0.45 / 0.37 / 0.29 fanning up to 110° back.
  - Pterosaur: humerus 0.14, forearm 0.22, metacarpal 0.12, and a single 0.52 finger.
- **Membrane geometry:**
  - Panels are ruled grids between finger polylines, with catenary-like scallops between
    the tips and a `billow` (camber) out of the wing plane between spars.
  - The main membrane (plagiopatagium) runs between arm + last finger and body + trailing
    edge.
  - Skin weights: each edge segment is carried by a bone (arm, finger) or is a free span
    (the body line, the trailing edge) blended from the bone at its start to the one at its
    end. A vertex blends its row's carriers on both edges by its place across, up to four
    bones. Weights ease into the next bone over the last 20 % of a bone, so the membrane
    bends round a joint. (The trailing edge used to ride the last finger bone right up to
    the body, so a folded wing stood up as a sail.)
  - The main membrane's weights are then **fitted to its folded shape**: the same ruled
    panel between the folded arm + finger and the body line, bowed out from the flank. Per
    vertex, the weights over the panel's bones (arm, last finger, body) that carry it nearest
    its folded place, kept near the weights above (projected gradient on the simplex, at most
    four bones). No bone carries a membrane's middle: weighted to the body it stays where the
    spread wing held it; weighted to the wing it swings up over the back as the plane turns
    down the flank. Linear blending is exact at the bind pose whatever the weights, so the
    spread wing is unchanged.
- **Feather tracts.** Rigid feathers on folding arm bones stood up like a display fan, so
  flight feathers now ride on tract bones:
  - secondaries + tertials on the forearm;
  - primaries split into inner and outer tracts, blended by fan position.
  - Folding turns the tracts so the rows lie back along the flank and the primary fan
    collapses to near-parallel, as a real folded wing does.
- **Folding:**
  - Membrane wings are posed by joint angles (`WingPose`: shoulder `sweep` and `raise`,
    `elbow`, `wrist`, finger `spread` and knuckle `curl`), one set `open` (the bind pose)
    and one `closed`, both editable per plan. Elbow, wrist and knuckles hinge across the wing
    plane, as a bat's do (the fingers close like umbrella ribs under the forearm); the
    shoulder turns the plane, down the flank when closed. A single-axis hinge slerps exactly
    as its angle lerps, so a half fold is half-way at every joint. Earlier folded frames were
    free directions, whose slerp swung the fingers down through the ground mid-fold.
  - `WingRig` stores folded *local* rotations, relative to the folded parent.
  - The pose solver slerps rest → folded by `fold`, and the crowd bake's idle frame does the
    same, so distant mobs show folded wings.
- **Shading (`COAT_WING`, kind in vertex colour w):**
  - **Membrane:** branching veins, radiating fibres, pebbled leather, ribs darker and
    thicker. `diffuse_transmission` (on the Hero material) is varied by thickness, and the
    Frostbite fast-translucency term is added after lighting.
  - **Flight feather:** asymmetric vane (narrow leading side), shaft, barbs, tip band and a
    Kajiya–Kay sheen.
  - **Insect:** longitudinal veins, cross-vein cells, costa and pterostigma, thin-film
    iridescence, 4×4 Bayer dithering at Hero.
  - **Elytron:** glossy punctured shell.
- **Bone counts:**
  - dragon 46 hero / 32 crowd;
  - bird 39 / 27;
  - bat 36 / 32;
  - beetle 35 / 32.
- **Not yet:**
  - wing-specific coat colours (`WingCoat`; wings use the coat's base / secondary / accent);
  - membrane panels in front of the arm (propatagium) and between the legs (uropatagium);
  - hindwing fan-folding under elytra (beetle hindwings simply fold back);
  - Near/Far simplification of sheets (they're cheap: a few hundred vertices per wing);
  - flapping and flight (motion API).
