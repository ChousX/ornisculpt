use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::collections::HashMap;

use super::{BodyPlan, Lever, LeverError, LeverKind, PartPath};

/// An individual's proportions: ordered patches applied over an expanded [`BodyPlan`].
///
/// Always supplied by the game; the plugin never randomizes it. Empty = the plan as authored.
#[derive(Component, Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Form {
    pub overall_scale: f32,
    /// Applied in order; later patches win.
    pub patches: Vec<FormPatch>,
}

impl Default for Form {
    fn default() -> Self {
        Self { overall_scale: 1.0, patches: Vec::new() }
    }
}

/// Turn one [`Lever`] on one part.
///
/// `part` addresses the expanded plan: `torso/limb.front.l` hits one leg, while the unsuffixed
/// `torso/limb.front` hits every copy of it. `value` is interpreted per [`Lever::kind`].
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FormPatch {
    pub part: PartPath,
    pub lever: Lever,
    pub value: f32,
}

#[derive(Debug, Error)]
pub enum FormError {
    #[error("no part at `{0}`")]
    UnknownPart(PartPath),
    #[error("part `{part}`: {source}")]
    Lever { part: PartPath, source: LeverError },
    /// Each patch was within its lever's range, but together they take the part out of it.
    #[error("part `{part}`: lever {lever:?} patches combine to {value}, outside {lo}..={hi}")]
    Combined { part: PartPath, lever: Lever, value: f32, lo: f32, hi: f32 },
    #[error("overall scale {value} is outside {lo}..={hi}")]
    Scale { value: f32, lo: f32, hi: f32 },
}

/// Expands symmetry, then applies `form` (patches in order, then overall scale).
/// The result is what a mob stores as its `LiveBody`.
///
/// # Errors
///
/// When a patch names no part, or its lever doesn't apply to the part.
pub fn apply_form(plan: &BodyPlan, form: &Form) -> Result<BodyPlan, FormError> {
    let (lo, hi) = Lever::Scale.range();
    if !(lo..=hi).contains(&form.overall_scale) {
        return Err(FormError::Scale { value: form.overall_scale, lo, hi });
    }
    let mut live = plan.expand_symmetry();
    // Each part's levers combined so far, so stacked patches stay in range too.
    let mut combined: HashMap<(PartPath, Lever), f32> = HashMap::new();
    for patch in &form.patches {
        let mut hit = false;
        let mut err = None;
        live.visit_mut(|path, node| {
            if err.is_none() && path_matches(&patch.part, path) {
                hit = true;
                if let Err(source) = node.apply_lever(patch.lever, patch.value) {
                    err = Some(FormError::Lever { part: path.clone(), source });
                    return;
                }
                let lever = patch.lever;
                let total = combined.entry((path.clone(), lever)).or_insert_with(|| lever.identity());
                *total = lever.combine(*total, patch.value);
                let (lo, hi) = lever.range();
                if lever.kind() != LeverKind::Count && !(lo..=hi).contains(total) {
                    err = Some(FormError::Combined { part: path.clone(), lever, value: *total, lo, hi });
                }
            }
        });
        if let Some(e) = err {
            return Err(e);
        }
        if !hit {
            return Err(FormError::UnknownPart(patch.part.clone()));
        }
    }
    // Scaling by exactly 1 changes nothing, so there is no need to skip it.
    live.visit_mut(|_, node| {
        let _ = node.apply_lever(super::Lever::Scale, form.overall_scale);
        node.offset.translate *= form.overall_scale;
    });
    Ok(live)
}

/// `pattern` matches `path` if every segment equals the path's segment, either exactly or with
/// its expansion suffixes (`.l`, `.r`, `.<n>`) removed.
#[must_use]
pub fn path_matches(pattern: &PartPath, path: &PartPath) -> bool {
    pattern.segments().count() == path.segments().count() && path_prefix_matches(pattern, path)
}

/// Like [`path_matches`], but `pattern` may also name an ancestor of `path`.
pub(super) fn path_prefix_matches(pattern: &PartPath, path: &PartPath) -> bool {
    let mut b = path.segments();
    pattern.segments().all(|p| b.next().is_some_and(|s| p == s || p == strip_suffixes(s) || p == strip_mirror(s)))
}

fn strip_mirror(s: &str) -> &str {
    s.strip_suffix(".l").or_else(|| s.strip_suffix(".r")).unwrap_or(s)
}

fn strip_suffixes(s: &str) -> &str {
    let s = strip_mirror(s);
    match s.rsplit_once('.') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => base,
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quadruped() -> BodyPlan {
        ron::from_str(include_str!("../../assets/mobs/quadruped.body.ron")).unwrap()
    }

    fn form(patches: &[(&str, Lever, f32)]) -> Form {
        let patches = patches.iter().map(|(p, lever, value)| FormPatch { part: PartPath((*p).into()), lever: *lever, value: *value }).collect();
        Form { overall_scale: 1.0, patches }
    }

    #[test]
    fn levers_stay_in_range() {
        let q = quadruped();
        let leg = "torso/limb.front";
        // A leg turned inside out, shrunk to nothing, swung onto the back: all refused.
        for (lever, value) in [(Lever::Length, -1.0), (Lever::Length, 0.0), (Lever::Girth, -1.0), (Lever::Splay, 400.0), (Lever::Around, 180.0), (Lever::Flatten, 0.0), (Lever::Length, f32::NAN)] {
            let err = apply_form(&q, &form(&[(leg, lever, value)])).unwrap_err();
            assert!(matches!(err, FormError::Lever { source: LeverError::OutOfRange { .. }, .. }), "{lever:?} {value}: {err}");
        }
        assert!(apply_form(&q, &form(&[(leg, Lever::Length, 1.5), (leg, Lever::Splay, 20.0)])).is_ok());
        // Each patch in range, but stacked past it.
        let err = apply_form(&q, &form(&[(leg, Lever::Length, 3.0), (leg, Lever::Length, 3.0)])).unwrap_err();
        assert!(matches!(err, FormError::Combined { lever: Lever::Length, .. }), "{err}");
        let err = apply_form(&q, &Form { overall_scale: 0.0, patches: vec![] }).unwrap_err();
        assert!(matches!(err, FormError::Scale { .. }), "{err}");
    }

    #[test]
    fn a_jaw_lever_needs_a_jaw() {
        let mut q = quadruped();
        q.visit_mut(|_, n| {
            if let crate::anatomy::PartKind::Head(h) = &mut n.kind {
                h.jaw = None;
            }
        });
        let err = apply_form(&q, &form(&[("torso/neck/head", Lever::Jaw, 1.2)])).unwrap_err();
        assert!(matches!(err, FormError::Lever { source: LeverError::Unsupported(Lever::Jaw), .. }), "{err}");
    }

    #[test]
    fn patch_paths_match_copies() {
        let p = |s: &str| PartPath(s.into());
        assert!(path_matches(&p("torso/leg"), &p("torso/leg.3.l")));
        assert!(path_matches(&p("torso/leg.3"), &p("torso/leg.3.r")));
        assert!(path_matches(&p("torso/leg.3.r"), &p("torso/leg.3.r")));
        assert!(!path_matches(&p("torso/leg.3.l"), &p("torso/leg.3.r")));
        assert!(!path_matches(&p("torso/leg"), &p("torso/leg.3.l/claw")));
    }
}
