//! Ornisculpt: procedural creature/mob generation for Bevy.
//!
//! Design reference: `docs/design/types.md`.
//!
//! Runs in the browser too (WebGPU): the `web` feature, built and served by `tools/web.sh`.

pub mod anatomy;
pub mod clothing;
pub mod coat;
pub mod equip;
pub mod mob;
pub mod motion;
pub mod num;
pub mod random;
pub mod surface;

use bevy::prelude::*;

#[cfg(all(target_arch = "wasm32", not(feature = "web")))]
compile_error!("browser builds need the `web` feature: `cargo build --target wasm32-unknown-unknown --features web`, or `tools/web.sh`");

/// Top-level plugin. Adds every Ornisculpt sub-plugin.
#[derive(Default)]
pub struct OrnisculptPlugin;

impl Plugin for OrnisculptPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            anatomy::AnatomyPlugin,
            coat::CoatPlugin,
            surface::SurfacePlugin,
            mob::MobPlugin,
            motion::MotionPlugin,
            equip::EquipPlugin,
            clothing::ClothingPlugin,
        ));
    }
}
