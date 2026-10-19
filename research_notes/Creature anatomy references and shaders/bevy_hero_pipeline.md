# Bevy 0.20 hero-shot render pipeline: camera, lighting, post-processing

Method note: most API facts below were checked directly against the Bevy 0.20.0 crate sources in the local cargo registry (`~/.cargo/registry/src/*/bevy_*-0.20.0`, the same code the project builds against). They are cited as GitHub paths at tag `v0.20.0`; the URLs follow the repo layout and their line numbers may differ slightly. Release-note facts come from bevy.org. Bevy 0.20 was released on 2026-10-08 ([bevy_solari on docs.rs](https://docs.rs/crate/bevy_solari/latest)).

## Q0: Cargo features with `default-features = false, features = ["3d", ...]`

### Takeaway
The `3d` feature already brings in `bevy_anti_alias`, `bevy_post_process`, `bevy_pbr`, `smaa_luts`, `tonemapping_luts` and `ktx2`/`zstd_rust`. Several hero-shot features are opt-in, though: `experimental_pbr_pcss` (soft shadows), `area_light_luts` (`RectLight`), `bluenoise_texture` (better SSR noise), `dfg_lut`, `dlss`, `bevy_solari`, `pan_orbit_camera`/`free_camera`, `hdr`/`exr` (loading .hdr/.exr images).

### Cited Findings
- In the bevy 0.20 Cargo.toml, `3d = [default_app, default_platform, 3d_bevy_render, scene, picking]`. `3d_bevy_render` adds `3d_api, bevy_render, bevy_core_pipeline, bevy_gizmos_render, bevy_anti_alias, bevy_gltf, bevy_pbr, bevy_post_process, gltf_animation`. `3d_api` includes `smaa_luts`, `tonemapping_luts`, `ktx2`, `zstd_rust` — [bevy/Cargo.toml v0.20.0](https://github.com/bevyengine/bevy/blob/v0.20.0/Cargo.toml)
- These are separate opt-in features: `experimental_pbr_pcss`, `area_light_luts` (pulls in ktx2 + zstd), `bluenoise_texture`, `dfg_lut`, `dlss` (→ `bevy_anti_alias/dlss`, `bevy_solari?/dlss`), `force_disable_dlss`, `bevy_solari`, `meshlet`, `hdr`, `exr`, `free_camera`, `pan_camera`, `pan_orbit_camera` (+ `extension_independent_skybox`), `pan_orbit_gizmo` — [bevy/Cargo.toml v0.20.0](https://github.com/bevyengine/bevy/blob/v0.20.0/Cargo.toml), [bevy_internal/Cargo.toml v0.20.0](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_internal/Cargo.toml)
- `Tonemapping::AgX`, `TonyMcMapface` (the default) and `BlenderFilmic` need the `tonemapping_luts` feature — [bevy_render view/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/view/mod.rs)
- `RectLight` needs the `area_light_luts` cargo feature — [bevy_light rect_light.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/rect_light.rs)
- `DirectionalLight::soft_shadow_size` and `PointLight`/`SpotLight::soft_shadows_enabled` only exist under `#[cfg(feature = "experimental_pbr_pcss")]` — [bevy_light directional_light.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/directional_light.rs), [point_light.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/point_light.rs)
- "Enable the `bluenoise_texture` feature to improve the quality of noise on rough reflections" (SSR) — [bevy_pbr ssr/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/ssr/mod.rs)

### Inferences
- A suggested hero-mode feature set for the dev/viewer build: `["3d", "serialize", "png", "experimental_pbr_pcss", "area_light_luts", "bluenoise_texture", "hdr", "pan_orbit_camera"]`. Optionally add `"exr"`, `"dfg_lut"`, and gated `"dlss"`/`"bevy_solari"` behind a project cargo feature such as `hero`, so normal builds stay lean.
- `dfg_lut` probably improves the specular DFG term (a split-sum LUT). Only the feature name was confirmed, not what it does.

### Gaps
- The exact visual effect of `dfg_lut` was not verified.

## Q1: Anti-aliasing (TAA / SMAA / FXAA / MSAA / DLSS) and motion vectors for custom vertex deformation

### Takeaway
For fine fur and scale detail with stochastic effects (SSAO, PCSS, temporal shadow filtering), use `TemporalAntiAliasing` with `Msaa::Off`. It is the only built-in option that removes specular and shader aliasing and denoises SSAO and soft shadows. It requires correct motion vectors, and the project's current prepass does not run the custom deformation, so this must be fixed first. For truly still hero frames, the best quality comes from accumulating many jittered frames offline, or from rendering at 2–4× resolution and downsampling. MSAA only helps geometry edges and can't be combined with TAA or deferred rendering.

### Cited Findings
- `TemporalAntiAliasing { reset: bool }` (default `reset: true`) has `#[require(TemporalJitter, MipBias, DepthPrepass, MotionVectorPrepass)]`. The docs say: "Any camera with this component must also disable `Msaa` by setting it to `Msaa::Off`." At runtime it warns "Temporal anti-aliasing requires MSAA to be disabled" and skips. "TAA also does not work well with alpha-blended meshes, as it requires depth writing to determine motion." "It is very important that correct motion vectors are written for everything on screen. Failure to do so will lead to ghosting." Listed cons: ghosting, and "Thin geometry, lighting detail, or texture lines may flicker noisily or disappear." Listed pros: filters texture and specular aliasing and "Greatly increases the quality of stochastic rendering techniques such as SSAO, certain shadow map sampling methods" — [bevy_anti_alias taa/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_anti_alias/src/taa/mod.rs)
- TAA runs in `Core3dSystems::EarlyPostProcess`. In 0.19 the render graph was replaced by systems (`add_systems` on `Core3d`) — [taa/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_anti_alias/src/taa/mod.rs), [Bevy 0.19 release notes](https://bevy.org/news/bevy-0-19/)
- SMAA: `Smaa { preset: SmaaPreset }` with presets `Low` (4 search steps), `Medium` (8), `High` (16 + 8 diagonal + corner detection, the default), `Ultra` (32 + 8 diagonal + corners) — [bevy_anti_alias smaa/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_anti_alias/src/smaa/mod.rs)
- FXAA: `Fxaa` component with a `Sensitivity` enum. `ContrastAdaptiveSharpening` (+ `DenoiseCas`) is available as a sharpening pass after TAA or DLSS — [bevy_anti_alias src](https://github.com/bevyengine/bevy/tree/v0.20.0/crates/bevy_anti_alias/src)
- MSAA: `Msaa` component, variants `Off`, `Sample2`, `Sample4`, `Sample8` — [bevy_render view/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/view/mod.rs). Deferred rendering comes "at the cost of not being able to use MSAA" — [bevy_material opaque.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_material/src/opaque.rs)
- DLSS: "Requires a NVIDIA RTX GPU, and the Windows/Linux Vulkan rendering backend." Usage steps: (1) enable the `dlss` feature; (2) insert the `DlssProjectId(Uuid)` resource before `DefaultPlugins`; (3) check `Option<Res<DlssSuperResolutionSupported>>`; (4) add `Dlss` to the camera with `perf_quality_mode: DlssPerfQualityMode` (default `Auto`); (5) optionally add `ContrastAdaptiveSharpening`; (6) custom render code must respect `MainPassResolutionOverride`. `Dlss<F = DlssSuperResolutionFeature>` has `#[require(TemporalJitter, MipBias, DepthPrepass, MotionVectorPrepass, Hdr)]`. `DlssRayReconstructionFeature` / `DlssRayReconstructionSupported` handle Solari denoising — [bevy_anti_alias dlss/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_anti_alias/src/dlss/mod.rs)
- In 0.20, `dlss_wgpu` supports DLSS-RR 4.5 and "requires the newest DLSS SDK"; users upgrading from 0.19 may get compile errors otherwise — [Bevy 0.20 release notes](https://bevy.org/news/bevy-0-20/)
- Bevy's prepass vertex shader computes `previous_world_position` from `get_previous_world_from_local` (or `skin_prev_model` when `SKINNED && HAS_PREVIOUS_SKIN`, `morph_prev_vertex` for morphs). The fragment shader writes `motion_vector = (clip - previous_clip) * vec2(0.5, -0.5)` using `unjittered_clip_from_world` and `previous_view_uniforms` — [bevy_pbr prepass/prepass.wesl](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/prepass/prepass.wesl)
- `MaterialExtension` exposes `prepass_vertex_shader()`, `prepass_fragment_shader()`, `deferred_vertex_shader()`, `deferred_fragment_shader()`, `enable_prepass()`, `enable_shadows()` — [bevy_pbr extended_material.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/extended_material.rs)
- **Project-specific finding:** `CoatExt` (in `/home/chousx/Projects/ornisculpt/src/coat/material.rs`) overrides only `vertex_shader()` and `fragment_shader()`. Its `specialize` deliberately leaves out `COAT_COR` and `COAT_MUSCLE` when the pipeline label ends in `prepass_pipeline` ("The shadow and depth prepass keeps Bevy's shader and vertex layout"). The depth, normal, motion-vector and shadow passes therefore see *undeformed* (non-CoR, non-muscle-bulged) positions. Crowd DQS skinning (`bones` storage buffer) is also not reproduced in Bevy's prepass shader — local source, `src/coat/material.rs`.
- `TemporalJitter { offset: Vec2 }` (range [-0.5, 0.5]) is a public component with `jitter_projection()`. `MipBias(f32)` is also public — [bevy_render camera.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/camera.rs)

### Inferences
- **Must-fix before TAA, SSAO, contact shadows or DoF look right:** implement `prepass_vertex_shader()` (and `deferred_vertex_shader()` if deferred is used) for `CoatExt`. It should run the same CoR/DQS/muscle deformation and also compute `previous_world_position` from *previous-frame* bone matrices and muscle amplitudes. That means keeping a second "prev bones" storage buffer, double-buffered each frame. Otherwise:
  - the main pass depth-tests against a mismatched prepass depth (holes and z-fighting);
  - SSAO and contact shadows use wrong normals and depth;
  - shadows are cast by the rest-pose silhouette;
  - TAA ghosts on any animated limb.
- An alternative for stills only: freeze animation in hero mode. Prev and current positions are then equal, so motion vectors are zero. The prepass still has to deform the geometry to match depth.
- Fur shells (stacked alpha-tested or blended layers) and feather cards: TAA handles alpha-*masked* (depth-writing) surfaces. Alpha-*blended* shells produce no motion vectors and ghost or smear. Prefer `AlphaMode::Mask`/alpha-to-coverage style dithering plus TAA in hero mode.
- Recommended hero AA for an interactive orbit: `TemporalAntiAliasing` + `Msaa::Off` + `ShadowFilteringMethod::Temporal` + `ContrastAdaptiveSharpening` (low strength). For a final still, freeze time and accumulate N frames (see Q5). Use SMAA `Ultra` as a no-motion-vector fallback (lower quality on sub-pixel fur). DLSS (as DLAA-like quality mode) is NVIDIA-only and needs the same motion-vector correctness.

### Gaps
- I did not confirm whether `DlssPerfQualityMode` has a `Dlaa` (native-resolution) variant in dlss_wgpu for 0.20. The source only re-exports `dlss_wgpu::DlssPerfQualityMode`.
- I did not confirm whether Bevy 0.20 TAA has settings beyond `reset` (e.g. history blend rate). Only `reset` is a public field.

## Q2: Ambient occlusion, contact shadows, shadow quality

### Takeaway
Use `ScreenSpaceAmbientOcclusion { quality_level: Ultra }` (a GTAO / visibility-bitmask implementation) together with TAA. Add `ContactShadows` on the camera plus `contact_shadows_enabled: true` on the key light for skin and scale crevices. For shadow maps: `DirectionalLightShadowMap { size: 4096 }` with a tight `CascadeShadowConfigBuilder` around the creature, and `ShadowFilteringMethod::Temporal` with TAA (or `Gaussian` without it). PCSS soft shadows come from `soft_shadow_size` or `soft_shadows_enabled` behind `experimental_pbr_pcss`.

### Cited Findings
- `ScreenSpaceAmbientOcclusion { quality_level, radius (default 0.7285 view-space units), constant_object_thickness (default 0.25) }` has `#[require(DepthPrepass, NormalPrepass)]`. Quality levels and samples per pixel: `Low` (1 slice × 2 = 4 spp), `Medium` (8 spp), `High` (default, 18 spp), `Ultra` (9 slices × 3 = 54 spp), `Custom { slice_count, samples_per_slice_side }`. "It strongly recommended that you use SSAO in conjunction with TAA". It is "not supported on WebGL2, and is not currently supported on WebGPU". SSAO affects only indirect diffuse, not direct lights — [bevy_pbr ssao/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/ssao/mod.rs)
- The SSAO shader is "heavily based on XeGTAO v1.30" and uses a sector `bitmask` with `updateSectors` (a visibility-bitmask variant) — [bevy_pbr ssao/ssao.wesl](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/ssao/ssao.wesl)
- Contact shadows (added in 0.19): the camera component is `ContactShadows { linear_steps: 16, thickness: 0.1, length: 0.3 }` (defaults), with `#[require(DepthPrepass)]`. It works in forward or deferred. Per-light flag: `contact_shadows_enabled` on `DirectionalLight`, `PointLight`, `SpotLight`. Cost scales with lit pixels × number of contact-shadow lights — [bevy_pbr contact_shadows.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/contact_shadows.rs), [Bevy 0.19 release notes](https://bevy.org/news/bevy-0-19/)
- `ShadowFilteringMethod` (camera component) has three variants:
  - `Hardware2x2`: "Fast but poor quality".
  - `Gaussian`: Castaño/The Witness 9-sample, equivalent to 5×5; "good when TAA isn't in use".
  - `Temporal`: Jimenez/CoD:AW 8-sample spiral rotated by interleaved gradient noise; "good when TAA is in use".
  — [bevy_light lib.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/lib.rs)
- `DirectionalLight` fields (0.20): `color`, `illuminance` (default `AMBIENT_DAYLIGHT` = 10,000 lux), `shadow_maps_enabled`, `contact_shadows_enabled`, `soft_shadow_size: Option<f32>` (PCSS, feature-gated), `affects_lightmapped_mesh_diffuse`, `shadow_depth_bias` (default 0.02), `shadow_normal_bias` (default 1.8). The PCSS doc says soft shadows "are rather noisy if not using the temporal mode… consider choosing `ShadowFilteringMethod::Temporal` and enabling … TAA" and are "significantly more expensive". `DirectionalLightTexture { image, tiled }` adds a cookie/gobo — [directional_light.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/directional_light.rs)
- `DirectionalLightShadowMap { size }` (default 2048, must be a power of two) controls the resolution of directional and spot shadow maps. Cascades are set with `CascadeShadowConfig`, built through `CascadeShadowConfigBuilder { num_cascades, minimum_distance, maximum_distance, first_cascade_far_bound, overlap_proportion }.build()` — [directional_light.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/directional_light.rs), [cascade.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/cascade.rs)
- **Rename:** `shadows_enabled` (0.18) became `shadow_maps_enabled` (0.19+). The project viewer already uses `shadow_maps_enabled` — [bevy_light 0.18.1 vs 0.19.1 directional_light.rs, local registry](https://github.com/bevyengine/bevy/blob/v0.19.1/crates/bevy_light/src/directional_light.rs)
- `NotShadowCaster`, `NotShadowReceiver`, `TransmittedShadowReceiver` (needs `diffuse_transmission > 0` and a tuned `thickness`) are available per mesh — [bevy_light lib.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/lib.rs)

### Inferences
- For a creature about 1–3 m in frame, set `num_cascades: 1`, `maximum_distance` of roughly 5–10 m, and `size: 4096` or 8192. All texels then land on the subject, which gives far crisper fur and scale self-shadowing than the default multi-cascade setup.
- Lower `shadow_normal_bias` and `shadow_depth_bias` in hero mode, since thin shells and feathers are prone to "Peter Panning". This has to be tuned per scene.
- Shells and feathers could be marked `NotShadowCaster` for all but the inner layers. Contact shadows then supply near-surface darkening cheaply.
- Contact shadows and SSAO both depend on the depth prepass matching the deformed mesh (see Q1).

### Gaps
- I did not verify whether `ScreenSpaceAmbientOcclusion` works with `Msaa` other than `Off` in 0.20. Historically it required MSAA off, but no explicit check was found in the 0.20 source.
- There is no built-in "temporal soft shadow" setting beyond PCSS plus `Temporal` filtering.

## Q3: Lighting rigs, IBL, atmosphere, area lights, fog, SSR, Solari

### Takeaway
Build a three-point rig from a shadowed `DirectionalLight` or `SpotLight` as key, a large `RectLight` as soft fill (it casts no shadows), and a rim `SpotLight` or `PointLight`. Add image-based lighting through `GeneratedEnvironmentMapLight` (from one HDR cubemap) or `AtmosphereEnvironmentMapLight` (procedural sky), and a `Skybox` or `Atmosphere` background. SSR needs deferred rendering. `VolumetricFog` + `VolumetricLight` + `FogVolume` give light shafts. Solari is available but experimental and **StandardMaterial-only**, so it is incompatible with the project's `ExtendedMaterial`.

### Cited Findings
- `RectLight { color, intensity (lumens, default VERY_LARGE_CINEMA_LIGHT), range (20), width, height }` lies in local XY and faces −Z. "Shadow maps are currently unsupported". It requires `area_light_luts` — [rect_light.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/rect_light.rs). It is implemented with Linearly Transformed Cosines, with no shadows and no anisotropic material support (added in 0.19) — [Bevy 0.19 release notes](https://bevy.org/news/bevy-0-19/)
- IBL components (bevy_light probe.rs):
  - `EnvironmentMapLight { diffuse_map, specular_map, intensity, … }` takes prefiltered maps.
  - `GeneratedEnvironmentMapLight { environment_map: Handle<Image>, intensity, rotation: Quat, affects_lightmapped_mesh_diffuse }` filters a plain cubemap at runtime.
  - `AtmosphereEnvironmentMapLight { intensity, affects_lightmapped_mesh_diffuse, size: UVec2 }` derives IBL from the procedural atmosphere.
  - `Skybox { image, brightness, rotation… }`, `LightProbe`, `IrradianceVolume { voxels, intensity }`.
  — [bevy_light probe.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/probe.rs)
- In 0.19, light probes gained parallax-corrected cubemaps by default (based on the probe's influence box). Opt out with `ParallaxCorrection::None` — [Bevy 0.19 release notes](https://bevy.org/news/bevy-0-19/)
- The built-in atmosphere uses `Atmosphere { inner_radius, outer_radius, … }` with a `ScatteringMedium` asset (`terms`, `falloff_resolution`, `phase_resolution`) and `AtmosphereSettings { transmittance_lut_size, multiscattering_lut_size, sky_view_lut_size, aerial_view_lut_size, … }` — [bevy_light atmosphere.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/atmosphere.rs), [bevy_pbr atmosphere/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/atmosphere/mod.rs)
- Volumetric fog: `VolumetricFog { ambient_color, ambient_intensity, jitter, step_count }` on the camera ("Requires using WebGPU on Wasm builds"). `VolumetricLight` is a marker on lights that produce shafts. `FogVolume { fog_color, density_factor, density_texture, density_texture_offset, absorption, scattering, scattering_asymmetry, light_tint, light_intensity }` — [bevy_light volumetric.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_light/src/volumetric.rs)
- `ScreenSpaceReflections { min_perceptual_roughness: Range, max_perceptual_roughness: Range, thickness, … }` has `#[require(DepthPrepass, DeferredPrepass)]`. "Currently only supported with deferred rendering… deferred rendering itself is not automatically enabled." It can only reflect what is on screen — [bevy_pbr ssr/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/ssr/mod.rs)
- Solari in 0.20:
  - Faster and "mostly unbiased", runs on macOS via Metal (but with no built-in denoiser there).
  - ReSTIR is off by default (`SolariLighting::restir` re-enables it).
  - New `SolariLighting` knobs: world cache size, light samples per pixel, temporal accumulation, bounce count.
  - Light sources: `DirectionalLight`, emissive meshes, and (new) `Atmosphere` and `EnvironmentMapLight`. `PointLight`, `SpotLight` and `RectLight` are not yet supported.
  — [Bevy 0.20 release notes](https://bevy.org/news/bevy-0-20/)
- Solari requirements: `SolariPlugins` is "An experimental set of plugins for raytraced lighting" and needs wgpu `EXPERIMENTAL_RAY_QUERY`, `BUFFER_BINDING_ARRAY`, `TEXTURE_BINDING_ARRAY`, `SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING`, `PARTIALLY_BOUND_BINDING_ARRAY`. An optional non-realtime pathtracer exists "for validation purposes" — [bevy_solari lib.rs v0.20.0](https://raw.githubusercontent.com/bevyengine/bevy/v0.20.0/crates/bevy_solari/src/lib.rs). `RaytracingMesh3d` meshes must have `Mesh::enable_raytracing` set to true, use exactly `{POSITION, NORMAL, UV_0, TANGENT}`, `TriangleList`, and `Indices::U32`; "The material used for this entity must be `MeshMaterial3d<StandardMaterial>`" — [bevy_solari scene/types.rs v0.20.0](https://raw.githubusercontent.com/bevyengine/bevy/v0.20.0/crates/bevy_solari/src/scene/types.rs)
- docs.rs failed to build bevy_solari 0.20.0 docs (the last successful build is 0.18.1) — [docs.rs bevy_solari](https://docs.rs/crate/bevy_solari/latest)
- `GlobalAmbientLight { brightness, .. }` is the resource the viewer uses (it was renamed from `AmbientLight` in earlier versions) — local `examples/viewer.rs`

### Inferences
- Solari is not usable for ornisculpt creatures as-is. They use `ExtendedMaterial<StandardMaterial, CoatExt>`, custom vertex attributes and GPU-side deformation, and the BLAS is built from the CPU mesh, so the custom deformation would not be reflected. At most, use the static ground or environment with Solari, which has little hero value. Treat it as a future option.
- Suggested hero rig, at roughly physical scale with `Exposure`:
  - key `DirectionalLight` (about 20–50k lux, `shadow_maps_enabled`, `contact_shadows_enabled`, `soft_shadow_size: Some(~0.5–2°-equivalent)`);
  - fill `RectLight` (2×2 m, off-axis, about 1/4 of key, no shadows);
  - rim `SpotLight` behind and above the creature (`shadow_maps_enabled`, which highlights fur silhouettes);
  - plus `GeneratedEnvironmentMapLight` from a studio HDRI (needs the `hdr`/`exr` feature for .hdr/.exr, or a ktx2 cubemap) at a low intensity, for specular on scales and eyes.
- The `bevy_atmosphere` community crate is unnecessary now that a built-in `Atmosphere` + `AtmosphereEnvironmentMapLight` exist.

### Gaps
- I did not verify the exact field list of `SolariLighting` or its hardware vendor support beyond the wgpu feature list.
- `RectLight` with `ExtendedMaterial`: presumably it works through the shared PBR lighting functions, but this was not verified.

## Q4: Post-processing (DoF, bloom, tonemapping, grading, exposure, CA, grain, vignette)

### Takeaway
Hero post stack: `Hdr`, then `Tonemapping::AgX` or `TonyMcMapface` (or `KhronosPbrNeutral` for colour-faithful product shots), `ColorGrading`, `Exposure` from `PhysicalCameraParameters`, and `DepthOfField { mode: Bokeh }` derived from the same physical camera. Add subtle `Bloom::NATURAL`, a light `Vignette`, and very light `ChromaticAberration`. There is no built-in film grain in 0.20; it would need a custom fullscreen pass.

### Cited Findings
- `DepthOfField { mode, focal_distance (m, default 10), sensor_height (default 18.66 mm Super 35), aperture_f_stops, max_circle_of_confusion_diameter (default 64 px), max_depth (default INFINITY) }`. `DepthOfFieldMode::Bokeh` is "more accurate… circles of confusion generate spots"; `Gaussian` (the default) is faster and "generally less aesthetically pleasing". `DepthOfField::from_physical_camera(&PhysicalCameraParameters)` matches `Exposure::from_physical_camera`. The CoC scale is `focal_length² / (sensor_height * aperture_f_stops)`, and focal length is derived from the FOV and sensor height — [bevy_post_process dof/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_post_process/src/dof/mod.rs)
- `PhysicalCameraParameters { aperture_f_stops (default 1.0), shutter_speed_s (1/125), sensitivity_iso (100), sensor_height (0.01866) }` with `.ev100()`. `Exposure { ev100 }` has presets `SUNLIGHT` (15), `OVERCAST` (12), `INDOOR` (7), `BLENDER` — [bevy_camera camera.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_camera/src/camera.rs)
- `Tonemapping` variants: `None`, `Linear`, `Reinhard`, `ReinhardLuminance`, `AcesFitted`, `AgX` (LUT), `SomewhatBoringDisplayTransform`, `TonyMcMapface` (the default, LUT), `BlenderFilmic` (LUT), `KhronosPbrNeutral`. `DebandDither` has `Disabled` (default) and `Enabled` — [bevy_render view/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/view/mod.rs)
- `ColorGrading { global: ColorGradingGlobal { exposure, … }, shadows/midtones/highlights: ColorGradingSection }` — [bevy_render view/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/view/mod.rs)
- `Hdr` is a marker component on the camera. It affects only the intermediate texture; HDR display output is not supported — [bevy_camera components.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_camera/src/components.rs)
- `Bloom` (`#[require(Hdr)]`) has fields `intensity`, `low_frequency_boost`, `low_frequency_boost_curvature`, `high_pass_frequency`, `prefilter: BloomPrefilter { threshold, threshold_softness }`, `composite_mode: BloomCompositeMode`, `max_mip_dimension` (default 512), `scale: Vec2`. Presets: `NATURAL`, `ANAMORPHIC`, `OLD_SCHOOL`, `SCREEN_BLUR` — [bevy_post_process bloom/settings.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_post_process/src/bloom/settings.rs)
- `AutoExposure` (`#[require(Hdr)]`, needs compute, no WebGL2) has `range`, `filter`, `speed_brighten`, `speed_darken`, `exponential_transition_distance`, `metering_mask: Handle<Image>`, `compensation_curve: Handle<AutoExposureCompensationCurve>` — [auto_exposure/settings.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_post_process/src/auto_exposure/settings.rs)
- The effect stack (bevy_post_process `effect_stack`) contains:
  - `ChromaticAberration { color_lut: Option<Handle<Image>>, intensity, max_samples }`;
  - `Vignette { intensity, radius, smoothness, roundness, center, edge_compensation, color }`, new in 0.19;
  - `LensDistortion { intensity (+ barrel / − pincushion), scale, multiplier, center, edge_curvature }`, new in 0.19.
  No film grain module exists in 0.20 (none found in bevy_post_process or bevy_core_pipeline) — [effect_stack src](https://github.com/bevyengine/bevy/tree/v0.20.0/crates/bevy_post_process/src/effect_stack), [Bevy 0.19 release notes](https://bevy.org/news/bevy-0-19/)
- `MotionBlur { shutter_angle, samples }` has `#[require(MotionVectorPrepass)]` — [motion_blur/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_post_process/src/motion_blur/mod.rs)

### Inferences
- Auto-focus: each frame, set `DepthOfField::focal_distance` to the camera-space distance to the creature's head or eye anchor (e.g. `(eye_world - cam.translation).dot(cam.forward())`), smoothed. A raycast from screen centre via `MeshRayCast` (picking) is an alternative. Use `Bokeh` with f/2.8–f/5.6 for portrait separation. A small creature at close range gives a shallow depth of field.
- Use `AutoExposure` only for interactive orbiting. Fixed `Exposure` keeps stills consistent.
- Keep `DebandDither::Enabled` in hero mode to avoid banding in soft studio gradients.
- Film grain can be added as a `FullscreenMaterial`-style pass (bevy_core_pipeline has `fullscreen_material.rs`) after tonemapping. This is unverified.

### Gaps
- I did not verify whether `DepthOfField` requires a depth prepass (no `#[require]` was found) or how it behaves with TAA ordering.
- The full `ColorGradingGlobal` field list (temperature, tint, hue, post_saturation, midtones_range) was not re-verified for 0.20.

## Q5: Camera, lens, orbit, photo mode, high-res stills

### Takeaway
Bevy 0.20 ships a built-in `PanOrbitCamera` (feature `pan_orbit_camera`; it comes from `bevy_editor_cam`). For stills, use the `Screenshot` entity API with `save_to_disk`. For anti-aliased, supersampled stills, render to a large `Image` target and/or accumulate jittered frames by driving `TemporalJitter.offset` manually.

### Cited Findings
- `PanOrbitCamera` (from `bevy_editor_cam`) lives in `bevy_camera_controller`. Add `MeshPickingPlugin` and `DefaultPanOrbitCameraPlugins`, then attach the component to a 3D camera. See the example `camera/pan_orbit_camera_cad` — [Bevy 0.20 release notes](https://bevy.org/news/bevy-0-20/). Cargo features: `pan_orbit_camera`, `pan_orbit_gizmo`, `free_camera`, `pan_camera` — [bevy_internal Cargo.toml](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_internal/Cargo.toml)
- Screenshots: spawn `Screenshot(RenderTarget)` using `Screenshot::primary_window()`, `Screenshot::image(handle)` or `Screenshot::texture_view(..)`. Observe the `ScreenshotCaptured` event, e.g. `.observe(save_to_disk(path))`. Capture is asynchronous, and the entity is despawned afterwards — [bevy_render view/window/screenshot.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/view/window/screenshot.rs)
- `TemporalJitter { offset: Vec2 }` with an offset range of [-0.5, 0.5] pixels is a public camera component — [bevy_render camera.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_render/src/camera.rs)

### Inferences
- Lens: Bevy's `PerspectiveProjection.fov` is *vertical* FOV in radians. For portrait framing use about 20–30° vertical (roughly 50–85 mm full-frame-equivalent). That reduces nose and snout distortion; keep `near` small (around 0.01) for close-ups. The `DepthOfField` focal length is derived from this FOV and `sensor_height`, so narrowing the FOV also strengthens bokeh.
- Offline still recipe ("photo mode"):
  1. Pause `Time<Virtual>`, which freezes animation, fur sway and muscle flex, so motion vectors are zero.
  2. Spawn a second `Camera3d` rendering to an `Image` render target at 2–4× the output size, or the window.
  3. For N = 16–64 frames, either let TAA converge with a static camera (TAA + `ShadowFilteringMethod::Temporal` + SSAO/PCSS noise all integrate), or remove TAA, set `TemporalJitter.offset` to Halton(2,3) samples, read back each frame with `Screenshot::image`, and average on the CPU in linear space.
  4. Downsample with a Lanczos or box filter and save.
- A turntable can be a simple system rotating the camera about the creature's bounding-sphere centre. Set `TemporalAntiAliasing.reset = true` on camera cuts.

### Gaps
- I found no built-in photo-mode or multi-frame accumulation API in Bevy 0.20; it must be hand-built.
- I did not verify whether `Screenshot` captures before or after tonemapping for `Hdr` image targets (format concerns when using `Screenshot::image`).

## Q6: Community crates and forward vs deferred

### Takeaway
Little community post-processing is needed; Bevy 0.20 built-ins cover almost everything. I could not confirm 0.20-compatible releases for `bevy_mod_outline` or other outline/beauty crates, which typically lag a few weeks behind Bevy releases. Stay on forward rendering with a prepass (depth + normal + motion vectors). Use deferred only if SSR is essential, since deferred requires `deferred_vertex_shader`/`deferred_fragment_shader` support in `CoatExt` and forbids MSAA.

### Cited Findings
- The latest `bevy_mod_outline` version found is 0.13 (README snippet); no Bevy 0.20 support was confirmed — [bevy_mod_outline README](https://raw.githubusercontent.com/komadori/bevy_mod_outline/master/README.md), [lib.rs bevy_mod_outline](https://www.lib.rs/crates/bevy_mod_outline)
- Deferred writes a G-buffer, "at the cost of not being able to use MSAA, and has heavier bandwidth usage". `OpaqueRendererMethod::Auto` falls back to the `DefaultOpaqueRendererMethod` resource — [bevy_material opaque.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_material/src/opaque.rs)
- SSR only works with deferred — [ssr/mod.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/ssr/mod.rs). Contact shadows work in forward or deferred — [contact_shadows.rs](https://github.com/bevyengine/bevy/blob/v0.20.0/crates/bevy_pbr/src/contact_shadows.rs)
- 0.20 converted Bevy shaders to WESL: `#import` became `import …;` and `#ifdef` became `@if(...)`. The project already uses `.wesl` — [Bevy 0.20 release notes](https://bevy.org/news/bevy-0-20/)

### Inferences
- Forward is the better fit. The coat fragment shader does custom shading on top of StandardMaterial, which deferred would flatten into a G-buffer and lose (unless the work is moved into `deferred_fragment_shader` and the G-buffer packing). Forward also keeps the options of MSAA (non-TAA mode), alpha-blended shells, and `OrderIndependentTransparencySettings` (bevy_core_pipeline `oit`) for layered fur shells or wing membranes.
- `bevy_panorbit_camera` is less necessary now that `PanOrbitCamera` is built in.
- Suggested hero-mode camera bundle:
  - `Camera3d`, `Hdr`, `Msaa::Off`, `TemporalAntiAliasing::default()`
  - `ScreenSpaceAmbientOcclusion { quality_level: Ultra, .. }`, `ContactShadows::default()`, `ShadowFilteringMethod::Temporal`
  - `DepthOfField { mode: Bokeh, .. }.from_physical_camera(..)`, `Exposure::from_physical_camera(..)`
  - `Tonemapping::AgX`, `DebandDither::Enabled`, `Bloom::NATURAL` (with low intensity), `Vignette { intensity: small, .. }`, `ContrastAdaptiveSharpening`
  - `GeneratedEnvironmentMapLight { .. }`, plus `Skybox` or a neutral `ClearColor`
  - `Projection::Perspective` with fov about 25°.
  Toggling back to "fast" mode means removing these components (TAA reset handles history).

### Gaps
- I did not verify a 0.20-compatible version for `bevy_mod_outline`, `bevy_panorbit_camera`, or any "photo mode" crate; searching did not surface one.
- I did not check `OrderIndependentTransparencySettings` fields and MSAA constraints for 0.20.
