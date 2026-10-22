# Anatomy reference: real proportions for the sample body plans

Numbers the sample plans in `assets/mobs/` are built from. Proportions are fractions of one
reference size per body plan, so a plan can be scaled without losing them.

## How the plan format expresses them

| Real feature | Plan field |
|---|---|
| Unequal limb bones (thigh vs shin, forearm vs cannon) | `chain.ratios` (relative segment lengths) |
| Elbow back, knee forward, hock back | `Limb.joints`: a `JointPlan` (`Foreleg`, `Hindleg`, `Leg`, `Arm`, `Arched`) and each hinge's flexion |
| Deep narrow chest vs wide flat chest | `cross: Ellipse(aspect)` (width = r·√aspect, depth = r/√aspect) |
| Waist tuck-up, broad shoulders | `radius.curve` along the torso |
| Hip joints closer than the hip's outer width | `offset.translate` (attach frame: +Y out of the skin) |
| Insect legs on the thorax only | thorax = root `Torso`, abdomen = `Chain` at its `Start` |

Each plan's hinges know which way they close and how far they turn (`JointPlan::hinges`):
flexion is positive the way the joint bends, and a rest flexion outside the hinge's range does
not build. The IK pole (which way a joint bends while walking) comes from the first hinge, and
with the `joint-limits` feature every declared joint is held within its range in motion. A limb
without `joints` falls back to raw `bends` (`(forward, up)` degrees) or the stance's defaults.

## Biped (human), reference size: stature H

Segment lengths as fractions of stature (Drillis & Contini 1966, as reprinted in Winter,
*Biomechanics and Motor Control of Human Movement*):

| Landmark / segment | × H |
|---|---|
| Shoulder (acromion) height | 0.818 |
| Hip joint height | 0.530 |
| Knee height | 0.285 |
| Ankle height | 0.039 |
| Thigh (hip → knee) | 0.245 |
| Shank (knee → ankle) | 0.246 |
| Foot length | 0.152 |
| Upper arm | 0.186 |
| Forearm | 0.146 |
| Hand | 0.108 |
| Shoulder width | 0.259 |
| Hip width | 0.191 |

Other canon:
- Head height ≈ H / 7.5 (≈ 0.13 H). Head breadth ≈ 0.09 H, depth ≈ 0.11 H.
- Thigh and shank are nearly equal. The forearm is ~0.78 × the upper arm. Fingertips hang at
  mid-thigh.
- The torso is ~1.5× wider than deep, and widest at the shoulders and hips. The waist is
  ~0.8 × the chest.
- The hip joints sit ~0.1 H apart, inside the hip's outer width (the rest is the greater
  trochanter and muscle).

Applied (`biped.body.ron`, H = 1.6 m):
- Legs 0.80 m, ratios `[1, 1]`, knee bent ~28° at rest.
- Arms 0.535 m, ratios `[1, 0.78]`, palm 0.09.
- Torso 0.52 m, aspect 1.5, with a waist.
- Neck 0.1 m. Head ~0.2 m tall.

## Quadruped carnivore (dog), reference size: withers height W

From conformation standards and veterinary anatomy (medium breeds):
- **Body.** Length (point of shoulder → point of buttock) ≈ 1.1 W. The topline is level.
- **Chest.** Depth ≈ 0.5 W, so the sternum sits level with the elbow. The chest is deep and
  narrow (width ≈ 0.6–0.7 × depth). The loin "tucks up" behind the ribs.
- **Forelimb.**
  - The scapula lies inside the body.
  - The humerus (≈ 0.3 W) runs down and **back**, so the elbow points backward.
  - The radius/ulna (≈ the humerus) is vertical.
  - The metacarpus (pastern, ≈ 0.4 × radius) leans slightly forward. The animal stands on
    its toes.
- **Hindlimb.**
  - The femur (≈ 0.35 W) runs down and **forward**, so the stifle (knee) points forward.
  - The tibia (≈ 1.1 × femur) runs down and back.
  - The metatarsus (hock → paw, ≈ 0.5 × femur) is near vertical. The hock points backward.
- **Neck, head and tail.**
  - The neck ≈ 0.4 W, carried ~35–45° above horizontal.
  - The head ≈ 0.4 W long, half of it muzzle (mesocephalic).
  - The tail ≈ 0.5–0.7 W.

Applied (`quadruped.body.ron`, `beast.body.ron`, W ≈ 0.6 m):
- Fore: shoulder swung back 30°, elbow flexed 35°, carpus over-extended 10–14°; ratios
  `[1, 1.05, 0.4]`.
- Hind: hip swung forward 30°, stifle flexed 70°, hock flexed 45°; ratios `[1, 1.1, 0.55]`.

## Quadruped ungulate (horse), reference size: withers height W

From equine conformation guides:
- **"Square" horse.** Height at the withers ≈ body length (shoulder → buttock). The
  height/length index is well proportioned at 95–105.
- **Depth and length.**
  - Chest (heart girth) depth ≈ the length of the leg from elbow to ground ≈ 0.5 W.
  - The neck ≈ 1/3 of the body length, and ≈ the length of the front leg.
  - The back ≈ 1/3 of the body length.
- **Legs.**
  - The forearm is **longer** than the cannon. The front cannon is shorter than the hind
    cannon.
  - The pastern slopes ~45–55° and stands on a hoof (unguligrade: on the tip of one digit).
- **Head.** ≈ 0.4 W, long and narrow.

Applied (`horse.body.ron`, W ≈ 1.5 m):
- Fore: four segments (humerus, forearm, cannon, pastern), ratios `[0.8, 1.0, 0.62, 0.28]`.
- Hind: ratios `[1.0, 1.1, 0.8, 0.28]`.

## Insect (hexapod), reference size: body length L

From insect morphology:
- **Body.** Three tagmata.
  - Head ≈ 0.15 L, with antennae.
  - **Thorax** ≈ 0.3 L: pro-, meso- and metathorax. **All six legs attach here**, one pair
    per segment.
  - **Abdomen** ≈ 0.55 L: legless and segmented, often wider and flatter than the thorax.
- **Legs.**
  - Segments are coxa, trochanter, femur, tibia and tarsus (3–5 tarsomeres).
  - The femur and tibia are the longest. The tibia is at least as long as the femur.
  - The tarsus lies along the ground.
- **Posture.** The femur rises above the body and the tibia drops to the ground, so the
  "knees" are high.
- **Leg directions.** Fore legs reach forward, mid legs out sideways, hind legs back. In
  walkers like cockroaches the hind legs are the longest.

Applied (`hexapod.body.ron`, L ≈ 0.6 m):
- Leg lengths: fore 0.30, mid 0.36, hind 0.44.
- Ratios `[1, 1.15, 0.75]` (femur, tibia, tarsus).
- Yaw: fore +40°, mid 0°, hind −40°.

## Sources

- Drillis R., Contini R., Bluestein M. (1964/66), *Body segment parameters: a survey of
  measurement techniques*: [O&P Library](https://the.oandplibrary.org/items/show/179693).
  The fraction table is as reprinted in Winter's *Biomechanics and Motor Control of Human
  Movement*. The primary table could not be fetched to re-check.
- Use and limits of these ratios: [Wisner et al., upper body segment lengths in
  children](https://ojs.ub.uni-konstanz.de/cpa/article/view/292/249).
- Proportionality constants, discussion: [Penn State
  OpenLab](https://www.openlab.psu.edu/2018/02/05/proportionality-constants).
- Horse conformation:
  - [UMN Extension](https://extension.umn.edu/horse-care-and-management/conformation-horse)
  - [Rutgers judging worksheet](https://esc.rutgers.edu/wp-content/uploads/2017/07/conformation-worksheet.pdf)
  - [American Farriers, form to function](https://americanfarriers.com/articles/8770-conformation-form-to-function)
  - [ANCCE breed standard (height/length index)](https://www.ancce.es/en/content/conformational-characteristics)
- Cursorial limb ratios (distal elongation, metatarsal/femur):
  - [Garland & Janis 1993](https://biologyfiles.ucr.edu/people/faculty/Garland/GarlandJanis1993.pdf)
  - [Grossi et al. 2019](https://repositorio.uchile.cl/handle/2250/172394)
- Insect legs:
  - [NDSU lab handout](https://ndsu.edu/faculty/rider/Pentatomoidea/Teaching%20Structure/Handouts/Laboratory%20Week%2007a%20Legs.pdf)
  - [Diptera morphology, legs](https://www.giand.it/diptera/morphology/legs)
- Dog limb proportions follow standard veterinary anatomy and conformation canon. The search
  found no single measured table for dog forelimb segments: Myers & Steudel (1997) measured
  dog fore/hind limbs, but their segment-level numbers weren't available.
