use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use thiserror::Error;

use super::BodyPlan;

/// Loads `*.body.ron` files into [`BodyPlan`]s.
#[derive(Default, TypePath)]
pub struct BodyPlanLoader;

#[derive(Debug, Error)]
pub enum BodyPlanLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for BodyPlanLoader {
    type Asset = BodyPlan;
    type Settings = ();
    type Error = BodyPlanLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<BodyPlan, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        &["body.ron"]
    }
}
