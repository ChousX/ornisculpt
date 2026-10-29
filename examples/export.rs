//! Exports a body plan as a `*.body.glb` to author in a DCC tool: an empty per part carrying the
//! part, the outfit's garments fitted on the body, and the hero skin on its skeleton to model
//! against (see `docs/design/interchange.md`).
//! Headless; authored part meshes are read straight from `assets/parts`.
//!
//! ```text
//! cargo run --release --example export --features interchange -- mobs/woman.body.ron woman.body.glb [coats/skin_woman.coat.ron|-] [outfits/peasant.outfit.ron]
//! ```

use std::process::ExitCode;

use ornisculpt::anatomy::{BodyPlan, LodLevel, PartLodRules};
use ornisculpt::coat::Coat;
use ornisculpt::clothing::Outfit;
use ornisculpt::interchange::{BodyExport, export_body, read_trimesh};
use ornisculpt::surface::{AdaptiveContour, SurfaceSettings, mesh_lod};

mod common;

fn run() -> Result<String, String> {
    let (Some(plan_path), Some(out)) = (common::asset_arg(1), std::env::args().nth(2)) else {
        return Err("usage: export <plan.body.ron> <out.body.glb> [coat.coat.ron|-] [outfit.outfit.ron]".into());
    };
    let read = |p: &str| std::fs::read_to_string(format!("{}/assets/{p}", env!("CARGO_MANIFEST_DIR"))).map_err(|e| format!("{p}: {e}"));
    let plan: BodyPlan = ron::from_str(&read(&plan_path)?).map_err(|e| format!("{plan_path}: {e}"))?;
    let coat = common::asset_arg(3).filter(|c| c != "-").map(|c| ron::from_str::<Coat>(&read(&c)?).map_err(|e| format!("{c}: {e}"))).transpose()?;
    let outfit = common::asset_arg(4).map(|o| ron::from_str::<Outfit>(&read(&o)?).map_err(|e| format!("{o}: {e}"))).transpose()?;
    let mut assets = common::part_assets(&plan);
    for mesh in outfit.iter().flat_map(Outfit::referenced_meshes) {
        let (file, label) = mesh.split_once('#').unwrap_or((mesh.as_str(), ""));
        let bytes = std::fs::read(format!("{}/assets/{file}", env!("CARGO_MANIFEST_DIR"))).map_err(|e| format!("{file}: {e}"))?;
        let m = read_trimesh(&bytes, label).ok_or_else(|| format!("{mesh}: not a triangle mesh"))?;
        assets.insert(mesh, std::sync::Arc::new(m));
    }
    let anatomy = common::build_plan(&plan, PartLodRules::default(), &assets)?;
    let settings = SurfaceSettings::default();
    let skin = mesh_lod(&AdaptiveContour, &anatomy, LodLevel::Hero, settings.get(LodLevel::Hero), 0).map_err(|e| e.to_string())?;
    let body = BodyExport { plan: &plan, anatomy: &anatomy, assets: &assets, coat: coat.as_ref(), outfit: outfit.as_ref(), skin: Some(&skin) };
    let bytes = export_body(&body).map_err(|e| e.to_string())?;
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
