//! A disk cache of generated body meshes, keyed by [`BodyKey`].
//!
//! Meshing and the deformation stages that follow it are nearly all of a body's generation cost
//! (about two thirds and a third of the rest on the bench), and they are a pure function of the
//! body's key. Keeping their output lets a second run of the same crowd skip them: a 4000-mob
//! cold start on a Steam Deck is 78 s of this work, paid once per install instead of once per
//! launch.
//!
//! The seam is [`BodyMeshData`] rather than the whole [`GenOutput`](super::GenOutput): a dozen
//! parallel arrays of plain data, written by hand the way the part SDFs are, leaving anatomy,
//! the motion bake and the contact set to rebuild (a tenth of a second once the sculpted head's
//! grid is cached too).
//!
//! Off by default — the crate writes nothing to disk unless an app asks it to with
//! [`BodyMeshCache::set_dir`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::prelude::*;

use super::BodyKey;
use crate::anatomy::PartLodRules;
use crate::coat::CoatMeshes;
use crate::num::{Clamp, Idx};
use crate::surface::{BodyMeshData, MaterialSlot, MuscleFlex, SoftVertex, Submesh};

/// Magic and version of a cached body on disk. Bump the digit when the layout changes: an older
/// file is then ignored and the body meshed again, rather than read as nonsense.
const BODY_FILE_MAGIC: [u8; 8] = *b"ORNIBOD1";

/// Where generated body meshes are kept between runs.
///
/// A body is keyed by its [`BodyKey`], which covers the generator version, the formed plan, the
/// surface settings, the coat, the outfit, the seed and the content of every authored mesh — so
/// a changed input writes a new file rather than colliding with the old one. Nothing is ever
/// evicted and a furry body is several MB, so point this at a cache directory the platform may
/// clear (`dirs::cache_dir()`), not at the save data.
///
/// A cache that can't be read or written is not an error: the body is generated as it would have
/// been without one.
#[derive(Resource, Clone, Debug, Default)]
pub struct BodyMeshCache {
    dir: Option<Arc<Path>>,
}

impl BodyMeshCache {
    /// Keeps generated body meshes in `dir` between runs, creating it if it isn't there. Set it
    /// at startup, before the first body is generated:
    ///
    /// ```no_run
    /// # use bevy::prelude::*;
    /// # use ornisculpt::mob::BodyMeshCache;
    /// fn body_cache(mut cache: ResMut<BodyMeshCache>) {
    ///     cache.set_dir("/tmp/my-game/bodies");
    /// }
    /// # fn add(app: &mut App) { app.add_systems(Startup, body_cache); }
    /// ```
    pub fn set_dir(&mut self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("body cache {}: {e}; generating every run instead", dir.display());
            return;
        }
        self.dir = Some(Arc::from(dir.as_path()));
    }

    /// Whether bodies are being cached on disk.
    #[must_use]
    pub const fn is_on(&self) -> bool {
        self.dir.is_some()
    }

    /// Where `key`'s meshes are read and written, or `None` when the cache is off.
    ///
    /// Part LOD thresholds are the one generation input a [`BodyKey`] does not cover (they are a
    /// resource, so they can't differ between two mobs of one process). The file records the ones
    /// it was written with and is refused if they have changed since.
    pub(super) fn entry(&self, key: BodyKey, rules: PartLodRules) -> Option<CachedBody> {
        let dir = self.dir.as_ref()?;
        Some(CachedBody { file: dir.join(format!("{:032x}.body", key.0)), rules })
    }
}

/// One body's file in a [`BodyMeshCache`], carried into its generation task.
#[derive(Clone, Debug)]
pub struct CachedBody {
    file: PathBuf,
    rules: PartLodRules,
}

impl CachedBody {
    /// The meshes this body was built with last run, if the file is there and this version wrote
    /// it. A damaged or stale file is a miss, not an error.
    pub(super) fn read(&self) -> Option<CachedMeshes> {
        let bytes = std::fs::read(&self.file).ok()?;
        let meshes = CachedMeshes::from_bytes(&bytes, self.rules);
        if meshes.is_none() {
            debug!("body cache {}: not usable, meshing again", self.file.display());
        }
        meshes
    }

    /// Keeps `meshes` for the next run. Through a temporary file so a reader never sees a
    /// half-written body, and so two processes generating the same body can't interleave.
    pub(super) fn write(&self, meshes: &CachedMeshes) {
        let temp = self.file.with_extension(format!("body.{}.part", std::process::id()));
        let written = std::fs::write(&temp, meshes.to_bytes(self.rules)).and_then(|()| std::fs::rename(&temp, &self.file));
        if let Err(e) = written {
            warn!("body cache {}: {e}", self.file.display());
            drop(std::fs::remove_file(&temp));
        }
    }
}

/// The part of a generated body that is cached: the geometry of every level, body and coat.
pub(super) struct CachedMeshes {
    pub lods: [BodyMeshData; 3],
    pub coat_meshes: [CoatMeshes; 3],
}

impl CachedMeshes {
    /// The magic, the part LOD thresholds, then the three levels and their coat geometry.
    fn to_bytes(&self, rules: PartLodRules) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&BODY_FILE_MAGIC);
        put_word(&mut out, rules.near_min_ratio.to_bits());
        put_word(&mut out, rules.far_min_ratio.to_bits());
        for mesh in &self.lods {
            put_mesh(&mut out, mesh);
        }
        for coat in &self.coat_meshes {
            for part in [&coat.shells, &coat.feathers, &coat.hair] {
                match part {
                    Some(mesh) => {
                        put_word(&mut out, 1);
                        put_mesh(&mut out, mesh);
                    }
                    None => put_word(&mut out, 0),
                }
            }
        }
        out
    }

    /// The inverse of [`to_bytes`](Self::to_bytes), or `None` if the bytes are not a body this
    /// version wrote, or were written under other part LOD thresholds, or are truncated, or
    /// describe a mesh that doesn't hang together. A cache file can be anything (a half-written
    /// file, a stale layout), so this never trusts what it reads.
    fn from_bytes(bytes: &[u8], rules: PartLodRules) -> Option<Self> {
        let (magic, rest) = bytes.split_at_checked(BODY_FILE_MAGIC.len())?;
        if magic != BODY_FILE_MAGIC {
            return None;
        }
        let mut r = Reader(rest);
        if r.word()? != rules.near_min_ratio.to_bits() || r.word()? != rules.far_min_ratio.to_bits() {
            return None;
        }
        let mut lods = [BodyMeshData::default(), BodyMeshData::default(), BodyMeshData::default()];
        for lod in &mut lods {
            *lod = r.mesh()?;
        }
        let mut coat_meshes = [CoatMeshes::default(), CoatMeshes::default(), CoatMeshes::default()];
        for coat in &mut coat_meshes {
            for part in [&mut coat.shells, &mut coat.feathers, &mut coat.hair] {
                *part = match r.word()? {
                    0 => None,
                    1 => Some(r.mesh()?),
                    _ => return None,
                };
            }
        }
        // Trailing bytes mean the file is not what this version writes.
        r.0.is_empty().then_some(Self { lods, coat_meshes })
    }
}

/// A per-vertex channel's encoding: `W` little-endian 4-byte words an item.
trait Channel<const W: usize>: Sized {
    fn write(&self, out: &mut Vec<u8>);
    fn read(words: [[u8; 4]; W]) -> Self;
}

fn put_word(out: &mut Vec<u8>, word: u32) {
    out.extend_from_slice(&word.to_le_bytes());
}

const fn word(w: [u8; 4]) -> u32 {
    u32::from_le_bytes(w)
}

const fn float(w: [u8; 4]) -> f32 {
    f32::from_le_bytes(w)
}

impl Channel<1> for u32 {
    fn write(&self, out: &mut Vec<u8>) {
        put_word(out, *self);
    }
    fn read(words: [[u8; 4]; 1]) -> Self {
        word(words[0])
    }
}

impl Channel<3> for Vec3 {
    fn write(&self, out: &mut Vec<u8>) {
        for v in self.to_array() {
            put_word(out, v.to_bits());
        }
    }
    fn read(words: [[u8; 4]; 3]) -> Self {
        Self::new(float(words[0]), float(words[1]), float(words[2]))
    }
}

impl Channel<2> for [u16; 4] {
    fn write(&self, out: &mut Vec<u8>) {
        put_word(out, u32::from(self[0]) | u32::from(self[1]) << 16);
        put_word(out, u32::from(self[2]) | u32::from(self[3]) << 16);
    }
    fn read(words: [[u8; 4]; 2]) -> Self {
        let halves = |w: [u8; 4]| {
            let v = word(w);
            [(v & 0xffff).clamp_u16(), (v >> 16).clamp_u16()]
        };
        let (lo, hi) = (halves(words[0]), halves(words[1]));
        [lo[0], lo[1], hi[0], hi[1]]
    }
}

impl Channel<2> for [u32; 2] {
    fn write(&self, out: &mut Vec<u8>) {
        put_word(out, self[0]);
        put_word(out, self[1]);
    }
    fn read(words: [[u8; 4]; 2]) -> Self {
        [word(words[0]), word(words[1])]
    }
}

impl Channel<2> for [f32; 2] {
    fn write(&self, out: &mut Vec<u8>) {
        for v in *self {
            put_word(out, v.to_bits());
        }
    }
    fn read(words: [[u8; 4]; 2]) -> Self {
        [float(words[0]), float(words[1])]
    }
}

impl Channel<3> for [f32; 3] {
    fn write(&self, out: &mut Vec<u8>) {
        for v in *self {
            put_word(out, v.to_bits());
        }
    }
    fn read(words: [[u8; 4]; 3]) -> Self {
        [float(words[0]), float(words[1]), float(words[2])]
    }
}

impl Channel<4> for [f32; 4] {
    fn write(&self, out: &mut Vec<u8>) {
        for v in *self {
            put_word(out, v.to_bits());
        }
    }
    fn read(words: [[u8; 4]; 4]) -> Self {
        [float(words[0]), float(words[1]), float(words[2]), float(words[3])]
    }
}

impl Channel<6> for MuscleFlex {
    fn write(&self, out: &mut Vec<u8>) {
        self.insertion.write(out);
        put_word(out, self.amplitude.to_bits());
        put_word(out, self.length.to_bits());
        put_word(out, u32::from(self.joint));
    }
    fn read(words: [[u8; 4]; 6]) -> Self {
        Self {
            insertion: Vec3::read([words[0], words[1], words[2]]),
            amplitude: float(words[3]),
            length: float(words[4]),
            joint: word(words[5]).clamp_u16(),
        }
    }
}

impl Channel<4> for SoftVertex {
    fn write(&self, out: &mut Vec<u8>) {
        put_word(out, self.joints[0]);
        put_word(out, self.joints[1]);
        put_word(out, self.weights[0].to_bits());
        put_word(out, self.weights[1].to_bits());
    }
    fn read(words: [[u8; 4]; 4]) -> Self {
        Self { joints: [word(words[0]), word(words[1])], weights: [float(words[2]), float(words[3])] }
    }
}

/// Every [`MaterialSlot`] has a number of its own, so a file keeps its meaning when the enum
/// gains a variant.
const fn slot_code(slot: MaterialSlot) -> u32 {
    match slot {
        MaterialSlot::Skin => 0,
        MaterialSlot::Hard => 1,
        MaterialSlot::Eye => 2,
        MaterialSlot::Cloth => 3,
        MaterialSlot::Wing => 4,
    }
}

const fn slot_of(code: u32) -> Option<MaterialSlot> {
    match code {
        0 => Some(MaterialSlot::Skin),
        1 => Some(MaterialSlot::Hard),
        2 => Some(MaterialSlot::Eye),
        3 => Some(MaterialSlot::Cloth),
        4 => Some(MaterialSlot::Wing),
        _ => None,
    }
}

fn put_channel<const W: usize, T: Channel<W>>(out: &mut Vec<u8>, items: &[T]) {
    put_word(out, items.len().clamp_u32());
    for item in items {
        item.write(out);
    }
}

/// `cloth` is one byte a vertex, padded so what follows stays word-aligned.
fn put_cloth(out: &mut Vec<u8>, cloth: &[u8]) {
    put_word(out, cloth.len().clamp_u32());
    out.extend_from_slice(cloth);
    out.extend(std::iter::repeat_n(0, pad(cloth.len())));
}

const fn pad(len: usize) -> usize {
    (4 - len % 4) % 4
}

fn put_mesh(out: &mut Vec<u8>, m: &BodyMeshData) {
    put_channel(out, &m.positions);
    put_channel(out, &m.normals);
    put_channel(out, &m.joints);
    put_channel(out, &m.weights);
    put_channel(out, &m.mob_data);
    put_channel(out, &m.uvs);
    put_channel(out, &m.coverage);
    put_channel(out, &m.flow);
    put_channel(out, &m.paint);
    put_cloth(out, &m.cloth);
    put_channel(out, &m.cor);
    put_channel(out, &m.muscle);
    put_channel(out, &m.soft);
    put_channel(out, &m.indices);
    put_word(out, m.submeshes.len().clamp_u32());
    for s in &m.submeshes {
        put_word(out, s.indices.start);
        put_word(out, s.indices.end);
        put_word(out, slot_code(s.slot));
    }
}

/// Reads a cached body, refusing anything it can't account for.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(taken)
    }

    fn word(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(*self.bytes(4)?.first_chunk()?))
    }

    fn len(&mut self) -> Option<usize> {
        Some(self.word()?.idx())
    }

    fn channel<const W: usize, T: Channel<W>>(&mut self) -> Option<Vec<T>> {
        let len = self.len()?;
        let bytes = self.bytes(len.checked_mul(W)?.checked_mul(4)?)?;
        let (items, _) = bytes.as_chunks::<4>().0.as_chunks::<W>();
        Some(items.iter().copied().map(T::read).collect())
    }

    fn cloth(&mut self) -> Option<Vec<u8>> {
        let len = self.len()?;
        let cloth = self.bytes(len)?.to_vec();
        self.bytes(pad(len))?;
        Some(cloth)
    }

    fn mesh(&mut self) -> Option<BodyMeshData> {
        let mesh = BodyMeshData {
            positions: self.channel()?,
            normals: self.channel()?,
            joints: self.channel()?,
            weights: self.channel()?,
            mob_data: self.channel()?,
            uvs: self.channel()?,
            coverage: self.channel()?,
            flow: self.channel()?,
            paint: self.channel()?,
            cloth: self.cloth()?,
            cor: self.channel()?,
            muscle: self.channel()?,
            soft: self.channel()?,
            indices: self.channel()?,
            submeshes: self.submeshes()?,
        };
        sane(&mesh).then_some(mesh)
    }

    fn submeshes(&mut self) -> Option<Vec<Submesh>> {
        let len = self.len()?;
        let mut out = Vec::with_capacity(len.min(1 << 10));
        for _ in 0..len {
            let (start, end) = (self.word()?, self.word()?);
            out.push(Submesh { indices: start..end, slot: slot_of(self.word()?)? });
        }
        Some(out)
    }
}

/// Whether a mesh read from disk hangs together: every channel either empty or one entry a
/// vertex, whole triangles, every index a vertex it has, and every submesh a range of them.
fn sane(m: &BodyMeshData) -> bool {
    let n = m.positions.len();
    let required = [m.normals.len(), m.joints.len(), m.weights.len(), m.mob_data.len()];
    let optional = [m.uvs.len(), m.coverage.len(), m.flow.len(), m.paint.len(), m.cloth.len(), m.cor.len(), m.muscle.len(), m.soft.len()];
    required.iter().all(|&l| l == n)
        && optional.iter().all(|&l| l == 0 || l == n)
        && m.indices.len() % 3 == 0
        && m.indices.iter().all(|&i| i.idx() < n)
        && m.submeshes.iter().all(|s| s.indices.start <= s.indices.end && s.indices.end.idx() <= m.indices.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{BodyPlan, Form, apply_form, test_assets};
    use crate::coat::Coat;
    use crate::clothing::Outfit;
    use crate::mob::{GenInput, StageTimes, body_key, generate_body};
    use crate::surface::SurfaceSettings;

    /// A mesh with every channel filled, so a round trip has to carry all of them.
    fn a_mesh(n: u32) -> BodyMeshData {
        let f = |i: u32| f32::from(i.clamp_u16());
        let mut mesh = BodyMeshData::default();
        for i in 0..n {
            mesh.positions.push(Vec3::new(f(i), f(i) + 0.5, -f(i)));
            mesh.normals.push(Vec3::Y);
            mesh.joints.push([i.clamp_u16(), 1, 2, 3]);
            mesh.weights.push([0.25, 0.25, 0.25, 0.25]);
            mesh.mob_data.push([i, i << 8]);
            mesh.uvs.push([f(i), 0.5]);
            mesh.coverage.push([0.1, 0.2, 0.3]);
            mesh.flow.push([0.0, 1.0, 0.0, f(i)]);
            mesh.paint.push([1.0, 0.5, 0.25, 0.75]);
            mesh.cloth.push(i.clamp_u8());
            mesh.cor.push(Vec3::splat(f(i)));
            mesh.muscle.push(MuscleFlex { insertion: Vec3::X, amplitude: 0.02, length: 0.3, joint: i.clamp_u16() });
            mesh.soft.push(SoftVertex { joints: [i, i + 1], weights: [0.5, 0.25] });
        }
        for i in 0..n.saturating_sub(2) {
            mesh.indices.extend_from_slice(&[i, i + 1, i + 2]);
        }
        let half = (mesh.indices.len() / 6 * 3).clamp_u32();
        mesh.submeshes = vec![
            Submesh { indices: 0..half, slot: MaterialSlot::Skin },
            Submesh { indices: half..mesh.indices.len().clamp_u32(), slot: MaterialSlot::Wing },
        ];
        mesh
    }

    fn a_body() -> CachedMeshes {
        // A bare level (no coat channels) alongside full ones, since an empty channel is its own
        // case, and coat geometry only on the hero level, as the generator does.
        let bare = BodyMeshData {
            positions: vec![Vec3::ZERO; 3],
            normals: vec![Vec3::Y; 3],
            joints: vec![[0; 4]; 3],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 3],
            mob_data: vec![[0, 0]; 3],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh { indices: 0..3, slot: MaterialSlot::Eye }],
            ..Default::default()
        };
        CachedMeshes {
            lods: [a_mesh(37), a_mesh(11), bare],
            coat_meshes: [
                CoatMeshes { shells: Some(a_mesh(9)), feathers: None, hair: Some(a_mesh(5)) },
                CoatMeshes::default(),
                CoatMeshes::default(),
            ],
        }
    }

    #[test]
    fn a_cached_body_round_trips_byte_for_byte() {
        let rules = PartLodRules::default();
        let bytes = a_body().to_bytes(rules);
        let read = CachedMeshes::from_bytes(&bytes, rules).expect("the bytes just written");
        assert_eq!(read.to_bytes(rules), bytes, "re-encoding what was read must give the same file");
        // Spot-check that the arrays came back rather than round-tripping as the same nothing.
        let hero = &read.lods[0];
        assert_eq!(hero.positions.len(), 37);
        assert_eq!(hero.positions[36], Vec3::new(36.0, 36.5, -36.0));
        assert_eq!(hero.joints[36], [36, 1, 2, 3]);
        assert_eq!(hero.cloth[36], 36);
        assert_eq!(hero.muscle[36].joint, 36);
        assert_eq!(hero.soft[36].weights, [0.5, 0.25]);
        assert_eq!(hero.submeshes[1].slot, MaterialSlot::Wing);
        assert!(read.lods[2].cor.is_empty(), "an empty channel stays empty");
        assert!(read.coat_meshes[0].feathers.is_none() && read.coat_meshes[0].hair.is_some());
        assert!(read.coat_meshes[1].shells.is_none());
    }

    /// A cache file can be anything on disk, so every way it can be wrong must be a miss rather
    /// than a mesh built from nonsense.
    #[test]
    fn a_damaged_cache_file_is_refused() {
        let rules = PartLodRules::default();
        let good = a_body().to_bytes(rules);
        let refused = |bytes: &[u8], why: &str| assert!(CachedMeshes::from_bytes(bytes, rules).is_none(), "{why}");
        refused(&[], "nothing at all");
        refused(&good[..good.len() - 1], "a truncated file");
        refused(&good[..40], "a file cut off inside the first level");
        let mut magic = good.clone();
        magic[7] = b'0';
        refused(&magic, "another version's layout");
        let mut trailing = good.clone();
        trailing.push(0);
        refused(&trailing, "bytes after the last level");
        let mut flipped = good.clone();
        // The last word is the final submesh's material slot.
        let last = flipped.len() - 4;
        flipped[last] = 99;
        refused(&flipped, "a material slot this version doesn't know");
        // Part LOD thresholds are not covered by a body key, so a file written under others is
        // refused rather than read as this build's body.
        let other = PartLodRules { near_min_ratio: 0.5, ..rules };
        assert!(CachedMeshes::from_bytes(&good, other).is_none(), "a file written under other part LOD rules");
        assert!(CachedMeshes::from_bytes(&a_body().to_bytes(other), other).is_some(), "a file written under its own rules");
    }

    #[test]
    fn an_inconsistent_mesh_is_refused() {
        let rules = PartLodRules::default();
        let bad = |make: fn(&mut BodyMeshData), why: &str| {
            let mut body = a_body();
            make(&mut body.lods[0]);
            assert!(CachedMeshes::from_bytes(&body.to_bytes(rules), rules).is_none(), "{why}");
        };
        bad(|m| { m.normals.pop(); }, "a channel short of a vertex");
        bad(|m| m.cor.truncate(3), "an optional channel neither empty nor full");
        bad(|m| m.indices.push(0), "indices that aren't whole triangles");
        bad(|m| m.indices[0] = 1000, "an index past the last vertex");
        bad(|m| m.submeshes[0].indices.end = 10_000, "a submesh past the last index");
        bad(|m| m.submeshes[0].indices.start = m.submeshes[0].indices.end + 3, "a submesh that ends before it starts");
    }

    /// The point of the whole thing: the second build of a body reads its meshes back instead of
    /// meshing, and gets the same geometry.
    #[test]
    fn a_cached_body_is_read_back_by_the_next_build() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/darner.body.ron")).expect("the darner's plan");
        let live = apply_form(&plan, &Form::default()).expect("a formed darner");
        let (settings, rules, coat, outfit) = (SurfaceSettings::default(), PartLodRules::default(), Coat::default(), Outfit::default());
        let assets = test_assets(&plan);
        let key = body_key(&live, &settings, 0, &assets, &coat, &outfit);
        let dir = std::env::temp_dir().join(format!("ornisculpt-body-cache-{}", std::process::id()));
        let mut cache = BodyMeshCache::default();
        cache.set_dir(&dir);
        let entry = cache.entry(key, rules).expect("a cache entry");
        let input = || GenInput {
            live: live.clone(),
            rules,
            assets: assets.clone(),
            settings: settings.clone(),
            seed: 0,
            coat: coat.clone(),
            outfit: outfit.clone(),
            cache: Some(entry.clone()),
        };

        let mut cold = StageTimes::default();
        let first = generate_body(input(), &mut cold).expect("the darner generates");
        let stage = |times: &StageTimes, name: &str| times.0.iter().any(|(n, _)| *n == name);
        assert!(stage(&cold, "mesh") && stage(&cold, "mesh_cache_write"), "the first build meshes and writes: {:?}", cold.0);

        let mut warm = StageTimes::default();
        let second = generate_body(input(), &mut warm).expect("the darner generates again");
        assert!(!stage(&warm, "mesh"), "the second build must not mesh: {:?}", warm.0);
        assert!(stage(&warm, "mesh_cache_read"), "it reads the cache instead: {:?}", warm.0);
        // Both ways round: the bytes of what was generated and of what was read must agree.
        let bytes = |out: &crate::mob::GenOutput| {
            CachedMeshes { lods: out.lods.clone(), coat_meshes: out.coat_meshes.clone() }.to_bytes(rules)
        };
        assert_eq!(bytes(&second), bytes(&first), "a body read back is the body that was built");
        drop(std::fs::remove_dir_all(&dir));
    }

    /// Timing (ignored): what the cache saves on a heavy body, and what it costs on disk.
    #[test]
    #[ignore = "timing"]
    fn cached_body_timing() {
        let plan: BodyPlan = ron::from_str(include_str!("../../assets/mobs/man.body.ron")).expect("the man's plan");
        let live = apply_form(&plan, &Form::default()).expect("a formed man");
        let (settings, rules, coat, outfit) = (SurfaceSettings::default(), PartLodRules::default(), Coat::default(), Outfit::default());
        let assets = test_assets(&plan);
        let key = body_key(&live, &settings, 0, &assets, &coat, &outfit);
        let dir = std::env::temp_dir().join(format!("ornisculpt-body-timing-{}", std::process::id()));
        let mut cache = BodyMeshCache::default();
        cache.set_dir(&dir);
        let entry = cache.entry(key, rules).expect("a cache entry");
        let input = |cache| GenInput {
            live: live.clone(),
            rules,
            assets: assets.clone(),
            settings: settings.clone(),
            seed: 0,
            coat: coat.clone(),
            outfit: outfit.clone(),
            cache,
        };
        let run = |cache| {
            let mut times = StageTimes::default();
            let t = std::time::Instant::now();
            let out = generate_body(input(cache), &mut times).expect("the man generates");
            (t.elapsed(), times, out)
        };
        // Once with no cache at all, for what generation costs; then cold (writing) and warm.
        // The first run converts the sculpted head's SDF and the rest share it in memory, so
        // only the uncached figure carries that half a second.
        let (uncached, _, _) = run(None);
        let (cold, _, _) = run(Some(entry.clone()));
        let (warm, stages, _) = run(Some(entry));
        let size = std::fs::read(dir.join(format!("{:032x}.body", key.0))).map_or(0, |b| b.len());
        eprintln!("man: {uncached:?} uncached, {cold:?} cold (writing), {warm:?} warm; {} MB on disk", size / (1 << 20));
        for (name, took) in &stages.0 {
            eprintln!("  warm stage {name}: {took:?}");
        }
        drop(std::fs::remove_dir_all(&dir));
    }
}
