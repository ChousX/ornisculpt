//! Genomes (`genetics` feature): everything a creature is, as DNA.
//!
//! A genome holds [`CreatureGenes`]: the whole [`BodyPlan`] and [`Coat`], and a [`Form`] whose
//! lever patches carry what varies between individuals. Every value is in the DNA. The ones a
//! design needs just so (the part tree, kinds, attach slots, names, enum choices) are fixed genes,
//! which point mutation never touches; the ones a [`CreatureDesign`] lists as traits vary within
//! their ranges.
//!
//! [`CreatureDesign::founder`] gives a lineage's first genome; [`express`] turns any genome of it
//! back into a body, gated by [`ViabilityChecks`]. Individuals that only differ in their form share
//! one [`Genome::lineage`], and so can share one body plan asset.

use bevy::asset::{AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use genetics::{Gene, Genome, Trait, Variation};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    Anatomy, AnatomyError, BodyPlan, Form, FormError, FormPatch, Lever, LeverKind, PartAssets, PartLodRules, PartPath,
    ViabilityChecks, ViabilityReport, apply_form, build_anatomy,
};
use crate::coat::Coat;
use crate::num::Saturate;

/// What a creature's genome encodes.
///
/// Never put `skip_serializing_if`, `skip`, `flatten`, `untagged` or anything else that changes
/// how many fields a type writes on any type reachable from here (the body plan and its parts,
/// the coat, the form): `genetics` encodes a value by position, field after field with no names,
/// so a field left out shifts every gene after it and no genome decodes. Every arm is checked by
/// `every_sample_body_round_trips_through_dna` and `every_sample_coat_and_a_form_round_trip_through_dna`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreatureGenes {
    pub body: BodyPlan,
    pub coat: Coat,
    pub form: Form,
}

/// One value of a design that may vary.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum CreatureTrait {
    /// A lever on a part (or, unsuffixed, every copy of it; see [`super::path_matches`]): one
    /// [`FormPatch`] whose value varies within `range`, which must lie inside [`Lever::range`].
    /// Count levers vary in whole steps.
    Lever { part: PartPath, lever: Lever, range: (f32, f32), founder: f32 },
    /// The whole body's size, within [`Lever::Scale`]'s range.
    Scale { range: (f32, f32), founder: f32 },
    /// Any value of [`CreatureGenes`] by its genome path (`coat.base.0`,
    /// `body.root.children.2.kind.Hard.size`), starting from the value as authored. Varying a
    /// value of the body plan stops individuals sharing one plan.
    Field(Trait),
}

/// A creature as a player designed it: the body and coat as authored, and what may vary.
#[derive(Asset, TypePath, Serialize, Deserialize, Clone, Debug)]
pub struct CreatureDesign {
    pub body: BodyPlan,
    pub coat: Coat,
    pub traits: Vec<CreatureTrait>,
}

#[derive(Debug, Error)]
pub enum DesignError {
    #[error("lever {lever:?} on `{part}`: range {lo}..={hi} is not inside {min}..={max}")]
    Range { part: PartPath, lever: Lever, lo: f32, hi: f32, min: f32, max: f32 },
    #[error(transparent)]
    Form(#[from] FormError),
    #[error(transparent)]
    Anatomy(#[from] AnatomyError),
    #[error(transparent)]
    Genetics(#[from] genetics::Error),
    #[error("unviable: {}", .0.join("; "))]
    Unviable(Vec<String>),
}

/// How elaborate a body is, for budgets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Complexity {
    pub bones: usize,
    pub parts: usize,
}

impl Complexity {
    #[must_use]
    pub const fn of(anatomy: &Anatomy) -> Self {
        Self { bones: anatomy.skeleton.len(), parts: anatomy.parts.len() }
    }
}

/// A genome made flesh.
#[derive(Debug)]
pub struct Expressed {
    pub genes: CreatureGenes,
    pub anatomy: Anatomy,
    pub complexity: Complexity,
    /// Every check passed or only warned.
    pub report: ViabilityReport,
}

impl CreatureDesign {
    /// The founder's genes, and which of them may vary.
    ///
    /// # Errors
    ///
    /// When a lever's range lies outside [`Lever::range`].
    pub fn genes(&self) -> Result<(CreatureGenes, Variation), DesignError> {
        let mut form = Form::default();
        let mut variation = Variation::new();
        for t in &self.traits {
            match t {
                CreatureTrait::Lever { part, lever, range, founder } => {
                    check_range(part, *lever, *range)?;
                    let path = format!("form.patches.{}.value", form.patches.len());
                    form.patches.push(FormPatch { part: part.clone(), lever: *lever, value: *founder });
                    variation = variation.with(path, lever_gene(*lever, *range));
                }
                CreatureTrait::Scale { range, founder } => {
                    check_range(&PartPath::default(), Lever::Scale, *range)?;
                    form.overall_scale = *founder;
                    variation = variation.with("form.overall_scale", lever_gene(Lever::Scale, *range));
                }
                CreatureTrait::Field(t) => variation.0.push(t.clone()),
            }
        }
        Ok((CreatureGenes { body: self.body.clone(), coat: self.coat.clone(), form }, variation))
    }

    /// The first genome of the design's lineage.
    ///
    /// # Errors
    ///
    /// When a trait is out of range or names nothing, or the founder's form doesn't apply.
    pub fn founder(&self) -> Result<Genome, DesignError> {
        let (genes, variation) = self.genes()?;
        apply_form(&genes.body, &genes.form)?;
        Ok(Genome::encode(&genes, &variation)?)
    }
}

/// Reads a genome back into a body and builds it: the form applied, the anatomy built with
/// `assets` (every authored mesh the plan references) and run past `checks`.
///
/// # Errors
///
/// When the genome doesn't decode (a mutation chose a variant that carries data), the form or
/// anatomy doesn't build, or a check fails: each makes the genome unviable.
pub fn express(genome: &Genome, assets: &PartAssets, checks: &ViabilityChecks) -> Result<Expressed, DesignError> {
    let genes: CreatureGenes = genome.decode()?;
    let live = apply_form(&genes.body, &genes.form)?;
    let anatomy = build_anatomy(&live, &PartLodRules::default(), assets)?;
    let report = checks.run(&anatomy);
    if !report.viable() {
        let fails = report.results.iter().filter(|(_, v)| v.is_fail()).map(|(name, v)| format!("{name}: {v:?}")).collect();
        return Err(DesignError::Unviable(fails));
    }
    Ok(Expressed { complexity: Complexity::of(&anatomy), genes, anatomy, report })
}

fn check_range(part: &PartPath, lever: Lever, (lo, hi): (f32, f32)) -> Result<(), DesignError> {
    let (min, max) = lever.range();
    if min <= lo && lo < hi && hi <= max {
        Ok(())
    } else {
        Err(DesignError::Range { part: part.clone(), lever, lo, hi, min, max })
    }
}

fn lever_gene(lever: Lever, (lo, hi): (f32, f32)) -> Gene {
    if lever.kind() == LeverKind::Count {
        Gene::Count { min: lo.ceil().sat_i64(), max: hi.floor().sat_i64() }
    } else {
        Gene::range(lo.into(), hi.into())
    }
}

/// A `*.creature.ron` file: a design whose body and coat are other assets.
#[derive(Deserialize)]
struct DesignFile {
    /// A `*.body.ron` path.
    body: String,
    /// A `*.coat.ron` path.
    coat: String,
    #[serde(default)]
    traits: Vec<CreatureTrait>,
}

/// Loads `*.creature.ron` files into [`CreatureDesign`]s, reading the body and coat they name.
#[derive(Default, TypePath)]
pub struct CreatureDesignLoader;

#[derive(Debug, Error)]
pub enum CreatureDesignLoaderError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ron(#[from] ron::error::SpannedError),
    #[error("loading `{path}`: {reason}")]
    Part { path: String, reason: String },
}

impl AssetLoader for CreatureDesignLoader {
    type Asset = CreatureDesign;
    type Settings = ();
    type Error = CreatureDesignLoaderError;

    async fn load(&self, reader: &mut dyn Reader, (): &(), cx: &mut LoadContext<'_>) -> Result<CreatureDesign, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let file: DesignFile = ron::de::from_bytes(&bytes)?;
        let part = |path: &str, e: &dyn std::fmt::Display| CreatureDesignLoaderError::Part { path: path.to_owned(), reason: e.to_string() };
        let body = cx.load_builder().load_value::<BodyPlan>(&file.body).await.map_err(|e| part(&file.body, &e))?.take();
        let coat = cx.load_builder().load_value::<Coat>(&file.coat).await.map_err(|e| part(&file.coat, &e))?.take();
        Ok(CreatureDesign { body, coat, traits: file.traits })
    }

    fn extensions(&self) -> &[&str] {
        &["creature.ron"]
    }
}

/// Registers [`CreatureDesign`] assets and their loader.
pub struct GenomePlugin;

impl Plugin for GenomePlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<CreatureDesign>().init_asset_loader::<CreatureDesignLoader>();
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash, Hasher};

    use genetics::Genome;
    use rand::{RngExt, SeedableRng};
    use rand_pcg::Pcg64Mcg;

    use super::*;

    /// `BodyPlan` has no `PartialEq`; RON prints floats exactly.
    fn ron<T: Serialize>(value: &T) -> String {
        ron::to_string(value).unwrap()
    }

    fn quadruped() -> BodyPlan {
        ron::from_str(include_str!("../../assets/mobs/quadruped.body.ron")).unwrap()
    }

    fn design() -> CreatureDesign {
        let lever = |part: &str, lever, range, founder| CreatureTrait::Lever { part: PartPath(part.into()), lever, range, founder };
        CreatureDesign {
            body: quadruped(),
            coat: Coat::default(),
            traits: vec![
                CreatureTrait::Scale { range: (0.8, 1.25), founder: 1.0 },
                lever("torso/limb.front", Lever::Length, (0.85, 1.2), 1.0),
                lever("torso/limb.hind", Lever::Length, (0.85, 1.2), 1.0),
                lever("torso", Lever::Fat, (0.5, 1.6), 1.0),
                CreatureTrait::Field(Trait { path: "coat.base.0".into(), gene: Gene::range(0.3, 0.8) }),
            ],
        }
    }

    #[test]
    fn a_body_round_trips_through_dna() {
        let genes = CreatureGenes { body: quadruped(), coat: Coat::default(), form: Form::default() };
        let genome = Genome::encode(&genes, &Variation::new()).unwrap();
        assert_eq!(ron(&genome.decode::<CreatureGenes>().unwrap()), ron(&genes));
    }

    /// Every sample body survives DNA, whatever parts and overrides it uses: a genome reads a
    /// struct field by field in order, so a field serde leaves out (`skip_serializing_if`)
    /// shifts every gene after it, and the next fixed gene is read as the skipped field's shape
    /// (`WrongGene { path: "body.root.side", found: "an option", gene: "fixed" }`).
    #[test]
    fn every_sample_body_round_trips_through_dna() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/mobs");
        let mut bodies = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.to_string_lossy().ends_with(".body.ron") {
                continue;
            }
            let body: BodyPlan = ron::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let genes = CreatureGenes { body, coat: Coat::default(), form: Form::default() };
            let genome = Genome::encode(&genes, &Variation::new()).unwrap();
            let back = genome.decode::<CreatureGenes>().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(ron(&back), ron(&genes), "{}", path.display());
            bodies += 1;
        }
        assert!(bodies > 20, "only {bodies} sample bodies");
    }

    /// The coat and form arms likewise: every sample coat, and a form that scales and patches,
    /// through DNA beside a body.
    #[test]
    fn every_sample_coat_and_a_form_round_trip_through_dna() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/coats");
        let form = Form {
            overall_scale: 1.1,
            patches: vec![
                FormPatch { part: PartPath("torso".into()), lever: Lever::Fat, value: 1.3 },
                FormPatch { part: PartPath("torso/limb.front".into()), lever: Lever::Length, value: 0.9 },
            ],
        };
        let mut coats = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.to_string_lossy().ends_with(".coat.ron") {
                continue;
            }
            let coat: Coat = ron::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let genes = CreatureGenes { body: quadruped(), coat, form: form.clone() };
            let genome = Genome::encode(&genes, &Variation::new()).unwrap();
            let back = genome.decode::<CreatureGenes>().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(ron(&back), ron(&genes), "{}", path.display());
            coats += 1;
        }
        assert!(coats > 10, "only {coats} sample coats");
    }

    #[test]
    fn mutants_stay_viable_and_of_one_lineage() {
        let founder = design().founder().unwrap();
        let checks = ViabilityChecks::with_builtins();
        let assets = PartAssets::default();
        let first = express(&founder, &assets, &checks).unwrap();
        assert!(first.complexity.bones > 0);
        let mut rng = Pcg64Mcg::seed_from_u64(11);
        let mut scales = Vec::new();
        let mut unviable = 0;
        for _ in 0..200 {
            let mutant = founder.mutant(0.1, &mut rng);
            assert_eq!(mutant.lineage(), founder.lineage());
            match express(&mutant, &assets, &checks) {
                Ok(body) => {
                    assert_eq!(ron(&body.genes.body), ron(&first.genes.body));
                    assert!((0.8..=1.25).contains(&body.genes.form.overall_scale));
                    scales.push(body.genes.form.overall_scale);
                }
                // Selection at work: legs mutated apart leave feet off the ground.
                Err(DesignError::Unviable(_)) => unviable += 1,
                Err(e) => panic!("{e}"),
            }
        }
        println!("{unviable} of 200 mutants unviable");
        assert!(unviable < 100, "{unviable} of 200 mutants unviable");
        assert!(scales.iter().any(|&s| (s - 1.0).abs() > 0.01), "nothing varied");
    }

    #[test]
    fn levers_must_fit_their_part_and_range() {
        let mut d = design();
        d.traits.push(CreatureTrait::Lever { part: PartPath("torso".into()), lever: Lever::Length, range: (0.1, 1.0), founder: 0.5 });
        assert!(matches!(d.founder(), Err(DesignError::Range { .. })));
        let mut d = design();
        // A torso has no digits to count.
        d.traits.push(CreatureTrait::Lever { part: PartPath("torso".into()), lever: Lever::Count, range: (2.0, 4.0), founder: 3.0 });
        assert!(matches!(d.founder(), Err(DesignError::Form(_))));
        let mut d = design();
        d.traits.push(CreatureTrait::Lever { part: PartPath("torso/tail".into()), lever: Lever::Length, range: (0.5, 1.5), founder: 2.0 });
        assert!(matches!(d.founder(), Err(DesignError::Genetics(_))));
    }

    /// The traits `design()` puts on the quadruped, over any body: they name parts, so a body
    /// without them is given the two that every creature has instead (overall scale, and a coat
    /// field). Returns the design and which set it got, so the report can say.
    fn design_for(body: BodyPlan, coat: Coat) -> (CreatureDesign, &'static str) {
        // A `Field` trait's gene must contain the authored value, or the founder cannot be
        // encoded at all (`0.25 is outside 0.3..=0.8`) — so each range is built around the
        // coat in hand rather than written down once.
        let around = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            Gene::range(f64::from((v - 0.15).max(0.0)), f64::from((v + 0.15).min(1.0)))
        };
        let fields = vec![
            CreatureTrait::Field(Trait { path: "coat.base.0".into(), gene: around(coat.base[0]) }),
            CreatureTrait::Field(Trait { path: "coat.roughness".into(), gene: around(coat.roughness) }),
        ];
        let mut rich = design().traits;
        rich.retain(|t| !matches!(t, CreatureTrait::Field(_)));
        rich.extend(fields.iter().cloned());
        let rich = CreatureDesign { body: body.clone(), coat: coat.clone(), traits: rich };
        if rich.founder().is_ok() {
            return (rich, "levers");
        }
        let mut traits = vec![CreatureTrait::Scale { range: (0.8, 1.25), founder: 1.0 }];
        traits.extend(fields);
        (CreatureDesign { body, coat, traits }, "scale+fields")
    }

    /// Every sample body with the default coat, and every sample coat on the quadruped: the
    /// populations the pool proof breeds.
    fn populations() -> Vec<(String, CreatureDesign, &'static str)> {
        let mut out = Vec::new();
        let read = |dir: &str, suffix: &'static str| {
            let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.to_string_lossy().ends_with(suffix))
                .collect();
            // `read_dir` order is the filesystem's; sort so the run is the same every time.
            paths.sort();
            paths
        };
        for path in read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/mobs"), ".body.ron") {
            let body: BodyPlan = ron::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let (design, kind) = design_for(body, Coat::default());
            out.push((format!("body {}", path.file_name().unwrap().to_string_lossy()), design, kind));
        }
        for path in read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/coats"), ".coat.ron") {
            let coat: Coat = ron::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let (design, kind) = design_for(quadruped(), coat);
            out.push((format!("coat {}", path.file_name().unwrap().to_string_lossy()), design, kind));
        }
        out
    }

    fn digest<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    /// The decode-side layout guard (`genetics` `Schema::validate`) checks the **schema's**
    /// layout, not the values on the strand — so nothing breeding can produce is refused by it.
    /// Crossing takes whole loci from parents that share a schema; mutation rewrites bases inside
    /// a locus. Neither moves a locus, so the reading frame stays exactly the one the encoder
    /// built, and the guard has nothing to object to however far the values travel.
    ///
    /// Proved here on the real creature genome rather than a toy schema, over every sample body
    /// and every sample coat, breeding at mutation rates from 0.05 to 0.5 — rates far above the
    /// 0.1 `mutants_stay_viable_and_of_one_lineage` uses — for several generations:
    ///
    /// 1. every child decodes, with no error of any kind;
    /// 2. decode-then-re-encode gives back the **identical** strand, so the transcode is
    ///    one-to-one and no creature is lost or aliased on the way through (a varying locus's
    ///    quantisation included: the re-encoded code must be the same code, not a near one);
    /// 3. how many distinct creatures the run reaches, printed, so the pool can be compared with
    ///    a `genetics` from before the guard.
    #[test]
    fn crossing_and_heavy_mutation_never_trip_the_layout_guard() {
        /// Genomes bred at once, and generations of them. The product is what decides the run's
        /// length; each child costs one decode and one encode of a whole body.
        const POP: usize = 10;
        const GENERATIONS: usize = 6;

        let mut rng = Pcg64Mcg::seed_from_u64(0x60_0D_C0_DE);
        let mut creatures = std::collections::HashSet::new();
        let mut strands = std::collections::HashSet::new();
        let mut children = 0usize;
        let mut levered = 0usize;
        let populations = populations();
        assert!(populations.len() > 40, "only {} populations", populations.len());

        for (name, design, kind) in &populations {
            if *kind == "levers" {
                levered += 1;
            }
            let (_, variation) = design.genes().unwrap_or_else(|e| panic!("{name}: {e}"));
            let founder = design.founder().unwrap_or_else(|e| panic!("{name}: {e}"));
            // Randomise every varying base, so the population does not start from one point.
            let mut pop: Vec<Genome> = (0..POP).map(|_| founder.mutant(1.0, &mut rng)).collect();
            for generation in 0..GENERATIONS {
                let mut next = Vec::with_capacity(POP);
                for _ in 0..POP {
                    let a = &pop[rng.random_range(0..pop.len())];
                    let b = &pop[rng.random_range(0..pop.len())];
                    let rate: f64 = rng.random_range(0.05..0.5);
                    let child = a
                        .cross(b, &mut rng)
                        .unwrap_or_else(|e| panic!("{name} gen {generation}: cross: {e}"))
                        .mutant(rate, &mut rng);

                    // (1) It decodes. A layout fault would land here, naming a locus.
                    let genes: CreatureGenes = child
                        .decode()
                        .unwrap_or_else(|e| panic!("{name} gen {generation} rate {rate:.3}: decode: {e}"));

                    // (2) And re-encoding what it decoded gives the same strand back, base for base.
                    let back = Genome::encode(&genes, &variation).unwrap_or_else(|e| panic!("{name}: re-encode: {e}"));
                    assert_eq!(*back.schema, *child.schema, "{name}: re-encode built a different frame");
                    if back.dna != child.dna {
                        // Name the locus rather than print two thousand bases.
                        let at = child
                            .schema
                            .loci()
                            .iter()
                            .find(|l| child.dna.get(l.bases()) != back.dna.get(l.bases()))
                            .map_or_else(|| "nowhere in any locus".to_string(), |l| format!("`{}` ({:?})", l.path, l.gene));
                        panic!("{name} gen {generation} rate {rate:.3}: re-encode changed {at}");
                    }

                    creatures.insert(digest(&ron(&genes)));
                    strands.insert(digest(&child.dna.to_string()));
                    children = children.saturating_add(1);
                    next.push(child);
                }
                pop = next;
            }
        }

        // The pool: printed, because the number is the point. Compare it with the same test on a
        // `genetics` without the guard — it must not be smaller.
        println!(
            "POOL {} populations ({levered} with levers), {children} children, {} distinct creatures, {} distinct strands",
            populations.len(),
            creatures.len(),
            strands.len()
        );
        assert_eq!(children, populations.len().saturating_mul(POP).saturating_mul(GENERATIONS));
        // Breeding must actually explore: a pool of one would pass everything above.
        assert!(creatures.len() > children / 2, "only {} distinct creatures of {children}", creatures.len());
    }
}
