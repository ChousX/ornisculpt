# Real-time Fur and Feather Rendering for Close-up / Hero Shots (Bevy 0.20)

Project context (checked locally): `Cargo.toml` pins `bevy = "0.20"`. `src/coat/mod.rs` says "Fur renders as shells: offset copies of the covered skin, alpha-tested by strand noise". The default `FurParams` is `{ length: 0.025, density: 500.0, thickness: 0.7, tip: 0.4, shells: 12 }`. Hero tiers get shells plus feather cards, and crowd tiers shade the coat on the skin only.

Bevy 0.20 source facts below come from the local registry copies (`~/.cargo/registry/src/*/bevy_*-0.20.0`), and the cited URLs point to the same files on docs.rs.

---

## 1. Shell-texturing improvements (LOD, fins, alpha handling, styling, shading)

### Takeaway
Shells by themselves will always show slicing up close, and it is worst at the silhouette and at grazing angles. The classic fix is **shells + silhouette fins** (Lengyel et al. 2001). Then make the cutout stable: **AlphaToCoverage with MSAA** (Bevy supports it, but it is incompatible with Bevy's OIT). Then add cheap strand shading: **Kajiya-Kay/Scheuermann shifted-tangent specular, shell-height AO, gravity/comb offset**. For true hero close-ups, a hybrid works best: shells for the undercoat plus a few explicit guard-hair strands or cards.

### Cited Findings
- Lengyel, Praun, Finkelstein, Hoppe (I3D 2001), "Real-time fur over arbitrary surfaces":
  - Hair is simulated offline with a particle system and stored in a volume texture. Lapped textures map it onto arbitrary topology, which also allows a global direction field on the fur.
  - At runtime the patches render as "a series of concentric shells of semi-transparent medium".
  - Fins are "placed normal to the surface and rendered using conventional 2D texture maps sampled from the volume texture in the direction of hair growth".
  - The method supports local control of hair colour, length and direction.
  - Sources: [Hoppe project page](https://hhoppe.com/proj/fur/), [paper PDF](https://www.hhoppe.com/fur.pdf), [Princeton PIXL](https://pixl.cs.princeton.edu/pubs/Lengyel_2001_RFO/index.php)
- NVIDIA's DX10 SDK "Fur – Shells and Fins" sample adds fins along silhouette edges to fix gaps at grazing angles. Fin generation became more efficient with DX10 geometry shaders. — [NVIDIA SDK doc](https://developer.download.nvidia.com/SDK/10.5/direct3d/Source/Fur/doc/FurShellsAndFins.pdf). I could not extract the PDF's text, so its shell counts and fade formulas are not verified.
- A community shell-fur breakdown (Godot, inspired by Acerola's video), [queenofsquiggles](https://queenofsquiggles.codeberg.page/tech/shell-fur-breakdown/):
  - Shells are offset along the averaged vertex normal and scaled by layer depth.
  - Strand thickness follows a curve along the length (smoothstep-like taper).
  - Each UV grid cell discards pixels outside a circle, and the circle shrinks on outer shells.
  - Motion comes from a lerped, damped physics offset, with a stiffness curve along the length.
  - Under PBR the fur uses the base mesh normals, so it "looks smooth rather than strand-shaded".
  - The author frames the method as meant for objects "not viewed too closely".
  - Shells did not follow the skeleton until the rig was handled.
- Acerola's shell-texturing video started a community "#AcerolaFurryChallenge". Ports exist in Godot ([godotshaders](https://godotshaders.com/shader/fur-grass-with-shell-texturing/)) and Unity ([jvandy](https://jvandy.itch.io/shell-texturing): Perlin noise, half-Lambert, sine wind). The original Unity repo is GarrettGunnell/Shell-Texturing ([fork listing](https://github.com/lcngh)). I did not retrieve the video's own numbers.
- **Bevy 0.20 alpha modes:**
  - `AlphaMode::AlphaToCoverage` "requires MSAA. Without MSAA, it behaves the same as `Mask` with a threshold of 0.5". No sorting is needed, so it is cheaper than Blend for "complex transparency such as foliage". — [Bevy dev docs AlphaMode (0.20.0-dev)](https://dev-docs.bevy.org/bevy/prelude/enum.AlphaMode.html)
  - In 0.20, A2C maps to `MeshPipelineKey::BLEND_ALPHA_TO_COVERAGE` only when `Msaa != Off`, and otherwise to `MAY_DISCARD`. The A2C pipeline uses `blend = None`, `depth_write_enabled = true`, `alpha_to_coverage_enabled = true`, and pushes shader def `ALPHA_TO_COVERAGE`. It goes into the **Opaque** render phase. — [bevy_pbr 0.20 material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/material.rs), [bevy_pbr 0.20 render/mesh.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/mesh.rs)
  - The prepass pipeline hard-codes `alpha_to_coverage_enabled: false`. Prepass and shadows treat `Opaque | AlphaToCoverage | Mask` alike (the shadow key gets `MAY_DISCARD`), so depth, normal and shadow passes see an alpha-tested version of the shells. — [bevy_pbr 0.20 prepass/mod.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/prepass/mod.rs), [render/light.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/light.rs)
- **Bevy 0.20 OIT:**
  - `OrderIndependentTransparencySettings { sorted_fragment_max_count (default 8), fragments_per_pixel_average (default 4.0), alpha_threshold (default 0.0) }`.
  - It is a 2-pass **per-pixel linked list**: a forward pass builds the list, then a fullscreen pass sorts and blends.
  - Supported modes are Blend, Premultiplied and Add. It requires `Material::enable_oit` (also exposed on `MaterialExtension`), and a custom material calls `oit_draw(position, color)` and outputs no colour.
  - `check_msaa` **panics**: "MSAA is not supported when using OrderIndependentTransparency".
  - Older versions (around 0.15) used a fixed `layer_count` instead. ([0.15-era docs](https://doc.qu1x.dev/bevy_trackball/bevy_core_pipeline/oit/struct.OrderIndependentTransparencySettings.html))
  - Source: [bevy_core_pipeline 0.20 oit/mod.rs](https://docs.rs/crate/bevy_core_pipeline/0.20.0/source/src/oit/mod.rs)
- Hybrid fur: animal fur is "very dense and thin undercoat hairs in combination with coarse guard hairs". Andersen et al. pair explicit guard hairs with a volumetric undercoat. — [DTU hybrid fur paper](https://people.compute.dtu.dk/jerf/papers/hybrid_fur.pdf)
- RDR2's "next-gen fur" mod raises fur LOD with "more strands and more height layers". This implies layered (shell-like) fur in the base game, but there is no Rockstar confirmation. — [DSOGaming](https://www.dsogaming.com/?p=185053)
- A Polycount thread speculates that Ghost of Tsushima horse coats are textured or sculpted, with manes and tails as hair cards. This is community speculation only. — [Polycount](https://polycount.com/discussion/comment/2749745/)

### Inferences (implementation notes for ornisculpt)
- **Shell count by distance:**
  - 12 shells over 2.5 cm is fine at mid range. For hero close-ups, scale shell count with *projected fur length in pixels*: aim for at most about 1–2 px between shells, so `shells ≈ clamp(fur_len_px / 1.5, 8, 64)`.
  - Implement this as a few prebuilt LOD shell meshes (for example 8/16/32/48), or as one instanced draw where `instance_index → shell height`. Instancing needs a custom draw or GPU-instancing path; shell meshes per LOD are simplest with ExtendedMaterial.
  - Fade the outermost shells toward the target count to avoid popping.
- **Fins:** Precompute an edge list (surface-nets meshes are manifold-ish). For each edge, emit a quad extruded along the averaged normal and the comb direction.
  - Bevy has no geometry shaders (wgpu/WebGPU). Do one of two things:
    - (a) Build a static fin mesh on the CPU with all edges, skin it with the same joints, and in the vertex shader fade alpha by `1 - |dot(N_face, V)|`, which shows fins only near the silhouette. This costs O(edges) vertices.
    - (b) Run a compute pass that writes only silhouette edges into an indirect draw buffer.
  - Start with (a) at the hero LOD only.
  - Fin texture: a 2D strand-profile texture sampled with (along-edge, height), using the same noise as the shells so fins and shells read as the same hairs.
- **Alpha:**
  - Switch shells to `AlphaToCoverage` with `Msaa::Sample4`. It gives soft, stable strand edges, depth writes and no sorting. Shadows and SSAO still use the prepass's alpha test.
  - Output alpha from a *distance-to-strand-centre* SDF rather than binary noise, so A2C has a gradient to dither. Scale alpha by `fwidth` to keep strands at least 1 px wide (prevents shimmer).
  - Avoid Bevy OIT for shells. Shells × layers can exceed `fragments_per_pixel_average`, and OIT forbids MSAA.
  - If MSAA is off (for example with TAA), A2C degrades to Mask(0.5). Then prefer alpha-test plus TAA, with per-pixel hash-dithered thresholds ("hashed alpha"), which TAA resolves.
- **Strand styling in the shell shader** (all cheap, per fragment):
  - Taper: `radius = thickness * (1 - h)^tip`.
  - Clumping: offset the strand-cell UV toward a Voronoi clump centre by `clump * h`.
  - Curl: `uv += curl_amp * h * vec2(cos, sin)(h * curl_freq + cell_rand)`.
  - Comb/flow: in the vertex shader, offset each shell by `h^2 * (comb_dir * stiffness + gravity_ws + wind)`. Store `comb_dir` as a per-vertex tangent from the anatomy builder (limb axis / spine direction), skinned like a normal.
  - Muscle flex: shells must reuse the same deformation as the skin (already done for skinning). Recompute the offset direction from the deformed normal.
- **Self-shadowing and AO:** Multiply diffuse by `mix(ao_root, 1, h)` (root ≈ 0.3–0.5). This is the single biggest depth cue.
  - Optionally add a cheap "deep opacity" approximation: `transmittance ≈ exp(-density * (1-h) * len / max(N·L, ε))`.
  - Real deep opacity maps (light-space density slices) are a custom render pass; not worth it initially.
- **Rim/backscatter:** Add a forward-scatter term `pow(saturate(dot(-L, V)), p) * (1-h)…` tinted by the fur colour, plus a fresnel/rim boost on the outer shells. This gives the halo seen on back-lit fur.

### Gaps
- No Acerola write-up numbers (shell counts, perf) were retrieved; only community ports.
- No official Sucker Punch, Rockstar, Bethesda (Starfield) or Team Ico (Shadow of the Colossus) technical write-up on fur was found in this pass. Shadow of the Colossus and Viva Piñata shell use is widely cited anecdotally but unverified here.
- I could not text-extract the NVIDIA Shells-and-Fins PDF (binary). Its exact shell counts and fade formulas are unverified.

---

## 2. Strand-based real-time hair/fur

### Takeaway
Strands give the best close-ups, but they need compute-heavy pipelines: rasterising thin lines with AA, OIT, and deep shadows. Modern results are fast:
- Hair meshes with mesh shaders: 100 × 100k strands in 2 ms at 8× MSAA on a 4090.
- Software rasterisation: about 127k strands in 3.1 ms on an RTX 5080.

In Bevy 0.20 (no mesh shaders in wgpu core), the feasible route is **instanced ribbon/line strands for guard hairs (thousands to tens of thousands) over shells**, shaded with Kajiya-Kay/Scheuermann or a cheap Marschner approximation. A full compute rasteriser is a large custom render-graph project.

### Cited Findings
- **Kajiya-Kay** uses the strand tangent T in place of N. Specular depends on T·H, and diffuse is based on sin(T,L).
- **Scheuermann** (ATI, SIGGRAPH/GDC 2004) adds two shifted-tangent specular lobes approximating Marschner:
  - The primary highlight comes from surface reflection and is shifted toward the tip.
  - The secondary is transmitted and back-reflected, tinted by hair colour, and noisy (sparkly). Inputs are a specular shift texture and a specular noise texture.
  - Diffuse is `max(0, 0.75·N·L + 0.25)`.
  - Sources: [SIGGRAPH 2004 talk PDF](https://history.siggraph.org/wp-content/uploads/2022/12/2004-Talks-Scheuermann_Practical-Real-Time-Hair-Rendering-and-Shading.pdf), [GDC 2004 slides](https://web.engr.oregonstate.edu/~mjb/cs557/Projects/Papers/HairRendering.pdf)
- **Marschner et al. 2003:** measurements show hair fibres produce multiple specular highlights (R, TT, TRT) that Kajiya-Kay does not predict. — [Marschner et al., UCSD](https://graphics.ucsd.edu/~henrik/papers/hair/)
- Engines expose this lineage as shading-model options: Stride has `KajiyaKayShifted`, `ScheuermannApproximation`, `ScheuermannImproved`. — [Stride API](https://doc.stride3d.net/4.0/en/api/Stride.Rendering.Materials.HairShared.HairShadingModel.html)
- **Unreal Groom:**
  - Pipeline stages include voxelisation (a density volume for shadow and transmittance) and Primary Visibility, where curves are rasterised and hair samples allocated for lighting.
  - Cost scales with groom count. Strands need a budget and are not on all platforms.
  - An LOD can mix strands and cards/meshes. "Use Stable Rasterization" prevents aliasing but makes hair groups look thicker.
  - Source: [Epic groom scalability docs](https://dev.epicgames.com/documentation/unreal-engine/groom-scalability-and-performance-with-unreal-engine)
- **Weta's "Meerkat" short (UE 4.26 strands):**
  - A 400k-strand meerkat groom and a 2.5M-strand eagle groom.
  - Feathers were built "with geometry-based spines in the skeletal mesh" so they could "bind the barbs as strands".
  - "Solid interactive performance" in the editor, with lower-res grooms for slow close-ups. Final frames could be rendered offline, so this is not a game budget.
  - Source: [befores & afters](https://beforesandafters.com/2020/11/18/eagle-vs-meerkat-behind-weta-digitals-stunning-unreal-engine-short/)
- **AMD TressFX:**
  - Uses a per-pixel linked list (PPLL) OIT that blends the K nearest fragments sorted. ShortCut (3.1) stores only the front k = 2–3 layers, 4 bytes each for depth, colour and accumulated alpha, at the cost of extra geometry passes.
  - TressFX 5.0 (GDC22) simplifies ShortCut to shade only the first layer. Its deep-shadow optimisation randomly samples 20 PPLL layers to approximate global scattering, avoiding pixels with hundreds of layers. It works with UE's TAA and Marschner shading.
  - Sources: [GPUOpen TressFX 3.1](https://gpuopen.com/tressfx-3-1/), [UE4 TressFX 5.0 GDC22 PDF](https://gpuopen.com/download/UE4_TressFX_5_0.pdf), [Wikipedia](https://en.wikipedia.org/wiki/TressFX)
- **NVIDIA HairWorks** (CoD Ghosts): about 400–500k strands per fur asset, generated on the GPU from about 10k guide hairs. — [Guru3D](https://www.guru3d.com/story/nvidia-hairworks-cod-ghosts-fur-and-hair-simulation-solution/)
- **Hair meshes + mesh shaders** (Bhokare, Montalvo, Diaz, Allen, Yuksel; SIGGRAPH 2024): 100 characters × 100k unique strands were rasterised in 2 ms on a 4090 with 8× MSAA, and all 100 hair-mesh models fit in 1.7 MB. Strands are generated on the fly from coarse "hair mesh" volumes. — [Yuksel project page](https://www.cemyuksel.com/research/hairmesh_rendering), [paper PDF](https://www.cemyuksel.com/research/hairmesh_rendering/Real-Time_Hair_Rendering_with_Hair_Meshes-SIGGRAPH24.pdf)
- **Software rasterisation of strands** (Lipp, Jarabo, Wimmer, Bode 2026):
  - About 127k strands at 1080p on an RTX 5080: 3.1 ms software rasteriser vs 13.6 ms mesh shaders at MSAA 1, no LOD (a 4.4× speedup).
  - Indirect-dispatch LOD pipeline with depth-map visibility correction.
  - The mesh-shader LOD variant is slower than non-LOD at close and mid range.
  - Sources: [arXiv 2607.04230](https://arxiv.org/abs/2607.04230), [TU Wien page](https://www.cg.tuwien.ac.at/research/publications/2026/LIPP-2026-HAIR/)
- **Frostbite** (EA) published a hair rendering and simulation blog series (Criterion and Frostbite teams). It covers the shading model, multiple-scattering simulation, "an approach to fine, antialiased hair", and an Eulerian/Lagrangian physics model. — [EA Frostbite blog](https://www.ea.com/news/frostbite-hair-rendering-and-simulation). The URL now 404s when fetched, and details are unverified.

### Inferences (Bevy feasibility and expected perf)
- **Bevy 0.20 constraints:** no mesh-shader pipeline in `bevy_pbr`. Custom render-graph nodes, compute shaders, storage buffers and indirect draws are available. A ribbon-strand pass is a custom `Material` or `ExtendedMaterial` on a generated mesh.
- **Recommended hero tier: "guard hairs" as camera-facing ribbons.**
  - Generate N strands (2–8k per creature, 4–8 segments each) from root points on the skin. Use the same root UVs as the shell strand cells so guard hairs coincide with shell strands.
  - Store root barycentrics and triangle id in a storage buffer. In the vertex shader, re-evaluate the root on the *deformed* skin, either from a skinned-position buffer written by compute or by skinning a root with the triangle's joints.
  - Extrude the ribbon along the strand tangent with width = `max(physical_width, 1px)`, and fade alpha by `physical/1px` coverage (a "strand width AA" trick; this is consistent with Unreal's stable-rasterization note about thicker groups).
  - Render with AlphaToCoverage + MSAA so ribbons resolve over the shells without sorting.
  - Shade with Kajiya-Kay plus a Scheuermann second lobe, using `h` (position along strand) for root AO.
  - Expected cost: 8k strands × 8 segments ≈ 128k triangles per creature, trivial for modern GPUs. The cost is overdraw and fill rate at hero framing, which a few creatures on screen can easily afford.
- A **compute rasteriser plus PPLL/OIT** (TressFX-like, 100k+ strands) is a multi-week custom pipeline in Bevy. It would also conflict with Bevy's OIT (no MSAA) unless self-contained. Not recommended for a procedural sculpting tool unless strands become the main look.
- Marschner (R/TT/TRT with azimuthal terms) can be approximated with two or three Gaussian lobes in longitudinal angle. This is the Scheuermann-style approach and is enough for animal fur. d'Eon and Chiang-style energy-conserving models are offline-oriented.

### Gaps
- No primary Frostbite talk (2023–2024 SIGGRAPH) details were retrievable. No EA SEED strand hair specifics were found.
- No HairWorks frame-time numbers were found. No Bevy implementation of strand hair was found.
- I did not verify d'Eon (2011) or Chiang (2016) model specifics in this pass.

---

## 3. Feather rendering

### Takeaway
Real-time feathers in games are essentially **textured cards** (alpha-tested or A2C) with a procedural rachis/barb pattern and anisotropic sheen along the barbs. The state of the art (2026 TOG) encodes barbs and barbules procedurally in the feather's (along, across) UV domain, with correct multiscale self-shadowing. For iridescent plumage, use the Belcour-Barla thin-film model; Bevy has no iridescence field, so it needs custom shader code.

### Cited Findings
- **Chen et al. 2026** (ACM TOG, DOI 10.1145/3811328), "A Real-time, Multiscale and Procedural Feather Appearance Model":
  - From a single photo it extracts shaft curve, outline and albedo into a compact hierarchical planar/curve (UV) representation.
  - Barbs and barbules are encoded procedurally, giving "continuous multiscale rendering with correct self-shadowing and masking".
  - A new scattering model for non-iridescent feathers.
  - No frame times appear in the abstract.
  - Sources: [Univ. Melbourne record](https://findanexpert.unimelb.edu.au/scholarlywork/2331275-a-real-time--multiscale-and-procedural-feather-appearance-model), [MBZUAI repo](https://irep.mbzuai.ac.ae/items/0c83439d-538e-42b5-949e-eec91f14ff28)
- **Baron, Dhillon, Patterson** (SIGGRAPH 2021 poster): shading that accounts for feather substructures for more accurate far-field appearance. — [SIGGRAPH history](https://history.siggraph.org/learning/procedural-shading-for-rendering-the-appearance-of-feathers-by-baron-dhillon-and-patterson/)
- **Weta's eagle** was strand-based: rachis spines as geometry in the skeletal mesh, with barbs bound as strands (2.5M strands). — [befores & afters](https://beforesandafters.com/2020/11/18/eagle-vs-meerkat-behind-weta-digitals-stunning-unreal-engine-short/)
- There is a patent on modelling and rendering realistic feathers (US7042456B2). — [Google Patents](https://patents.google.com/patent/US7042456)
- **Belcour & Barla 2017** (TOG 36(4)):
  - Thin-film-on-rough-base microfacet extension. It pre-integrates the spectral response analytically, so RGB renderers avoid spectral aliasing, at modest overhead.
  - Adopted in Unity HDRP and the basis of glTF `KHR_materials_iridescence`. That adoption claim comes from a less authoritative source.
  - An IBL follow-up exists: Kneiphof, Golla, Klein, EGSR 2019.
  - Sources: [Unity blog](https://blog.unity.com/es/technology/a-practical-extension-to-microfacet-theory-for-the-modeling-of-varying-iridescence), [SIGGRAPH history](https://history.siggraph.org/?p=102374), [EG 2019](https://diglib7.eg.org/handle/10.1111/cgf13772), [Scrapbox summary](https://scrapbox.io/0b5vr/A Practical Extension to Microfacet Theory for the Modeling of Varying Iridescence)
- Horizon Forbidden West's GDC 2022 deferred-texturing talk accelerates "foliage and alpha tested geometry". I found no feather-specific Guerrilla talk. — [GDC Vault](https://www.gdcvault.com/play/1027553/contactUs), [Guerrilla GDC 2022](https://www.guerrilla-games.com/read/guerrilla-at-gdc-2022)

### Inferences (implementation notes)
- **Procedural feather alpha in the card shader** (uv = across ∈ [-1,1], along ∈ [0,1]):
  - Vane outline: `|across| < w(along)`, asymmetric for flight feathers.
  - Rachis: a bright, narrow band `|across| < r(along)` with its own normal (cylinder).
  - Barbs: lines at angle θ to the rachis, `fract((along + |across|·tan θ) * n_barbs)`. Sharpen with `fwidth` for AA, and add per-barb hash gaps and "splits" for realism.
  - Down feathers: random-angle, low-density barbs, no rachis, with alpha falling off radially.
  - Output an A2C-friendly soft alpha.
- **Anisotropic sheen:** The barb tangent is known analytically in UV. Feed it as the anisotropy direction: either set `pbr_input.anisotropy_T/B` in a custom fragment, or use Kajiya-Kay along the barb tangent. The rachis gets its own glossy lobe.
  - `StandardMaterial.anisotropy_strength` and `anisotropy_rotation` exist in 0.20 and need mesh tangents. The cards already have a natural tangent along the "along" axis.
- **Layering:** Contour feathers are the outer cards, oriented along the comb direction with overlap (like shingles). Down is a short shell layer under them. Darken down and root regions with AO by depth in the stack. This is cheaper than shadow maps for card self-occlusion.
- **Iridescence:** Implement a Belcour-Barla-style thin-film Fresnel (an Airy reflectance approximation with 3-wavelength RGB evaluation is the cheap variant). Use it only on contour cards (e.g. the "wyvern/dragon" scales and feathers), with film thickness varying along the barbs.

### Gaps
- No technical write-up was found for feathers in Horizon Forbidden West, Ghost of Yotei, or RDR2 birds.
- No Pixar feather pipeline sources were retrieved in this pass.
- No real-time perf figures exist for Chen et al. 2026.

---

## 4. Horse / short coat rendering without shells

### Takeaway
For short coats (horse, short-haired beasts), shells add little up close relative to their cost. Use **skin plus an anisotropic sheen aligned to the hair-flow direction** (Bevy `StandardMaterial` anisotropy, driven by a per-vertex tangent from the comb field), a fine flow-aligned normal/roughness micro-pattern, and optional 2–4 very short shells only at the silhouette or hero LOD.

### Cited Findings
- Bevy's `anisotropy_strength` "increases the roughness along a specific direction… typically used for modeling surfaces such as brushed metal and hair". It aligns with the mesh tangent, rotated by `anisotropy_rotation` in tangent space, with an optional `anisotropy_texture` (feature `pbr_anisotropy_texture`), following `KHR_materials_anisotropy`. Added in PR #13450. — [bevy_pbr 0.20 pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs), [Bevy PR #13450](https://github.com/bevyengine/bevy/pull/13450)
- The community speculates that Ghost of Tsushima horses use textured or sculpted short coats, with cards for mane and tail. This is unconfirmed. — [Polycount](https://polycount.com/discussion/comment/2749745/), [The Mane Quest blog on GoT horses](https://www.themanequest.com/blog/2023/5/1/the-horses-in-ghost-of-tsushima-wonderful-details-and-fundamental-problems)
- Kajiya-Kay (SIGGRAPH 1989) is the origin of fur shading with 3D textures and tangent-based lighting. — [SIGGRAPH history](https://history.siggraph.org/learning/rendering-fur-with-three-dimensional-textures-by-kajiya-and-kay)

### Inferences
- Procedural meshes from surface nets have no authored tangents. Set `Mesh::ATTRIBUTE_TANGENT` from the coat comb direction (projected onto the tangent plane, w = handedness), *not* from `generate_tangents` (MikkTSpace on UVs). Then anisotropy follows the hair flow for free.
- Add a flow-aligned procedural normal "striation" (high-frequency ridges along the comb direction) and slight roughness variation for the short-coat look.
- Use a Scheuermann-style secondary tinted lobe for the warm sheen of bay or chestnut coats.
- Manes and tails need cards or strands (section 2/3 techniques), not shells.

### Gaps
- No primary game source documents how RDR2 or Ghost of Tsushima horses are shaded.

---

## 5. Bevy-specific capability summary (0.20) and community projects

### Takeaway
Bevy 0.20 has everything needed for shells+fins+ribbons: A2C with MSAA, anisotropy in StandardMaterial, custom prepass/shadow shaders on `MaterialExtension`, and compute/indirect for GPU-driven work. OIT exists but is MSAA-incompatible and limited to blend modes, so it is a poor fit for fur. No mature Bevy fur or hair crate was found.

### Cited Findings
- **MaterialExtension hooks (0.20):** `MaterialExtension` exposes `enable_prepass`, `enable_shadows`, `enable_oit`, `prepass_vertex_shader`, prepass fragment, `deferred_vertex_shader`/`deferred_fragment_shader`, and `specialize`. — [bevy_pbr 0.20 extended_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/extended_material.rs)
- **OIT (0.20):** linked list. It panics with MSAA. Blend/Premultiplied/Add only. Custom shaders must call `oit_draw`. — [oit/mod.rs](https://docs.rs/crate/bevy_core_pipeline/0.20.0/source/src/oit/mod.rs)
- **A2C (0.20):** MSAA-only, opaque phase with depth write, and a prepass that alpha-tests (A2C disabled in the prepass pipeline). — [material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/material.rs), [prepass/mod.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/prepass/mod.rs)
- **Other 0.20 modules:** the bevy_pbr 0.20 source tree contains `contact_shadows.rs`, `ssao`, `meshlet`, `decal`, `volumetric_fog`, `transmission`. — local source listing (bevy_pbr-0.20.0/src)
- **Community projects:**
  - Searches found no Bevy fur, shell or hair crate on crates.io or GitHub. GitHub's "fur" topic lists Godot and Unity shell projects only. — [GitHub topic fur](https://www.GitHub.com/topics/fur)
  - The ornisculpt dependency tree contains `bevy_feathers-0.20.0`, but that is Bevy's UI widget kit, not plumage. (Local registry listing; Bevy's UI "feathers" crate.)

### Inferences
- **Shadows and SSAO:**
  - Because prepass and shadow passes alpha-test the shells, each shell casts a binary-masked shadow. Many shells give noisy self-shadow acne. Consider `enable_shadows` only for the base skin plus the inner 1–2 shells (spawn shells as separate meshes with `NotShadowCaster` on the outer layers).
  - Have the custom `prepass_fragment_shader` discard with the same strand SDF so depth and normals match the main pass. Otherwise SSAO and contact shadows will see the wrong silhouette.
  - Outer shells should probably write prepass depth but bias their normal toward the strand tangent, or be excluded from SSAO, to avoid a dark crust.
- **TAA vs MSAA:** A2C needs MSAA. If the project uses TAA (MSAA off), switch the strategy to hashed alpha test, which TAA averages. Decide per camera, since the viewer could use MSAA 4× for hero shots.
- **GPU-driven:** strand-root generation and deformation (re-sampling roots on the skinned/flexed mesh) fit a compute node writing a storage buffer read by the ribbon vertex shader. The same mechanism could cull shells or fins by distance with indirect draws.
- Version notes: OIT changed from `layer_count` (around 0.15) to a linked list with `fragments_per_pixel_average` (by 0.19/0.20). The shader extension moved to `.wesl` files in 0.20's internals (e.g. `oit/draw.wesl`, `pbr_types.wesl`).

### Gaps
- I did not verify whether Bevy 0.20 exposes `@builtin(sample_mask)` output in custom material fragment shaders. That would allow manual coverage dithering (wgpu supports `sample_mask`, but it is not confirmed in Bevy's material path).
- I did not benchmark Bevy A2C with MSAA 4× and 32+ shells on the project's meshes.
