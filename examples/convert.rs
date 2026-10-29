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

use ornisculpt::interchange::{place_moved, read_body, write_meshes_gltf};

mod common;

fn run() -> Result<(), String> {
    let Some(input) = std::env::args().nth(1) else {
        return Err("usage: convert <in.body.glb> [out.body.ron]".into());
    };
    let bytes = std::fs::read(&input).map_err(|e| format!("{input}: {e}"))?;
    let out = std::env::args().nth(2).map(|out| {
        // A relative path is an asset path, like the plans the other examples take.
        let file = if std::path::Path::new(&out).is_absolute() { out.clone() } else { format!("{}/assets/{out}", env!("CARGO_MANIFEST_DIR")) };
        (out, file)
    });
    // Part meshes modelled in the DCC go to a parts file beside the plan.
    let parts = out.as_ref().map_or_else(|| "parts.gltf".to_owned(), |(out, _)| format!("{}.parts.gltf", out.trim_end_matches(".body.ron")));
    let read = read_body(&bytes, &parts).map_err(|e| format!("{input}: {e}"))?;
    let assets = common::part_assets(&read.body.plan);
    let body = place_moved(read, &assets).map_err(|e| format!("{input}: {e}"))?;
    for path in &body.moved {
        eprintln!("moved: {path}");
    }
    for w in &body.warnings {
        eprintln!("warning: {w}");
    }
    let text = ron::ser::to_string_pretty(&body.plan, ron::ser::PrettyConfig::new()).map_err(|e| e.to_string())?;
    match out {
        Some((_, file)) => {
            std::fs::write(&file, text).map_err(|e| format!("{file}: {e}"))?;
            eprintln!("wrote {file}");
            if !body.meshes.is_empty() {
                let parts_file = format!("{}.parts.gltf", file.trim_end_matches(".body.ron"));
                let meshes: Vec<_> = body.meshes.into_iter().map(|(_, m)| m).collect();
                std::fs::write(&parts_file, write_meshes_gltf(&meshes)).map_err(|e| format!("{parts_file}: {e}"))?;
                eprintln!("wrote {parts_file} ({} part meshes)", meshes.len());
            }
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
