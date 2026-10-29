//! Viability checks: runs the built-in checks on every body plan in `assets/mobs`, then on broken
//! variants that each check must reject (a hip raised out of reach of the ground, feet moved out
//! from under the body, a skeleton over the bone budget). Headless. Plans with authored part
//! meshes need the asset server, so they are skipped.
//!
//! ```text
//! cargo run --release --example viability
//! ```
//!
//! Exits with a failure code when a shipped plan fails a check or a broken variant passes.

use std::path::Path;
use std::process::ExitCode;

use bevy::prelude::*;
use ornisculpt::anatomy::{
    Anatomy, BodyPlan, BoneBudget, FeetReachGround, MAX_BONES, PartAssets, PartLodRules, StaticBalance, Viability, ViabilityCheck, ViabilityChecks,
};

mod common;

fn build(path: &Path) -> Result<Anatomy, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let plan: BodyPlan = ron::from_str(&text).map_err(|e| e.to_string())?;
    common::build_plan(&plan, PartLodRules::default(), &PartAssets::default())
}

fn describe(v: &Viability) -> String {
    match v {
        Viability::Ok => "ok".into(),
        Viability::Warn(m) => format!("warn: {m}"),
        Viability::Fail(m) => format!("FAIL: {m}"),
    }
}

/// A broken copy of `base`, and the check that must reject it.
struct Broken {
    name: &'static str,
    check: Box<dyn ViabilityCheck>,
    breaks: fn(&mut Anatomy),
}

fn broken_variants() -> [Broken; 3] {
    [
        Broken {
            name: "one leg lifted 0.5 m",
            check: Box::new(FeetReachGround::default()),
            breaks: |a| {
                // The whole leg moves up, its contact point with it: the foot dangles.
                let Some(leg) = a.legs.first_mut() else { return };
                leg.rest_foot.y += 0.5;
                // In the cached model-space rest pose: bone-local axes need not point up.
                for b in &leg.chain {
                    if let Some(m) = a.skeleton.rest_model.get_mut(b.index()) {
                        m.translation.y += 0.5;
                    }
                }
            },
        },
        Broken {
            name: "feet moved 5 m sideways",
            check: Box::new(StaticBalance),
            breaks: |a| a.legs.iter_mut().for_each(|l| l.rest_foot.x += 5.0),
        },
        Broken {
            name: "skeleton over the bone budget",
            check: Box::new(BoneBudget),
            breaks: |a| {
                if let Some(bone) = a.skeleton.bones.first().cloned() {
                    a.skeleton.bones.resize(MAX_BONES + 1, bone);
                }
            },
        },
    ]
}

fn main() -> ExitCode {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mobs");
    let mut plans: Vec<_> = std::fs::read_dir(&dir)
        .map_or_default(|entries| entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(".body.ron")).collect());
    plans.sort();
    let checks = ViabilityChecks::with_builtins();
    let mut problems = 0usize;
    let mut horse = None;

    println!("== shipped plans");
    for path in &plans {
        let name = path.file_name().map_or_default(|n| n.to_string_lossy().trim_end_matches(".body.ron").to_owned());
        let anatomy = match build(path) {
            Ok(a) => a,
            Err(e) if e.contains("needs mesh asset") => {
                println!("{name:<10} skipped (needs authored part meshes)");
                continue;
            }
            Err(e) => {
                println!("{name:<10} does not build: {e}");
                problems += 1;
                continue;
            }
        };
        let report = checks.run(&anatomy);
        let legs = anatomy.legs.len();
        println!("{name:<10} {} bones, {legs} legs, {}", anatomy.skeleton.len(), if report.viable() { "viable" } else { "NOT viable" });
        for (check, result) in &report.results {
            println!("    {check:<24} {}", describe(result));
        }
        problems += usize::from(!report.viable());
        if name == "horse" {
            horse = Some(anatomy);
        }
    }

    println!("\n== broken horses (each must fail its check)");
    let Some(base) = horse else {
        println!("no horse plan to break");
        return ExitCode::FAILURE;
    };
    for broken in broken_variants() {
        let mut anatomy = base.clone();
        let before = broken.check.check(&anatomy);
        (broken.breaks)(&mut anatomy);
        let after = broken.check.check(&anatomy);
        let expected = before == Viability::Ok && after.is_fail();
        println!("{:<30} {:<18} before: {}, after: {} {}", broken.name, broken.check.name(), describe(&before), describe(&after), if expected {
            "✓"
        } else {
            "✗ unexpected"
        });
        problems += usize::from(!expected);
    }

    if problems == 0 {
        println!("\nall as expected");
        ExitCode::SUCCESS
    } else {
        println!("\n{problems} unexpected result(s)");
        ExitCode::FAILURE
    }
}
