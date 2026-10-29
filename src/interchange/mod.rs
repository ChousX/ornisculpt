//! Interchange (`interchange` feature): body plans, parts and clothes to and from glTF, so artists
//! can author in their own tools. See `docs/design/interchange.md`.
//!
//! A `*.body.glb` holds one empty per authored part, nested as the part tree, each carrying its
//! [`PartNode`](crate::anatomy::PartNode) (without children) in `extras.ornisculpt.part`, plus a
//! reference skeleton and skin to model against. [`export_body`] writes one; importing reads the
//! plan back from the part empties, so an unedited file round-trips to an equal plan.

mod attach;
mod authored;
mod export;
mod garment;
mod glb;
mod import;
mod loader;
mod mesh;

use bevy::prelude::*;

pub use attach::axis_coords;
pub use export::{BodyExport, ExportError, export_body};
pub use garment::{Unwrapped, infer_on, unwrap_garment};
pub use glb::{GlbError, read_glb, write_glb};
pub use import::{ImportError, ImportedBody, ReadBody, import_body, place_moved, read_body};
pub use loader::{BodyGltfLoader, BodyGltfLoaderError, OUTFIT_LABEL};
pub use mesh::{gltf_document, read_trimesh, trimesh, write_meshes_gltf};

/// Version of the `extras.ornisculpt` layout written by [`export_body`].
pub const FORMAT: u32 = 1;

/// Name of the scene's root node, and of the `extras` key every Ornisculpt value sits under.
pub const KEY: &str = "ornisculpt";

/// Registers the glTF loaders (`*.body.glb`, `*.body.gltf`).
pub struct InterchangePlugin;

impl Plugin for InterchangePlugin {
    fn build(&self, app: &mut App) {
        app.init_asset_loader::<BodyGltfLoader>();
    }
}

/// Every sample plan in `assets/mobs`, by name. Unoptimised test builds leave out plans with a
/// sculpted head: its mesh converts to a fine SDF, which takes minutes there (seconds in
/// `--release`, which covers them).
#[cfg(test)]
pub(crate) fn sample_plans() -> Vec<(String, crate::anatomy::BodyPlan)> {
    use crate::anatomy::{BodyPlan, PartKind};
    let dir = format!("{}/assets/mobs", env!("CARGO_MANIFEST_DIR"));
    let mut out: Vec<(String, BodyPlan)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let file = e.ok()?.file_name().into_string().ok()?;
            let name = file.strip_suffix(".body.ron")?.to_owned();
            let plan: BodyPlan = ron::from_str(&std::fs::read_to_string(format!("{dir}/{file}")).unwrap()).unwrap();
            Some((name, plan))
        })
        .filter(|(_, plan)| {
            let mut sculpted = false;
            plan.visit(|_, n| sculpted |= matches!(&n.kind, PartKind::Head(h) if h.sculpt.is_some()));
            !(cfg!(debug_assertions) && sculpted)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() > 10);
    out
}
