//! Build-time provenance, for the benchmark's report header (`examples/bench.rs`).
//!
//! The commit a measurement came from has to be stamped into the *binary*, not read back from
//! whatever checkout the binary is later pointed at: `bench` is routinely built on toth and run
//! against another checkout (`ORNISCULPT_BENCH_ROOT`, the Steam Deck), and reading `git rev-parse
//! HEAD` at run time there records the checkout's commit, not the build's. Six A/B result trees
//! have already been written claiming the same commit for both sides of the comparison.
//!
//! Emitted (all `env!`-readable, all empty-string-safe when there is no git):
//!
//! - `ORNISCULPT_BUILD_GIT` — full `HEAD` at compile time, `""` outside a git checkout.
//! - `ORNISCULPT_BUILD_DIRTY` — `1` if any *watched* path (see [`WATCHED`]) was modified at
//!   compile time, else `0`. Only the watched paths, because those are the ones this script asks
//!   cargo to rerun it for: a change outside them cannot flip the flag, so claiming to cover them
//!   would be a lie. They are the paths a benchmark's behaviour can come from.
//! - `ORNISCULPT_BUILD_RUSTC`, `ORNISCULPT_BUILD_PROFILE`, `ORNISCULPT_BUILD_TARGET`,
//!   `ORNISCULPT_BUILD_FEATURES` — toolchain, profile, target triple and the enabled cargo
//!   features, which change a measurement as surely as the source does.
//!
//! Nothing here varies between two builds of the same tree (no timestamp, no hostname): a value
//! that changed every build would be part of the crate's fingerprint and recompile the whole
//! crate every time.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Paths `ORNISCULPT_BUILD_DIRTY` is computed over, and that cargo is asked to rerun this script
/// for. Everything a bench run's behaviour comes out of; `docs/` and `bench/` are left out so
/// writing up a result doesn't recompile the crate.
const WATCHED: [&str; 6] = ["src", "examples", "assets", "Cargo.toml", "Cargo.lock", "build.rs"];

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    for path in WATCHED {
        rerun(&root.join(path));
    }
    for path in git_watch_paths(&root) {
        rerun(&path);
    }

    let head = git(&root, &["rev-parse", "HEAD"]).unwrap_or_default();
    // `--` so a watched name that is also a ref ("assets") can't be read as a revision.
    let status = git(&root, &[&["status", "--porcelain", "--untracked-files=no", "--"], WATCHED.as_slice()].concat());
    // No git at all: say clean rather than invent a dirt that can't be checked. `ORNISCULPT_BUILD_GIT`
    // is empty in that case, which is the louder signal and the one the header leads with.
    let dirty = status.is_some_and(|s| !s.trim().is_empty());

    emit("ORNISCULPT_BUILD_GIT", &head);
    emit("ORNISCULPT_BUILD_DIRTY", if dirty { "1" } else { "0" });
    emit("ORNISCULPT_BUILD_RUSTC", &rustc_version());
    emit("ORNISCULPT_BUILD_PROFILE", &env("PROFILE"));
    emit("ORNISCULPT_BUILD_TARGET", &env("TARGET"));
    emit("ORNISCULPT_BUILD_FEATURES", &features());
}

fn emit(key: &str, value: &str) {
    println!("cargo:rustc-env={key}={value}");
}

fn rerun(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// `git`'s stdout, trimmed; `None` when git is missing, this is not a checkout, or it failed.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()) } else { None }
}

/// The git files that move when the commit does: `HEAD`, the ref it names (so a commit on the
/// current branch is seen), the index, and `packed-refs` for a worktree whose ref is packed.
/// Asked of git rather than built from `.git/...`, because in a linked worktree — which is how the
/// perf agents work — `.git` is a file and the real directory is elsewhere.
fn git_watch_paths(root: &Path) -> Vec<PathBuf> {
    let path = |rel: &str| git(root, &["rev-parse", "--git-path", rel]).map(PathBuf::from);
    let mut paths: Vec<PathBuf> = ["HEAD", "index", "packed-refs"].iter().filter_map(|r| path(r)).collect();
    if let Some(r) = git(root, &["symbolic-ref", "--quiet", "HEAD"]).filter(|r| !r.is_empty()) {
        paths.extend(path(&r));
    }
    paths
}

/// `rustc -vV`'s version line (`rustc 1.93.0 (...)`), which pins the compiler a measurement was
/// made with.
fn rustc_version() -> String {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    Command::new(rustc)
        .arg("-vV")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_owned))
        .unwrap_or_default()
}

/// The enabled cargo features, comma-separated and sorted. Cargo reports them upper-cased with
/// `-` turned into `_`, so the `_` is turned back: every feature this crate has is hyphenated or
/// one word, and a genuinely underscored name would come back hyphenated here.
fn features() -> String {
    let mut names: Vec<String> = std::env::vars()
        .filter_map(|(k, _)| k.strip_prefix("CARGO_FEATURE_").map(|f| f.to_lowercase().replace('_', "-")))
        .collect();
    names.sort_unstable();
    names.join(",")
}
