//! `*.body.glb` / `*.body.gltf` as a [`BodyPlan`] asset, so a save from a DCC hot-reloads.

use std::sync::Arc;

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use thiserror::Error;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};

use super::{ImportError, place_moved, read_body, read_trimesh};
use crate::anatomy::{BodyPlan, PartAssets, TriMesh, referenced_assets};

/// Loads a body plan from glTF (see [`import_body`](super::import_body)). Warnings are logged.
#[derive(Default, TypePath)]
pub struct BodyGltfLoader;

#[derive(Debug, Error)]
pub enum BodyGltfLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Import(#[from] ImportError),
}

impl AssetLoader for BodyGltfLoader {
    type Asset = BodyPlan;
    type Settings = ();
    type Error = BodyGltfLoaderError;

    async fn load(&self, reader: &mut dyn Reader, (): &(), cx: &mut LoadContext<'_>) -> Result<BodyPlan, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        // Part meshes modelled in the file are served as its own labelled meshes.
        let own = cx.path().without_label().to_string();
        let read = read_body(&bytes, &own)?;
        // Placing a moved part builds the body, which needs the meshes it references.
        let mut assets = PartAssets::default();
        if !read.body.moved.is_empty() {
            for path in referenced_assets(&read.body.plan).into_iter().filter(|p| !p.starts_with(&format!("{own}#"))) {
                let (file, label) = path.split_once('#').unwrap_or((path.as_str(), ""));
                let mesh = cx.read_asset_bytes(file.to_owned()).await.ok().and_then(|b| read_trimesh(&b, label));
                if let Some(mesh) = mesh {
                    assets.insert(path, Arc::new(mesh));
                } else {
                    warn!("{}: can't read part mesh `{path}`", cx.path());
                }
            }
        }
        let body = place_moved(read, &assets)?;
        for w in &body.warnings {
            warn!("{}: {w}", cx.path());
        }
        for (path, mesh) in body.meshes {
            if let Some((_, label)) = path.split_once('#') {
                cx.add_labeled_asset(label.to_owned(), bevy_mesh(&mesh));
            }
        }
        Ok(body.plan)
    }

    fn extensions(&self) -> &[&str] {
        &["body.glb", "body.gltf"]
    }
}

/// A part mesh as a Bevy mesh, which is what generation reads authored meshes from.
fn bevy_mesh(m: &TriMesh) -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, m.positions.iter().map(Vec3::to_array).collect::<Vec<_>>());
    if m.normals.len() == m.positions.len() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, m.normals.iter().map(Vec3::to_array).collect::<Vec<_>>());
    }
    mesh.insert_indices(Indices::U32(m.indices.clone()));
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{Anatomy, PartKind, PartLodRules, test_assets};
    use crate::interchange::{KEY, export_body, read_glb, write_glb};

    /// The asset server loads a `*.body.glb` as a plan, and serves the part meshes modelled in it
    /// (here every authored mesh, its exported fingerprint dropped) as its labelled meshes.
    #[test]
    fn loads_a_body_with_its_meshes() {
        let src = std::fs::read_to_string(format!("{}/assets/mobs/beast.body.ron", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let plan: BodyPlan = ron::from_str(&src).unwrap();
        let assets = test_assets(&plan);
        let anatomy = Anatomy::from_plan(&plan, &PartLodRules::default(), &assets).unwrap();
        let (mut doc, bin) = read_glb(&export_body(&plan, None, &anatomy, &assets, None).unwrap()).map(|(d, b)| (d, b.to_vec())).unwrap();
        for n in doc["nodes"].as_array_mut().unwrap() {
            if let Some(o) = n.pointer_mut(&format!("/extras/{KEY}")).and_then(serde_json::Value::as_object_mut) {
                o.remove("mesh_print");
            }
        }
        let dir = std::env::temp_dir().join(format!("ornisculpt-loader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("beast.body.glb"), write_glb(&doc, &bin).unwrap()).unwrap();

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin { file_path: dir.to_string_lossy().into_owned(), ..default() }))
            .init_asset::<BodyPlan>()
            .init_asset::<Mesh>()
            .init_asset_loader::<BodyGltfLoader>();
        let server = app.world().resource::<AssetServer>().clone();
        let handle: Handle<BodyPlan> = server.load("beast.body.glb");
        let mesh: Handle<Mesh> = server.load("beast.body.glb#Mesh0/Primitive0");
        for _ in 0..2000 {
            app.update();
            if server.is_loaded_with_dependencies(&handle) && server.is_loaded(&mesh) {
                break;
            }
            assert!(!server.load_state(&handle).is_failed(), "{:?}", server.load_state(&handle));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = std::fs::remove_dir_all(&dir);
        let loaded = app.world().resource::<Assets<BodyPlan>>().get(&handle).expect("the plan loads");
        let mut sculpts = Vec::new();
        loaded.visit(|_, n| {
            if let PartKind::Sculpt(s) = &n.kind {
                sculpts.push(s.mesh.clone());
            }
        });
        assert!(!sculpts.is_empty() && sculpts.iter().all(|m| m.starts_with("beast.body.glb#Mesh")), "{sculpts:?}");
        let mesh = app.world().resource::<Assets<Mesh>>().get(&mesh).expect("its part mesh loads");
        assert!(mesh.count_vertices() > 0);
    }
}
