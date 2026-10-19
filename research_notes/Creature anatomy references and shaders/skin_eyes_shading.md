# Close-up shading for creature skin, scales, membranes, eyes and wet surfaces (Bevy 0.20, WGSL/WESL)

Method note: I checked the Bevy facts against the bevy_pbr 0.20.0 crate source in the local cargo registry (`~/.cargo/registry/src/index.crates.io-*/bevy_pbr-0.20.0/`). The project's Cargo.lock pins 0.20.0. Those findings cite the matching docs.rs source URL. I also read the project's own shader (`src/coat/coat_fragment.wesl`, `src/mob/generate.rs`) to tie the recommendations to existing code. Inferences, including the WGSL sketches, are my own and are not sourced.

## 1. What Bevy 0.20 StandardMaterial supports natively, and how the extension hooks behave

### Takeaway
Bevy 0.20 `StandardMaterial` has diffuse transmission, specular (screen-space) transmission, thickness/IOR/attenuation, clearcoat (with its own roughness and normal), anisotropy, `specular_tint`, `reflectance`, and parallax/relief depth maps. It has **no subsurface scattering of any kind**: no screen-space SSS, no pre-integrated skin, no wrap diffuse. The biggest caveat for an `ExtendedMaterial` is that the clearcoat, anisotropy and transmission code paths are **compiled in or out by shader defs taken from the Rust-side base material**. Writing `pbr.material.clearcoat = x` in WGSL does nothing unless the base `StandardMaterial` had `clearcoat > 0.0`.

### Cited Findings
**StandardMaterial fields in 0.20** (exact names): `base_color`, `emissive`, `emissive_exposure_weight`, `perceptual_roughness`, `metallic`, `reflectance`, `specular_tint` (Color), `diffuse_transmission`, `specular_transmission`, `thickness`, `ior` (default 1.5), `attenuation_distance`, `attenuation_color`, `normal_map_texture`, `flip_normal_map_y`, `occlusion_texture`, `specular_texture`, `specular_tint_texture`, `clearcoat`, `clearcoat_perceptual_roughness`, `clearcoat_normal_texture`, `anisotropy_strength`, `anisotropy_rotation`, `anisotropy_texture`, `double_sided`, `cull_mode`, `unlit`, `fog_enabled`, `alpha_mode`, `depth_bias`, `depth_map`, `parallax_depth_scale`, `parallax_mapping_method`, `max_parallax_layer_count`, `lightmap_exposure`, `opaque_render_method`, `deferred_lighting_pass_id`, `uv_transform`. Each texture also has a `*_channel: UvChannel`. — [bevy_pbr 0.20 pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- **`specular_tint`**: "currently unsupported in the deferred rendering path, in order to reduce the size of the geometry buffers." — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- **`diffuse_transmission`** is "a second, flipped Lambertian diffuse lobe". It is meant for "thin dielectric objects (e.g. paper, leaves, some fabrics) or thicker volumetric materials with short scattering distances (e.g. porcelain, wax)". Values above 0.5 transmit more than they reflect. `base_color` modulates the transmitted light. Shadows on the back lobe need the `TransmittedShadowReceiver` component (in `bevy_light`). — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- **How diffuse transmission is implemented**: `diffuse_transmissive_color = output_color.rgb * (1 - metallic) * (1 - specular_transmission) * diffuse_transmission`. The back lobe is evaluated at `world_position - world_normal * thickness`, with `N = -in.N`, `V = -in.V`, roughness 1 and F0 0, so **`thickness` offsets the shadow-lookup point for the back lobe**. Front diffuse is scaled by `(1 - diffuse_transmission)`. — [pbr_functions.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_functions.wesl)
- **`specular_transmission`** "[r]equires the `ScreenSpaceTransmission` component on the camera". It is "a relatively expensive screen-space effect". `steps = 0` falls back to refracting only the environment map. Materials using it render in the `Transmissive3d` pass even if they are alpha-blended. — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs), [transmission/mod.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/transmission/mod.rs)
- **API move across versions**: in 0.18.1, transmission settings were fields on `Camera3d` (`screen_space_specular_transmission_steps` / `_quality` in bevy_core_pipeline). By 0.19.0 they were a separate `ScreenSpaceTransmission { steps, quality: ScreenSpaceTransmissionQuality }` component in `bevy_pbr::transmission`, and 0.20 keeps that. — local registry sources bevy_core_pipeline-0.18.1 `core_3d/mod.rs`, bevy_pbr-0.19.0 / 0.20.0 `transmission/mod.rs`; [0.20 source](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/transmission/mod.rs)
- **Shader-def gating (the critical caveat)**: `StandardMaterialKey` sets `DIFFUSE_TRANSMISSION` from `material.diffuse_transmission > 0.0`, `CLEARCOAT` from `material.clearcoat > 0.0`, `ANISOTROPY` from `material.anisotropy_strength > 0.0`, and `SPECULAR_TRANSMISSION` from screen-space transmission use. These become the WESL defs `STANDARD_MATERIAL_DIFFUSE_TRANSMISSION`, `STANDARD_MATERIAL_CLEARCOAT`, `STANDARD_MATERIAL_ANISOTROPY` and `STANDARD_MATERIAL_SPECULAR_TRANSMISSION`, and `apply_pbr_lighting` is wrapped in `@if(...)` on them. — [pbr_material.rs `specialize`/key](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs), [pbr_functions.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_functions.wesl)
- **Anisotropy needs tangents and a feature**: in `pbr_input_from_standard_material`, the T/B vectors are filled only under `@if(PBR_ANISOTROPY_TEXTURE_SUPPORTED && VERTEX_TANGENTS && STANDARD_MATERIAL_ANISOTROPY)`. The cargo features are `pbr_anisotropy_texture`, `pbr_multi_layer_material_textures` (clearcoat textures), `pbr_specular_textures` and `pbr_transmission_textures`. — [pbr_fragment.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_fragment.wesl), [bevy_pbr Cargo.toml](https://docs.rs/crate/bevy_pbr/0.20.0/source/Cargo.toml)
- **`PbrInput` fields an extension can overwrite** before `apply_pbr_lighting`: `material` (a full `StandardMaterial` struct, including `reflectance: vec3`, `clearcoat`, `clearcoat_perceptual_roughness`, `diffuse_transmission`, `thickness`, `anisotropy_*`), `diffuse_occlusion: vec3`, `specular_occlusion: f32`, `world_normal`, `N`, `V`, `clearcoat_N`, `anisotropy_strength`, `anisotropy_T`, `anisotropy_B`, `lightmap_light`, `flags`, and `directional_shadow_factor` (new: "Multiplier applied to all directional lights before cascade and contact shadows… for custom shadow maps"). — [pbr_types.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_types.wesl)
- **Hook functions**: `pbr_fragment::pbr_input_from_standard_material(in, is_front)`, `pbr_functions::apply_pbr_lighting(pbr_input) -> vec4`, and `pbr_functions::main_pass_post_lighting_processing(pbr_input, color)`, which handles fog, in-shader tonemapping and deband dither. Lighting building blocks such as `D_GGX`, `V_SmithGGXCorrelated`, `F_Schlick_vec`, `Fd_Burley`, `EnvBRDFApprox`, `specular_clearcoat` and `point_light`/`directional_light`/`rect_light` live in `pbr_lighting.wesl`. — [pbr_functions.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_functions.wesl), [pbr_lighting.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_lighting.wesl)
- **No SSS in Bevy**: a GitHub search of bevyengine/bevy for "subsurface" finds only issue #14487, "Mesh thickness prepass texture" (open since 2024-07-26), PR #8015 "StandardMaterial Light Transmission" (merged 2023-10-31) and older unrelated items. No SSS PR was merged. The 0.20 bevy_pbr source has no subsurface code. — [GitHub search API](https://api.github.com/search/issues?q=repo:bevyengine/bevy+subsurface&per_page=20), [issue #14487](https://github.com/bevyengine/bevy/issues/14487), [PR #8015](https://github.com/bevyengine/bevy/pull/8015)
- **0.19 (released June 19, 2026) added**: rectangular area lights using Linearly Transformed Cosines, behind the `area_light_luts` cargo feature. They do not yet cast shadows or support anisotropic materials. It also added **contact shadows** (`contact_shadows_enabled` on the light plus the `ContactShadows` camera component), parallax-corrected cubemaps for light probes (on by default; opt out with `ParallaxCorrection::None`), and white-furnace energy fixes. — [Bevy 0.19 release notes](https://bevy.org/news/bevy-0-19/)
- **0.20 (release post dated October 8, 2026)**: all Bevy shaders are now WESL, and the old custom WGSL dialect (`#import`/`#ifdef`) was removed. Custom shaders need converting and renaming to `.wesl`, though plain WGSL still works. The release also added `ExtendedMaterial2d`, mesh shaders and Solari ReSTIR changes. It has no new PBR material features. — [Bevy 0.20 release notes](https://bevy.org/news/bevy-0-20/)
- **Relevant 0.20 examples** in `examples/3d`: `anisotropy.rs`, `clearcoat.rs`, `transmission.rs`, `specular_tint.rs`, `parallax_mapping.rs`, `rect_light.rs`, `spherical_area_lights.rs`, `contact_shadows.rs`, `pcss.rs`, `ssr.rs`, `ssao.rs`, `decal.rs`, `clustered_decals.rs`, `light_textures.rs`. — [GitHub contents API, v0.20.0](https://api.github.com/repos/bevyengine/bevy/contents/examples/3d?ref=v0.20.0)

### Inferences
- **Project status: wings are set up, other layers are not.** The project's `src/mob/generate.rs` sets `diffuse_transmission = 0.5` on the base material for the Wing layer only. So the WGSL `pbr.material.diffuse_transmission = transmit` works for wings and is silently ignored for the other layers. Ears, frills and the skin layer need a base `diffuse_transmission > 0` (any epsilon such as 0.001) to compile the back lobe.
- **Enabling clearcoat (wet noses, eyes, scales)**: set `clearcoat: 1e-3` on the base material for any layer whose shader may want it, then drive it per pixel in WGSL. The same applies to anisotropy, which also needs mesh tangents and the `pbr_anisotropy_texture` feature. The project already writes tangents for wings.
- **Specular-tint caveat**: if the project ever switches to deferred rendering (`OpaqueRendererMethod::Deferred`), `specular_tint` stops working and the clearcoat/transmission paths are reconstructed from the G-buffer. Stay forward.
- **Lighting choice for close-ups**: for hero shots, `RectLight` (LTC, `area_light_luts` feature) gives soft, wide speculars on wet surfaces and eyes. It has no shadows, so pair it with a shadowed directional key light. Contact shadows help small-scale self-shadowing in scale crevices and eyelids.
- **Custom shadowing**: `directional_shadow_factor` lets an extension darken directional light itself, for example a cheap cavity-based "micro-shadow" term.

### Gaps
- I did not run the 0.20 examples, so caveats such as clearcoat combined with deferred rendering are taken from source reading only.
- I found no official Bevy roadmap item for SSS. Issue #14487, a mesh thickness prepass, is the closest open request.

## 2. Skin SSS options and which are feasible in Bevy

### Takeaway
In a Bevy `ExtendedMaterial`, the best cost/benefit is: (a) curvature-driven **pre-integrated / wrap diffuse**, computed per light inside a custom light loop or approximated as a post-lighting additive term; (b) a **"fast translucency" back-light term** for thin parts (already in the project for wings); (c) the native `diffuse_transmission` back lobe with `thickness`. True **separable screen-space SSS** (Jimenez 2015) gives the best skin quality but needs a render-graph post pass that blurs only diffuse lighting. Bevy does not split diffuse from specular, so it is a substantial engine-level job.

### Cited Findings
- **Separable SSS** (Jimenez, Zsolnai, Jarabo, Freude, Auzinger, Wu, von der Pahlen, Wimmer, Gutierrez; CGF 34(6) / EGSR 2015) approximates the 2D diffuse reflectance profile with a single separable kernel, so it needs only two 1D screen-space convolutions. It measured 0.489 ms/frame at 1080p on a Radeon HD 7970, and offers a physically based mode and an artist-controllable mode. — [TU Wien publication page](https://cg.tuwien.ac.at/research/publications/2015/Jimenez_SSS_2015), [Paper PDF](https://graphics.unizar.es/papers/Separable-Subsurface-Scattering.pdf)
- The technique was also implemented in Blender. Blender EEVEE's (legacy) SSS blurs diffuse lighting in screen space, with jitter to hide sampling artifacts at large radii. — [Zsolnai-Fehér page](https://users.cg.tuwien.ac.at/zsolnai/gfx/separable-subsurface-scattering-with-activision-blizzard/), [Blender manual](https://docs.blender.org/manual/en/4.1/render/eevee/render_settings/subsurface_scattering.html)
- Unity HDRP's production screen-space SSS is described in "Efficient Screen-Space Subsurface Scattering" (SIGGRAPH 2018 Advances). — [Advances 2018 PDF](https://www.advances.realtimerendering.com/s2018/Efficient%20screen%20space%20subsurface%20scattering%20Siggraph%202018.pdf)
- **Pre-integrated skin** (Penner & Borshukov, GPU Pro 2, 2011, pp. 41–55) pre-integrates scattering over light gradients. It is indexed by curvature (1/r) and N·L in a lookup texture. Low curvature gives a black-to-white band; high curvature gives a band that passes through red. The chapter also covers "scattering and normal maps" (blurred normals per channel) and "shadow scattering". — [Game Gems DB](https://gameenginegems.com/gemsdb/article.php?id=1103), [Penner SIGGRAPH 2011 slides](https://www.slideshare.net/slideshow/penner-preintegrated-skin-rendering-siggraph-2011-advances-in-realtime-rendering-course/13966747), [patent describing the LUT](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11276227)
- **Practical pitfall**: per-pixel curvature from screen-space derivatives (`length(fwidth(N))/length(fwidth(P))`) produces banding and artifacts. The LUT is data and must not be colour-space converted (load it as linear, not sRGB). — [GameDev.net thread](https://www.gamedev.net/forums/topic/658069-how-can-i-have-a-softer-skin-using-brdf/5165432/)
- **Wrap diffuse, energy-conserving form (Filament cloth model)**: `Fd = (c_diff/π)(1−F) · saturate((NoL + w)/(1 + w)²) · saturate(c_subsurface + NoL)`, with `w = 0.5` fixed. The diffuse term is not multiplied by NoL. — [Filament docs](https://google.github.io/filament/Filament.md.html)
- **Bevy's own translucency** is a flipped Lambertian lobe intended for thin objects and short scattering distances, at much lower cost than specular transmission. — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- **Fast translucency (Barré-Brisebois & Bouchard, GDC 2011)**, already used in `coat_fragment.wesl`: `I = pow(saturate(dot(V, -(L + N·distortion))), power) · scale · thickness_inverse`. — project shader `src/coat/coat_fragment.wesl` (lines ~444–450). I did not fetch the original talk this session.

### Inferences
- **Recommended cheap skin pipeline** (no engine changes):
  1. **Curvature per vertex, not per pixel.** The mesh comes from an SDF, so curvature is analytic: the mean curvature is the Laplacian of the SDF, or the trace of the SDF Hessian, from central differences at the vertex. Bake it into a vertex attribute when meshing with surface nets. This avoids the derivative banding in the GameDev thread. The same value drives SSS, cavity/dirt and specular occlusion.
  2. **Pre-integrated diffuse in the extension.** Bevy's light loop is inside `apply_pbr_lighting` and cannot be overridden per lobe. There are two options:
     - **(A) Re-implement a minimal light loop.** Iterate `view_bindings::lights.directional_lights[i]` plus clustered point lights, and use Bevy's shadow-sampling functions for the scattering term. Set the material so `apply_pbr_lighting` gives specular plus a reduced diffuse.
     - **(B, cheaper) Additive correction.** Compute `skin_scatter(NoL, curvature) - saturate(NoL)` for the key directional light only, multiply by the light colour, shadow, albedo and `view.exposure`, and add it after `apply_pbr_lighting`, the same way the project adds `glow`. An analytic substitute for Penner's LUT avoids a texture binding. A per-channel wrap where the wrap width depends on curvature looks very similar (my approximation, not Penner's):
     ```wgsl
     fn skin_scatter(ndl: f32, curv: f32, radius_rgb: vec3<f32>) -> vec3<f32> {
         // wider wrap for red, and on high curvature (thin/curved = more visible bleed)
         let w = clamp(curv * radius_rgb, vec3(0.0), vec3(1.0));
         return clamp((vec3(ndl) + w) / ((1.0 + w) * (1.0 + w)), vec3(0.0), vec3(1.0));
     }
     ```
     Typical `radius_rgb` ≈ (1.0, 0.4, 0.25) × the scatter distance in mesh units.
  3. **Shadow scattering.** Penner softens shadow terminators toward red. Approximate this by reading the shadow term (`directional_shadow_factor` path or `shadows::fetch_directional_shadow`) and tinting penumbrae: `shadow_rgb = pow(vec3(s), vec3(1.0, 2.0, 3.0)*0.5)`.
  4. **Thin parts** (ears, nostrils, eyelids, webbing): set base `diffuse_transmission` > 0 and drive per-pixel transmission from an SDF thickness estimate. The SDF is available at generation time, so bake "distance to the opposite surface" per vertex by marching the SDF along −N. Feed it into `pbr.material.thickness` and the fast-translucency term.
- **Separable SSS in Bevy** would require: an MRT or a separate diffuse-only target, a stencil or mask for SSS pixels, two 1D blur passes in the render graph (render passes became systems in schedules like `Core3d` in 0.19, per the 0.19 notes), then recombining with specular. It is roughly 1–2 weeks of engine work. That is not justified unless human-like skin is a hero target. Reptile/dragon scales tolerate wrap/pre-integrated well because their scattering radii are smaller than human skin's.
- **Cost**: wrap or pre-integrated adds roughly 10–20 ALU per light. Fast translucency is similar. The native diffuse transmission lobe roughly doubles per-light cost for those pixels, because it is a second full light evaluation with a shadow lookup at the offset position. Screen-space SSS is about 0.5–1 ms at 1080p, per Jimenez's numbers.

### Gaps
- The exact pre-integration integral and LUT parameterisation are in GPU Pro 2, which I could not read. The analytic wrap above is an approximation, not Penner's formula.
- The Barré-Brisebois GDC 2011 slides were not re-fetched; the formula is quoted from the project's existing code and comments.

## 3. Procedural close-up detail: 3D-space normals, derivative bump, anti-aliasing, cavity/AO, scale shading, iridescence

### Takeaway
The project already uses Mikkelsen's surface-gradient bump from screen-space derivatives (`bump()` in `coat_fragment.wesl`) and fades detail by pixel footprint (`detail()`). The main wins left are:
1. Fade detail amplitude **and** convert removed normal variance into roughness (Toksvig/LEAN-style) instead of just fading it.
2. Add geometric specular AA from normal derivatives (Kaplanyan/Tokuyoshi; Filament defaults: variance 0.15, threshold 0.2).
3. Drive scale crevice darkening, dirt and specular occlusion from the cellular F2−F1 edge distance plus baked SDF curvature/AO.
4. Thin-film iridescence (Belcour & Barla 2017) for snake/beetle/insect sheens.

### Cited Findings
- The project's `bump(n, world, height)` computes `r1 = cross(dp_dy, n)`, `r2 = cross(n, dp_dx)`, `det = dot(dp_dx, r1)`, `grad = sign(det)*(dh_dx*r1 + dh_dy*r2)`, `N' = normalize(|det| n − grad)`. That is Mikkelsen's "Bump Mapping Unparametrized Surfaces on the GPU" (2010) formulation. `detail(size, pixel) = 1 − smoothstep(0.3, 0.9, pixel/size)` fades features once they shrink toward a pixel. — project `src/coat/coat_fragment.wesl` lines 111–164. I did not re-fetch the Mikkelsen paper.
- **Filament specular AA parameters**: `specularAntiAliasing` "[r]educes specular aliasing and preserves the shape of specular highlights as an object moves away from the camera" (default false). `specularAntiAliasingVariance` is the "screen space variance of the filter kernel" (default 0.15). `specularAntiAliasingThreshold` is a "clamping threshold used to suppress estimation errors" (default 0.2). — [Filament Materials docs](https://google.github.io/filament/Materials.md.html)
- Filament also clamps roughness to a minimum (0.089 for fp16; Frostbite uses 0.045 for fp32 analytic lights) to reduce specular aliasing and precision problems. — [Filament docs](https://google.github.io/filament/Filament.md.html)
- **Geometric specular AA**: Kaplanyan et al. GDC 2016 ([slides](https://developer.download.nvidia.com/gameworks/events/GDC2016/akaplanyan_specular_aa.pdf)), then Tokuyoshi & Kaplanyan, "Improved Geometric Specular Antialiasing", I3D 2019 ([DBLP](https://dblp1.uni-trier.de/pid/18/7866.html)), and "Stable Geometric Specular Antialiasing with Projected-Space NDF Filtering", JCGT 10(2) 2021, which fixes rim artifacts of the slope-space method ([JCGT PDF](https://www.jcgt.org/published/0010/02/02/paper-lowres.pdf)).
- **Bevy's built-in options**: parallax/relief mapping uses `depth_map` with `ParallaxMappingMethod`, but it needs UV/tangent textures. That makes it unusable for the project's UV-less 3D procedural detail. — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- **Thin-film iridescence**: Belcour & Barla, "A Practical Extension to Microfacet Theory for the Modeling of Varying Iridescence" (SIGGRAPH 2017). It analytically pre-integrates the spectral response to avoid RGB spectral aliasing and plugs into any microfacet model as a replacement Fresnel term. — [Unity blog](https://blog.unity.com/es/technology/a-practical-extension-to-microfacet-theory-for-the-modeling-of-varying-iridescence), [SIGGRAPH history](https://history.siggraph.org/?p=102374). Kneiphof, Golla & Klein (EGSR 2019) extend it to rough surfaces, where single-direction evaluation causes artifacts. — [EG diglib](https://diglib.eg.org/bitstreams/4f73c3ce-cde6-45be-bad6-2b61477d477b/download)

### Inferences
- **Specular AA in the extension** (cheap, about 6 ALU; Tokuyoshi-Kaplanyan style). Apply it after the final bumped `pbr.N` is known:
  ```wgsl
  fn aa_roughness(perceptual: f32, n: vec3<f32>) -> f32 {
      let du = dpdx(n); let dv = dpdy(n);
      let variance = 0.15 * (dot(du, du) + dot(dv, dv));      // Filament default variance
      let kernel = min(2.0 * variance, 0.2);                  // Filament default threshold
      let a2 = perceptual * perceptual;                       // Bevy roughness = perceptual^2
      return sqrt(sqrt(clamp(a2 * a2 + kernel, 0.0, 1.0)));   // back to perceptual
  }
  ```
  This must run in uniform control flow. Compute it once per fragment, outside layer branches, as `pixel_size()` already avoids derivatives for that reason.
- **Variance-to-roughness instead of fade-to-flat (Toksvig-like)**: when `detail()` attenuates a bump of slope amplitude `s`, add the lost slope variance to roughness: `a2' = a2 + (1 − detail²) · s²`. Distant scales then look properly satin instead of mirror-smooth, which is the classic LEAN/Toksvig insight. It is especially visible on glossy snake or dragon scales.
- **Scale shading recipe** using the existing `cellular()`, which returns F1, F2 and the cell id:
  - **Edge mask**: `e = smoothstep(0, k·pixel_aware_width, F2 − F1)`. Use F2−F1, not F1, for crisp borders. Widen the band by `fwidth` or pixel size to stay anti-aliased.
  - **Height profile**: dome = `1 − (F1/R)²` (rounded scutes), or keeled = dome plus a ridge along the body axis projected into the cell. Overlapping (imbricate) scales use an asymmetric profile: `h = saturate(1 − F1/R) · saturate(dot(offset_from_seed, axis)·a + b)`, so the scale's free edge is raised and the overlapped edge is buried.
  - **Per-scale variation** from the cell id hash: hue/value jitter ±5–10 %, roughness jitter ±0.1, and a small normal tilt per scale (rotate N by a hash-driven 2–5°). The tilt is the cheapest big win for close-ups, because each scale catches the light differently.
  - **Crevice**: darken albedo by roughly 30–50 % and raise roughness at the edge mask. Set `pbr.diffuse_occlusion *= mix(0.4, 1, e)` and `pbr.specular_occlusion *= mix(0.2, 1, e)`, which stops highlights glowing out of cracks. Add dirt as low-frequency value noise multiplied by (1 − e) and by concave curvature.
  - **Macro cavity from the SDF**: at mesh-generation time, compute SDF ambient occlusion per vertex with the IQ-style 5-tap march along N (`ao = 1 − Σ k_i (h_i − sdf(p + N h_i))`). Store it in a vertex attribute and multiply it into `diffuse_occlusion`. This is free at runtime. Inigo Quilez's SDF AO technique is well known, but I did not fetch it this session.
- **Iridescence, cheap version**: replace F0 with a thin-film Fresnel approximation. Phase is `φ = 4π·n_film·d·cosθ_t / λ` for λ = (650, 532, 450) nm, giving `R ≈ F0 + (1−F0)·0.5·(1 + cos φ)·amp`. Use this as the specular colour via `pbr.material.reflectance` (vec3) or `specular_tint`. The project's insect wing already does an ad-hoc version. Belcour & Barla's analytic spectral integration removes the RGB banding at large thicknesses. Port it from the authors' supplemental code, which I did not verify this session. For rough surfaces, evaluate at the half-vector's VdotH, not N·V.

### Gaps
- I did not fetch the exact Tokuyoshi 2019 formula. The snippet follows the commonly used form with Filament's default constants, so verify the factor of 2 against the paper.
- I did not re-fetch the LEAN mapping paper (Olano & Baker 2010) or Toksvig (2005) this session.

## 4. Wet and glossy surfaces: wet darkening, clearcoat, mucous membranes

### Takeaway
Use native clearcoat for wet layers, enabled with a base `clearcoat > 0`, and drive it per pixel. Combine it with albedo darkening, reduced base roughness, and a separate flat or low-frequency clearcoat normal so the wet film smooths over the scale bumps. Mucous membranes (gums, tongue, inner eyelids, nostrils) need low roughness, wrap/SSS-like reddish diffuse, and clearcoat.

### Cited Findings
- **Filament clearcoat model**: polyurethane-like layer with IOR 1.5, so F0 = 0.04. It uses GGX NDF, Kelemen visibility `1/(4·LoH²)` and Schlick Fresnel. The base is attenuated by the coat Fresnel: `f = (f_d + f_r)(1 − Fc) + f_c`. Base F0 is remapped under the coat as `f0_base = ((1 − 5√F0)/(5 − √F0))²`. — [Filament docs](https://google.github.io/filament/Filament.md.html)
- Bevy's `pbr_lighting.wesl` has `specular_clearcoat` and `V_Kelemen`, and `PbrInput.clearcoat_N` is a separate normal used for the coat layer. — [pbr_lighting.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_lighting.wesl), [pbr_types.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_types.wesl)
- Filament: "The `clearCoatNormal` property defines the normal of the clear coat layer". It behaves like the base `normal` detail input. — [Filament Materials docs](https://google.github.io/filament/Materials.md.html)
- Engadget's coverage of Jimenez's GDC 2013 talk highlighted "a sheen of sweat or reddened eyes" as key emotional cues. — [Engadget](https://www.engadget.com/2013-04-04-gdc-2013-activisions-pursuit-of-photorealism.html)

### Inferences
- **Wetness function** (`wet` in 0..1), set from a per-region mask: nose pad, lips, eye margins, a drool zone below the mouth, or a global rain/swim value:
  - `albedo *= mix(1.0, 0.55 + 0.45*albedo_luma, wet)`. Porous surfaces darken more; this is the common Lagarde-style "wet porosity" heuristic, which I did not verify this session.
  - `perceptual_roughness = mix(r, 0.05..0.15, wet)` on the base layer for non-porous surfaces. For porous ones, keep the base rough and rely on the coat.
  - `clearcoat = wet`, `clearcoat_perceptual_roughness ≈ 0.03–0.1`, `clearcoat_N = normalize(mix(pbr.N, pbr.world_normal, 0.7))`. The film fills small crevices, so its normal follows the macro surface rather than each scale bump. Puddling in crevices: raise `wet` where the scale-edge mask is concave.
- **Nose leather/rhinarium**: cellular bump (small polygonal tubercles, the same `cellular()` at about 1–2 mm), dark albedo, clearcoat 0.6–1.0, base roughness 0.4. The coat gives the glossy-wet look while the bumps stay visible in the base layer's diffuse.
- **Mucous membranes** (mouth interior, tongue, gums): albedo pinkish-red, wrap/pre-integrated diffuse with a large red radius, roughness about 0.25 plus clearcoat about 0.5, and strong `diffuse_occlusion` from the baked SDF AO. Mouth interiors are deep cavities, and without AO they glow.
- **Reflectance**: wet skin and water have F0 ≈ 0.02 (IOR 1.33), so set `reflectance` near 0.35 instead of 0.5 for the water film. The coat's fixed F0 of 0.04 is acceptable.
- **Lighting**: wet highlights need a bright, small or area source and an environment map. A `RectLight` key light plus `EnvironmentMapLight` gives the "hero" wet look. SSR (`ssr.rs`) helps eyes and wet surfaces reflect nearby geometry, but it applies only to deferred or with prepass setups. I did not verify those SSR constraints in 0.20.

### Gaps
- I did not source an authoritative wet-surface paper this session (Lagarde's "Water drop" series is the usual reference). The darkening constants above are common heuristics.

## 5. Eye shaders: cornea, iris parallax and refraction, limbus, caustic, meniscus, pupils, tapetum

### Takeaway
Build the eye as a single-mesh analytic shader, since the project's eye is a hard-part sphere with a forward axis already in `uv`:
1. Physically based refraction of the view ray through a spherical cornea onto a planar or concave iris (Jimenez GDC 2013 / UE "Eye" shading model approach).
2. A procedural iris pattern in polar coordinates with a parametric pupil (round, vertical slit, horizontal bar, W shape), and a limbal ring darkening.
3. A sclera with vein noise, wrap/SSS diffuse, and an AO band from the lids.
4. A clearcoat-like cornea specular, set by Bevy clearcoat with a smooth `clearcoat_N` equal to the cornea normal.
5. An optional tapetum: retro-reflective eyeshine when the light is near the view direction.

No Bevy crate implements this, so it has to be written by hand.

### Cited Findings
- Jimenez's GDC 2013 talk "Next-Generation Character Rendering" was originally listed as "Photorealistic Eyes Rendering and Deformation". It covered eye shading alongside SSS, AA, DOF and film grain, and the demo ran at 180 FPS on a GTX 680. Slides are a large PowerPoint linked from Jimenez's blog. — [GDC Vault](https://gdcvault.com/play/1018270/Next-Generation-Character), [Softpedia](https://games.softpedia.com/blog/Activision-Details-Photo-Realistic-Characters-for-Next-Gen-Consoles-at-GDC-2013-323270.shtml), [PCPer](https://pcper.com/2013/04/gdc-2013-activision-shows-off-real-time-character-rendering-technology-with-realistic-faces/), [iryoku blog post](https://www.iryoku.com/stare-into-the-future)
- Jimenez's bio says the talk had 1.5M YouTube views and influenced digital-human rendering in several AAA engines. — [Jimenez CV PDF](https://iryoku.com/Jorge-Jimenez-2024-12-08.pdf)
- UE's Eye shading model: a forum user reported that changing "Iris Mask" and "Iris Distance" made no visible difference. The refraction in Epic's Content Examples is done by fairly involved, UV-specific material math, meaning the shading model alone does not produce refraction. — [UE AnswerHub thread](https://answers.unrealengine.com/questions/412119/view.html?sort=votes), [UE shading models docs](https://dev.epicgames.com/documentation/en-us/unreal-engine/shading-models-in-unreal-engine)
- A commercial UE eye pack requires "Clear Coat Enable Second Normal" for the cornea and secondary normal details. That is the same idea as Bevy's `clearcoat_N`: a coat (cornea) normal separate from the iris normal. — [Fab listing](https://www.fab.com/listings/31c329e0-58a1-4e1e-ac29-0a3b3678299f?lang=es-mx)
- The project's eye path currently uses `uv = (cosine from the eye's forward axis, 1 + how much white shows)` with plain colour and roughness 0.05. — project `coat_fragment.wesl` (eye branch, ~line 320)
- No Bevy crate for skin SSS or eye shading turned up. The Bevy shader crates found were MToon (toon), CRT, wisp and similar. — [search: bevy_mtoon](https://rust-digger.code-maven.com/crates/bevy_mtoon), [bevy_wisp](https://docs.rs/bevy_wisp)

### Inferences
**WGSL eye recipe.** All of it is analytic in eye-local space, with no textures:
1. **Eye frame**: pass the eye centre, forward axis F and radius R per instance. The project already has the forward-axis cosine. Local position: `p = world - centre`.
2. **Cornea bulge**: the cornea is a sphere cap of radius about 0.8R, centred about 0.3R ahead along F, covering where `dot(normalize(p), F) > cos(iris_angle)`. Use its normal as `clearcoat_N`. That gives a moving highlight offset from the sclera highlight, which is the main "alive eye" cue.
3. **Refraction / parallax iris**: compute the refracted ray `t = refract(-V, Ncornea, 1.0/1.376)`. Intersect it with the iris plane at depth `d` behind the cornea apex, about 0.15–0.25 of the cornea radius in mammals. `iris_uv = (hit - plane_centre)` projected onto plane axes, converted to polar coordinates (r, θ). This is the Jimenez/UE "physically based refraction" approach, and it also gives the correct iris shift at grazing angles. A cheaper fallback is parallax: `iris_uv = p_plane + V_tangent * depth / dot(V,F)`.
4. **Pupil shapes**, as a distance in pupil space scaled by `aperture`:
   - round: `length(q)`
   - vertical slit (cats, vipers, crocodiles): `length(q * vec2(1/slit_w, 1))`, or a superellipse `pow(|x|/a, n) + pow(|y|/b, n)`
   - horizontal (goats, sheep, octopus): swap axes
   - W / U (cuttlefish, geckos): superellipse plus a sine-warped lower edge.

   Animate dilation with `aperture`. Slit eyes compress mostly along x.
5. **Iris pattern**: radial fibres `fbm(θ·k, r)` stretched radially; a collarette ring at about r = 0.35; crypts as cellular holes. Darken the limbal ring with `smoothstep(0.85, 1.0, r)·0.6`. Add a soft dark rim (about 0.05R) at the iris/sclera boundary. That rim reads as the limbus, and it reads more strongly in younger or more "cute" eyes.
6. **Caustic**: brighten the iris on the side opposite the light, `max(0, dot(-L_tangent, dir_from_centre))·smoothstep(...)`. It is cheap, but it sells the refraction.
7. **Sclera**: wrap diffuse, low-frequency pink/yellow tint, vein lines (warped value-noise ridges), and a fade to darker near the lids. Roughness about 0.15 under clearcoat 1.0.
8. **Wet meniscus / tear line**: a thin strip where the lid meets the eye. Use the eye-lid SDF distance, which is available since the head is an SDF. Give it strong clearcoat with a normal tilted toward the lid, which produces the bright bead of light along the lower lid.
9. **Lid AO and shadow**: the eye must be occluded by lids, or it will look pasted on. Multiply `diffuse_occlusion` and `specular_occlusion` by a function of SDF distance to the lid surface (baked or analytic). Contact shadows (0.19+) also help here.
10. **Tapetum lucidum** (cats, dogs, deer, crocodiles): add `emissive`-like retroreflection `tapetum_color · pow(saturate(dot(L, V)), 64) · light` inside the pupil only. Yellow-green for cats, blue-green for dogs, red for crocodiles. The "flash photo eyeshine" needs a light nearly coaxial with the camera.
11. **Bird and reptile specifics**: birds often have a large dark iris with a flat cornea, and a nictitating membrane that is a semi-transparent lid layer. Reptile eyes often have no visible sclera, so the iris fills the visible eye. The project already handles a larger iris for "most animals".
12. **Bevy settings**: set the eye base material `clearcoat > 0` to enable the coat path. Base `perceptual_roughness` about 0.3–0.4 for the iris (it is under the cornea, so it should not be mirror-like), coat roughness about 0.02–0.05, and `reflectance` about 0.5 (cornea IOR ≈ 1.376, F0 ≈ 0.025). Turn on specular AA. Tiny eyes at distance shimmer badly otherwise.

### Gaps
- I could not access Jimenez's GDC 2013 slide contents (a PowerPoint download) or Epic's MetaHuman eye documentation. The step-by-step recipe above is built from general knowledge of those approaches, not quoted from them. Verify the iris depth and cornea IOR constants against the MetaHuman docs or the slides before treating them as authoritative.

## 6. Membrane and insect-wing shading: translucency, veins, thin-film iridescence

### Takeaway
The project already does about 80 % of a good membrane shader: veins, a fast-translucency backlit glow, native diffuse transmission (base 0.5 on wings), and ad-hoc thin-film on insect wings. The highest-value upgrades are:
1. Thickness-aware absorption colour (Beer–Lambert) instead of a fixed glow tint.
2. Shadow-correct back lighting via `TransmittedShadowReceiver`, plus the `thickness` offset.
3. Veins with their own thickness, so they occlude backlight.
4. Physically phased thin-film for insect wings, with specular AA.
5. For see-through insect wings, a choice between alpha-blend/dither and `specular_transmission` (screen-space, expensive).

### Cited Findings
- `diffuse_transmission` above 0.5 gives a darker appearance on the lit side than the opposite side, "e.g. plant leaves". The back lobe receives shadows only with `TransmittedShadowReceiver`. — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- The back lobe is lit at `world_position − world_normal·thickness`, so a non-zero `thickness` makes shadow tests on the far side meaningful for thicker membranes. — [pbr_functions.wesl](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/render/pbr_functions.wesl)
- `attenuation_distance` and `attenuation_color` exist (Beer's law for volume transmission), but they are used in the specular transmission path. — [pbr_material.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/pbr_material.rs)
- Specular transmission is screen-space, needs `ScreenSpaceTransmission` on the camera, and renders in `Transmissive3d`. Transmissive objects seen through other transmissive objects need `steps > 1`, with a texture copy per step. — [transmission/mod.rs](https://docs.rs/crate/bevy_pbr/0.20.0/source/src/transmission/mod.rs)
- Thin-film references: Belcour & Barla 2017, and Kneiphof et al. 2019 for rough films. — [Unity blog](https://blog.unity.com/es/technology/a-practical-extension-to-microfacet-theory-for-the-modeling-of-varying-iridescence), [EG diglib](https://diglib.eg.org/bitstreams/4f73c3ce-cde6-45be-bad6-2b61477d477b/download)
- The project's insect-wing branch already uses longitudinal veins, cross-vein cells, costa/pterostigma, thin-film phase from per-cell thickness and view angle, and dithered alpha about 0.22. The membrane branch uses vein lines, thickness-based transmit (0.7→0.05) and a warm glow tint. — project `coat_fragment.wesl` (wing branch)

### Inferences
- **Backlit membrane colour**: replace the fixed `glow_tint` with Beer–Lambert transmittance, `T = exp(-sigma_rgb * thickness_mm)`, where `sigma_rgb` is high in blue and green and low in red (blood). Thin membranes then glow orange-red, thick areas and veins go dark, and the colour saturates naturally as thickness rises. Multiply the result into both the fast-translucency term and the base colour used by the back lobe. Bevy multiplies the back lobe by `base_color`, so `diffuse_transmission` alone cannot tint it differently from the front. The custom glow is where the tint lives.
- **Veins**: treat each vein as thickness, so `thickness += vein·k`. They then block backlight and appear as dark lines against the light while reading lighter or raised in front light, via the bump. Add a slight blood-red emissive-like scatter right next to veins. Bat wings have elastin fibres and muscle bands (plagiopatagiales) running roughly parallel to the trailing edge. A fine anisotropic striation detail in that direction adds realism. The direction is already available as the "rib/shaft/wing direction" tangent.
- **Shadowing**: add `TransmittedShadowReceiver` to wing entities so the wing bones' and body's shadows appear on the backlit side. The project's fast-translucency term ignores shadows, so multiply it by the directional shadow (`shadows::fetch_directional_shadow` with the back-offset position) to avoid glow through occluders.
- **Insect wings**:
  - Thin-film phase `φ = 4π·n·d·cosθ_t/λ` per channel with `n ≈ 1.56` (chitin) and `d` ≈ 100–700 nm varying smoothly per cell. That gives the rainbow "soap-film" bands real dragonfly and fly wings show.
  - Apply the film to specular reflectance (`pbr.material.reflectance` vec3 or `specular_tint`), not to albedo. Keep albedo almost black or transparent for clear wings.
  - Fade iridescence amplitude with the same `detail()` footprint to prevent RGB sparkle at distance.
  - Transparency: keep dithered alpha or alpha-to-coverage (`AlphaMode::AlphaToCoverage` exists in the flag bits). Only use `specular_transmission` if refraction of what is behind is needed, because wings are too thin for visible refraction. Dithered alpha should be paired with TAA, which is covered by the post-processing researcher.
- **Dragon/bat membrane specular**: leathery, roughness 0.5–0.65, with a mild clearcoat (about 0.2) on the dorsal side for a fresh or wet look. Specular AA matters because membranes are thin and curved and spark at silhouettes.

### Gaps
- I found no Bevy-specific community implementations of membrane or insect-wing shaders.
- Measured absorption coefficients for bat-wing membrane were not sourced. The σ values must be tuned by eye.

## Summary of priorities (inference)
1. **Correctness first.** Enable shader defs on base materials: `clearcoat: 1e-3` on skin, eye and wet layers; `diffuse_transmission: 1e-3` on skin layers that need ears or frills to glow; anisotropy only with tangents and the `pbr_anisotropy_texture` feature.
2. **Specular AA plus variance-to-roughness**: about 10 lines, the biggest close-up and mid-distance stability gain.
3. **Eyes**: cornea `clearcoat_N`, refracted procedural iris, pupil shapes, limbus, lid AO, meniscus. This is the largest perceived-realism gain per hour.
4. **Scales**: per-scale normal tilt, F2−F1 crevice occlusion feeding `diffuse_occlusion`/`specular_occlusion`, and baked SDF AO and curvature per vertex.
5. **Skin**: curvature-driven wrap or pre-integrated additive term, and SDF-derived thickness for ears and membranes with Beer–Lambert colour.
6. **Wet layer**: clearcoat with a smoothed coat normal, darkening, and a mucous recipe.
7. **Optional big-ticket item**: a separable screen-space SSS render pass. It needs a diffuse/specular split that Bevy does not provide.
