//! Garments modelled on a body, back into wrap space: the inverse of the fit.
//!
//! Each vertex is solved for the wrap-space point (`y` along the span, angle around the part,
//! `r`) that the fit ([`garment_point`]) places where the vertex is, by damped Gauss-Newton from
//! where the vertex lies along and around the part. Wrap space is relative to the body, so a
//! garment brought back from one body fits every other.

use bevy::prelude::*;

use super::axis_coords;
use crate::anatomy::{Anatomy, BodyPlan, PartPath, TriMesh, path_matches, sample_axis};
use crate::clothing::{Garment, first_piece, garment_point};
use crate::surface::BodyField;

/// How close (metres) a solved vertex must land to where it was modelled.
const LANDED: f32 = 1e-4;

/// A garment brought back into wrap space.
///
/// `worst` is how far (metres) the worst vertex lands from where it was modelled: a vertex the fit
/// can't reach, such as one inside the body, lands at the nearest place it can.
pub struct Unwrapped {
    pub mesh: TriMesh,
    pub worst: f32,
}

/// `mesh` (stored model space, modelled on `anatomy`'s rest pose) in `garment`'s wrap space on
/// the first part it wraps. `None` when it wraps no part.
#[must_use]
pub fn unwrap_garment(anatomy: &Anatomy, field: &BodyField, garment: &Garment, mesh: &TriMesh) -> Option<Unwrapped> {
    let part = first_piece(anatomy, garment)?;
    let built = anatomy.parts.get(usize::from(part))?;
    let solve = |p: &Vec3| solve_vertex(anatomy, field, part, garment, &built.axis, *p);
    let solved: Vec<(Vec3, f32)> = parallel_map(&mesh.positions, solve);
    let worst = solved.iter().map(|(_, e)| *e).fold(0.0, f32::max);
    let positions = solved.into_iter().map(|(w, _)| w).collect();
    let mut indices = mesh.indices.clone();
    if built.mirrored {
        indices.as_chunks_mut::<3>().0.iter_mut().for_each(|t| t.swap(1, 2));
    }
    Some(Unwrapped { mesh: TriMesh::new(positions, Vec::new(), indices), worst })
}

/// The part a garment modelled as `mesh` wraps, when its `on` wasn't given: the authored part
/// whose axis most vertices are nearest, as a pattern over its copies; and the stretch of it they
/// cover, as `span`.
#[must_use]
pub fn infer_on(plan: &BodyPlan, anatomy: &Anatomy, mesh: &TriMesh) -> Option<(String, (f32, f32))> {
    let parts: Vec<(usize, &crate::anatomy::BuiltPart)> = anatomy.parts.iter().enumerate().filter(|(_, p)| !p.axis.is_empty()).collect();
    let nearest = |p: Vec3| {
        parts
            .iter()
            .filter_map(|(i, part)| {
                let (t, around) = axis_coords(&part.axis, p)?;
                let s = sample_axis(&part.axis, t)?;
                Some((*i, t, s.centre().distance(p) - s.radius_toward(around.to_radians())))
            })
            .min_by(|a, b| a.2.total_cmp(&b.2))
    };
    let hits: Vec<(usize, f32)> = mesh.positions.iter().filter_map(|p| nearest(*p)).map(|(i, t, _)| (i, t)).collect();
    let mut counts: Vec<(usize, usize)> = Vec::new();
    for (i, _) in &hits {
        match counts.iter_mut().find(|(j, _)| j == i) {
            Some((_, n)) => *n += 1,
            None => counts.push((*i, 1)),
        }
    }
    let (best, _) = counts.into_iter().max_by_key(|(_, n)| *n)?;
    let built = &anatomy.parts.get(best)?.path;
    // The authored path whose copies include this one.
    let mut on = None;
    plan.visit(|path, _| {
        if on.is_none() && path_matches(path, built) {
            on = Some(path.clone());
        }
    });
    let ts = hits.iter().filter(|(i, _)| *i == best).map(|(_, t)| *t);
    let span = ts.fold((1.0f32, 0.0f32), |(lo, hi), t| (lo.min(t), hi.max(t)));
    Some((on.unwrap_or_else(|| PartPath(built.to_string())).to_string(), span))
}

/// The wrap-space point the fit places at `target`, and how far from it that lands.
fn solve_vertex(anatomy: &Anatomy, field: &BodyField, part: u16, g: &Garment, axis: &[crate::anatomy::AxisSample], target: Vec3) -> (Vec3, f32) {
    let wrap = |q: Vec3| Vec3::new(q.z * q.y.sin(), q.x, -q.z * q.y.cos());
    let place = |q: Vec3| garment_point(anatomy, field, part, g, wrap(q));
    let (t0, t1) = g.span;
    // From where the vertex lies along and around the part: `y`, the angle, and `r` from its
    // height over the axis sample's cross-section.
    let mut starts = Vec::new();
    if let Some((t, around)) = axis_coords(axis, target)
        && let Some(s) = sample_axis(axis, t)
    {
        let theta = around.to_radians();
        let y = if (t1 - t0).abs() > 1e-6 { (t - t0) / (t1 - t0) } else { 0.5 };
        let r = 1.0 + (s.centre().distance(target) - s.radius_toward(theta)) - g.ease;
        starts.push(Vec3::new(y, theta, r.max(0.5)));
    }
    // Past an edge of the span: cloth hanging from it, or a cap over the part's end there.
    for (edge, beyond) in [(t0, -1.0), (t1, 1.0)] {
        if let Some(start) = past_edge(axis, g, edge, beyond, target) {
            starts.push(start);
        }
    }
    let mut best = (Vec3::new(0.5, 0.0, 1.0), f32::INFINITY);
    for start in starts {
        let (q, err) = gauss_newton(&place, target, start);
        if err < best.1 {
            best = (q, err);
        }
        if best.1 < LANDED {
            break;
        }
    }
    (wrap(best.0), best.1)
}

/// A first guess for a vertex past the span edge at part `t` = `edge` (`beyond` -1 = the start
/// edge, 1 = the end), from where it lies relative to that edge's cross-section: `None` when it
/// lies on the span's side of it.
fn past_edge(axis: &[crate::anatomy::AxisSample], g: &Garment, edge: f32, beyond: f32, target: Vec3) -> Option<Vec3> {
    let s = sample_axis(axis, edge)?;
    let tip = s.model.rotation * Vec3::Y * beyond;
    let v = target - s.centre();
    let along = v.dot(tip);
    if along <= 0.0 {
        return None;
    }
    let radial = v - tip * along;
    let theta = radial.dot(s.model.rotation * Vec3::X).atan2(radial.dot(s.model.rotation * Vec3::NEG_Z));
    let edge_y = if beyond < 0.0 { 0.0 } else { 1.0 };
    let over = g.hang.map_or_else(
        // Capping: the angle over the end, a quarter turn at the tip.
        || along.atan2(radial.length()) / std::f32::consts::FRAC_PI_2,
        // Hanging: the drop below the edge, in hang lengths.
        |hang| {
            let dir = tip.lerp(Vec3::NEG_Y, hang.droop.clamp(0.0, 1.0)).normalize_or(Vec3::NEG_Y);
            (v.dot(dir) / hang.length.max(1e-3)).max(0.0)
        },
    );
    let r = 1.0 + (v.length() - s.radius_toward(theta)).max(0.0) - g.ease;
    Some(Vec3::new(beyond.mul_add(over, edge_y), theta, r.max(0.5)))
}

/// Damped Gauss-Newton for `place(q) = target` over `q` = (`y`, angle, `r`), from `start`: the
/// best `q` found and how far from `target` it lands.
fn gauss_newton(place: &impl Fn(Vec3) -> Option<Vec3>, target: Vec3, start: Vec3) -> (Vec3, f32) {
    const STEPS: Vec3 = Vec3::new(1e-3, 1e-3, 1e-4);
    let err = |q: Vec3| place(q).map_or(f32::INFINITY, |p| p.distance(target));
    let mut q = start;
    let Some(mut at) = place(q) else { return (q, f32::INFINITY) };
    let mut damping = 1e-4;
    for _ in 0..24 {
        let residual = target - at;
        if residual.length() < LANDED {
            break;
        }
        let column = |axis: usize| -> Option<Vec3> {
            let step = Vec3::AXES.get(axis).copied()? * STEPS;
            Some((place(q + step)? - at) / step.max_element())
        };
        let (Some(dy), Some(dangle), Some(dr)) = (column(0), column(1), column(2)) else { break };
        let jacobian = Mat3::from_cols(dy, dangle, dr);
        let jtj = jacobian.transpose() * jacobian;
        let jtr = jacobian.transpose() * residual;
        let mut improved = false;
        for _ in 0..8 {
            let diagonal = Vec3::new(jtj.x_axis.x, jtj.y_axis.y, jtj.z_axis.z);
            let damped = jtj + Mat3::from_diagonal(Vec3::splat(damping) + diagonal * damping);
            if damped.determinant().abs() < 1e-18 {
                damping *= 10.0;
                continue;
            }
            let next = q + damped.inverse() * jtr;
            if let Some(landed) = place(next)
                && landed.distance(target) < residual.length()
            {
                q = next;
                at = landed;
                damping = (damping * 0.3).max(1e-7);
                improved = true;
                break;
            }
            damping *= 10.0;
        }
        if !improved {
            break;
        }
    }
    (q, err(q))
}

/// `f` over `items` on every core (vertices solve independently).
fn parallel_map<T: Sync, U: Send>(items: &[T], f: impl Fn(&T) -> U + Sync) -> Vec<U> {
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if threads < 2 || items.len() < 64 || cfg!(target_arch = "wasm32") {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        let handles: Vec<_> = items.chunks(chunk).map(|c| s.spawn(|| c.iter().map(&f).collect::<Vec<U>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::{PartAssets, PartLodRules, test_assets};
    use crate::clothing::{Outfit, fit_first_piece, fit_outfit, garment_field};
    use crate::interchange::{BodyExport, KEY, export_body, import_body, read_glb, read_trimesh, write_glb};

    fn load<T: serde::de::DeserializeOwned>(path: &str) -> T {
        ron::from_str(&std::fs::read_to_string(format!("{}/assets/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap()
    }

    /// The plan's meshes and the outfit's garment meshes.
    fn assets(plan: &BodyPlan, outfit: &Outfit) -> PartAssets {
        let mut assets = test_assets(plan);
        for mesh in outfit.referenced_meshes() {
            let (file, label) = mesh.split_once('#').unwrap();
            let bytes = std::fs::read(format!("{}/assets/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            assets.insert(mesh.clone(), std::sync::Arc::new(read_trimesh(&bytes, label).unwrap()));
        }
        assets
    }

    fn export(plan: &BodyPlan, outfit: &Outfit, assets: &PartAssets) -> Vec<u8> {
        let anatomy = Anatomy::from_plan(plan, &PartLodRules::default(), assets).unwrap();
        export_body(&BodyExport { plan, anatomy: &anatomy, assets, coat: None, outfit: Some(outfit), skin: None }).unwrap()
    }

    /// Garments exported with a body and not touched come back as they were.
    #[test]
    fn unchanged_outfit_round_trips() {
        let plan: BodyPlan = load("mobs/biped.body.ron");
        for name in ["peasant", "warrior"] {
            let outfit: Outfit = load(&format!("outfits/{name}.outfit.ron"));
            let assets = assets(&plan, &outfit);
            let back = import_body(&export(&plan, &outfit, &assets), "test.body.glb", &assets).unwrap();
            assert_eq!(back.outfit.as_ref(), Some(&outfit), "{name}");
            assert!(back.meshes.is_empty(), "{name}");
        }
    }

    /// A garment edited in the DCC (here: its fingerprint dropped, so it counts as edited) is
    /// brought back into wrap space: fitted again on the same body it lands where it was
    /// modelled, and it fits another body.
    #[test]
    fn edited_garments_land_where_modelled() {
        let plan: BodyPlan = load("mobs/biped.body.ron");
        let other: BodyPlan = load("mobs/orc.body.ron");
        for name in ["peasant", "warrior"] {
            let outfit: Outfit = load(&format!("outfits/{name}.outfit.ron"));
            let assets = assets(&plan, &outfit);
            let (mut doc, bin) = read_glb(&export(&plan, &outfit, &assets)).map(|(d, b)| (d, b.to_vec())).unwrap();
            for n in doc["nodes"].as_array_mut().unwrap() {
                if let Some(o) = n.pointer_mut(&format!("/extras/{KEY}")).and_then(serde_json::Value::as_object_mut) {
                    o.remove("mesh_print");
                }
            }
            let back = import_body(&write_glb(&doc, &bin).unwrap(), "test.body.glb", &assets).unwrap();
            let brought = back.outfit.unwrap();
            assert_eq!(brought.garments.len(), outfit.garments.len(), "{name}");

            let anatomy = Anatomy::from_plan(&plan, &PartLodRules::default(), &assets).unwrap();
            let field = garment_field(&anatomy);
            let mut with_new = assets.clone();
            for (path, mesh) in &back.meshes {
                with_new.insert(path.clone(), std::sync::Arc::new(mesh.clone()));
            }
            for (was, now) in outfit.garments.iter().zip(&brought.garments) {
                let modelled = fit_first_piece(&anatomy, &field, was, assets.meshes.get(&was.mesh).unwrap()).unwrap();
                let refit = fit_first_piece(&anatomy, &field, now, with_new.meshes.get(&now.mesh).unwrap()).unwrap();
                let mut d: Vec<f32> = modelled.positions.iter().zip(&refit.positions).map(|(a, b)| a.distance(*b)).collect();
                d.sort_by(f32::total_cmp);
                let (median, worst) = (d[d.len() / 2], d[d.len() - 1]);
                println!("{name}/{}: {} vertices, median {:.3} mm, worst {:.3} mm", was.name, d.len(), median * 1e3, worst * 1e3);
                assert!(worst < 1e-3, "{name}/{}: a vertex lands {worst} m off", was.name);
            }

            let mut other_assets = test_assets(&other);
            for (path, mesh) in &back.meshes {
                other_assets.insert(path.clone(), std::sync::Arc::new(mesh.clone()));
            }
            let orc = Anatomy::from_plan(&other, &PartLodRules::default(), &other_assets).unwrap();
            let (_, cloth) = fit_outfit(orc, &brought, &other_assets);
            assert!(cloth.is_some_and(|c| !c.indices.is_empty()), "{name} fits the orc");
        }
    }

    /// A garment modelled from scratch says only what it is: which part it wraps, and over what
    /// stretch, come from where it lies.
    #[test]
    fn new_garment_finds_its_part() {
        let plan: BodyPlan = load("mobs/biped.body.ron");
        let outfit: Outfit = load("outfits/peasant.outfit.ron");
        let assets = assets(&plan, &outfit);
        let (mut doc, bin) = read_glb(&export(&plan, &outfit, &assets)).map(|(d, b)| (d, b.to_vec())).unwrap();
        for n in doc["nodes"].as_array_mut().unwrap() {
            if let Some(o) = n.pointer_mut(&format!("/extras/{KEY}")).and_then(serde_json::Value::as_object_mut)
                && let Some(g) = o.get_mut("garment").and_then(serde_json::Value::as_object_mut)
            {
                g.remove("on");
                g.remove("span");
                g.remove("mesh");
                o.remove("mesh_print");
            }
        }
        let back = import_body(&write_glb(&doc, &bin).unwrap(), "test.body.glb", &assets).unwrap();
        let brought = back.outfit.unwrap();
        for (was, now) in outfit.garments.iter().zip(&brought.garments).filter(|(g, _)| ["tunic", "sleeve", "trousers"].contains(&g.name.as_str())) {
            println!("{}: on {} span {:?} (was {} {:?})", was.name, now.on, now.span, was.on, was.span);
            assert_eq!(now.on, was.on, "{}", was.name);
            assert!((now.span.0 - was.span.0).abs() < 0.05 && (now.span.1 - was.span.1).abs() < 0.05, "{}: {:?} vs {:?}", was.name, now.span, was.span);
        }
    }
}
