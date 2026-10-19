# Ornisculpt: requirements for hand-authored parts and clothes

Research only. No files were edited and no builds were run. File:line references point at the working tree of `ornisculpt` (`main`) and may move as the other agents edit.

## Summary

- **Hand-authored parts already work, in two forms.**
  - `PartKind::Sculpt`: a watertight glTF mesh is turned into an SDF grid and smooth-blended into the body field, then meshed and skinned like the rest of the body.
  - `HardKind::Mesh`: a glTF mesh rigidly attached to one bone with 100% weight.
  - Only `assets/mobs/beast.body.ron` uses them (its feet and crest). Neither form supports colours or textures, and neither has LOD variants.
- **Clothes are mostly built already.**
  - `src/clothing/` has an `Outfit` asset of garments authored in "wrap space". Each vertex is fitted by marching out to the real body SDF.
  - Loose cloth hangs on spring-bone chains. Each LOD gets its own cloth mesh, and crowd tiers are remapped to the crowd skeleton.
  - Sample outfits: `assets/outfits/{peasant,warrior,harness}.outfit.ron`.
- **What clothes are missing:** skin and fur are not hidden under cloth, there are no head or hand garments, layering is set only by `ease`, cloth has no textures, spring chains ignore gravity and the body, and cloth costs one extra draw per crowd mob.

---

## A. Hand-authored parts

### A1. Declaring, loading and attaching
- **Sculpt:** `kind: Sculpt((mesh: "parts/x.gltf#Mesh0/Primitive0", scale, blend, resolution, orient, region, own_bone))`, defined at `src/anatomy/part.rs:41-68`. Defaults: `scale 1`, `blend 0.02`, `resolution 40`, `Orient::Model`, `RegionKind::Mass`, `own_bone true`.
- **Rigid:** `kind: Hard((kind: Mesh(path, orient: Model|Slot, scale)))`, defined at `part.rs:458-466`. A rigid mesh can also be a digit tip through `Digits.tip` (`build.rs:124-141`).
- **Loading:**
  - `referenced_assets` (`build.rs:124`) collects the mesh paths.
  - `PartAssetCache::resolve` (`src/mob/generate.rs:58-76`) loads them through the Bevy `AssetServer`.
  - `extract_trimesh` (`generate.rs:79-95`) keeps only positions, normals and indices.
  - Bodies are cached by a content hash (`BodyKey`) and meshes hot-reload.
- **Frame:** metres, Y up, +Z forward, origin at the attach point (`external.rs:7-8`). glTF node transforms are ignored, so bake them into the mesh.
- **Orient** (`parts.rs:745-750`): `Model` keeps the authored axes; `Slot` aligns +Y with the slot's outward direction and +Z with its forward.
- **Bone:**
  - Hard binds to the parent's bone.
  - Sculpt gets its own `sculpt` bone when `own_bone` is set (`parts.rs:752-815`).
  - A Sculpt exposes slots, so children and garments can attach to it.
- **Mirroring:** automatic for both kinds (`builder.rs:409-414`, `surface/hard.rs:53-68`).
- **Scaling:** only the uniform `Scale` lever affects an authored part (`lever.rs:339,342`), plus `Blend` for Sculpt. **Authored parts do not follow the parent's girth, length, muscle or fat.**

### A2. How a part interacts with the body
- **Sculpt is unioned into the body SDF:**
  - It is added as `PrimShape::Grid` with a `Union` op and a smooth-min `blend` (`parts.rs:790-798`), then skinned automatically with no authored weights.
  - The mesh must be watertight and consistently wound (`external.rs:88-119`).
  - Detail is limited to about `extent / resolution`.
  - `types.md:31` still says the sign comes from a raycast; the code now uses normals, so that doc line is stale.
- **Hard is a separate rigid mesh:**
  - It uses fixed vertex data and `MaterialSlot::Hard` (`hard.rs:110-130`).
  - If the glTF has no normals, every normal becomes +Y, which shades flat and wrong.
- **LOD:**
  - Sculpt is re-meshed per LOD with the body, so small detail is lost at Near and Far.
  - Hard meshes show at Hero and Near only, using the same full-resolution mesh at both.
- **Crowds:** both kinds ride on bones that are remapped to the 32-bone crowd skeleton (`build.rs:78-99`).
- **Materials:**
  - All Hard meshes share one fixed colour (`generate.rs:232`).
  - UVs, vertex colours and glTF materials are thrown away.
  - A Sculpt uses the body's coat shading for its declared region.
- **Mass:** a Sculpt counts toward mass (`mass.rs:108-125`); Hard parts don't.

### A3. Bugs and gaps
1. **LOD radius is wrong:**
   - `resolve_lods` uses `params.size`, which defaults to 0.03, for `HardKind::Mesh` instead of the mesh's real bounds (`builder.rs:612-615`).
   - The radius for a Grid ignores its scale (`builder.rs:688-691`).
   - So `detail: Auto` misjudges authored parts.
2. **Dead levers:** `Size`, `Curve` and `Twist` are offered for `Mesh` but do nothing (`lever.rs:157,295`).
3. **Stuck loading:** a non-triangle-list mesh sets `ready = false` forever. The mob stays pending and logs an error every frame (`generate.rs:66-69`).
4. **Possible Sculpt sign errors** (inferred, not tested): `extract_trimesh` doesn't weld vertices split at UV seams or hard edges.
5. **Missing features:** per-part colour or textures, Hard LOD meshes, a motion target for a Sculpt's own bone, and fitting to the parent's girth.

### A4. Artist checklist
- [ ] glTF/glb file with one mesh primitive per part, referenced as `file.gltf#MeshN/PrimitiveM`.
- [ ] Metres, Y up, +Z forward, origin at the joint, transforms applied.
- [ ] Indexed triangle list. Hard meshes need normals.
- [ ] **Sculpt only:**
  - Watertight and manifold, with consistent winding.
  - No UV seams and no flat-shading splits.
  - Overlaps the parent by about `blend`.
  - Features larger than the voxel and grid cell size.
- [ ] Model the left or centre copy and let `mirror: true` make the right one.
- [ ] Set `detail: Always` or `UpTo(..)` explicitly until A3.1 is fixed.
- [ ] References: `tools/gen_sample_parts.py`, `assets/mobs/beast.body.ron`.

---

## B. Clothes

### B1. What exists
- **Outfit asset:** `Outfit { garments }` in `*.outfit.ron`, worn with `MobOutfit(Handle<Outfit>)` (`src/clothing/mod.rs:33-136`). Each garment has `on` (a part path pattern), `span`, `mesh`, `color`, `roughness`, `ease`, `reach` and `hang`.
- **Wrap space:** the mesh is authored around a unit cylinder. `y` 0..1 runs along the span, the angle runs around the part, and `r-1` is metres above the skin.
- **Fitting** (`fit.rs`):
  - Each vertex ray-marches out to the real body SDF.
  - Distance is clamped near neighbouring parts using `reach`, so armholes work.
  - Skin weights are copied from the body surface point the ray hits.
- **Loose cloth:**
  - `Hang` adds spring-bone chains to the skeleton.
  - Hanging vertices are skinned to the two nearest chains.
  - `follow_legs` makes skirts swing with the thighs.
  - The springs are included in the crowd bake.
- **LOD:** Near and Far are decimated to 50% and 25% and pushed out by half a voxel.
- **Shading:** one Cloth submesh with an 8-colour palette and a procedural weave.
- **Cost** (`types.md:18`): 4000 dressed mobs take about 23–27 ms per frame, against about 15 ms undressed.

### B2. Gaps
| Requirement | Status |
|---|---|
| Fit to any body shape | Yes, but only on parts with an axis. Heads, hard parts and digits have none, so no hats, hoods or gloves. |
| Skin weight transfer | Copied from the surface point, with no smoothing. Possible seams at armholes (inferred). |
| No poke-through | Only `ease`. **Skin is never hidden under cloth, and fur and feathers show through** (`coat/geometry.rs:60-64,114-144`). Spring chains have no body collision, and `gravity` is never read (`rig.rs:32`). |
| Layering | Only by giving the outer garment a larger `ease`. |
| Scale | `ease`, hems and hang length are absolute metres, so garments don't scale with the body (inferred to look wrong on very large or small bodies). |
| Hang | Only from one edge. |
| Budget | 64 bones in total, 32 for crowds. Cloth chains compete with tails and wings and become rigid when the budget runs out. At most 8 garments. |
| Materials | Palette colour only, no UVs or textures. |
| Crowd cost | One extra draw per dressed mob. |

### B3. How others do it
- **Spore:** creature skin is an implicit (metaball) surface, with rigid parts blended on ([Hecker](https://www.chrishecker.com/My_Liner_Notes_for_Spore), [janeng](https://janeng.com/?p=143)). This is the same split as Sculpt versus Hard here.
- **The Sims:**
  - Body shapes come from blend-shape morphs, and clothing deforms with the same morphs ([patent](https://patents.justia.com/patent/9626808), [s4s](https://sims4studio.com/thread/10806/)).
  - A per-mesh region map decides which body or clothing pieces are hidden ([s4s](https://sims4studio.com/thread/26392/dress-broken-smoothing-groups-game)).
- **MetaHuman / Unreal:**
  - Outfits get skin weights transferred from the body, with painted masks to control it ([docs](https://dev.epicgames.com/documentation/metahuman/building-a-metahuman-parametric-outfit-with-a-render-mesh)).
  - Mutable clips away body geometry hidden under clothes ([Mutable](https://dev.epicgames.com/documentation/unreal-engine/mutable-overview-in-unreal-engine?lang=en-US)).
  - Robust weight transfer: [Abdrashitov 2023](https://www.dgp.toronto.edu/~rinat/projects/RobustSkinWeightsTransfer).
- **SDF approaches:** Implicit Skinning by Vaillant et al. 2013 ([video](https://vimeo.com/65966757)). TressFX 4 collides hair against SDFs of skinned bodies ([GPUOpen](https://gpuopen.com/tressfx-4-simulation-changes/)).
- **Bevy:** `bevy_silk` is CPU Verlet cloth targeting Bevy 0.17. At most it would suit hero capes, not crowds.

### B4. Recommended, cheapest first
1. **Opaque garments hide what's under them, at build time.** Add a flag per garment and a coverage mask from the fit, then drop skin triangles under the garment and exclude them from fur shells and feather cards. This fixes poke-through, fur showing through and overdraw at once, with no runtime cost.
2. **Layering and scale:**
   - Add a `layer` per garment and fit each layer against the body plus the layers beneath it.
   - Optionally express sizes relative to the part's radius.
   - Give `Head` an axis so hats and hoods can fit.
   - Allow hanging from both edges.
   - Make spring chains use gravity and push out of a few body capsules.
3. **Merge cloth into the skin mesh** to get one draw per crowd mob. Add wrap-space UVs, which come free from the wrap coordinates, plus a texture array. Hold off on real cloth simulation.

### Garment checklist
- [ ] Mesh in wrap space as an indexed triangle list, referenced as `#MeshN/PrimitiveM`. Normals are recomputed, so they don't matter.
- [ ] `on` targets a part with an axis (not a head or a hard part). `span` is a range of the part's `t`.
- [ ] `ease` is larger than that of any garment it sits over. Use a low `reach` for armholes.
- [ ] Hanging garments: vertices on one side only, and `chains × segments` fits the remaining bone budget.
- [ ] At most 8 garments, with palette colours only.

---

## Open questions
1. Should clothing hide the body underneath (removing it at build time) or only push the body inward? Do you need furry mobs that wear clothes?
2. Do garments need textures, or are palette colours plus the weave enough?
3. Do you need hats, hoods, gloves or armour plates?
4. Should authored parts follow the parent's girth, muscle and fat, or is a uniform `Scale` enough?
5. What is the budget for dressed mobs in crowds (draws, bones)?
6. Should outfits change at runtime without regenerating the whole body?

## Recommended next step
Implement B4.1 (opaque garments hide the skin, fur and feathers under them). Then fix the small authored-part bugs:
- the LOD radius (A3.1)
- the dead levers (A3.2)
- the stuck loading (A3.3)
- welding vertices for Sculpts (A3.4)

Do this after the two agents editing the tree have finished.
