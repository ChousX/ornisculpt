//! Converts a `*.body.glb` (or `.gltf`) authored in a DCC tool back to a `*.body.ron`, listing the
//! parts whose nodes moved and anything approximated (see `docs/design/interchange.md`). Headless;
//! authored part meshes are read straight from `assets/parts`.
//!
//! ```text
//! cargo run --release --example convert --features interchange -- woman.body.glb mobs/woman.body.ron
//! ```
//!
//! Without an output path the plan is printed.

use std::process::ExitCode;

use ornisculpt::interchange::{place_moved, read_body};

mod common;

fn run() -> Result<(), String> {
    let Some(input) = std::env::args().nth(1) else {
        return Err("usage: convert <in.body.glb> [out.body.ron]".into());
    };
    let bytes = std::fs::read(&input).map_err(|e| format!("{input}: {e}"))?;
    let read = read_body(&bytes).map_err(|e| format!("{input}: {e}"))?;
    let assets = common::part_assets(&read.body.plan);
    let body = place_moved(read, &assets).map_err(|e| format!("{input}: {e}"))?;
    for path in &body.moved {
        eprintln!("moved: {path}");
    }
    for w in &body.warnings {
        eprintln!("warning: {w}");
    }
    let text = ron::ser::to_string_pretty(&body.plan, ron::ser::PrettyConfig::new()).map_err(|e| e.to_string())?;
    match std::env::args().nth(2) {
        Some(out) => {
            // A relative path is an asset path, like the plans the other examples take.
            let out = if std::path::Path::new(&out).is_absolute() { out } else { format!("{}/assets/{out}", env!("CARGO_MANIFEST_DIR")) };
            std::fs::write(&out, text).map_err(|e| format!("{out}: {e}"))?;
            eprintln!("wrote {out}");
        }
        None => println!("{text}"),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
