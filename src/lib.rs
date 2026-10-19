//! Ornisculpt: procedural creature/mob generation for Bevy.
//!
//! Design reference: `docs/design/types.md`.

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
