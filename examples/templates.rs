//! Writes the part and garment templates the Blender add-on offers
//! (`tools/blender/ornisculpt/templates.json`): one part of each kind (and role, hard kind or
//! shape) lifted from the sample plans with what hangs off it (an arm with its fingers), blank
//! sculpt and mesh parts to model in place, and a blank garment. Run after changing the part types
//! or the sample plans.
//!
//! ```text
//! cargo run --example templates --features interchange
//! ```

use std::process::ExitCode;

use ornisculpt::anatomy::{
    AttachSlot, BodyPlan, HardKind, HardParams, Orient, PartDetail, PartKind, PartNode, PartOffset, PartTone, Repeat, SculptParams, Side, StampSet,
};
use ornisculpt::clothing::Garment;
use serde_json::{Value, json};

const OUT: &str = "tools/blender/ornisculpt/templates.json";

/// What tells two parts of one kind apart: a limb's or chain's role, a hard part's kind, a mass's
/// shape, a wing's kind.
fn key(node: &PartNode) -> Result<String, String> {
    let kind = serde_json::to_value(&node.kind).map_err(|e| e.to_string())?;
    let (variant, params) = match &kind {
        Value::Object(o) => o.iter().next().map_or_default(|(k, v)| (k.clone(), v.clone())),
        Value::String(s) => (s.clone(), Value::Null),
        _ => (String::new(), Value::Null),
    };
    let detail = ["role", "kind", "shape"].iter().find_map(|k| match params.get(k)? {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.keys().next().cloned(),
        _ => None,
    });
    Ok(detail.map_or_else(|| variant.clone(), |d| format!("{variant} {d}")))
}

/// `node` as JSON without `null`s (Blender can't store them) or the `side` expansion sets.
fn clean(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.into_iter().filter(|(k, v)| !v.is_null() && k != "side").map(|(k, v)| (k, clean(v))).collect()),
        Value::Array(a) => Value::Array(a.into_iter().map(clean).collect()),
        v => v,
    }
}

fn blank(name: &str, kind: PartKind) -> PartNode {
    PartNode {
        name: name.into(),
        kind,
        attach: AttachSlot::Surface { t: 0.5, around_deg: 0.0 },
        offset: PartOffset::default(),
        mirror: false,
        repeat: Repeat::Once,
        side: Side::Center,
        detail: PartDetail::default(),
        tone: PartTone::default(),
        stamps: StampSet::default(),
        density: 1.0,
        children: Vec::new(),
    }
}

fn run() -> Result<String, String> {
    let dir = format!("{}/assets/mobs", env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(&dir).map_err(|e| e.to_string())?.filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|f| f.ends_with(".body.ron")).collect();
    files.sort();
    let mut parts: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for file in files {
        let plan: BodyPlan = ron::from_str(&std::fs::read_to_string(format!("{dir}/{file}")).map_err(|e| e.to_string())?).map_err(|e| format!("{file}: {e}"))?;
        let body = file.trim_end_matches(".body.ron").to_owned();
        let mut found: Vec<(String, PartNode)> = Vec::new();
        plan.visit(|path, node| found.push((path.to_string(), node.clone())));
        for (path, node) in found {
            // The root is a whole body, not a part to add.
            if path == plan.root.name {
                continue;
            }
            let key = key(&node)?;
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            parts.push(json!({ "label": format!("{key} ({body})"), "from": format!("{body}: {path}"), "part": clean(serde_json::to_value(&node).map_err(|e| e.to_string())?) }));
        }
    }
    let sculpt = blank("sculpt", PartKind::Sculpt(SculptParams::default()));
    let mesh = blank("mesh", PartKind::Hard(HardParams { kind: HardKind::Mesh { path: String::new(), orient: Orient::default(), scale: 1.0 }, ..Default::default() }));
    let doc = json!({
        "format": ornisculpt::interchange::FORMAT,
        "parts": parts,
        "modelled": { "Sculpt": clean(serde_json::to_value(&sculpt).map_err(|e| e.to_string())?), "Mesh": clean(serde_json::to_value(&mesh).map_err(|e| e.to_string())?) },
        "garment": clean(serde_json::to_value(Garment::default()).map_err(|e| e.to_string())?),
    });
    let out = format!("{}/{OUT}", env!("CARGO_MANIFEST_DIR"));
    std::fs::write(&out, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n").map_err(|e| format!("{out}: {e}"))?;
    Ok(format!("{OUT}: {} part templates", parts.len()))
}

fn main() -> ExitCode {
    match run() {
        Ok(done) => {
            println!("{done}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
