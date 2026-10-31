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
    /// Relative to water (`1.0`): hollow bird bones ~`0.6`, armour plating `2.0`. Scales the
    /// part's weight, which the gait feels (see `motion::Heft`).
    #[serde(default = "unit")]
    pub density: f32,
    #[serde(default)]
    pub children: Vec<Self>,
    /// On a mirrored part, how its left copy differs from the part as authored; applied by
    /// [`BodyPlan::expand_symmetry`]. See [`SideOverride`].
    #[serde(default, skip_serializing_if = "SideOverride::is_empty")]
    pub left: SideOverride,
    /// Likewise for the right copy: a lobster's crusher claw opposite its cutter, a boar that has
    /// lost a tusk.
    #[serde(default, skip_serializing_if = "SideOverride::is_empty")]
    pub right: SideOverride,
}

/// How one mirrored copy of a part differs from the part as authored.
///
/// [`BodyPlan::expand_symmetry`] applies it to that copy (to every copy on that side, for a
/// mirrored series), so everything downstream sees ordinary nodes, and a
/// [`FormPatch`](super::FormPatch) on the suffixed path still reaches the one copy. Unset fields
/// leave the copy as authored. On a part that doesn't `mirror` the overrides are ignored.
///
/// The end of a limb is part of its `kind`, and hard parts are children, so those two fields
/// cover them; a part's placement levers and tone are the same on both sides.
#[derive(Reflect, Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SideOverride {
    /// The copy isn't there: a one-tusked boar, a goat that broke a horn.
    pub absent: bool,
    /// Replaces the part's parameters whole, a limb's end among them (a heavier claw).
    pub kind: Option<PartKind>,
    /// Replaces the part's offset.
    pub offset: Option<PartOffset>,
    /// Replaces the part's children (this claw's own teeth). They expand in turn, so they may
    /// mirror or repeat themselves.
    pub children: Option<Vec<PartNode>>,
}

impl SideOverride {
    /// Nothing overridden: the copy is the part as authored.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.absent && self.kind.is_none() && self.offset.is_none() && self.children.is_none()
    }

    /// `node`, the copy on `side`, with this override applied.
    fn apply(&self, mut node: PartNode, side: Side) -> PartNode {
        node.side = side;
        if let Some(kind) = &self.kind {
            node.kind = kind.clone();
        }
        if let Some(offset) = self.offset {
            node.offset = offset;
        }
        if let Some(children) = &self.children {
            node.children = expand_children(children);
        }
        node
    }
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn expand_symmetry(&self) -> Self {
        let mut roots = expand_node(&self.root);
        debug_assert_eq!(roots.len(), 1, "the root part cannot repeat or mirror");
        Self { root: roots.swap_remove(0), tone: self.tone }
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
    #[must_use]
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
    let base = PartNode {
        children: expand_children(&node.children),
        mirror: false,
        repeat: Repeat::Once,
        left: SideOverride::default(),
        right: SideOverride::default(),
        ..node.clone()
    };
    if !node.mirror {
        if !(node.left.is_empty() && node.right.is_empty()) {
            warn!("part `{}`: `left`/`right` overrides need `mirror: true`; ignored", node.name);
        }
        return repeat_copies(&base, node.repeat);
    }
    let side_copies = |side: Side, over: &SideOverride, suffix: &str| -> Vec<PartNode> {
        if over.absent {
            return Vec::new();
        }
        repeat_copies(&over.apply(base.clone(), side), node.repeat)
            .into_iter()
            .map(|c| PartNode { name: format!("{}.{suffix}", c.name), ..c })
            .collect()
    };
    let left = side_copies(Side::Left, &node.left, LEFT_SUFFIX);
    let right = side_copies(Side::Right, &node.right, RIGHT_SUFFIX);
    // The copies interleave, `.l` before `.r`, as they did before a side could differ.
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut left, mut right) = (left.into_iter(), right.into_iter());
    loop {
        match (left.next(), right.next()) {
            (None, None) => break,
            (l, r) => out.extend(l.into_iter().chain(r)),
        }
    }
    out
}

fn expand_children(children: &[PartNode]) -> Vec<PartNode> {
    children.iter().flat_map(expand_node).collect()
}

/// `base`'s copies under `repeat`, named `.0`, `.1`, ...; `base` itself for `Once`.
fn repeat_copies(base: &PartNode, repeat: Repeat) -> Vec<PartNode> {
    match repeat {
        Repeat::Once => vec![base.clone()],
        Repeat::Series { count, from, to } => (0..count)
            .map(|i| {
                let t = if count > 1 { from + (to - from) * f32::from(i) / f32::from(count - 1) } else { from };
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
                offset.rotate_deg.y += 360.0 * f32::from(i) / f32::from(count.max(1));
                PartNode { name: format!("{}.{i}", base.name), offset, ..base.clone() }
            })
            .collect(),
    }
}

const fn unit() -> f32 {
    1.0
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
            density: 1.0,
            children: vec![],
            left: default(),
            right: default(),
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

    fn plan(src: &str) -> BodyPlan {
        ron::from_str(src).unwrap()
    }

    fn names(nodes: &[PartNode]) -> Vec<&str> {
        nodes.iter().map(|c| c.name.as_str()).collect()
    }

    /// Every plan in `assets/mobs`, by name.
    fn sample_plans() -> Vec<(String, BodyPlan)> {
        let dir = format!("{}/assets/mobs", env!("CARGO_MANIFEST_DIR"));
        let mut out: Vec<(String, BodyPlan)> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| {
                let file = e.ok()?.file_name().into_string().ok()?;
                let name = file.strip_suffix(".body.ron")?.to_owned();
                Some((name, plan(&std::fs::read_to_string(format!("{dir}/{file}")).unwrap())))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// `expand_symmetry` as it was before a side could differ, kept verbatim. A plan with no
    /// side overrides must still expand to exactly what it did then.
    fn expand_v1(plan: &BodyPlan) -> BodyPlan {
        fn expand_node(node: &PartNode) -> Vec<PartNode> {
            let children: Vec<PartNode> = node.children.iter().flat_map(expand_node).collect();
            let base = PartNode { children, mirror: false, repeat: Repeat::Once, ..node.clone() };
            let repeated: Vec<PartNode> = match node.repeat {
                Repeat::Once => vec![base],
                Repeat::Series { count, from, to } => (0..count)
                    .map(|i| {
                        let t = if count > 1 { from + (to - from) * f32::from(i) / f32::from(count - 1) } else { from };
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
                        offset.rotate_deg.y += 360.0 * f32::from(i) / f32::from(count.max(1));
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
        let mut roots = expand_node(&plan.root);
        BodyPlan { root: roots.swap_remove(0), tone: plan.tone }
    }

    fn without_overrides(plan: &BodyPlan) -> BodyPlan {
        let mut stripped = plan.clone();
        stripped.visit_mut(|_, n| {
            n.left = SideOverride::default();
            n.right = SideOverride::default();
        });
        stripped
    }

    /// Side overrides changed nothing for the plans that don't use them: every sample plan, its
    /// overrides stripped, expands to the tree the previous expander gave.
    #[test]
    fn existing_plans_expand_as_before() {
        let plans = sample_plans();
        assert!(plans.len() > 20);
        for (name, authored) in &plans {
            let plain = without_overrides(authored);
            let before = ron::to_string(&expand_v1(&plain)).unwrap();
            let after = ron::to_string(&plain.expand_symmetry()).unwrap();
            assert_eq!(before, after, "{name}");
            // An expanded plan carries no overrides, so nothing downstream can meet one.
            assert!(!after.contains("left:") && !after.contains("right:"), "{name}");
        }
    }

    const CLAWS: &str = r#"(root: (name: "torso", kind: Torso(()), children: [
        (
            name: "claw",
            kind: Limb((chain: (segments: 2, length: 0.2, radius: (scale: 0.02)), role: Manipulator, end: Pincer(size: 0.08))),
            attach: Surface(t: 0.8, around_deg: 110.0),
            mirror: true,
            children: [(name: "tooth", kind: Hard((kind: Tooth, size: 0.004)), attach: End)],
            right: (
                kind: Some(Limb((chain: (segments: 2, length: 0.2, radius: (scale: 0.03)), role: Manipulator, end: Pincer(size: 0.12)))),
                offset: Some((rotate_deg: (10.0, 0.0, 0.0))),
                children: Some([(name: "molar", kind: Hard((kind: Tooth, size: 0.008)), attach: End, mirror: true)]),
            ),
        ),
    ]))"#;

    #[test]
    fn a_side_can_differ() {
        let e = plan(CLAWS).expand_symmetry();
        let [left, right] = e.root.children.as_slice() else { panic!("{:?}", names(&e.root.children)) };
        assert_eq!((left.name.as_str(), left.side), ("claw.l", Side::Left));
        assert_eq!((right.name.as_str(), right.side), ("claw.r", Side::Right));
        let pincer = |n: &PartNode| match &n.kind {
            PartKind::Limb(l) => l.end,
            k => panic!("{k:?}"),
        };
        assert_eq!(pincer(left), crate::anatomy::LimbEnd::Pincer { size: 0.08 });
        assert_eq!(pincer(right), crate::anatomy::LimbEnd::Pincer { size: 0.12 });
        assert_eq!(left.offset, PartOffset::default());
        assert_eq!(right.offset.rotate_deg, Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(names(&left.children), ["tooth"]);
        assert_eq!(names(&right.children), ["molar.l", "molar.r"]);
        assert!(right.children.iter().all(|c| !c.mirror), "the override's children expand too");
        assert!(left.right.is_empty() && right.right.is_empty() && right.left.is_empty());
        // Downstream sees two ordinary nodes, reachable one at a time.
        assert!(e.node(&PartPath("torso/claw.r/molar.l".into())).is_some());
        // The plan builds, with a grip socket on each side.
        let a = crate::anatomy::Anatomy::from_plan(&plan(CLAWS), &default(), &default()).unwrap();
        let mut grips: Vec<&str> = a.sockets.iter().filter(|s| s.name.starts_with("grip")).map(|s| s.name.as_str()).collect();
        grips.sort_unstable();
        assert_eq!(grips, ["grip.l", "grip.r"]);
        assert_eq!(a.sockets.iter().filter(|s| s.name.starts_with("grip")).map(|s| s.size).fold(0.0, f32::max), 0.12);
    }

    #[test]
    fn a_side_can_be_absent() {
        let e = plan(r#"(root: (name: "torso", kind: Torso(()), children: [
            (name: "head", kind: Head(()), children: [
                (name: "tusk", kind: Hard((kind: Tusk, size: 0.1)), attach: Jaw, mirror: true, right: (absent: true)),
            ]),
            (name: "leg", kind: Limb(()), attach: Surface(t: 0.5, around_deg: 100.0), repeat: Series(count: 3, from: 0.2, to: 0.8), mirror: true, left: (absent: true)),
        ]))"#)
        .expand_symmetry();
        let head = &e.root.children[0];
        assert_eq!(names(&head.children), ["tusk.l"]);
        assert_eq!(head.children[0].side, Side::Left);
        assert_eq!(names(&e.root.children[1..]), ["leg.0.r", "leg.1.r", "leg.2.r"]);
        assert!(e.root.children[1..].iter().all(|l| l.side == Side::Right));
        assert_eq!(e.root.children[2].attach, AttachSlot::Surface { t: 0.5, around_deg: 100.0 });
    }

    #[test]
    fn overrides_need_mirror() {
        let e = plan(r#"(root: (name: "torso", kind: Torso(()), children: [
            (name: "spur", kind: Hard(()), attach: Surface(t: 0.5, around_deg: 0.0), right: (absent: true)),
        ]))"#)
        .expand_symmetry();
        assert_eq!(names(&e.root.children), ["spur"]);
        assert!(e.root.children[0].right.is_empty());
    }

    /// Overrides are plan text like any other: an authored plan with them reads and writes the
    /// same, and one without them writes no `left`/`right` at all.
    #[test]
    fn overrides_round_trip_through_ron() {
        let p = plan(CLAWS);
        let text = ron::to_string(&p).unwrap();
        assert!(text.contains("right:(absent:false,kind:Some(Limb(") && !text.contains("left:"), "{text}");
        let again: BodyPlan = ron::from_str(&text).unwrap();
        assert_eq!(ron::to_string(&again).unwrap(), text);
        assert_eq!(ron::to_string(&again.expand_symmetry()).unwrap(), ron::to_string(&p.expand_symmetry()).unwrap());
    }

    /// Before overrides, `realism.md` suggested authoring a differing right part on its own, as
    /// `Side::Center` at `around_deg` 270 (and noted it was untested). It lands where the mirrored
    /// copy would, and a pitch offset mirrors right, but yaw and roll don't, and the part has no
    /// side: its sockets lose their `.r`, and a leg would phase as a centre leg.
    #[test]
    fn center_part_at_270_is_not_a_right_copy() {
        use crate::anatomy::Anatomy;
        use bevy::math::Affine3A;
        let torso = "kind: Torso((chain: (segments: 3, length: 0.3, radius: (scale: 0.05))))";
        let arm = |rot: &str| format!("kind: Limb((chain: (segments: 2, length: 0.2, radius: (scale: 0.02)), role: Manipulator, end: Pincer(size: 0.08))), offset: (rotate_deg: {rot})");
        let mirrored = |rot: &str| {
            let src = format!(r#"(root: (name: "torso", {torso}, children: [(name: "arm", {}, attach: Surface(t: 0.8, around_deg: 100.0), mirror: true)]))"#, arm(rot));
            Anatomy::from_plan(&plan(&src), &default(), &default()).unwrap()
        };
        let authored = |rot: &str| {
            let src = format!(
                r#"(root: (name: "torso", {torso}, children: [
                    (name: "arm_l", {a}, attach: Surface(t: 0.8, around_deg: 100.0)),
                    (name: "arm_r", {a}, attach: Surface(t: 0.8, around_deg: 260.0)),
                ]))"#,
                a = arm(rot)
            );
            Anatomy::from_plan(&plan(&src), &default(), &default()).unwrap()
        };
        let bones = |a: &Anatomy, path: &str| -> Vec<Affine3A> {
            let part = a.parts.iter().find(|p| p.path.0 == path).unwrap_or_else(|| panic!("{path}"));
            let model = a.skeleton.rest_model();
            part.bones.clone().map(|b| model[usize::from(b)]).collect()
        };
        let same = |x: &[Affine3A], y: &[Affine3A]| x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.abs_diff_eq(*q, 1e-4));

        // Pitch only: the Center part at 270 is where the mirrored right copy is.
        let (m, w) = (mirrored("(98.0, 0.0, 0.0)"), authored("(98.0, 0.0, 0.0)"));
        assert!(same(&bones(&m, "torso/arm.l"), &bones(&w, "torso/arm_l")));
        assert!(same(&bones(&m, "torso/arm.r"), &bones(&w, "torso/arm_r")), "pitch mirrors");
        // A yaw or a roll in the offset doesn't mirror: the workaround's arm turns the other way.
        for rot in ["(0.0, 30.0, 0.0)", "(0.0, 0.0, 30.0)"] {
            let (m, w) = (mirrored(rot), authored(rot));
            assert!(same(&bones(&m, "torso/arm.l"), &bones(&w, "torso/arm_l")), "{rot}");
            assert!(!same(&bones(&m, "torso/arm.r"), &bones(&w, "torso/arm_r")), "{rot}");
        }
        // And the part has no side, so both grips are the centre `grip`.
        let grips = |a: &Anatomy| -> Vec<(String, Side)> { a.sockets.iter().filter(|s| s.name.starts_with("grip")).map(|s| (s.name.clone(), s.side)).collect() };
        assert_eq!(grips(&m), [("grip.l".to_owned(), Side::Left), ("grip.r".to_owned(), Side::Right)]);
        assert_eq!(grips(&w), [("grip".to_owned(), Side::Center), ("grip".to_owned(), Side::Center)]);
    }
}
