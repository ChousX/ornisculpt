//! `*.body.glb` / `*.body.gltf` as a [`BodyPlan`] asset, so a save from a DCC hot-reloads.

use std::sync::Arc;

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use thiserror::Error;

use super::{ImportError, place_moved, read_body, read_trimesh};
use crate::anatomy::{BodyPlan, PartAssets, referenced_assets};

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
        let read = read_body(&bytes)?;
        // Placing a moved part builds the body, which needs the meshes it references.
        let mut assets = PartAssets::default();
        if !read.body.moved.is_empty() {
            for path in referenced_assets(&read.body.plan) {
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
        Ok(body.plan)
    }

    fn extensions(&self) -> &[&str] {
        &["body.glb", "body.gltf"]
    }
}
