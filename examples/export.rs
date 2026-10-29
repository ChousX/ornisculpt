//! Exports a body plan as a `*.body.glb` to author in a DCC tool: an empty per part carrying the
//! part, and the hero skin on its skeleton to model against (see `docs/design/interchange.md`).
//! Headless; authored part meshes are read straight from `assets/parts`.
//!
//! ```text
//! cargo run --release --example export --features interchange -- mobs/woman.body.ron woman.body.glb [coats/skin_woman.coat.ron]
//! ```

use std::process::ExitCode;

use ornisculpt::anatomy::{BodyPlan, LodLevel, PartLodRules};
use ornisculpt::coat::Coat;
use ornisculpt::interchange::export_body;
use ornisculpt::surface::{AdaptiveContour, SurfaceSettings, mesh_lod};

mod common;

fn run() -> Result<String, String> {
    let (Some(plan_path), Some(out)) = (common::asset_arg(1), std::env::args().nth(2)) else {
        return Err("usage: export <plan.body.ron> <out.body.glb> [coat.coat.ron]".into());
    };
    let read = |p: &str| std::fs::read_to_string(format!("{}/assets/{p}", env!("CARGO_MANIFEST_DIR"))).map_err(|e| format!("{p}: {e}"));
    let plan: BodyPlan = ron::from_str(&read(&plan_path)?).map_err(|e| format!("{plan_path}: {e}"))?;
    let coat = common::asset_arg(3).map(|c| ron::from_str::<Coat>(&read(&c)?).map_err(|e| format!("{c}: {e}"))).transpose()?;
    let assets = common::part_assets(&plan);
    let anatomy = common::build_plan(&plan, PartLodRules::default(), &assets)?;
    let settings = SurfaceSettings::default();
    let skin = mesh_lod(&AdaptiveContour, &anatomy, LodLevel::Hero, settings.get(LodLevel::Hero), 0).map_err(|e| e.to_string())?;
    let bytes = export_body(&plan, coat.as_ref(), &anatomy, &assets, Some(&skin)).map_err(|e| e.to_string())?;
    std::fs::write(&out, &bytes).map_err(|e| format!("{out}: {e}"))?;
    Ok(format!("{out}: {} parts, {} bones, {} triangles, {} KB", anatomy.parts.len(), anatomy.skeleton.len(), skin.triangle_count(), bytes.len() / 1024))
}

fn main() -> ExitCode {
    match run() {
        Ok(done) => {
            println!("{done}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
