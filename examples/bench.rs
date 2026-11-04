//! The fixed benchmark (`docs/design/perf-and-look-plan.md` §1): generation, hero stills, a hero
//! in motion and a crowd, with frame-time statistics, per-pass GPU times where the adapter has
//! timestamp queries, and screenshots for image comparison.
//!
//! ```text
//! cargo run --release --example bench -- <gen|hero|motion|crowd|all> [--out DIR] [--allow-battery] [--no-gpu-timing]
//! cargo run --release --example bench -- compare BASE[,BASE...] NEW [--accept]
//! cargo run --release --example bench -- provenance
//! ```
//!
//! Every report carries a provenance header: the commit the binary was **compiled** from and the
//! binary's own xxh3 (`build.rs`), the checkout it was run against, the command line, every
//! `ORNISCULPT_*` and graphics environment variable, and the workload's compile-time defaults.
//! A binary built at one commit and run against a checkout at another says so loudly
//! (`stale_binary`), and two A/B sides are provably different builds exactly when their
//! `build.exe_xxh3` differ — `bench provenance` prints the header without measuring anything.
//!
//! Results go to `bench/results/<build sha>[-dirty]/` unless `--out` is given: one `<scenario>.ron`
//! per scenario plus a PNG per shot. Every workload parameter is fixed here; changing one bumps
//! [`BENCH_VERSION`], and results are only compared within a version. Input files are hashed into
//! each report, so an edited plan or coat shows up in `compare`.
//!
//! - `gen`: [`generate_body`] on one thread, per stage, 1 warm-up + 5 runs (no window).
//! - `hero`: each [`MOBS`] entry alone at hero tier, standing; full-body, head and eye framings.
//! - `motion`: two of them walking a circle, camera on a fixed orbit.
//! - `crowd`: 4000 mobs as in `examples/crowd.rs` (coats on); cold start, then steady state.
//!
//! `ORNISCULPT_CACHE=<dir>` keeps converted part SDFs and generated bodies there between runs, so
//! a second `crowd`, `hero` or `motion` run starts warm. Off by default, and the comparable way
//! to run the bench: a result from a warm cache measures something else, so say so in the log.
//!
//! Runs headless, as ornithochory's grove: no window, every case renders into a fixed 1920×1080
//! image that screenshots copy out of, and nothing waits on a display. Animation advances exactly
//! 1/60 s per frame and is paused until bodies are ready, so frame N of a case renders the same
//! pose on every run. Runs on battery are refused (`--allow-battery` to
//! override; the report records it).
//!
//! `ORNISCULPT_BENCH_ROOT=<checkout>` reads assets, goldens and git state from another checkout of
//! the same commit, for a binary built elsewhere (the Steam Deck runs toth's build).
//!
//! `ORNISCULPT_CAPTURE=<case>:<frame>` (`crowd4000:600`, `spider.eye:300`) saves a `RenderDoc` capture
//! of that measured frame when the bench runs under `renderdoccmd capture` (see `bench/LOG.md`).
//!
//! `compare` takes one or more baseline result directories (several = their spread is the noise
//! floor) and a new one, prints per-metric deltas, diffs same-named PNGs, and exits non-zero when
//! a median, mean or total time regresses beyond max(3 %, 2 × noise) or an image changes beyond the gate (widened by the
//! baselines' own image differences). `--accept`
//! copies the new PNGs to `bench/golden/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use bevy::app::ScheduleRunnerPlugin;
use bevy::asset::{AssetPlugin, RenderAssetUsages};
use bevy::camera::{PerspectiveProjection, Projection, RenderTarget};
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
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use ornisculpt::OrnisculptPlugin;
use ornisculpt::anatomy::{BodyPlan, Form, HardKind, LodLevel, PartLodRules, apply_form};
use ornisculpt::coat::{Coat, MobCoat};
use ornisculpt::mob::{
    BodyFailed, BodyReady, CrowdUploads, ForceTier, GenCache, GenInput, GenOutput, GeneratedBody, Mob, MobBody, MobSkeleton, MobTier, QualityProfile, StageTimes,
    TierCamera, generate_body,
};
use ornisculpt::num::{Saturate, ToF32};
use ornisculpt::surface::{BodyMeshData, SurfaceSettings};
use serde::{Deserialize, Serialize};

mod common;
use common::crowd::{self, Wander};
use common::{root, spawn_stage};

/// Bump whenever a workload parameter below changes.
const BENCH_VERSION: u32 = 5;

/// (case name, body plan, coat): the hero and generation set.
const MOBS: [(&str, &str, Option<&str>); 6] = [
    ("man", "mobs/man.body.ron", Some("coats/skin_fair.coat.ron")),
    ("woman", "mobs/woman.body.ron", Some("coats/skin_woman.coat.ron")),
    ("horse_bay", "mobs/horse.body.ron", Some("coats/bay.coat.ron")),
    ("ox_hereford", "mobs/ox.body.ron", Some("coats/hereford.coat.ron")),
    ("spider", "mobs/spider.body.ron", Some("coats/wolf_spider.coat.ron")),
    ("darner", "mobs/darner.body.ron", Some("coats/green_darner.coat.ron")),
];
/// Indices into [`MOBS`] for the motion scenario.
const MOTION_MOBS: [usize; 2] = [1, 2];

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
/// Render resolution.
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
        // The header a run would write, without running anything: for checking what a binary is
        // before trusting a result that came out of it, and cheap enough to do on every build.
        Some("provenance") => {
            print_provenance(&meta("provenance", None, None, None));
            Ok(())
        }
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
                match run_app(s, out) {
                    AppExit::Success => Ok(()),
                    AppExit::Error(_) => Err("a body failed to load or generate (see the log)".into()),
                }
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
    "usage: bench <gen|hero|motion|crowd|all> [--out DIR] [--allow-battery] | bench compare BASE[,BASE...] NEW [--accept] | bench provenance".into()
}

/// Each rendered scenario in its own process: a fresh renderer, and its own peak memory.
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

/// Every field is `serde(default)` so a report written before a field existed still reads: a new
/// header field must not make old baselines unreadable to `compare`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct Meta {
    bench_version: u32,
    scenario: String,
    /// What produced this report: stamped into the binary at compile time, so it survives being
    /// carried to another machine and run against another checkout.
    build: Build,
    /// `HEAD` of the checkout the run *read* ([`root`]: this one, or `ORNISCULPT_BENCH_ROOT`) at
    /// run time. Not the provenance of the measurement — [`Build::git`] is. The two differ
    /// whenever a binary outlives the checkout it was built from, which is the mistake this
    /// header exists to catch.
    run_git: String,
    run_dirty: bool,
    /// `run_git` is a real commit and is not [`Build::git`]: **the binary is stale**, the numbers
    /// below were produced by code that is not what the checkout now says.
    stale_binary: bool,
    machine: String,
    cpus: usize,
    gpu: String,
    driver: String,
    backend: String,
    on_mains: bool,
    /// The whole command line, so `--allow-battery`, `--no-gpu-timing` and `--out` are on the
    /// record. A flag that isn't in the header can't be checked from the record afterwards.
    args: Vec<String>,
    /// Every `ORNISCULPT_*` variable set for the run, plus the graphics variables that change
    /// what the GPU did (see [`ENV_PREFIXES`]). Captured by prefix, not by name, so a knob added
    /// later lands in the header without anyone remembering to add it.
    env: BTreeMap<String, String>,
    /// Workload knobs that are compile-time defaults rather than flags — resolution, MSAA, the
    /// tier thresholds, `PartLodRules`, `SurfaceSettings`. A commit that changes one of these
    /// changes the workload, and without them in the header the change is invisible.
    settings: BTreeMap<String, String>,
    /// Asset path → xxh3 of its content.
    inputs: BTreeMap<String, String>,
}

/// Build-time provenance (`build.rs`) plus the binary's own fingerprint. Two result trees are
/// provably from two different builds when their [`Self::exe_xxh3`] differ, and provably from the
/// same one when they match — the check the tarasaur Deck driver does with md5, in the report
/// itself so it also holds for runs nobody drove.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
struct Build {
    /// `HEAD` when this binary was compiled.
    git: String,
    /// A watched path (`build.rs`: `src`, `examples`, `assets`, the manifests) was modified when
    /// this binary was compiled, so `git` alone does not describe it.
    dirty: bool,
    /// xxh3-128 of the running executable: the same hash the `inputs` map uses, so no new
    /// dependency. Not a cryptographic hash — it answers "are these two binaries the same file",
    /// which is the question an A/B has.
    exe_xxh3: String,
    exe_bytes: u64,
    /// The file the fingerprint was taken of — not always `current_exe` (see [`exe_path`]).
    exe_file: String,
    rustc: String,
    profile: String,
    target: String,
    /// Enabled cargo features, comma-separated (`soft-tissue,joint-limits`).
    features: String,
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
        .current_dir(root())
        .output()
        .ok()
        .map_or_default(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

/// Named after the commit the **binary** was built from, not the checkout it is run against: the
/// old run-time naming is how six result trees — the three baseline-named ones included — came to
/// be stamped `ecb283c`. Falls back to the run-time sha only when there is no build stamp at all.
fn default_out_dir() -> PathBuf {
    let build = env!("ORNISCULPT_BUILD_GIT");
    let sha: String = if build.is_empty() { git(&["rev-parse", "--short", "HEAD"]) } else { build.chars().take(7).collect() };
    let dirty = env!("ORNISCULPT_BUILD_DIRTY") == "1";
    root().join("bench/results").join(format!("{sha}{}", if dirty { "-dirty" } else { "" }))
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
    root().join("assets").join(rel)
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
        "crowd" => crowd::BODIES.iter().chain(&crowd::COATS).copied().collect(),
        "motion" => MOTION_MOBS.iter().filter_map(|&i| MOBS.get(i)).flat_map(|(_, p, c)| std::iter::once(*p).chain(*c)).collect(),
        _ => MOBS.iter().flat_map(|(_, p, c)| std::iter::once(*p).chain(*c)).collect(),
    };
    v.sort_unstable();
    v.dedup();
    v
}

/// Variables captured into [`Meta::env`], by prefix: everything the crate and its examples read
/// (`ORNISCULPT_`), and the graphics environment, where one variable can move a frame time more
/// than anything in this repo.
const ENV_PREFIXES: [&str; 9] = ["ORNISCULPT_", "WGPU_", "VK_", "MESA_", "RADV_", "AMD_", "LIBGL_", "MANGOHUD", "RUST_LOG"];

/// A `build.rs` stamp, owned. Through a function because `env!` expands to a literal, and an
/// empty one — no cargo features enabled — reads to clippy as a hand-written `String::new()`.
fn stamp(s: &str) -> String {
    s.to_owned()
}

/// What the binary was compiled from, and which binary it is.
fn provenance() -> Build {
    let (exe_xxh3, exe_bytes) = exe_fingerprint();
    Build {
        exe_file: exe_path().unwrap_or_default().display().to_string(),
        git: stamp(env!("ORNISCULPT_BUILD_GIT")),
        dirty: env!("ORNISCULPT_BUILD_DIRTY") == "1",
        exe_xxh3,
        exe_bytes,
        rustc: stamp(env!("ORNISCULPT_BUILD_RUSTC")),
        profile: stamp(env!("ORNISCULPT_BUILD_PROFILE")),
        target: stamp(env!("ORNISCULPT_BUILD_TARGET")),
        features: stamp(env!("ORNISCULPT_BUILD_FEATURES")),
    }
}

/// The bench binary's own path. `current_exe` is not always it: on the Steam Deck the bench is
/// started through an explicit dynamic loader (`tools/deck/bench.sh`, nix glibc against `SteamOS`'s
/// libraries), and then `/proc/self/exe` is `ld-linux-x86-64.so.2` — the same file for both sides
/// of an A/B, which would make the fingerprint below silently useless on the one machine whose
/// numbers decide things. The loader execs with `argv[0]` set to the program, so fall back to that.
fn exe_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let is_loader = exe
        .as_deref()
        .and_then(Path::file_name)
        .is_some_and(|n| n.to_string_lossy().starts_with("ld-linux") || n.to_string_lossy().starts_with("ld.so"));
    if !is_loader {
        return exe;
    }
    std::env::args_os().next().map(PathBuf::from).filter(|p| p.is_file()).or(exe)
}

/// xxh3-128 of this executable and its length. Read in chunks rather than whole: the report
/// records peak RSS, and slurping a release binary would show up in it.
fn exe_fingerprint() -> (String, u64) {
    let Some(path) = exe_path() else { return ("unknown".into(), 0) };
    let Ok(mut file) = std::fs::File::open(&path) else { return ("unreadable".into(), 0) };
    let mut hash = xxhash_rust::xxh3::Xxh3::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0u64;
    loop {
        match std::io::Read::read(&mut file, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                len += u64::try_from(n).unwrap_or(0);
                match buf.get(..n) {
                    Some(chunk) => hash.update(chunk),
                    None => return ("unreadable".into(), len),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => (),
            Err(_) => return ("unreadable".into(), len),
        }
    }
    (format!("{:032x}", hash.digest128()), len)
}

/// The run's environment, filtered to [`ENV_PREFIXES`] and sorted. A value that is a path is kept
/// whole: `ORNISCULPT_CACHE=<dir>` changes the workload completely, and which dir it was matters.
fn env_snapshot() -> BTreeMap<String, String> {
    // `vars`, not `vars_os`, panics on a value that isn't UTF-8, and a benchmark must not die of
    // its own header.
    std::env::vars_os()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        .filter(|(k, _)| ENV_PREFIXES.iter().any(|p| k.starts_with(p)))
        .collect()
}

/// The workload's compile-time defaults, flattened to strings: things an A/B could turn that are
/// not flags. `quality` and `msaa` come from the running app, which is why they are passed in;
/// `gen` has no renderer and records neither.
fn settings_snapshot(quality: Option<&QualityProfile>, msaa: Option<&Msaa>) -> BTreeMap<String, String> {
    let mut s = BTreeMap::new();
    s.insert("resolution".into(), format!("{}x{}", RESOLUTION.x, RESOLUTION.y));
    s.insert("hero_fov_deg".into(), format!("{HERO_FOV_DEG}"));
    s.insert("part_lod_rules".into(), format!("{:?}", PartLodRules::default()));
    s.insert("surface_settings".into(), format!("{:016x}", xxhash_rust::xxh3::xxh3_64(format!("{:?}", SurfaceSettings::default()).as_bytes())));
    if let Some(q) = quality {
        s.insert("quality_profile".into(), format!("{q:?}"));
    }
    if let Some(m) = msaa {
        s.insert("msaa".into(), format!("{}", m.samples()));
    }
    s
}

fn meta(scenario: &str, adapter: Option<&RenderAdapterInfo>, quality: Option<&QualityProfile>, msaa: Option<&Msaa>) -> Meta {
    let build = provenance();
    let run_git = git(&["rev-parse", "HEAD"]);
    Meta {
        bench_version: BENCH_VERSION,
        scenario: scenario.into(),
        stale_binary: !run_git.is_empty() && !build.git.is_empty() && run_git != build.git,
        build,
        run_git,
        run_dirty: !git(&["status", "--porcelain", "--untracked-files=no"]).is_empty(),
        args: std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect(),
        env: env_snapshot(),
        settings: settings_snapshot(quality, msaa),
        machine: machine(),
        cpus: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        gpu: adapter.map_or_default(|a| a.name.clone()),
        driver: adapter.map_or_default(|a| format!("{} {}", a.driver, a.driver_info)),
        backend: adapter.map_or_default(|a| format!("{:?}", a.backend)),
        on_mains: on_mains(),
        inputs: input_hashes(&scenario_inputs(scenario)),
    }
}

/// The provenance block, printed beside every report as it is written and by the `provenance`
/// subcommand. A mismatch between the commit the binary was built from and the checkout it ran
/// against is shouted, not noted: measuring a stale binary against a moved checkout is the exact
/// mistake this block exists to catch, and a quiet line in a log would be read past.
fn print_provenance(meta: &Meta) {
    let b = &meta.build;
    let short = |s: &str| -> String { if s.is_empty() { "unknown".into() } else { s.chars().take(7).collect() } };
    println!("bench provenance ({}):", meta.scenario);
    println!("  built from   {}{}", short(&b.git), if b.dirty { " -dirty (watched paths modified at build time)" } else { "" });
    println!("  binary       xxh3 {} ({} bytes) {}", b.exe_xxh3, b.exe_bytes, b.exe_file);
    println!("  toolchain    {} / {} / {}", b.rustc, b.profile, b.target);
    println!("  features     {}", if b.features.is_empty() { "(none)" } else { b.features.as_str() });
    println!("  checkout     {}{} at {}", short(&meta.run_git), if meta.run_dirty { " -dirty" } else { "" }, root().display());
    println!("  bench        version {}, {}", meta.bench_version, meta.settings.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "));
    if meta.env.is_empty() {
        println!("  env          (no ORNISCULPT_*/graphics variables set)");
    } else {
        for (k, v) in &meta.env {
            println!("  env          {k}={v}");
        }
    }
    if b.git.is_empty() {
        eprintln!("bench: !! NO BUILD STAMP: this binary was compiled outside a git checkout; its commit is unknown");
    }
    if meta.stale_binary {
        eprintln!(
            "bench: !! STALE BINARY: built from {}, run against a checkout at {} — these numbers are NOT {}'s",
            short(&b.git),
            short(&meta.run_git),
            short(&meta.run_git)
        );
    }
    if b.dirty {
        eprintln!("bench: !! DIRTY BUILD: the tree had uncommitted changes under src/examples/assets when this binary was compiled; {} does not describe it", short(&b.git));
    }
}

fn write_report(out: &Path, report: &Report) -> Result<(), String> {
    print_provenance(&report.meta);
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

/// Bit-exact fingerprint of a generated body's geometry and deformation data.
///
/// For asking whether generation is deterministic *within one process*. `gen` already calls
/// `generate_body` six times per mob, so comparing these answers the question that decides the
/// whole non-repeatability hunt: if the six agree but differ between processes, the cause is
/// per-process state (a `RandomState`-seeded map, an address); if they disagree inside one
/// process, it is per-invocation (threads, uninitialised memory).
///
/// Over the raw bits, not rounded: this is an equality test, so a difference of one ulp must count.
fn body_fingerprint(out: &GenOutput) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x1000_0000_01b3);
    };
    for lod in &out.lods {
        for v in &lod.positions {
            for c in v.to_array() {
                mix(u64::from(c.to_bits()));
            }
        }
        for v in &lod.normals {
            for c in v.to_array() {
                mix(u64::from(c.to_bits()));
            }
        }
        for v in &lod.cor {
            for c in v.to_array() {
                mix(u64::from(c.to_bits()));
            }
        }
        for w in &lod.weights {
            for c in *w {
                mix(u64::from(c.to_bits()));
            }
        }
        for j in &lod.joints {
            for c in *j {
                mix(u64::from(c));
            }
        }
        for &i in &lod.indices {
            mix(u64::from(i));
        }
    }
    h
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
        assets: common::part_assets(&plan_def),
        settings: SurfaceSettings::default(),
        seed: 0,
        coat,
        outfit: default(),
        cache: None,
    })
}

fn run_gen(out: &Path) -> Result<(), String> {
    let mut report = Report { meta: meta("gen", None, None, None), cases: Vec::new() };
    for (name, plan, coat) in MOBS {
        let mut totals = Vec::new();
        let mut prints: Vec<u64> = Vec::new();
        let mut stages: BTreeMap<&'static str, Vec<f32>> = BTreeMap::new();
        let mut metrics = BTreeMap::new();
        for run in 0..=GEN_RUNS {
            let input = gen_input(plan, coat)?;
            let mut times = StageTimes::default();
            let t = Instant::now();
            let output = generate_body(input, &mut times)?;
            let total = ms(t.elapsed());
            prints.push(body_fingerprint(&output));
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
        // How many *distinct* bodies the six invocations produced. 1 means generation is
        // deterministic within this process; more means it is not.
        prints.sort_unstable();
        let distinct = prints.windows(2).filter(|w| w[0] != w[1]).count() + usize::from(!prints.is_empty());
        metrics.insert("gen.distinct_bodies".into(), distinct.to_f32());
        metrics.insert("gen.fingerprint24".into(), (prints.first().copied().unwrap_or(0) & 0xff_ffff).to_f32());
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
// Rendered scenarios

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

/// Runs the app scenario; the exit says whether every body came up.
fn run_app(scenario: &str, out: PathBuf) -> AppExit {
    let cases = match scenario {
        "hero" => {
            let mut v: Vec<CaseKind> = (0..MOBS.len()).map(|mob| CaseKind::Still { mob }).collect();
            // `ORNISCULPT_BENCH_REVERSE=1` runs the cases back to front. A diagnostic, not a
            // workload knob: the image gate is byte-stable for the *first* mob generated and not
            // for any later one, and reversing the order says whether that is about position in
            // the sequence or about the mob's own identity.
            if std::env::var("ORNISCULPT_BENCH_REVERSE").is_ok_and(|v| v == "1") {
                v.reverse();
            }
            v
        }
        "motion" => MOTION_MOBS.iter().map(|&mob| CaseKind::Motion { mob }).collect(),
        _ => vec![CaseKind::Crowd],
    };
    let mut app = App::new();
    app.add_plugins((
        DefaultPlugins
            .set(AssetPlugin { file_path: root().join("assets").to_string_lossy().into_owned(), ..default() })
            .set(WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..default() })
            .disable::<WinitPlugin>(),
        // With a window plugin, DefaultPlugins leaves the runner to winit. Uncapped: `wait_for_gpu`
        // paces the frames.
        ScheduleRunnerPlugin::run_loop(Duration::ZERO),
        OrnisculptPlugin,
    ));
    // `--no-gpu-timing`: no timestamp queries (per-pass GPU times are then missing). The `tracy`
    // feature brings this plugin in itself, and adding it twice is a panic.
    if !std::env::args().any(|a| a == "--no-gpu-timing") && !app.is_plugin_added::<RenderDiagnosticsPlugin>() {
        app.add_plugins(RenderDiagnosticsPlugin);
    }
    // Finish each frame on the GPU before the next: headless frames have no swapchain to hold the
    // CPU back, so submissions would queue up without bound (and fill memory), and frame times
    // would measure submission rather than rendering.
    if let Some(render) = app.get_sub_app_mut(RenderApp) {
        render.add_systems(Render, (wait_for_gpu, graphics_capture).chain().in_set(RenderSystems::Cleanup));
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
        // A body that fails (a bad asset) would leave a still waiting out `READY_CAP` and a crowd
        // measuring the wrong thing: stop, with the error, whatever the mode.
        .add_observer(|ev: On<BodyFailed>, mut exit: MessageWriter<AppExit>| {
            error!("bench: mob {} failed: {}", ev.entity, ev.error);
            exit.write(AppExit::error());
        })
        .add_systems(Startup, (common::disk_caches, setup))
        .add_systems(First, record_frame)
        .add_systems(Update, (drive, crowd::wander, place_camera).chain().after(ornisculpt::mob::MobSet::Levers))
        .run()
}

#[derive(Component)]
struct BenchCamera;

/// The image every case renders into.
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
    commands.insert_resource(Target(target));
    spawn_stage(&mut commands, &mut meshes, &mut materials, 400.0, Vec3::new(3.0, 6.0, 2.0));
}

/// Blocks the render thread until the GPU has finished the frame just submitted.
fn wait_for_gpu(device: Res<RenderDevice>) {
    if let Err(e) = device.poll(PollType::wait_indefinitely()) {
        error!("bench: waiting for the GPU: {e}");
    }
}

/// `ORNISCULPT_CAPTURE=<case>:<frame>` (`crowd4000:600`, `spider.eye:300`): a graphics-debugger
/// capture of that measured frame, for a run under `RenderDoc` (its in-application API brackets
/// the frame, so it works without a window). Capture state: 0 idle, 1 requested, 2 capturing,
/// 3 done.
static CAPTURE: AtomicU8 = AtomicU8::new(0);

/// Raises the capture request when the named case reaches its frame.
fn request_capture(bench: &Bench, n: u32) {
    let Ok(spec) = std::env::var("ORNISCULPT_CAPTURE") else { return };
    let Some((case, frame)) = spec.rsplit_once(':') else { return };
    if frame.parse() == Ok(n) && bench.case_name() == case {
        _ = CAPTURE.compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// Starts a requested capture once the frame before it has finished on the GPU, and stops it
/// after the next one (the render world runs after `wait_for_gpu`, so exactly one frame is in it).
fn graphics_capture() {
    let state = CAPTURE.load(Ordering::Relaxed);
    if state != 1 && state != 2 {
        return;
    }
    let Some(api) = renderdoc::api() else {
        error!("bench: ORNISCULPT_CAPTURE needs the app run under RenderDoc (renderdoccmd capture)");
        CAPTURE.store(3, Ordering::Relaxed);
        return;
    };
    if state == 1 {
        api.start();
        CAPTURE.store(2, Ordering::Relaxed);
        info!("bench: graphics capture started");
    } else {
        api.end();
        CAPTURE.store(3, Ordering::Relaxed);
        info!("bench: graphics capture done");
    }
}

/// `RenderDoc`'s in-application API, found in the process once its Vulkan layer has loaded it.
/// (wgpu's own capture calls look for the library when the instance is created, before the
/// layer brings it in, so under `renderdoccmd` they do nothing.)
mod renderdoc {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    const RTLD_NOW: c_int = 2;
    const RTLD_NOLOAD: c_int = 4;
    /// `eRENDERDOC_API_Version_1_1_2`.
    const VERSION: c_int = 10102;
    /// Indices of `StartFrameCapture` and `EndFrameCapture` in `RENDERDOC_API_1_1_2`.
    const START: usize = 19;
    const END: usize = 21;

    type GetApi = unsafe extern "C" fn(c_int, *mut *mut c_void) -> c_int;
    type Frame = unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32;

    pub struct Api(*const *const c_void);

    pub fn api() -> Option<Api> {
        // SAFETY: plain libdl calls on a library already in the process (RTLD_NOLOAD); the table
        // `RENDERDOC_GetAPI` fills is `RenderDoc`'s static API struct, valid while it is loaded.
        unsafe {
            let lib = dlopen(c"librenderdoc.so".as_ptr(), RTLD_NOW | RTLD_NOLOAD);
            if lib.is_null() {
                return None;
            }
            let get = dlsym(lib, c"RENDERDOC_GetAPI".as_ptr());
            if get.is_null() {
                return None;
            }
            let get: GetApi = std::mem::transmute::<*mut c_void, GetApi>(get);
            let mut table: *mut c_void = std::ptr::null_mut();
            (get(VERSION, &raw mut table) == 1 && !table.is_null()).then(|| Api(table.cast()))
        }
    }

    impl Api {
        fn call(&self, index: usize) {
            // SAFETY: entries `START` and `END` of the 1.1.2 table take (device, window); null for
            // both captures whichever device is active, which headless is the only one.
            unsafe {
                let f: Frame = std::mem::transmute::<*const c_void, Frame>(*self.0.add(index));
                f(std::ptr::null_mut(), std::ptr::null_mut());
            }
        }

        pub fn start(&self) {
            self.call(START);
        }

        pub fn end(&self) {
            self.call(END);
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
    // `ORNISCULPT_BENCH_NO_COAT=1` spawns the body without its coat. A diagnostic, not a workload
    // knob: generation is now known bit-identical across processes, so the image variation is in
    // rendering, upload or capture — and the coat is the part of the render whose fragments are
    // alpha-tested and overlapping, where draw order can reach the pixels. If the shots go
    // byte-identical without coats, the coat's render path is the carrier.
    if let Some(coat) = coat.filter(|_| !no_coat()) {
        e.insert(MobCoat(assets.load(coat)));
    }
    Some(e.id())
}

/// `ORNISCULPT_BENCH_TWICE=1`: for the still cases, hold virtual time paused through the measured
/// phase and shoot the **same framing twice**, 20 frames apart, as `<name>.png` and
/// `<name>.again.png`.
///
/// The decisive split left after wave 3. Generation is bit-identical and the coat's drawing carries
/// most of the variation, but whether what remains is *per-process* or *per-frame* is unknown, and
/// the two are very different repairs. With the pose frozen, two captures in one process at the
/// same pose must be byte-identical if the render is stable; if they differ, the instability is
/// inside a single run and does not need two processes to reproduce — which makes it far cheaper
/// to chase.
fn twice() -> bool {
    static TWICE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TWICE.get_or_init(|| std::env::var("ORNISCULPT_BENCH_TWICE").is_ok_and(|v| v == "1"))
}

/// Frames between the two captures of [`twice`], from `ORNISCULPT_BENCH_TWICE_GAP` (default 20).
///
/// The gap is the measurement. With the pose frozen, two captures must agree; they do not, and the
/// question is why. If the difference **scales with the gap**, something in the scene or the
/// renderer evolves per frame and the shot is photographing a moving target. If it is **flat in
/// the gap**, the two captures are equally unlike each other however far apart they are, which is
/// capture-side noise rather than an evolving scene.
///
/// Must stay at or above [`SHOT_SETTLE`] (15): below it the second screenshot is requested while
/// the first is still being read back, and the two would contend for the same render target.
fn twice_gap() -> u32 {
    static GAP: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *GAP.get_or_init(|| {
        std::env::var("ORNISCULPT_BENCH_TWICE_GAP").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(20).max(SHOT_SETTLE + 1)
    })
}

/// `ORNISCULPT_BENCH_NO_COAT=1`: spawn bodies without their coats, in **every** scenario.
///
/// Without coats the hero shots' worst same-binary difference falls from 2.242 % to 0.010 %
/// (`woman.head`), 1.137 % to 0.072 % (`ox_hereford.eye`) and 0.664 % to 0.026 %
/// (`horse_bay.eye`), so the coat's drawing carries nearly all of the gate's non-repeatability.
/// The crowd honours it too, because `crowd4000` draws coats as well and its instability had been
/// blamed on its capture trigger instead.
fn no_coat() -> bool {
    static NO_COAT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *NO_COAT.get_or_init(|| std::env::var("ORNISCULPT_BENCH_NO_COAT").is_ok_and(|v| v == "1"))
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
            if let Some(coat) = crowd::coat(i).filter(|_| !no_coat()) {
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
    mobs: &Query<(Entity, Has<MobBody>, Option<&MobTier>), With<Mob>>,
) {
    let name = bench.case_name();
    match case {
        // The first of `twice`'s two captures: shoot and keep measuring.
        CaseKind::Still { .. } if twice() && n == STILL_FRAMES => {
            shoot(commands, bench, target, &name);
            bench.phase = Phase::Measure(n);
        }
        CaseKind::Still { .. } if n >= STILL_FRAMES + if twice() { twice_gap() } else { 0 } => {
            // Before the result closes, so the shot is filed with it.
            let shot_name = if twice() { format!("{name}.again") } else { name.clone() };
            shoot(commands, bench, target, &shot_name);
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
            let counts = LodLevel::ALL.map(|l| mobs.iter().filter(|(_, _, t)| t.is_some_and(|t| t.0 == l)).count());
            for (level, c) in LodLevel::ALL.iter().zip(counts) {
                bench.metrics.insert(format!("steady.tier.{}", format!("{level:?}").to_lowercase()), c.to_f32());
            }
            // *Which* mobs hold which tier, not just how many. The counts were identical across
            // six runs of one binary while the image was not, so the counts cannot say whether the
            // same 32 mobs are the heroes each time. Summed, so it does not depend on the order the
            // query happens to iterate.
            let mix = |v: u64| v.wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let (mut tier_fp, mut entity_fp) = (0u64, 0u64);
            for (e, _, t) in mobs {
                let id = e.to_bits();
                entity_fp = entity_fp.wrapping_add(mix(id));
                if let Some(t) = t {
                    tier_fp = tier_fp.wrapping_add(mix(id.wrapping_mul(3).wrapping_add(t.0 as u64 + 1)));
                }
            }
            // The low 24 bits, which an f32 metric holds exactly: enough to notice a change, and
            // `compare` treats it as a number.
            //
            // `entity_fingerprint24` is the control and must be read first. It ignores tiers, so
            // if *it* moves between runs the entity ids themselves differ and the tier figure says
            // nothing; only when the control is stable does a moving tier figure mean the same
            // mobs took different tiers.
            bench.metrics.insert("steady.entity_fingerprint24".into(), (entity_fp & 0xff_ffff).to_f32());
            bench.metrics.insert("steady.tier_fingerprint24".into(), (tier_fp & 0xff_ffff).to_f32());
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
    quality: Option<Res<QualityProfile>>,
    msaa: Query<&Msaa, With<BenchCamera>>,
    target: Res<Target>,
    mobs: Query<(Entity, Has<MobBody>, Option<&MobTier>), With<Mob>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(case) = bench.cases.get(bench.case).cloned() else {
        if bench.phase != Phase::Done {
            bench.phase = Phase::Done;
            let report = Report { meta: meta(&bench.scenario, adapter.as_deref(), quality.as_deref(), msaa.iter().next()), cases: std::mem::take(&mut bench.results) };
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
                    let attached = bench.entities.iter().filter(|e| mobs.get(**e).is_ok_and(|(_, b, t)| b && t.is_some())).count();
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
            // `twice` freezes the pose so its two captures differ only in when they were taken.
            if !(twice() && matches!(case, CaseKind::Still { .. })) {
                time.unpause();
            }
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
        Phase::Measure(n) => {
            request_capture(&bench, n + 1);
            measure(&mut commands, &mut bench, &target.0, &case, n + 1, &mobs);
        }
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
///
/// # This bar is below the gate's own repeatability on some shots
///
/// Measured 2026-11-03 on toth: **three `bench all` runs of one unchanged binary**, compared
/// pairwise. The worst difference a shot showed *against itself* — its noise floor — is nothing
/// like uniform, and for one shot it is **six times this threshold**:
///
/// | shot | floor | vs the 0.5 % bar |
/// |---|---|---|
/// | `crowd4000` | **3.153 %** | **6.3× the bar — this shot cannot judge a change of this size** |
/// | `ox_hereford.eye` | 0.282 % | |
/// | `woman.head` | 0.249 % | |
/// | `horse_bay.eye` | 0.208 % | |
/// | `spider.head` | 0.076 % | |
/// | `darner.eye` | 0.061 % | |
/// | `man.full`, `man.head`, `man.eye` | **0.000 %, PSNR inf** | byte-identical in all three runs |
///
/// So **compare a figure against its own shot's floor, not against this constant**. The spread
/// across shots is the point: a single global floor would be the wrong lesson, and the `man.*`
/// shots at exactly zero show the pipeline *can* be deterministic.
///
/// The non-determinism is **per mob, not positional**. It spares the **man** specifically: with
/// the case order reversed so the man generates *last* and the darner *first*, `man.full`,
/// `man.head` and `man.eye` are still byte-identical (PSNR inf) and `darner.*` is still not
/// (0.001 %, finite PSNR), while `woman.head` reaches 2.242 % and `ox_hereford.eye` 1.137 %. An
/// earlier revision of this comment said the cause was "order-dependent and spares the first entry
/// in `MOBS`" — that was a confound, asserted on the wrong side, and it is withdrawn.
///
/// Three candidate causes are **disconfirmed**, not merely unconfirmed:
///
/// - **Not work distribution.** `ORNISCULPT_WORKERS=1` is still non-repeatable and mostly worse
///   (`crowd4000` 3.153 % → 6.255 %, `woman.head` 0.249 % → 0.745 %), so `par_jobs` chunking and
///   `VoxelBinding::spread`'s heap order cannot be it.
/// - **Not tier-count drift.** Six runs give identical `steady.tier.*` (32 / 1707 / 2261) and
///   identical `cold.bodies` (24) while the image differs.
/// - **Not tier *identity* either.** `steady.tier_fingerprint24` is identical (5628265) across
///   three further runs whose `cold.frames` were 1455 / 1683 / 1687, with its control
///   `steady.entity_fingerprint24` also identical (12013424) — so entity ids are stable, the tier
///   figure is meaningful, and the same mobs hold the same tiers every run while `crowd4000` still
///   differs by up to 0.808 %.
///
/// **`crowd4000` specifically is not reproducible by construction, and no threshold can fix it.**
/// `cold.frames` across identical runs: 1614 / 1966 / 1667 at sixteen workers, 5396 / 4392 / 4308
/// at one, 1455 / 1683 / 1687 again later. The cold phase ends on a wall-clock condition — every
/// body attached and nothing in flight — and the steady shot fires a fixed number of frames later.
///
/// **The variance is measured; the mechanism is not.** An earlier revision said the shot therefore
/// "photographs the crowd at a different point in its animation", which is now doubtful: virtual
/// time is paused throughout the cold phase and advances exactly 1/60 s a frame afterwards, and
/// the tier assignment is identical by identity, so the obvious route from a longer cold phase to a
/// different picture is closed. That half is withdrawn. What stands is that `crowd4000` is not
/// reproducible and `cold.frames` is not stable.
///
/// The hero shots' mechanism is still open and is deliberately not guessed at here.
///
/// The PSNR half of the gate is sound: verified against an independent decoder sharing no code with
/// this one (40.09 against 40.1, 32.91 against 32.9). It is the thresholded *count* that cannot
/// carry a verdict at this scale — being a count over a threshold, it is a non-linear function of
/// the underlying difference, and a population of pixels near 8/255 can cross the line and move it
/// a long way while the image barely changes.
///
/// Also note the `noise` column printed beside each shot is the baseline set compared **against
/// itself**, so it reads 0.000 % / inf dB always. That demonstrates the comparison works; it is
/// **not** a measurement of run-to-run noise, and it was mistaken for one.
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

/// What the two sides of a comparison can and cannot prove about each other. None of this fails a
/// run — a deliberate same-binary comparison is how the noise floor is measured — but all of it is
/// said out loud, because every one of these is a confounder that has already produced a wrong
/// conclusion somewhere in this repo's bench history.
fn compare_provenance(olds: &[&Report], new: &Report) {
    let short = |s: &str| -> String { if s.is_empty() { "unknown".into() } else { s.chars().take(7).collect() } };
    let b = &new.meta.build;
    let unstamped = std::iter::once(new).chain(olds.iter().copied()).filter(|r| r.meta.build.git.is_empty()).count();
    if unstamped > 0 {
        println!("   !! {unstamped} of {} reports have no build stamp: this comparison cannot prove what it ran", olds.len() + 1);
    }
    if olds.iter().all(|o| o.meta.build.exe_xxh3 == b.exe_xxh3) && !b.exe_xxh3.is_empty() {
        println!("   !! same binary as every baseline (xxh3 {}): this measures run-to-run noise, not a change", short(&b.exe_xxh3));
    } else {
        println!("   binaries differ: baseline xxh3 {} → new {}", short(&first_of(olds).map_or_default(|o| o.meta.build.exe_xxh3.clone())), short(&b.exe_xxh3));
    }
    for (what, r) in std::iter::once(("new", new)).chain(olds.iter().map(|o| ("baseline", *o))) {
        if r.meta.stale_binary {
            println!("   !! {what} was a stale binary: built {}, run against {}", short(&r.meta.build.git), short(&r.meta.run_git));
        }
        if r.meta.build.dirty {
            println!("   !! {what} was built from a dirty tree ({})", short(&r.meta.build.git));
        }
    }
    let Some(base) = first_of(olds) else { return };
    for (what, old, now) in [
        ("rustc", &base.meta.build.rustc, &b.rustc),
        ("profile", &base.meta.build.profile, &b.profile),
        ("target", &base.meta.build.target, &b.target),
        ("features", &base.meta.build.features, &b.features),
    ] {
        if old != now {
            println!("   !! {what} differs: {old:?} → {now:?}");
        }
    }
    diff_map("setting", &base.meta.settings, &new.meta.settings);
    diff_map("env", &base.meta.env, &new.meta.env);
    // `--out` names the result directory and is expected to differ; nothing else should.
    let strip_out = |a: &[String]| -> Vec<String> {
        let at = a.iter().position(|x| x == "--out");
        a.iter().enumerate().filter(|(i, _)| at.is_none_or(|o| *i != o && *i != o + 1)).map(|(_, s)| s.clone()).collect()
    };
    let (old_args, new_args) = (strip_out(&base.meta.args), strip_out(&new.meta.args));
    // The program path itself differs whenever the two sides were built into different trees.
    if old_args.get(1..) != new_args.get(1..) {
        println!("   !! command line differs: {:?} → {:?}", old_args.get(1..).unwrap_or_default(), new_args.get(1..).unwrap_or_default());
    }
}

/// Where an accepted golden set came from, written beside the PNGs by `--accept`.
const ACCEPTED: &str = "accepted.ron";

/// Says which machine, GPU and build the goldens in `dir` were accepted on, and shouts when that
/// is not the machine now being judged. **A golden set is per-machine**: the image gate compares
/// renders, and two GPUs do not produce the same pixels, so a set recorded on toth has no
/// authority over tir's or the Deck's renders. Without this the gate would be run across machines
/// and its verdict read as if it meant something.
fn report_golden_origin(dir: &Path, scenario: &str, now: &Meta) {
    let Ok(text) = std::fs::read_to_string(dir.join(ACCEPTED)) else { return };
    let Ok(accepted) = ron::from_str::<BTreeMap<String, Meta>>(&text) else {
        println!("   note: {}/{ACCEPTED} could not be read", dir.display());
        return;
    };
    let Some(m) = accepted.get(scenario) else { return };
    let short: String = m.build.git.chars().take(7).collect();
    println!("   goldens accepted on {} / {} from {} (xxh3 {})", m.machine, m.gpu, short, m.build.exe_xxh3.chars().take(7).collect::<String>());
    if m.machine != now.machine || m.gpu != now.gpu {
        println!("   !! these goldens judge {} / {} only — this run is {} / {}", m.machine, m.gpu, now.machine, now.gpu);
    }
    if m.settings != now.settings {
        diff_map("golden setting", &m.settings, &now.settings);
    }
}

const fn first_of<'a>(olds: &[&'a Report]) -> Option<&'a Report> {
    olds.first().copied()
}

/// Keys that differ between the baseline's header map and the new one's, in either direction.
fn diff_map(what: &str, old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) {
    for (k, v) in new {
        match old.get(k) {
            Some(o) if o == v => (),
            Some(o) => println!("   !! {what} {k} differs: {o:?} → {v:?}"),
            None => println!("   !! {what} {k} set only in the new run: {v:?}"),
        }
    }
    for (k, v) in old {
        if !new.contains_key(k) {
            println!("   !! {what} {k} set only in the baseline: {v:?}");
        }
    }
}

fn compare(bases: &str, new: &Path, accept: bool) -> Result<(), String> {
    let base_dirs: Vec<PathBuf> = bases.split(',').map(PathBuf::from).collect();
    let base_reports: Vec<_> = base_dirs.iter().map(|d| read_reports(d)).collect();
    let new_reports = read_reports(new);
    let mut failures = Vec::new();
    for (scenario, report) in &new_reports {
        let olds: Vec<&Report> = base_reports.iter().filter_map(|r| r.get(scenario)).collect();
        println!("== {scenario}");
        // `bench/golden` holds accepted PNGs and no `.ron` — which is exactly how the image gate
        // is meant to be run (§1.4 of the plan: "passing `bench/golden` as a base compares images
        // against the accepted look"). Having no metric baseline must therefore skip the metrics
        // and nothing else: skipping the whole scenario made that documented command silently
        // check nothing at all. The metric loop below already skips metrics it has no baseline for.
        if let Some(first) = olds.first() {
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
            compare_provenance(&olds, report);
        } else {
            println!("   no metric baseline: images only (no .ron in the base, as in bench/golden)");
        }
        for dir in &base_dirs {
            report_golden_origin(dir, scenario, &report.meta);
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
        let golden = root().join("bench/golden");
        std::fs::create_dir_all(&golden).map_err(|e| e.to_string())?;
        for case in new_reports.values().flat_map(|r| &r.cases) {
            for image in &case.images {
                std::fs::copy(new.join(image), golden.join(image)).map_err(|e| format!("{image}: {e}"))?;
            }
        }
        // Whose look this is. A golden set is a record of one machine's renders by one binary, and
        // without this the accepted PNGs say nothing about which — the same hole the result trees
        // had. `compare` reads it back and shouts when the gate is being run on another machine.
        let accepted: BTreeMap<&String, &Meta> = new_reports.iter().map(|(s, r)| (s, &r.meta)).collect();
        let text = ron::ser::to_string_pretty(&accepted, ron::ser::PrettyConfig::default()).map_err(|e| e.to_string())?;
        std::fs::write(golden.join(ACCEPTED), text).map_err(|e| e.to_string())?;
        println!("accepted images into {} ({ACCEPTED} records the machine, GPU and build they came from)", golden.display());
    }
    if failures.is_empty() {
        println!("ok");
        Ok(())
    } else {
        Err(format!("{} gate failure(s):\n  {}", failures.len(), failures.join("\n  ")))
    }
}
