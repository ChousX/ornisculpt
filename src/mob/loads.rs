//! The assets a mob depends on, as generation sees them: loaded, still loading, or failed. A
//! failed load (a RON parse error, a missing file) used to leave the mob requested for ever;
//! now it fails the body through [`BodyFailed`](super::BodyFailed).

use std::collections::HashSet;

use bevy::asset::{LoadState, UntypedAssetId};
use bevy::prelude::*;

use crate::motion::{Gait, MobGait};

/// An asset a requested mob needs.
pub(super) enum Loaded<A> {
    /// Still loading.
    Waiting,
    Ready(A),
    /// The load failed, with the asset server's reason.
    Failed(UntypedAssetId, String),
}

impl<A> Loaded<A> {
    pub(super) fn map<B>(self, f: impl FnOnce(A) -> B) -> Loaded<B> {
        match self {
            Self::Waiting => Loaded::Waiting,
            Self::Ready(a) => Loaded::Ready(f(a)),
            Self::Failed(id, e) => Loaded::Failed(id, e),
        }
    }
}

/// The asset `handle` points at, once loaded; failed if its load has.
pub(super) fn loaded<'a, A: Asset>(handle: &Handle<A>, assets: &'a Assets<A>, server: &AssetServer) -> Loaded<&'a A> {
    if let Some(a) = assets.get(handle) {
        return Loaded::Ready(a);
    }
    match server.load_state(handle.id()) {
        LoadState::Failed(e) => Loaded::Failed(handle.id().untyped(), e.to_string()),
        _ => Loaded::Waiting,
    }
}

/// As [`loaded`], cloned; the default asset without a handle.
pub(super) fn loaded_or_default<A: Asset + Clone + Default>(handle: Option<&Handle<A>>, assets: &Assets<A>, server: &AssetServer) -> Loaded<A> {
    handle.map_or_else(|| Loaded::Ready(A::default()), |h| loaded(h, assets, server).map(Clone::clone))
}

/// The mob's [`MobGait`] has been seen loaded, or failed and reported.
#[derive(Component)]
pub(super) struct GaitChecked;

/// Reports a gait that fails to load (a bad `*.gait.ron`), once per asset. Motion falls back
/// to the built-in gait for the body's leg count, as it does for a mob without a [`MobGait`],
/// so the mob still walks; the report is what says why it walks like that.
pub(super) fn report_failed_gaits(
    mut commands: Commands,
    server: Res<AssetServer>,
    gaits: Res<Assets<Gait>>,
    mobs: Query<(Entity, &MobGait), Without<GaitChecked>>,
    mut reported: Local<HashSet<AssetId<Gait>>>,
) {
    for (entity, gait) in &mobs {
        let id = gait.0.id();
        // A gait added in code (no path) never loads; it is checked once seen in `Assets`.
        let checked = match server.load_state(id) {
            _ if gaits.contains(id) => true,
            LoadState::Failed(e) => {
                if reported.insert(id) {
                    error!("mob {entity}: gait failed to load, walking on the built-in gait: {e}");
                }
                true
            }
            LoadState::NotLoaded => gait.0.path().is_none(),
            LoadState::Loading | LoadState::Loaded => false,
        };
        if checked {
            commands.entity(entity).insert(GaitChecked);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bevy::asset::AssetPlugin;
    use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
    use bevy::render::storage::ShaderBuffer;

    use super::*;
    use crate::anatomy::{BodyPlan, BodyPlanLoader, PartLodRules};
    use crate::clothing::Outfit;
    use crate::coat::{Coat, CoatLoader, MobCoat, MobMaterial};
    use crate::mob::{BodyFailed, GenRequest, LiveBody, Mob, MobPlugin};
    use crate::motion::GaitLoader;
    use crate::surface::SurfaceSettings;

    /// A malformed plan, a malformed coat and a missing plan, through the asset loaders and the
    /// generation systems: each mob gets `BodyFailed` naming its file, and stops asking for a
    /// body. A malformed gait is reported and its mob checked, apart from its body.
    #[test]
    fn bad_assets_fail_the_mob() {
        let dir = std::env::temp_dir().join(format!("ornisculpt-loads-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.body.ron"), "(root: (name: \"torso\", kind: Torso(").unwrap();
        std::fs::write(dir.join("quadruped.body.ron"), include_str!("../../assets/mobs/quadruped.body.ron")).unwrap();
        std::fs::write(dir.join("bad.coat.ron"), "(base: [0.5, 0.5").unwrap();
        std::fs::write(dir.join("bad.gait.ron"), "(duty: )").unwrap();

        let failures: Arc<Mutex<Vec<(Entity, String)>>> = Arc::default();
        let seen = failures.clone();
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin { file_path: dir.to_string_lossy().into_owned(), ..default() }))
            .init_asset::<BodyPlan>()
            .init_asset_loader::<BodyPlanLoader>()
            .init_asset::<Coat>()
            .init_asset_loader::<CoatLoader>()
            .init_asset::<Gait>()
            .init_asset_loader::<GaitLoader>()
            .init_asset::<Outfit>()
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .init_asset::<MobMaterial>()
            .init_asset::<ShaderBuffer>()
            .init_asset::<SkinnedMeshInverseBindposes>()
            .init_resource::<SurfaceSettings>()
            .init_resource::<PartLodRules>()
            .add_plugins(MobPlugin)
            .add_observer(move |ev: On<BodyFailed>| seen.lock().unwrap().push((ev.entity, ev.error.clone())));
        let server = app.world().resource::<AssetServer>().clone();
        let bad_plan = app.world_mut().spawn(Mob::new(server.load("bad.body.ron"))).id();
        let bad_coat = app.world_mut().spawn((Mob::new(server.load("quadruped.body.ron")), MobCoat(server.load("bad.coat.ron")))).id();
        let missing = app.world_mut().spawn((Mob::new(server.load("missing.body.ron")), MobGait(server.load("bad.gait.ron")))).id();
        let mut checked = false;
        for _ in 0..2000 {
            app.update();
            checked = app.world().entity(missing).contains::<GaitChecked>();
            if failures.lock().unwrap().len() >= 3 && checked {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let failed = failures.lock().unwrap().clone();
        let error_of = |e: Entity| failed.iter().find(|(f, _)| *f == e).map(|(_, m)| m.as_str());
        assert!(error_of(bad_plan).is_some_and(|m| m.contains("bad.body.ron")), "{failed:?}");
        assert!(error_of(bad_coat).is_some_and(|m| m.contains("bad.coat.ron")), "{failed:?}");
        assert!(error_of(missing).is_some_and(|m| m.contains("missing.body.ron")), "{failed:?}");
        assert!(checked, "the bad gait was checked");
        assert!(!app.world().entity(bad_plan).contains::<GenRequest>(), "a failed mob stops asking");
        assert!(!app.world().entity(bad_coat).contains::<GenRequest>(), "a failed mob stops asking");
        assert!(!app.world().entity(missing).contains::<GenRequest>(), "a failed mob stops asking");

        // The plan fixed and reloaded: the mob asks again, and gets as far as its live body.
        std::fs::write(dir.join("bad.body.ron"), include_str!("../../assets/mobs/quadruped.body.ron")).unwrap();
        server.reload("bad.body.ron");
        for _ in 0..2000 {
            app.update();
            if app.world().entity(bad_plan).contains::<LiveBody>() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(app.world().entity(bad_plan).contains::<LiveBody>(), "the fixed plan is taken up");
    }
}
