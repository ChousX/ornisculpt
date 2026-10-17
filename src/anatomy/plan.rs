use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::{PartDetail, PartKind, PartPath, Side, StampSet};
use crate::random::NoiseSettings;

/// The authored anatomical topology of a mob: a tree of parts.
///
/// Loaded from `*.body.ron`. A spawned mob keeps its own expanded, edited copy (`LiveBody`), so
/// this asset is never mutated at runtime.
#[derive(Asset, Reflect, Serialize, Deserialize, Clone, Debug)]
pub struct BodyPlan {
    /// Usually a [`PartKind::Torso`].
    pub root: PartNode,
    #[serde(default)]
    pub tone: Tone,
}

/// One part in the body plan tree.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug)]
pub struct PartNode {
    /// Unique among siblings; joined with ancestors to form a [`PartPath`].
    pub name: String,
    pub kind: PartKind,
    /// Where on the parent this part attaches. Ignored for the root.
    #[serde(default)]
    pub attach: AttachSlot,
    #[serde(default)]
    pub offset: PartOffset,
    /// Left/right copies mirrored across the parent's local X; names get `.l` / `.r`.
    #[serde(default)]
    pub mirror: bool,
    /// Repeated copies; combines with `mirror` (centipede legs = `Series` + mirror).
    #[serde(default)]
    pub repeat: Repeat,
    /// Set by [`BodyPlan::expand_symmetry`]; authored plans leave it `Center`.
    #[serde(default)]
    pub side: Side,
    #[serde(default)]
    pub detail: PartDetail,
    /// Local multipliers on the body [`Tone`] (driven by the Muscle/Fat/Folds levers).
    #[serde(default)]
    pub tone: PartTone,
    /// Muscle/fat/landmark stamps on top of the base shape.
    #[serde(default)]
    pub stamps: StampSet,
    #[serde(default)]
    pub children: Vec<PartNode>,
}

/// Per-part multipliers on the body [`Tone`]. `1.0` = body default.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct PartTone {
    pub muscle: f32,
    pub fat: f32,
    pub folds: f32,
}

impl Default for PartTone {
    fn default() -> Self {
        Self { muscle: 1.0, fat: 1.0, folds: 1.0 }
    }
}

impl PartTone {
    pub fn apply(&self, body: &Tone) -> Tone {
        Tone {
            muscle: body.muscle * self.muscle,
            fat: body.fat * self.fat,
            folds: body.folds * self.folds,
            ..*body
        }
    }
}

/// Repetition of a part on its parent. Copies are named `.0`, `.1`, ... (before any mirror
/// suffix: `leg.3.l`).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum Repeat {
    #[default]
    Once,
    /// `count` copies spaced evenly from `from` to `to` along the parent; requires an `Along` or
    /// `Surface` attach (its `t` is replaced). Centipede legs, dorsal spikes, ribs.
    Series { count: u8, from: f32, to: f32 },
    /// `count` copies rotated evenly around the attach axis. Claws on a pad, starfish arms.
    Radial { count: u8 },
}

/// Where on the parent a child attaches. Each [`PartKind`] documents which slots it exposes;
/// attaching to a missing slot is an [`AnatomyError::MissingAttachSlot`](super::AnatomyError).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub enum AttachSlot {
    Start,
    #[default]
    End,
    /// On the central axis of a segmented part, `0.0` = start, `1.0` = end.
    Along(f32),
    /// On the skin of a segmented part: `t` along it, `around_deg` around its axis
    /// (`0` = dorsal/top, `90` = left side, `180` = ventral/belly). Limbs, spikes, crests.
    Surface { t: f32, around_deg: f32 },
    Jaw,
    /// Eye/ear/horn slots are on the head's left side; mirrored copies are flipped.
    Eye,
    Ear,
    Horn,
    Mouth,
    /// Snout tip (trunks, nose horns, whiskers).
    Nose,
    /// Top of the skull (crests, single horns).
    Crown,
}

/// Mirror suffixes added by [`BodyPlan::expand_symmetry`].
pub const LEFT_SUFFIX: &str = "l";
pub const RIGHT_SUFFIX: &str = "r";

/// Hand-authorable local offset (no quaternions in RON).
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct PartOffset {
    pub translate: Vec3,
    /// Euler XYZ, degrees.
    pub rotate_deg: Vec3,
    pub scale: f32,
}

impl Default for PartOffset {
    fn default() -> Self {
        Self { translate: Vec3::ZERO, rotate_deg: Vec3::ZERO, scale: 1.0 }
    }
}

impl PartOffset {
    pub fn to_transform(&self) -> Transform {
        let r = self.rotate_deg * (std::f32::consts::PI / 180.0);
        Transform {
            translation: self.translate,
            rotation: Quat::from_euler(EulerRot::XYZ, r.x, r.y, r.z),
            scale: Vec3::splat(self.scale),
        }
    }
}

/// Whole-body surface character. Individual parts may scale these.
#[derive(Reflect, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Tone {
    /// Muscle stamp intensity, `0.0` = smooth blob.
    pub muscle: f32,
    /// Fat inflation by region.
    pub fat: f32,
    /// Skin folds at joints.
    pub folds: f32,
    /// Bone landmarks (knuckles, brow, spine ridge).
    pub landmarks: f32,
    /// Shape of the fold/wrinkle displacement.
    pub fold_noise: NoiseSettings,
}

impl Default for Tone {
    fn default() -> Self {
        Self {
            muscle: 0.5,
            fat: 0.3,
            folds: 0.3,
            landmarks: 0.3,
            fold_noise: NoiseSettings { frequency: 20.0, amplitude: 0.004, ..default() },
        }
    }
}

impl BodyPlan {
    /// Returns a copy where every repeated/mirrored copy is an explicit node (`mirror: false`,
    /// `repeat: Once`), named with its suffixes (`limb.front.l`, `claws.2`, `leg.3.r`). Each copy
    /// can then be patched or severed on its own.
    ///
    /// Geometry is not flipped here: a right copy is built as if it were the left one, then
    /// reflected by the builder (see `AnatomyBuilder`), so bones never carry negative scale.
    pub fn expand_symmetry(&self) -> BodyPlan {
        let mut roots = expand_node(&self.root);
        debug_assert_eq!(roots.len(), 1, "the root part cannot repeat or mirror");
        BodyPlan { root: roots.swap_remove(0), tone: self.tone }
    }

    /// Visits every node depth-first with its path.
    pub fn visit(&self, mut f: impl FnMut(&PartPath, &PartNode)) {
        fn go(path: &PartPath, n: &PartNode, f: &mut impl FnMut(&PartPath, &PartNode)) {
            f(path, n);
            for c in &n.children {
                go(&path.child(&c.name), c, f);
            }
        }
        go(&PartPath::root(&self.root.name), &self.root, &mut f);
    }

    /// Visits every node mutably depth-first with its path.
    pub fn visit_mut(&mut self, mut f: impl FnMut(&PartPath, &mut PartNode)) {
        fn go(path: &PartPath, n: &mut PartNode, f: &mut impl FnMut(&PartPath, &mut PartNode)) {
            f(path, n);
            for c in &mut n.children {
                let p = path.child(&c.name);
                go(&p, c, f);
            }
        }
        let root = PartPath::root(&self.root.name);
        go(&root, &mut self.root, &mut f);
    }

    /// Finds a node by exact path.
    pub fn node(&self, path: &PartPath) -> Option<&PartNode> {
        let mut segs = path.segments();
        if segs.next()? != self.root.name {
            return None;
        }
        segs.try_fold(&self.root, |n, s| n.children.iter().find(|c| c.name == s))
    }

    pub fn node_mut(&mut self, path: &PartPath) -> Option<&mut PartNode> {
        let mut segs = path.segments();
        if segs.next()? != self.root.name {
            return None;
        }
        segs.try_fold(&mut self.root, |n, s| n.children.iter_mut().find(|c| c.name == s))
    }
}

/// Expands one node (and recursively its children) into its explicit copies.
fn expand_node(node: &PartNode) -> Vec<PartNode> {
    let children: Vec<PartNode> = node.children.iter().flat_map(expand_node).collect();
    let base = PartNode { children, mirror: false, repeat: Repeat::Once, ..node.clone() };

    let repeated: Vec<PartNode> = match node.repeat {
        Repeat::Once => vec![base],
        Repeat::Series { count, from, to } => (0..count)
            .map(|i| {
                let t = if count > 1 { from + (to - from) * i as f32 / (count - 1) as f32 } else { from };
                let attach = match base.attach {
                    AttachSlot::Along(_) => AttachSlot::Along(t),
                    AttachSlot::Surface { around_deg, .. } => AttachSlot::Surface { t, around_deg },
                    other => other,
                };
                PartNode { name: format!("{}.{i}", base.name), attach, ..base.clone() }
            })
            .collect(),
        Repeat::Radial { count } => (0..count)
            .map(|i| {
                let mut offset = base.offset;
                offset.rotate_deg.y += 360.0 * i as f32 / count.max(1) as f32;
                PartNode { name: format!("{}.{i}", base.name), offset, ..base.clone() }
            })
            .collect(),
    };

    if !node.mirror {
        return repeated;
    }
    repeated
        .into_iter()
        .flat_map(|n| {
            let left = PartNode { name: format!("{}.{LEFT_SUFFIX}", n.name), side: Side::Left, ..n.clone() };
            let right = PartNode { name: format!("{}.{RIGHT_SUFFIX}", n.name), side: Side::Right, ..n };
            [left, right]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{HardParams, TorsoParams};

    fn leaf(name: &str) -> PartNode {
        PartNode {
            name: name.into(),
            kind: PartKind::Hard(HardParams::default()),
            attach: AttachSlot::Surface { t: 0.0, around_deg: 90.0 },
            offset: default(),
            mirror: false,
            repeat: Repeat::Once,
            side: Side::Center,
            detail: default(),
            tone: default(),
            stamps: default(),
            children: vec![],
        }
    }

    #[test]
    fn expands_series_then_mirror() {
        let mut leg = leaf("leg");
        leg.mirror = true;
        leg.repeat = Repeat::Series { count: 3, from: 0.0, to: 1.0 };
        leg.children.push(PartNode { mirror: true, ..leaf("spur") });
        let plan = BodyPlan {
            root: PartNode { kind: PartKind::Torso(TorsoParams::default()), children: vec![leg], ..leaf("torso") },
            tone: default(),
        };
        let e = plan.expand_symmetry();
        let names: Vec<_> = e.root.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["leg.0.l", "leg.0.r", "leg.1.l", "leg.1.r", "leg.2.l", "leg.2.r"]);
        assert_eq!(e.root.children[3].side, Side::Right);
        assert_eq!(e.root.children[2].attach, AttachSlot::Surface { t: 0.5, around_deg: 90.0 });
        assert_eq!(e.root.children[0].children.len(), 2);
        assert!(e.node(&PartPath("torso/leg.2.r/spur.l".into())).is_some());
    }
}
