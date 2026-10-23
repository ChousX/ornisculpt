//! The fixed benchmark (`docs/design/perf-and-look-plan.md` §1): generation, hero stills, a hero
//! in motion and a crowd, with frame-time statistics, per-pass GPU times where the adapter has
//! timestamp queries, and screenshots for image comparison.
//!
//! ```text
//! cargo run --release --example bench -- <gen|hero|motion|crowd|all> [--out DIR] [--allow-battery] [--no-gpu-timing]
//! cargo run --release --example bench -- compare BASE[,BASE...] NEW [--accept]
//! ```
//!
//! Results go to `bench/results/<git sha>[-dirty]/` unless `--out` is given: one `<scenario>.ron`
//! per scenario plus a PNG per shot. Every workload parameter is fixed here; changing one bumps
//! [`BENCH_VERSION`], and results are only compared within a version. Input files are hashed into
//! each report, so an edited plan or coat shows up in `compare`.
//!
//! - `gen`: [`generate_body`] on one thread, per stage, 1 warm-up + 5 runs (no window).
//! - `hero`: each [`MOBS`] entry alone at hero tier, standing; full-body, head and eye framings.
//! - `motion`: two of them walking a circle, camera on a fixed orbit.
//! - `crowd`: 4000 mobs as in `examples/crowd.rs` (coats on); cold start, then steady state.
//!
//! Animation advances exactly 1/60 s per frame and is paused until bodies are ready, so frame N of
//! a case renders the same pose on every run. Runs on battery are refused (`--allow-battery` to
//! override; the report records it).
//!
//! `compare` takes one or more baseline result directories (several = their spread is the noise
//! floor) and a new one, prints per-metric deltas, diffs same-named PNGs, and exits non-zero when
//! a median, mean or total time regresses beyond max(3 %, 2 × noise) or an image changes beyond the gate (widened by the
//! baselines' own image differences). `--accept`
//! copies the new PNGs to `bench/golden/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use bevy::asset::{AssetPlugin, RenderAssetUsages};
use bevy::camera::visibility::RenderLayers;
use bevy::camera::{OrthographicProjection, PerspectiveProjection, Projection, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::render::render_resource::{PollType, TextureFormat, TextureUsages};
use bevy::render::renderer::RenderDevice;
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::diagnostic::DiagnosticsStore;
use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::render::view::window::screenshot::{Screenshot, save_to_disk};
use bevy::time::TimeUpdateStrategy;
use bevy::window::{PresentMode, WindowResolution};
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{BodyPlan, Form, HardKind, LodLevel, PartAssets, PartLodRules, apply_form};
use ornisculpt::coat::{Coat, MobCoat};
use ornisculpt::mob::{
    BodyReady, CrowdUploads, ForceTier, GenCache, GenInput, GeneratedBody, Mob, MobBody, MobSkeleton, MobTier, StageTimes, TierCamera,
    generate_body,
};
use ornisculpt::num::{Saturate, ToF32};
use ornisculpt::surface::{BodyMeshData, SurfaceSettings};
use serde::{Deserialize, Serialize};

mod common;
use common::crowd::{self, Wander};
use common::spawn_stage;

/// Bump whenever a workload parameter below changes.
const BENCH_VERSION: u32 = 3;

/// (case name, body plan, coat): the hero and generation set.
const MOBS: [(&str, &str, Option<&str>); 6] = [
    ("biped", "mobs/biped.body.ron", None),
    ("quadruped_furry", "mobs/quadruped.body.ron", Some("coats/furry.coat.ron")),
    ("horse_furry", "mobs/horse.body.ron", Some("coats/furry.coat.ron")),
    ("bird", "mobs/bird.body.ron", Some("coats/bird.coat.ron")),
    ("dragon_chimera", "mobs/dragon.body.ron", Some("coats/chimera.coat.ron")),
    ("beetle_lizard", "mobs/beetle.body.ron", Some("coats/lizard.coat.ron")),
];
/// Indices into [`MOBS`] for the motion scenario.
const MOTION_MOBS: [usize; 2] = [4, 1];

const GEN_RUNS: usize = 5;
const STILL_WARM: u32 = 120;
const STILL_FRAMES: u32 = 600;
const MOTION_WARM: u32 = 120;
const MOTION_FRAMES: u32 = 600;
const MOTION_SHOTS: [u32; 3] = [150, 300, 450];
const CROWD_COUNT: usize = 4000;
const CROWD_COLD_CAP: Duration = Duration::from_secs(180);
/// A single hero body that takes longer than this has failed.
const READY_CAP: Duration = Duration::from_secs(60);
const CROWD_WARM: u32 = 120;
const CROWD_FRAMES: u32 = 1200;
/// Frames between a screenshot request and the next phase (the capture is asynchronous).
const SHOT_SETTLE: u32 = 15;
/// Empty-scene frames before the first case.
const PREROLL: u32 = 60;
const HERO_FOV_DEG: f32 = 30.0;
/// Render resolution (an offscreen target, so the window manager can't change it).
const RESOLUTION: UVec2 = UVec2::new(1920, 1080);

const SCENARIOS: [&str; 4] = ["gen", "hero", "motion", "crowd"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let out = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map_or_else(default_out_dir, PathBuf::from);
    let result = match args.first().map(String::as_str) {
        Some("compare") => match (args.get(1), args.get(2)) {
            (Some(base), Some(new)) => compare(base, Path::new(new), flag("--accept")),
            _ => Err(usage()),
        },
        Some(s) if SCENARIOS.contains(&s) || s == "all" => {
            if !flag("--allow-battery") && !on_mains() {
                Err("on battery: plug in, or pass --allow-battery (recorded in the report)".into())
            } else if s == "all" {
                run_all(&out, &args)
            } else if s == "gen" {
                run_gen(&out)
            } else {
                run_app(s, out);
                Ok(())
            }
        }
        _ => Err(usage()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("bench: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    "usage: bench <gen|hero|motion|crowd|all> [--out DIR] [--allow-battery] | bench compare BASE[,BASE...] NEW [--accept]".into()
}

/// Each windowed scenario in its own process (a winit event loop can't be made twice).
fn run_all(out: &Path, args: &[String]) -> Result<(), String> {
    run_gen(out)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    for s in SCENARIOS.iter().filter(|s| **s != "gen") {
        let mut cmd = Command::new(&exe);
        cmd.arg(s).arg("--out").arg(out);
        for flag in ["--allow-battery", "--no-gpu-timing"] {
            if args.iter().any(|a| a == flag) {
                cmd.arg(flag);
            }
        }
        let status = cmd.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("scenario {s} failed: {status}"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Reports

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Report {
    meta: Meta,
    cases: Vec<CaseResult>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct Meta {
    bench_version: u32,
    scenario: String,
    git: String,
    dirty: bool,
    machine: String,
    cpus: usize,
    gpu: String,
    driver: String,
    backend: String,
    on_mains: bool,
    /// Asset path → xxh3 of its content.
    inputs: BTreeMap<String, String>,
}

/// Metrics ending in `_ms` or `_s` are times (lower is better, gated); the rest are informational.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct CaseResult {
    name: String,
    metrics: BTreeMap<String, f32>,
    images: Vec<String>,
}

fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .map_or_default(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

fn default_out_dir() -> PathBuf {
    let sha = git(&["rev-parse", "--short", "HEAD"]);
    let dirty = !git(&["status", "--porcelain", "--untracked-files=no"]).is_empty();
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/results").join(format!("{sha}{}", if dirty { "-dirty" } else { "" }))
}

fn on_mains() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/power_supply") else { return true };
    let supplies: Vec<PathBuf> = dir.filter_map(Result::ok).map(|e| e.path()).collect();
    let read = |p: &Path, f: &str| std::fs::read_to_string(p.join(f)).unwrap_or_default().trim().to_owned();
    // No battery at all: a desktop.
    if !supplies.iter().any(|p| read(p, "type") == "Battery") {
        return true;
    }
    supplies.iter().any(|p| read(p, "type") != "Battery" && read(p, "online") == "1")
}

fn machine() -> String {
    ["/sys/firmware/devicetree/base/model", "/sys/class/dmi/id/product_name"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map_or_default(|s| s.trim_matches(['\0', ' ', '\n']).to_owned())
}

fn asset_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join(rel)
}

fn input_hashes(paths: &[&str]) -> BTreeMap<String, String> {
    paths
        .iter()
        .map(|p| {
            let hash = std::fs::read(asset_path(p)).map_or_else(|_| "missing".into(), |b| format!("{:016x}", xxhash_rust::xxh3::xxh3_64(&b)));
            ((*p).to_owned(), hash)
        })
        .collect()
}

fn scenario_inputs(scenario: &str) -> Vec<&'static str> {
    let mut v: Vec<&str> = match scenario {
        "crowd" => crowd::BODIES.iter().copied().chain(crowd::COATS.iter().flatten().copied()).collect(),
        "motion" => MOTION_MOBS.iter().filter_map(|&i| MOBS.get(i)).flat_map(|(_, p, c)| std::iter::once(*p).chain(*c)).collect(),
        _ => MOBS.iter().flat_map(|(_, p, c)| std::iter::once(*p).chain(*c)).collect(),
    };
    v.sort_unstable();
    v.dedup();
    v
}

fn meta(scenario: &str, adapter: Option<&RenderAdapterInfo>) -> Meta {
    Meta {
        bench_version: BENCH_VERSION,
        scenario: scenario.into(),
        git: git(&["rev-parse", "HEAD"]),
        dirty: !git(&["status", "--porcelain", "--untracked-files=no"]).is_empty(),
        machine: machine(),
        cpus: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        gpu: adapter.map_or_default(|a| a.name.clone()),
        driver: adapter.map_or_default(|a| format!("{} {}", a.driver, a.driver_info)),
        backend: adapter.map_or_default(|a| format!("{:?}", a.backend)),
        on_mains: on_mains(),
        inputs: input_hashes(&scenario_inputs(scenario)),
    }
}

fn write_report(out: &Path, report: &Report) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let text = ron::ser::to_string_pretty(report, ron::ser::PrettyConfig::default()).map_err(|e| e.to_string())?;
    let path = out.join(format!("{}.ron", report.meta.scenario));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    println!("bench: wrote {}", path.display());
    Ok(())
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

/// A `kB` field of a `/proc` status file (`VmHWM:` in `/proc/self/status`, `MemAvailable:` in
/// `/proc/meminfo`).
fn mem_kb(file: &str, field: &str) -> Option<u64> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines().find_map(|l| l.strip_prefix(field)?.trim().strip_suffix("kB")?.trim().parse().ok())
}

/// Peak resident memory of this process so far, and the least memory the system had available
/// while measuring (MB). On unified-memory GPUs, GPU allocations may count only in the latter.
fn memory_stats(metrics: &mut BTreeMap<String, f32>, prefix: &str, min_avail_kb: u64) {
    if let Some(peak) = mem_kb("/proc/self/status", "VmHWM:") {
        metrics.insert(format!("{prefix}mem.peak_rss_mb"), peak.to_f32() / 1024.0);
    }
    if min_avail_kb != u64::MAX {
        metrics.insert(format!("{prefix}mem.min_available_mb"), min_avail_kb.to_f32() / 1024.0);
    }
}

/// Value at fraction `p` of the sorted samples (nearest rank).
fn percentile(sorted: &[f32], p: f32) -> f32 {
    let i = (sorted.len().saturating_sub(1).to_f32() * p).round().sat_usize();
    sorted.get(i).copied().unwrap_or(0.0)
}

/// p50/p95/p99/max/mean of frame times (ms) under `prefix`.
fn frame_stats(metrics: &mut BTreeMap<String, f32>, prefix: &str, samples: &[f32]) {
    let mut s = samples.to_vec();
    s.sort_unstable_by(f32::total_cmp);
    let mean = s.iter().sum::<f32>() / s.len().max(1).to_f32();
    metrics.insert(format!("{prefix}frames"), s.len().to_f32());
    for (name, v) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
        metrics.insert(format!("{prefix}{name}_ms"), percentile(&s, v));
    }
    metrics.insert(format!("{prefix}max_ms"), s.last().copied().unwrap_or(0.0));
    metrics.insert(format!("{prefix}mean_ms"), mean);
}

// ---------------------------------------------------------------------------------------------
// `gen`: generation stages, CPU only

const fn mesh_bytes(d: &BodyMeshData) -> usize {
    use std::mem::size_of_val;
    size_of_val(d.positions.as_slice())
        + size_of_val(d.normals.as_slice())
        + size_of_val(d.joints.as_slice())
        + size_of_val(d.weights.as_slice())
        + size_of_val(d.mob_data.as_slice())
        + size_of_val(d.uvs.as_slice())
        + size_of_val(d.coverage.as_slice())
        + size_of_val(d.flow.as_slice())
        + size_of_val(d.cloth.as_slice())
        + size_of_val(d.cor.as_slice())
        + size_of_val(d.muscle.as_slice())
        + size_of_val(d.indices.as_slice())
}

fn gen_input(plan: &str, coat: Option<&str>) -> Result<GenInput, String> {
    let read = |p: &str| std::fs::read_to_string(asset_path(p)).map_err(|e| format!("{p}: {e}"));
    let plan_def: BodyPlan = ron::from_str(&read(plan)?).map_err(|e| format!("{plan}: {e}"))?;
    let coat = match coat {
        Some(c) => ron::from_str::<Coat>(&read(c)?).map_err(|e| format!("{c}: {e}"))?,
        None => Coat::default(),
    };
    Ok(GenInput {
        live: apply_form(&plan_def, &Form::default()).map_err(|e| e.to_string())?,
        rules: PartLodRules::default(),
        assets: PartAssets::default(),
        settings: SurfaceSettings::default(),
        seed: 0,
        coat,
        outfit: default(),
    })
}

fn run_gen(out: &Path) -> Result<(), String> {
    let mut report = Report { meta: meta("gen", None), cases: Vec::new() };
    for (name, plan, coat) in MOBS {
        let mut totals = Vec::new();
        let mut stages: BTreeMap<&'static str, Vec<f32>> = BTreeMap::new();
        let mut metrics = BTreeMap::new();
        for run in 0..=GEN_RUNS {
            let input = gen_input(plan, coat)?;
            let mut times = StageTimes::default();
            let t = Instant::now();
            let output = generate_body(input, &mut times)?;
            let total = ms(t.elapsed());
            // Run 0 warms caches and the allocator.
            if run == 0 {
                for (level, lod) in LodLevel::ALL.iter().zip(&output.lods) {
                    let l = format!("{level:?}").to_lowercase();
                    metrics.insert(format!("{l}.verts"), lod.positions.len().to_f32());
                    metrics.insert(format!("{l}.tris"), (lod.indices.len() / 3).to_f32());
                }
                let coat_bytes: usize =
                    output.coat_meshes.iter().flat_map(|c| [&c.shells, &c.feathers]).flatten().map(mesh_bytes).sum();
                metrics.insert("mesh_kb".into(), output.lods.iter().map(mesh_bytes).sum::<usize>().to_f32() / 1024.0);
                metrics.insert("coat_mesh_kb".into(), coat_bytes.to_f32() / 1024.0);
                continue;
            }
            totals.push(total);
            for (stage, d) in times.0 {
                stages.entry(stage).or_default().push(ms(d));
            }
        }
        let median = |v: &mut Vec<f32>| {
            v.sort_unstable_by(f32::total_cmp);
            percentile(v, 0.5)
        };
        metrics.insert("total_min_ms".into(), totals.iter().copied().fold(f32::INFINITY, f32::min));
        metrics.insert("total_ms".into(), median(&mut totals));
        for (stage, mut v) in stages {
            metrics.insert(format!("stage.{stage}_ms"), median(&mut v));
        }
        memory_stats(&mut metrics, "", u64::MAX);
        println!("bench gen {name}: {:.0} ms", metrics.get("total_ms").copied().unwrap_or(0.0));
        report.cases.push(CaseResult { name: name.into(), metrics, images: Vec::new() });
    }
    write_report(out, &report)
}

// ---------------------------------------------------------------------------------------------
// Windowed scenarios

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Framing {
    Full,
    Head,
    Eye,
}

impl Framing {
    const ALL: [Self; 3] = [Self::Full, Self::Head, Self::Eye];

    const fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Head => "head",
            Self::Eye => "eye",
        }
    }
}

#[derive(Clone, Debug)]
enum CaseKind {
    /// One standing mob, measured at each framing in turn.
    Still { mob: usize },
    /// One mob walking a circle.
    Motion { mob: usize },
    Crowd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Empty-scene frames before the first case: renderer start-up (pipelines every view needs)
    /// isn't counted as any case's cost.
    Preroll(u32),
    Spawn,
    /// Virtual time paused until the body (or every crowd body) is attached.
    WaitReady,
    Warm(u32),
    Measure(u32),
    /// Waiting for an asynchronous screenshot, then on to `next`.
    Settle(u32, Next),
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    /// Next framing of the same still.
    Framing,
    Case,
}

#[derive(Resource)]
struct Bench {
    scenario: String,
    out: PathBuf,
    cases: Vec<CaseKind>,
    case: usize,
    framing: usize,
    phase: Phase,
    entities: Vec<Entity>,
    ready: usize,
    /// Real time between frames (ms) while measuring.
    samples: Vec<f32>,
    /// GPU pass time samples (ms) while measuring, by diagnostic path.
    gpu: BTreeMap<String, Vec<f32>>,
    last_frame: Option<Instant>,
    cold_start: Option<Instant>,
    /// Lowest `MemAvailable` seen while measuring (kB; `u64::MAX` = none yet).
    min_avail_kb: u64,
    /// When the current case spawned (a body that never arrives fails the run).
    spawned: Instant,
    uploads: usize,
    results: Vec<CaseResult>,
    images: Vec<String>,
    metrics: BTreeMap<String, f32>,
}

impl Bench {
    fn case_name(&self) -> String {
        match self.cases.get(self.case) {
            Some(CaseKind::Still { mob }) => {
                let f = Framing::ALL.get(self.framing).map_or("?", |f| f.name());
                format!("{}.{f}", MOBS.get(*mob).map_or("?", |m| m.0))
            }
            Some(CaseKind::Motion { mob }) => format!("{}.walk", MOBS.get(*mob).map_or("?", |m| m.0)),
            Some(CaseKind::Crowd) => format!("crowd{CROWD_COUNT}"),
            None => String::new(),
        }
    }
}

fn run_app(scenario: &str, out: PathBuf) {
    let cases = match scenario {
        "hero" => (0..MOBS.len()).map(|mob| CaseKind::Still { mob }).collect(),
        "motion" => MOTION_MOBS.iter().map(|&mob| CaseKind::Motion { mob }).collect(),
        _ => vec![CaseKind::Crowd],
    };
    let mut app = App::new();
    app.add_plugins((
            DefaultPlugins
                .set(AssetPlugin { file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into(), ..default() })
                .set(WindowPlugin {
                primary_window: Some(Window {
                    title: format!("ornisculpt bench: {scenario}"),
                    resolution: WindowResolution::new(1920, 1080).with_scale_factor_override(1.0),
                    present_mode: PresentMode::AutoNoVsync,
                    resizable: false,
                    ..default()
                }),
                ..default()
            }),
            OrnisculptPlugin,
        ));
    // `--no-gpu-timing`: no timestamp queries (per-pass GPU times are then missing).
    if !std::env::args().any(|a| a == "--no-gpu-timing") {
        app.add_plugins(RenderDiagnosticsPlugin);
    }
    // Finish each frame on the GPU before the next: offscreen frames have no swapchain to hold the
    // CPU back, so submissions would queue up without bound (and fill memory), and frame times
    // would measure submission rather than rendering.
    if let Some(render) = app.get_sub_app_mut(RenderApp) {
        render.add_systems(Render, wait_for_gpu.in_set(RenderSystems::Cleanup));
    }
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .insert_resource(GlobalAmbientLight { brightness: 300.0, ..default() })
        .insert_resource(Bench {
            scenario: scenario.into(),
            out,
            cases,
            case: 0,
            framing: 0,
            phase: Phase::Preroll(PREROLL),
            entities: Vec::new(),
            ready: 0,
            samples: Vec::new(),
            gpu: BTreeMap::new(),
            last_frame: None,
            cold_start: None,
            min_avail_kb: u64::MAX,
            spawned: Instant::now(),
            uploads: 0,
            results: Vec::new(),
            images: Vec::new(),
            metrics: BTreeMap::new(),
        })
        .add_observer(|_: On<BodyReady>, mut bench: ResMut<Bench>| bench.ready += 1)
        .add_systems(Startup, setup)
        .add_systems(First, record_frame)
        .add_systems(Update, (drive, toggle_display, crowd::wander, place_camera).chain().after(ornisculpt::mob::MobSet::Levers))
        .run();
}

#[derive(Component)]
struct BenchCamera;

/// Shows the render target in the window. Off while measuring: Bevy keys per-pass GPU times by pass
/// name, so a second camera's passes would overwrite the scene camera's (and add their cost).
#[derive(Component)]
struct DisplayCamera;

/// The image every case renders into: a fixed resolution whatever size the window manager gives
/// the window (which only shows a copy).
#[derive(Resource)]
struct Target(Handle<Image>);

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut time: ResMut<Time<Virtual>>,
) {
    time.pause();
    let mut image = Image::new_target_texture(RESOLUTION.x, RESOLUTION.y, TextureFormat::Rgba8UnormSrgb, None);
    // Screenshots copy out of it.
    image.texture_descriptor.usage |= TextureUsages::COPY_SRC;
    let target = images.add(image);
    commands.spawn((
        Camera3d::default(),
        RenderTarget::Image(target.clone().into()),
        BenchCamera,
        TierCamera,
        Projection::Perspective(PerspectiveProjection { fov: HERO_FOV_DEG.to_radians(), near: 0.005, ..default() }),
    ));
    // The window: the target on an unlit quad, on its own render layer.
    let screen = RenderLayers::layer(1);
    commands.spawn((
        Camera3d::default(),
        Camera { order: 1, ..default() },
        DisplayCamera,
        Tonemapping::None,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::AutoMin { min_width: 16.0, min_height: 9.0 },
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 1.0),
        screen.clone(),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(16.0, 9.0))),
        MeshMaterial3d(materials.add(StandardMaterial { base_color_texture: Some(target.clone()), unlit: true, ..default() })),
        screen,
    ));
    commands.insert_resource(Target(target));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 400.0, Vec3::new(3.0, 6.0, 2.0));
}

/// Blocks the render thread until the GPU has finished the frame just submitted.
fn wait_for_gpu(device: Res<RenderDevice>) {
    if let Err(e) = device.poll(PollType::wait_indefinitely()) {
        error!("bench: waiting for the GPU: {e}");
    }
}

/// The window shows the scene except while measuring (it keeps the last frame shown).
fn toggle_display(bench: Res<Bench>, mut display: Query<&mut Camera, With<DisplayCamera>>) {
    let show = !matches!(bench.phase, Phase::Measure(_));
    for mut camera in &mut display {
        if camera.is_active != show {
            camera.is_active = show;
        }
    }
}

/// Real frame-to-frame time and GPU pass times while measuring (and while a crowd generates).
fn record_frame(mut bench: ResMut<Bench>, diagnostics: Res<DiagnosticsStore>, uploads: Res<CrowdUploads>) {
    let now = Instant::now();
    let dt = bench.last_frame.map(|t| ms(now.duration_since(t)));
    bench.last_frame = Some(now);
    let Some(dt) = dt else { return };
    let measuring = matches!(bench.phase, Phase::Measure(_)) || (bench.phase == Phase::WaitReady && bench.cold_start.is_some());
    if !measuring {
        return;
    }
    bench.samples.push(dt);
    if bench.samples.len() % 30 == 1 {
        let avail = mem_kb("/proc/meminfo", "MemAvailable:");
        bench.min_avail_kb = bench.min_avail_kb.min(avail.unwrap_or(u64::MAX));
    }
    if matches!(bench.phase, Phase::Measure(_)) {
        bench.uploads += uploads.instances.len();
        for d in diagnostics.iter() {
            let path = d.path().as_str();
            if path.starts_with("render/") && path.ends_with("elapsed_gpu")
                && let Some(v) = d.value()
            {
                bench.gpu.entry(path.to_owned()).or_default().push(v.to_f32());
            }
        }
    }
}

fn spawn_mob(commands: &mut Commands, assets: &AssetServer, mob: usize, extra: impl Bundle) -> Option<Entity> {
    let (_, plan, coat) = MOBS.get(mob)?;
    let mut e = commands.spawn((Mob::new(assets.load(*plan)), ForceTier(LodLevel::Hero), extra));
    if let Some(coat) = coat {
        e.insert(MobCoat(assets.load(*coat)));
    }
    Some(e.id())
}

/// The crowd of `examples/crowd.rs` with `ORNISCULPT_COATS=1`.
fn spawn_crowd(commands: &mut Commands, assets: &AssetServer) -> Vec<Entity> {
    let side = CROWD_COUNT.to_f32().sqrt().ceil().sat_usize();
    crowd::BODIES
        .iter()
        .zip(crowd::ROOTS)
        .cycle()
        .take(CROWD_COUNT)
        .enumerate()
        .map(|(i, (path, root))| {
            let mut mob = crowd::spawn(commands, assets, i, side, (*path, root));
            if let Some(coat) = crowd::coat(i) {
                mob.insert(MobCoat(assets.load(coat)));
            }
            mob.id()
        })
        .collect()
}

fn shoot(commands: &mut Commands, bench: &mut Bench, target: &Handle<Image>, name: &str) {
    let file = format!("{name}.png");
    let path = bench.out.join(&file);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    commands.spawn(Screenshot::image(target.clone())).observe(save_to_disk(path));
    bench.images.push(file);
}

/// Closes the current measurement into a result.
fn finish_measure(bench: &mut Bench, name: String, prefix: &str) {
    let samples = std::mem::take(&mut bench.samples);
    let mut metrics = std::mem::take(&mut bench.metrics);
    frame_stats(&mut metrics, prefix, &samples);
    memory_stats(&mut metrics, prefix, std::mem::replace(&mut bench.min_avail_kb, u64::MAX));
    for (path, mut v) in std::mem::take(&mut bench.gpu) {
        v.sort_unstable_by(f32::total_cmp);
        let key = path.trim_start_matches("render/").trim_end_matches("/elapsed_gpu").replace('/', ".");
        metrics.insert(format!("{prefix}gpu.{key}_ms"), percentile(&v, 0.5));
    }
    if bench.uploads > 0 {
        metrics.insert(format!("{prefix}uploaded_slots_per_frame"), bench.uploads.to_f32() / samples.len().max(1).to_f32());
    }
    bench.uploads = 0;
    let images = std::mem::take(&mut bench.images);
    bench.results.push(CaseResult { name, metrics, images });
}

/// The crowd's cold start, from spawn to every body attached.
fn cold_metrics(bench: &mut Bench, start: Instant, cache: &GenCache) -> BTreeMap<String, f32> {
    let samples = std::mem::take(&mut bench.samples);
    let mut m = BTreeMap::new();
    frame_stats(&mut m, "cold.", &samples);
    memory_stats(&mut m, "cold.", std::mem::replace(&mut bench.min_avail_kb, u64::MAX));
    m.insert("cold.total_s".into(), start.elapsed().as_secs_f32());
    m.insert("cold.frames_over_33ms".into(), samples.iter().filter(|s| **s > 33.3).count().to_f32());
    m.insert("cold.frames_over_100ms".into(), samples.iter().filter(|s| **s > 100.0).count().to_f32());
    m.insert("cold.bodies".into(), cache.bodies.len().to_f32());
    m
}

/// Measured frame `n` of `case`: shots on their frames, and the result once the case has run.
fn measure(
    commands: &mut Commands,
    bench: &mut Bench,
    target: &Handle<Image>,
    case: &CaseKind,
    n: u32,
    mobs: &Query<(Has<MobBody>, Option<&MobTier>), With<Mob>>,
) {
    let name = bench.case_name();
    match case {
        CaseKind::Still { .. } if n >= STILL_FRAMES => {
            // Before the result closes, so the shot is filed with it.
            shoot(commands, bench, target, &name);
            finish_measure(bench, name, "");
            let last = bench.framing + 1 >= Framing::ALL.len();
            bench.phase = Phase::Settle(SHOT_SETTLE, if last { Next::Case } else { Next::Framing });
        }
        CaseKind::Motion { .. } if MOTION_SHOTS.contains(&n) => {
            shoot(commands, bench, target, &format!("{name}.{n}"));
            bench.phase = Phase::Measure(n);
        }
        CaseKind::Motion { .. } if n >= MOTION_FRAMES => {
            finish_measure(bench, name, "");
            bench.phase = Phase::Settle(SHOT_SETTLE, Next::Case);
        }
        CaseKind::Crowd if n >= CROWD_FRAMES => {
            let counts = LodLevel::ALL.map(|l| mobs.iter().filter(|(_, t)| t.is_some_and(|t| t.0 == l)).count());
            for (level, c) in LodLevel::ALL.iter().zip(counts) {
                bench.metrics.insert(format!("steady.tier.{}", format!("{level:?}").to_lowercase()), c.to_f32());
            }
            shoot(commands, bench, target, &name);
            finish_measure(bench, name, "steady.");
            bench.phase = Phase::Settle(SHOT_SETTLE, Next::Case);
        }
        _ => bench.phase = Phase::Measure(n),
    }
}

#[allow(clippy::too_many_arguments)]
fn drive(
    mut commands: Commands,
    mut bench: ResMut<Bench>,
    assets: Res<AssetServer>,
    mut time: ResMut<Time<Virtual>>,
    cache: Res<GenCache>,
    adapter: Option<Res<RenderAdapterInfo>>,
    target: Res<Target>,
    mobs: Query<(Has<MobBody>, Option<&MobTier>), With<Mob>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(case) = bench.cases.get(bench.case).cloned() else {
        if bench.phase != Phase::Done {
            bench.phase = Phase::Done;
            let report = Report { meta: meta(&bench.scenario, adapter.as_deref()), cases: std::mem::take(&mut bench.results) };
            if let Err(e) = write_report(&bench.out, &report) {
                error!("bench: {e}");
            }
            exit.write(AppExit::Success);
        }
        return;
    };
    match bench.phase {
        Phase::Preroll(n) => bench.phase = if n > 1 { Phase::Preroll(n - 1) } else { Phase::Spawn },
        Phase::Spawn => {
            time.pause();
            bench.ready = 0;
            bench.spawned = Instant::now();
            bench.entities = match case {
                CaseKind::Still { mob } => spawn_mob(&mut commands, &assets, mob, ()).into_iter().collect(),
                CaseKind::Motion { mob } => {
                    let wander = Wander { centre: Vec3::ZERO, radius: 2.5, speed: 0.9, offset: 0.0 };
                    spawn_mob(&mut commands, &assets, mob, wander).into_iter().collect()
                }
                CaseKind::Crowd => {
                    bench.cold_start = Some(Instant::now());
                    spawn_crowd(&mut commands, &assets)
                }
            };
            info!("bench: {} generating", bench.case_name());
            bench.phase = Phase::WaitReady;
        }
        Phase::WaitReady => {
            let ready = match case {
                CaseKind::Crowd => {
                    let attached = bench.entities.iter().filter(|e| mobs.get(**e).is_ok_and(|(b, t)| b && t.is_some())).count();
                    let timed_out = bench.cold_start.is_some_and(|t| t.elapsed() > CROWD_COLD_CAP);
                    (attached == bench.entities.len() && cache.in_flight() == 0) || timed_out
                }
                _ => bench.ready > 0,
            };
            if !ready && bench.spawned.elapsed() > READY_CAP && !matches!(case, CaseKind::Crowd) {
                error!("bench: {} never became ready (asset missing or generation failed)", bench.case_name());
                exit.write(AppExit::error());
                bench.phase = Phase::Done;
                return;
            }
            if !ready {
                return;
            }
            if let (CaseKind::Crowd, Some(start)) = (&case, bench.cold_start.take()) {
                bench.metrics = cold_metrics(&mut bench, start, &cache);
            }
            time.unpause();
            let warm = match case {
                CaseKind::Still { .. } => STILL_WARM,
                CaseKind::Motion { .. } => MOTION_WARM,
                CaseKind::Crowd => CROWD_WARM,
            };
            bench.phase = Phase::Warm(warm);
        }
        Phase::Warm(n) => {
            bench.phase = if n > 1 { Phase::Warm(n - 1) } else { Phase::Measure(0) };
        }
        Phase::Measure(n) => measure(&mut commands, &mut bench, &target.0, &case, n + 1, &mobs),
        Phase::Settle(n, next) if n > 1 => bench.phase = Phase::Settle(n - 1, next),
        Phase::Settle(_, next) => match next {
            Next::Framing => {
                bench.framing += 1;
                bench.phase = Phase::Warm(STILL_WARM / 2);
            }
            Next::Case => {
                for e in std::mem::take(&mut bench.entities) {
                    commands.entity(e).despawn();
                }
                bench.case += 1;
                bench.framing = 0;
                bench.phase = Phase::Spawn;
            }
        },
        Phase::Done => {}
    }
}

/// Scripted camera: fixed per case and framing, following the mob (or its eye) as it moves.
fn place_camera(
    bench: Res<Bench>,
    time: Res<Time>,
    bodies: Res<Assets<GeneratedBody>>,
    mobs: Query<(&GlobalTransform, &MobBody, Option<&MobSkeleton>)>,
    joints: Query<&GlobalTransform>,
    mut camera: Query<(&mut Transform, &mut Projection), With<BenchCamera>>,
) {
    let Ok((mut cam, mut proj)) = camera.single_mut() else { return };
    let orbit = |target: Vec3, yaw: f32, pitch: f32, distance: f32| {
        let rot = Quat::from_euler(EulerRot::YXZ, yaw, -pitch, 0.0);
        Transform::from_translation(target + rot * Vec3::Z * distance).looking_at(target, Vec3::Y)
    };
    let Some(case) = bench.cases.get(bench.case) else { return };
    if matches!(case, CaseKind::Crowd) {
        if let Projection::Perspective(p) = proj.as_mut() {
            p.fov = std::f32::consts::FRAC_PI_4;
        }
        *cam = orbit(Vec3::ZERO, 0.6, 0.5, 30.0);
        return;
    }
    let Some(&entity) = bench.entities.first() else { return };
    let Ok((root, body, skeleton)) = mobs.get(entity) else { return };
    let Some(body) = bodies.get(&body.0) else { return };
    let half_fov = (HERO_FOV_DEG.to_radians() * 0.5).tan();
    let centre = root.translation() + Vec3::Y * body.radius * 0.5;
    let full = orbit(centre, 0.9, 0.25, body.radius * 1.15 / half_fov);
    let framing = Framing::ALL.get(bench.framing).copied().unwrap_or(Framing::Full);
    *cam = match case {
        CaseKind::Motion { .. } => orbit(centre, time.elapsed_secs().mul_add(0.3, 0.6), 0.3, body.radius * 1.6 / half_fov),
        _ if framing == Framing::Full => full,
        _ => {
            // The first eye, on its live head joint; looking back at it along head-root → eye.
            let a = &body.anatomy;
            let eye = a.hard_parts.iter().find(|h| matches!(h.params.kind, HardKind::Eye));
            let head = eye.and_then(|e| Some((e, skeleton?.0.get(usize::from(e.bone.0))?)));
            let Some((eye, Ok(joint))) = head.map(|(e, j)| (e, joints.get(*j))) else {
                *cam = full;
                return;
            };
            let eye_pos = joint.transform_point(eye.local.translation);
            let out = (eye_pos - joint.translation()).normalize_or(Vec3::Z);
            let head_len = a.skeleton.get(eye.bone).map_or(0.2, |b| b.length);
            match framing {
                Framing::Head => {
                    let target = eye_pos.lerp(joint.translation(), 0.4);
                    Transform::from_translation(target + out * (head_len * 3.0).clamp(0.2, 2.0)).looking_at(target, Vec3::Y)
                }
                _ => Transform::from_translation(eye_pos + out * (eye.params.size * 5.0).max(0.04)).looking_at(eye_pos, Vec3::Y),
            }
        }
    };
}

// ---------------------------------------------------------------------------------------------
// `compare`

/// Relative change in a time that counts as real, before noise is considered.
const MIN_DELTA: f32 = 0.03;
/// Image gate: share of pixels with any channel off by more than [`PIXEL_TOLERANCE`].
const MAX_CHANGED_PIXELS: f32 = 0.005;
const PIXEL_TOLERANCE: u8 = 8;
const MIN_PSNR_DB: f32 = 40.0;

fn read_reports(dir: &Path) -> BTreeMap<String, Report> {
    SCENARIOS
        .iter()
        .filter_map(|s| {
            let text = std::fs::read_to_string(dir.join(format!("{s}.ron"))).ok()?;
            Some(((*s).to_owned(), ron::from_str::<Report>(&text).ok()?))
        })
        .collect()
}

const fn is_time(metric: &str) -> bool {
    let b = metric.as_bytes();
    matches!(b, [.., b'_', b'm', b's'] | [.., b'_', b's'])
}

/// Times that gate: medians, means and totals. Tails (p95, p99, max) swing too much run to run, and
/// per-pass GPU times are unreliable on some drivers; both are reported but don't fail a run.
fn is_gated(metric: &str) -> bool {
    is_time(metric) && !metric.contains("gpu.") && !["p95_ms", "p99_ms", "max_ms"].iter().any(|t| metric.ends_with(t))
}

/// (changed pixel share, PSNR dB) between two same-sized PNGs.
fn image_diff(a: &Path, b: &Path) -> Result<(f32, f32), String> {
    let load = |p: &Path| -> Result<Image, String> {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, true, ImageSampler::Default, RenderAssetUsages::default())
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    let (ia, ib) = (load(a)?, load(b)?);
    if ia.size() != ib.size() {
        return Err(format!("{} is {:?}, {} is {:?}", a.display(), ia.size(), b.display(), ib.size()));
    }
    let (Some(da), Some(db)) = (ia.data.as_ref(), ib.data.as_ref()) else { return Err("no pixel data".into()) };
    let (mut changed, mut sq, mut px) = (0usize, 0f64, 0usize);
    for (pa, pb) in da.as_chunks::<4>().0.iter().zip(db.as_chunks::<4>().0) {
        let mut off = false;
        for (ca, cb) in pa.iter().zip(pb).take(3) {
            let d = ca.abs_diff(*cb);
            off |= d > PIXEL_TOLERANCE;
            sq = f64::from(d).mul_add(f64::from(d), sq);
        }
        changed += usize::from(off);
        px += 1;
    }
    let mse = sq / f64::from(px.max(1).to_f32() * 3.0);
    let psnr = if mse == 0.0 { f32::INFINITY } else { (10.0 * (255.0f64 * 255.0 / mse).log10()).to_f32() };
    Ok((changed.to_f32() / px.max(1).to_f32(), psnr))
}

fn compare(bases: &str, new: &Path, accept: bool) -> Result<(), String> {
    let base_dirs: Vec<PathBuf> = bases.split(',').map(PathBuf::from).collect();
    let base_reports: Vec<_> = base_dirs.iter().map(|d| read_reports(d)).collect();
    let new_reports = read_reports(new);
    let mut failures = Vec::new();
    for (scenario, report) in &new_reports {
        let olds: Vec<&Report> = base_reports.iter().filter_map(|r| r.get(scenario)).collect();
        let Some(first) = olds.first() else {
            println!("== {scenario}: no baseline");
            continue;
        };
        println!("== {scenario}");
        if first.meta.bench_version != report.meta.bench_version {
            failures.push(format!("{scenario}: bench version {} vs {}", first.meta.bench_version, report.meta.bench_version));
            continue;
        }
        for (path, hash) in &report.meta.inputs {
            if first.meta.inputs.get(path).is_some_and(|h| h != hash) {
                println!("   note: input {path} changed since the baseline");
            }
        }
        if first.meta.gpu != report.meta.gpu || first.meta.machine != report.meta.machine {
            println!("   note: different machine/GPU ({} / {} vs {} / {})", first.meta.machine, first.meta.gpu, report.meta.machine, report.meta.gpu);
        }
        for case in &report.cases {
            for (metric, &v) in &case.metrics {
                let mut vals: Vec<f32> =
                    olds.iter().filter_map(|r| r.cases.iter().find(|c| c.name == case.name)?.metrics.get(metric).copied()).collect();
                if vals.is_empty() {
                    continue;
                }
                vals.sort_unstable_by(f32::total_cmp);
                let base = percentile(&vals, 0.5);
                let spread = vals.last().copied().unwrap_or(base) - vals.first().copied().unwrap_or(base);
                let noise = if base.abs() > f32::EPSILON { spread / base.abs() } else { 0.0 };
                let delta = if base.abs() > f32::EPSILON { (v - base) / base.abs() } else { 0.0 };
                let gate = MIN_DELTA.max(2.0 * noise);
                let verdict = if !is_gated(metric) || delta.abs() <= gate {
                    ""
                } else if delta > 0.0 {
                    failures.push(format!("{scenario} {} {metric}: {base:.3} → {v:.3} ({:+.1} %)", case.name, delta * 100.0));
                    "  REGRESSION"
                } else {
                    "  improved"
                };
                if is_time(metric) || (v - base).abs() > f32::EPSILON {
                    println!("   {:<28} {metric:<40} {base:>10.3} → {v:>10.3}  {:+6.1} % (noise {:.1} %){verdict}", case.name, delta * 100.0, noise * 100.0);
                }
            }
            for image in &case.images {
                let olds: Vec<PathBuf> = base_dirs.iter().map(|d| d.join(image)).filter(|p| p.exists()).collect();
                let Some(old) = olds.first() else { continue };
                // Image noise: how far the baselines differ from the first one (fur and scale close-ups
                // aren't bit-exact run to run).
                let (mut noise_changed, mut noise_psnr) = (0.0f32, f32::INFINITY);
                for other in olds.iter().skip(1) {
                    if let Ok((c, p)) = image_diff(old, other) {
                        noise_changed = noise_changed.max(c);
                        noise_psnr = noise_psnr.min(p);
                    }
                }
                match image_diff(old, &new.join(image)) {
                    Ok((changed, psnr)) => {
                        let bad = changed > MAX_CHANGED_PIXELS.max(2.0 * noise_changed) || psnr < MIN_PSNR_DB.min(noise_psnr - 3.0);
                        println!(
                            "   {image:<40} {:.3} % pixels changed, PSNR {psnr:.1} dB (noise {:.3} %, {noise_psnr:.1} dB){}",
                            changed * 100.0,
                            noise_changed * 100.0,
                            if bad { "  IMAGE CHANGED" } else { "" }
                        );
                        if bad {
                            failures.push(format!("{image}: {:.3} % pixels, {psnr:.1} dB", changed * 100.0));
                        }
                    }
                    Err(e) => failures.push(e),
                }
            }
        }
    }
    if accept {
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/golden");
        std::fs::create_dir_all(&golden).map_err(|e| e.to_string())?;
        for case in new_reports.values().flat_map(|r| &r.cases) {
            for image in &case.images {
                std::fs::copy(new.join(image), golden.join(image)).map_err(|e| format!("{image}: {e}"))?;
            }
        }
        println!("accepted images into {}", golden.display());
    }
    if failures.is_empty() {
        println!("ok");
        Ok(())
    } else {
        Err(format!("{} gate failure(s):\n  {}", failures.len(), failures.join("\n  ")))
    }
}
