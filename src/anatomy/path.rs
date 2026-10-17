use std::fmt;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// Stable address of a part within a body: `"torso/limb.front.l/digits"`.
///
/// Used by form patches, RNG forking, region names and (later) genetics.
/// Mirrored copies get `.l` / `.r`, radial copies get `.0`, `.1`, ...
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct PartPath(pub String);

impl PartPath {
    pub const SEPARATOR: char = '/';

    pub fn root(name: &str) -> Self {
        Self(name.to_owned())
    }

    pub fn child(&self, name: &str) -> Self {
        Self(format!("{}{}{name}", self.0, Self::SEPARATOR))
    }

    /// Copy suffix for symmetry: `limb.front` → `limb.front.l`.
    pub fn with_suffix(&self, suffix: &str) -> Self {
        Self(format!("{}.{suffix}", self.0))
    }

    pub fn parent(&self) -> Option<PartPath> {
        self.0.rsplit_once(Self::SEPARATOR).map(|(p, _)| Self(p.to_owned()))
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split(Self::SEPARATOR)
    }

    pub fn depth(&self) -> usize {
        self.segments().count()
    }

    pub fn is_ancestor_of(&self, other: &PartPath) -> bool {
        other.0.len() > self.0.len()
            && other.0.starts_with(&self.0)
            && other.0[self.0.len()..].starts_with(Self::SEPARATOR)
    }
}

impl fmt::Display for PartPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let torso = PartPath::root("torso");
        let leg = torso.child("limb.front").with_suffix("l");
        assert_eq!(leg.0, "torso/limb.front.l");
        assert_eq!(leg.parent(), Some(torso.clone()));
        assert_eq!(leg.depth(), 2);
        assert!(torso.is_ancestor_of(&leg));
        assert!(!PartPath::root("tor").is_ancestor_of(&leg));
    }
}
