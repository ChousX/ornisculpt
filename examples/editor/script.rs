//! The editor's self-test (`ORNISCULPT_EDITOR_TEST`): scripted edits, timed.

use bevy::prelude::*;
use bevy::render::view::window::screenshot::save_to_disk;
use ornisculpt::anatomy::{AttachSlot, PartLodRules, path_matches};
use ornisculpt::mob::PartAssetCache;
use ornisculpt::num::ToF32;

use crate::common::ShotTarget;
use crate::{BuildCtx, Drag, DragMode, Editor, FieldKind};

/// Scripted edits (`ORNISCULPT_EDITOR_TEST`): a held key on the first number of the first
/// child part, a drag of the first part attached to its parent's skin, a pause, then undo back.
/// Frames count from the first preview.
#[derive(Resource, Default)]
pub struct Script {
    frame: u32,
    pub done: bool,
    logged: String,
    /// Preview build times (ms) over the edits.
    preview_ms: Vec<f64>,
    /// Per preview built: percentage of its bricks baked anew, outside a drag (the held key edits
    /// the body tone: every brick) and during it.
    rebaked: [Vec<f64>; 2],
    /// Where the dragged part's root was at the grab.
    grab: Option<Vec3>,
    /// Frame times (ms): the preview standing, the preview rebuilt that frame, the mesh.
    frames: [Vec<f64>; 3],
    /// The preview version last frame.
    last_previewed: Option<(u64, usize)>,
}

/// Files the last frame's time (`ms`) by what was on screen, and how much of a preview that went
/// up then was baked anew.
fn measure_frame(script: &mut Script, editor: &Editor, ms: f64) {
    // Skip the first frames: pipelines compile.
    // Frame time is the time since the last frame: the frame before this one, which rebuilt the
    // preview if the version moved since the frame before it.
    if script.frame > 30 {
        let kind = if editor.shown == Some(editor.version) {
            2
        } else {
            usize::from(script.last_previewed != editor.previewed)
        };
        if let Some(frames) = script.frames.get_mut(kind) {
            frames.push(ms);
        }
    }
    if script.last_previewed != editor.previewed && editor.previewed.is_some_and(|(v, _)| v == editor.version) {
        let share = f64::from(editor.rebaked.0) * 100.0 / f64::from(editor.rebaked.1.max(1));
        if let Some(shares) = script.rebaked.get_mut(usize::from(editor.drag.is_some())) {
            shares.push(share);
        }
    }
    script.last_previewed = editor.previewed;
}

pub fn scripted_edits(
    mut commands: Commands,
    mut script: ResMut<Script>,
    mut editor: ResMut<Editor>,
    rules: Res<PartLodRules>,
    cache: Res<PartAssetCache>,
    target: Option<Res<ShotTarget>>,
    time: Res<Time>,
) {
    if script.done || editor.body.is_none() {
        return;
    }
    script.frame += 1;
    measure_frame(&mut script, &editor, time.delta_secs_f64() * 1000.0);
    let frame = script.frame;
    // The part to drag: the first whose attach slides, and a ray straight down onto its root.
    let slider = || {
        let plan = editor.plan.as_ref()?;
        let body = editor.body.as_ref()?;
        let entry = editor.parts.iter().position(|e| {
            plan.node(&e.path).is_some_and(|n| matches!(n.attach, AttachSlot::Surface { .. }) && n.name != plan.root.name)
        })?;
        let authored = &editor.parts.get(entry)?.path;
        let copy = body.anatomy.parts.iter().position(|p| path_matches(authored, &p.path) && !p.mirrored)?;
        let root = body.model.get(usize::from(body.anatomy.parts.get(copy)?.bones.start))?.translation;
        Some((entry, copy, Vec3::from(root)))
    };
    let down = |p: Vec3| Ray3d::new(p + Vec3::Y * 2.0, Dir3::NEG_Y);
    match frame {
        1 => {
            editor.select_part(1);
            editor.field = editor.fields.iter().position(|f| f.kind == FieldKind::Float).unwrap_or(0);
        }
        // Held key: an edit every third frame (the key repeat rate).
        10..=100 if frame % 3 == 0 => editor.adjust_selected(1.0, 1.0),
        // A drag: grab the part, then slide the cursor backwards 2 mm a frame.
        120 => {
            if let Some((entry, copy, root)) = slider() {
                editor.select_part(entry);
                let ctx = BuildCtx { rules: &rules, assets: &cache.assets };
                let hit = editor.pick(down(root)).map_or(root, |(_, hit)| hit);
                editor.begin_drag(copy, hit, down(root), Vec2::ZERO, false, &ctx);
                script.grab = Some(root);
                script.logged.clear();
                info!("test: dragging {}", editor.parts.get(entry).map_or("?", |e| e.path.0.as_str()));
            }
        }
        121..=180 => {
            if let Some(Drag { mode: DragMode::Slide { .. }, .. }) = &editor.drag
                && let Some(root) = script.grab
            {
                let at = root - Vec3::Z * 0.002 * (frame - 120).to_f32();
                editor.continue_drag(down(at), Vec2::ZERO);
            }
            // The preview mid-drag, beside the final screenshot.
            if frame == 170
                && let Ok(path) = std::env::var("ORNISCULPT_SCREENSHOT")
            {
                let path = std::path::Path::new(&path).with_extension("preview.png");
                commands.spawn(crate::common::screenshot(target.as_deref())).observe(save_to_disk(path));
            }
        }
        181 => editor.end_drag(),
        260..=340 if frame % 8 == 0 => editor.undo(),
        // Then wait for the full mesh (up to a while), so its frames are measured too.
        420.. if script.frames.last().is_some_and(|m| m.len() >= 60) || frame > 3000 => {
            script.done = true;
            let stats_in = |mut ms: Vec<f64>, unit: &str| {
                ms.sort_by(f64::total_cmp);
                let pick = |percent: usize| ms.get(ms.len().saturating_sub(1) * percent / 100).copied().unwrap_or_default();
                format!("{} samples, p50 {:.2}{unit}, p95 {:.2}{unit}, max {:.2}{unit}", ms.len(), pick(50), pick(95), pick(100))
            };
            let stats = |ms: Vec<f64>| stats_in(ms, " ms");
            info!("test: preview builds: {}", stats(script.preview_ms.clone()));
            let [still, dragged] = script.rebaked.clone();
            info!("test: bricks rebaked per preview, not dragging: {}", stats_in(still, "%"));
            info!("test: bricks rebaked per preview, dragging: {}", stats_in(dragged, "%"));
            let [standing, rebuilt, mesh] = script.frames.clone();
            info!("test: frames with the preview standing: {}", stats(standing));
            info!("test: frames with the preview rebuilt: {}", stats(rebuilt));
            info!("test: frames with the mesh: {}", stats(mesh));
            // The screenshot counts from the full body: the one shown, or the one on its way.
            editor.ready_frames = (editor.shown == Some(editor.version)).then_some(0);
        }
        _ => {}
    }
    if editor.is_changed()
        && let Some(ms) = editor.preview_ms
        && editor.previewed.is_some_and(|(v, _)| v == editor.version)
    {
        script.preview_ms.push(ms);
    }
    let attach = editor.parts.get(editor.part).and_then(|e| editor.plan.as_ref()?.node(&e.path)).map(|n| format!("{:?}", n.attach));
    let line = format!(
        "{} [{}] full {:?} ms ({} = {}, attach {})",
        editor.status,
        if editor.shown == Some(editor.version) { "mesh" } else { "preview" },
        editor.full_ms.map(f64::round),
        editor.fields.get(editor.field).map_or("", |f| f.label.as_str()),
        editor.fields.get(editor.field).map_or("", |f| f.value.as_str()),
        attach.unwrap_or_default(),
    );
    if script.logged != line {
        info!("test frame {frame}: {line}");
        script.logged = line;
    }
}
