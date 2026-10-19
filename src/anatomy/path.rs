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

    #[must_use]
    pub fn root(name: &str) -> Self {
        Self(name.to_owned())
    }

    #[must_use]
    pub fn child(&self, name: &str) -> Self {
        Self(format!("{}{}{name}", self.0, Self::SEPARATOR))
    }

    /// Copy suffix for symmetry: `limb.front` → `limb.front.l`.
    #[must_use]
    pub fn with_suffix(&self, suffix: &str) -> Self {
        Self(format!("{}.{suffix}", self.0))
    }

    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        self.0.rsplit_once(Self::SEPARATOR).map(|(p, _)| Self(p.to_owned()))
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split(Self::SEPARATOR)
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.segments().count()
    }

    #[must_use]
    pub fn is_ancestor_of(&self, other: &Self) -> bool {
        other.0.strip_prefix(self.0.as_str()).is_some_and(|rest| rest.starts_with(Self::SEPARATOR))
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
