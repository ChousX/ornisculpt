//! One asset loader for every authored `*.ron` asset that loads as it deserializes.

use std::marker::PhantomData;

use bevy::asset::{Asset, AssetLoader, LoadContext, io::Reader};
use bevy::reflect::TypePath;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// An asset read straight from RON, by file extension.
pub trait RonAsset: Asset + DeserializeOwned {
    /// Extensions without the leading dot (`"coat.ron"`).
    const EXTENSIONS: &'static [&'static str];
}

/// Loads a [`RonAsset`].
#[derive(TypePath)]
pub struct RonLoader<A>(PhantomData<fn() -> A>);

impl<A> Default for RonLoader<A> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[derive(Debug, Error)]
pub enum RonLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
}

impl<A: RonAsset> AssetLoader for RonLoader<A> {
    type Asset = A;
    type Settings = ();
    type Error = RonLoaderError;

    async fn load(&self, reader: &mut dyn Reader, (): &(), _: &mut LoadContext<'_>) -> Result<A, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(ron::de::from_bytes(&bytes)?)
    }

    fn extensions(&self) -> &[&str] {
        A::EXTENSIONS
    }
}
