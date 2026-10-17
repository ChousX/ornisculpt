use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{BodyPlan, Lever, LeverError, PartPath};

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
}

/// Expands symmetry, then applies `form` (patches in order, then overall scale).
/// The result is what a mob stores as its `LiveBody`.
pub fn apply_form(plan: &BodyPlan, form: &Form) -> Result<BodyPlan, FormError> {
    let mut live = plan.expand_symmetry();
    for patch in &form.patches {
        let mut hit = false;
        let mut err = None;
        live.visit_mut(|path, node| {
            if err.is_none() && path_matches(&patch.part, path) {
                hit = true;
                if let Err(source) = node.apply_lever(patch.lever, patch.value) {
                    err = Some(FormError::Lever { part: path.clone(), source });
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
    if form.overall_scale != 1.0 {
        live.visit_mut(|_, node| {
            let _ = node.apply_lever(super::Lever::Scale, form.overall_scale);
            node.offset.translate *= form.overall_scale;
        });
    }
    Ok(live)
}

/// `pattern` matches `path` if every segment equals the path's segment, either exactly or with
/// its expansion suffixes (`.l`, `.r`, `.<n>`) removed.
pub fn path_matches(pattern: &PartPath, path: &PartPath) -> bool {
    let (mut a, mut b) = (pattern.segments(), path.segments());
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(p), Some(s)) if p == s || p == strip_suffixes(s) || p == strip_mirror(s) => {}
            _ => return false,
        }
    }
}

/// Like [`path_matches`], but `pattern` may also name an ancestor of `path`.
pub(crate) fn path_prefix_matches(pattern: &PartPath, path: &PartPath) -> bool {
    let mut b = path.segments();
    for p in pattern.segments() {
        match b.next() {
            Some(s) if p == s || p == strip_suffixes(s) || p == strip_mirror(s) => {}
            _ => return false,
        }
    }
    true
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
