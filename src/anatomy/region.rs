use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Anatomy, LimbRole, PartPath};

/// Index into a mob's [`RegionTable`]. At most 64 per mob so a mask fits in a `u64`.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionId(pub u8);

impl RegionId {
    pub const MAX: usize = 64;
}

/// A surface region, emitted by the part that owns it.
#[derive(Reflect, Clone, Debug, PartialEq, Eq)]
pub struct RegionDef {
    pub part: PartPath,
    pub kind: RegionKind,
    pub side: Side,
}

/// Anatomical meaning of a region, independent of body plan.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionKind {
    TorsoFront,
    TorsoMid,
    TorsoRear,
    Belly,
    Neck,
    Skull,
    Face,
    Snout,
    Jaw,
    LimbUpper,
    LimbLower,
    LimbEnd,
    Digit,
    TailBase,
    TailTip,
    /// Ears, antennae, tentacles.
    Appendage,
    Mass,
    Hard,
    /// Wing arm and finger bones (and insect wing hinges).
    Wing,
    /// Wing membranes, flight feathers and insect wing blades.
    WingMembrane,
}

#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Side {
    #[default]
    Center,
    Left,
    Right,
}

/// All regions of one built mob, indexed by [`RegionId`].
#[derive(Clone, Debug, Default)]
pub struct RegionTable {
    pub regions: Vec<RegionDef>,
}

impl RegionTable {
    #[must_use]
    pub fn get(&self, id: RegionId) -> Option<&RegionDef> {
        self.regions.get(usize::from(id.0))
    }

    pub fn iter(&self) -> impl Iterator<Item = (RegionId, &RegionDef)> {
        (0u8..).zip(&self.regions).map(|(i, r)| (RegionId(i), r))
    }
}

/// Bitset of [`RegionId`]s.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RegionMask(pub u64);

impl RegionMask {
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn with(self, id: RegionId) -> Self {
        Self(self.0 | 1 << id.0)
    }

    #[must_use]
    pub const fn contains(self, id: RegionId) -> bool {
        self.0 & (1 << id.0) != 0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    #[must_use]
    pub const fn complement(self) -> Self {
        Self(!self.0)
    }
}

/// Picks regions on *any* body plan. Used by coats, fur, clothing and hide masks.
///
/// Example, "everything except feet and face":
/// `AllExcept([Kind(LimbEnd), Kind(Face)])`.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum RegionSelector {
    All,
    Kind(RegionKind),
    /// The part and all its descendants.
    Part(PartPath),
    /// Regions of limbs with this role (and their descendants).
    Role(LimbRole),
    Side(Side),
    And(Vec<Self>),
    Or(Vec<Self>),
    /// Complement of the union of the inner selectors (stands in for `Not`, which would need a
    /// `Box` that Bevy reflection cannot represent).
    AllExcept(Vec<Self>),
}

impl RegionSelector {
    /// Resolves to a mask for one built mob.
    #[must_use]
    pub fn mask(&self, anatomy: &Anatomy) -> RegionMask {
        let mut mask = RegionMask::NONE;
        for (id, def) in anatomy.regions.iter() {
            if self.matches(def, anatomy) {
                mask = mask.with(id);
            }
        }
        mask
    }

    fn matches(&self, def: &RegionDef, anatomy: &Anatomy) -> bool {
        match self {
            Self::All => true,
            Self::Kind(k) => def.kind == *k,
            Self::Part(path) => super::form::path_prefix_matches(path, &def.part),
            Self::Role(role) => {
                // The region's part or any ancestor part has the role.
                let mut part = anatomy.parts.iter().position(|p| p.path == def.part);
                while let Some(p) = part.and_then(|i| anatomy.parts.get(i)) {
                    if p.role == Some(*role) {
                        return true;
                    }
                    part = p.parent.map(usize::from);
                }
                false
            }
            Self::Side(s) => def.side == *s,
            Self::And(all) => all.iter().all(|s| s.matches(def, anatomy)),
            Self::Or(any) => any.iter().any(|s| s.matches(def, anatomy)),
            Self::AllExcept(none) => !none.iter().any(|s| s.matches(def, anatomy)),
        }
    }
}
