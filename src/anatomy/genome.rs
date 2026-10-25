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
    use genetics::Genome;
    use rand::SeedableRng;
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
}
