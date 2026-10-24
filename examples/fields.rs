//! Field combinators and the sweep SDF, voxelised so any [`Field`] can be seen (the mesher only
//! takes body fields). Each shape's `bounds()` is drawn around it: yellow for the shown field,
//! grey for its inputs.
//!
//! ```text
//! cargo run --release --example fields [mobs/plan.body.ron]
//! ```
//!
//! Front row, sweeps by cross-section: round · ellipse · flat · teardrop.
//! Back row, the body: plain · `Intersect` with a ball · `Displace` by noise · `RegionClip` to the
//! torso.
//!
//! Controls: left-drag orbit · scroll zoom.
//! `ORNISCULPT_SCREENSHOT=out.png` saves a screenshot after a few frames and exits;
//! `ORNISCULPT_CAM=yaw,pitch,distance,x,y,z` sets the camera.

use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::math::Vec3A;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::shape::Aabb3d;
use ornisculpt::anatomy::{
    Anatomy, BodyPlan, CrossSection, LodLevel, PartAssets, PartLodRules, PrimLayer, RegionId, RegionKind, RegionSelector,
};
use ornisculpt::num::Saturate;
use ornisculpt::random::{Noise, NoiseSettings};
use ornisculpt::surface::{BodyField, BodyFieldOptions, Displace, Field, Influences, Intersect, RegionClip, SurfaceInfo, sdf};

mod common;
use common::{Orbit, build_plan, default_plugins, orbit_camera, screenshot_at, spawn_stage};

fn main() {
    App::new()
        .add_plugins(default_plugins(Window { title: "ornisculpt fields".into(), ..default() }))
        .insert_resource(Orbit::from_env(0.0, 0.3, 6.5, Vec3::new(0.0, 0.6, 0.6), (0.3, 30.0)))
        .add_systems(Startup, setup)
        .add_systems(Update, (orbit_camera, draw_bounds, screenshot_at(60)))
        .run();
}

/// A swept tube, straight from [`sdf::sweep`].
struct Sweep {
    points: Vec<Vec3A>,
    radii: Vec<f32>,
    cross: CrossSection,
}

impl Field for Sweep {
    fn bounds(&self) -> Aabb3d {
        let r = self.radii.iter().copied().fold(0.0, f32::max) * 2.0;
        let (min, max) = self.points.iter().fold((Vec3A::INFINITY, Vec3A::NEG_INFINITY), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        Aabb3d { min: min - r, max: max + r }
    }

    fn distance(&self, p: Vec3A) -> f32 {
        sdf::sweep(p, &self.points, &self.radii, self.cross)
    }

    fn surface_info(&self, _: Vec3A) -> SurfaceInfo {
        SurfaceInfo { influences: Influences::default(), region: RegionId(0), layer: PrimLayer::Base }
    }
}

struct Ball {
    centre: Vec3A,
    radius: f32,
}

impl Field for Ball {
    fn bounds(&self) -> Aabb3d {
        Aabb3d { min: self.centre - self.radius, max: self.centre + self.radius }
    }

    fn distance(&self, p: Vec3A) -> f32 {
        p.distance(self.centre) - self.radius
    }

    fn surface_info(&self, _: Vec3A) -> SurfaceInfo {
        SurfaceInfo { influences: Influences::default(), region: RegionId(0), layer: PrimLayer::Base }
    }
}

/// Voxels of a field: its own size, where it sits, and the bounds to draw.
struct Shown {
    name: &'static str,
    mesh: Mesh,
    at: Vec3,
    colour: Color,
    bounds: Aabb3d,
    inputs: Vec<Aabb3d>,
}

/// Bounds drawn every frame, offset to where each shape sits.
#[derive(Resource, Default)]
struct Bounds(Vec<(Vec3, Aabb3d, Color)>);

/// Faces of a voxel, as (normal, u, v) with `u × v = normal` so quads wind outward.
const FACES: [(IVec3, Vec3, Vec3); 6] = [
    (IVec3::X, Vec3::Y, Vec3::Z),
    (IVec3::NEG_X, Vec3::Z, Vec3::Y),
    (IVec3::Y, Vec3::Z, Vec3::X),
    (IVec3::NEG_Y, Vec3::X, Vec3::Z),
    (IVec3::Z, Vec3::X, Vec3::Y),
    (IVec3::NEG_Z, Vec3::Y, Vec3::X),
];

/// Blocky mesh of the cells of `field` whose centres are inside (distance < 0); only faces
/// against outside cells are kept.
fn voxel_mesh(field: &impl Field, cell: f32) -> Mesh {
    let bounds = field.bounds();
    let min = Vec3::from(bounds.min) - cell;
    let span = (Vec3::from(bounds.max) + cell - min) / cell;
    let [nx, ny, nz] = span.to_array().map(|s| s.ceil().sat_i32().max(1));
    let centre = |c: IVec3| min + (c.as_vec3() + 0.5) * cell;
    let index = |c: IVec3| -> Option<usize> {
        let inside = c.cmpge(IVec3::ZERO).all() && c.cmplt(IVec3::new(nx, ny, nz)).all();
        inside.then(|| usize::try_from((c.z * ny + c.y) * nx + c.x).ok()).flatten()
    };
    let cells = (0..nz).flat_map(|z| (0..ny).flat_map(move |y| (0..nx).map(move |x| IVec3::new(x, y, z))));
    let solid: Vec<bool> = cells.clone().map(|c| field.distance(centre(c).into()) < 0.0).collect();
    let is_solid = |c: IVec3| index(c).and_then(|i| solid.get(i)).copied().unwrap_or(false);

    let (mut positions, mut normals, mut indices) = (Vec::<Vec3>::new(), Vec::<Vec3>::new(), Vec::<u32>::new());
    for c in cells.filter(|c| is_solid(*c)) {
        for (dir, u, v) in FACES {
            if is_solid(c + dir) {
                continue;
            }
            let Ok(base) = u32::try_from(positions.len()) else { break };
            let n = dir.as_vec3();
            let half = cell * 0.5;
            for corner in [n - u - v, n + u - v, n + u + v, n - u + v] {
                positions.push(centre(c) + corner * half);
                normals.push(n);
            }
            indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_indices(Indices::U32(indices))
}

fn load_anatomy(path: &str) -> Result<Anatomy, String> {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join(path)).map_err(|e| format!("{path}: {e}"))?;
    let plan: BodyPlan = ron::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    build_plan(&plan, PartLodRules::default(), &PartAssets::default())
}

fn sweeps() -> Vec<Shown> {
    let points = vec![Vec3A::new(0.0, 0.0, 0.0), Vec3A::new(0.0, 0.25, 0.06), Vec3A::new(0.0, 0.5, 0.0), Vec3A::new(0.06, 0.7, -0.06)];
    let radii = vec![0.09, 0.07, 0.055, 0.02];
    let crosses = [
        ("sweep round", CrossSection::Round),
        ("sweep ellipse 0.5", CrossSection::Ellipse { aspect: 0.5 }),
        ("sweep flat 0.2", CrossSection::Flat { aspect: 0.2 }),
        ("sweep teardrop 1.6", CrossSection::Teardrop { aspect: 1.6 }),
    ];
    std::thread::scope(|s| {
        // Collected so every thread starts before the first join.
        #[allow(clippy::needless_collect)]
        let jobs: Vec<_> = crosses
            .into_iter()
            .zip([-1.8, -0.6, 0.6, 1.8])
            .map(|((name, cross), x)| {
                let field = Sweep { points: points.clone(), radii: radii.clone(), cross };
                s.spawn(move || Shown {
                    name,
                    mesh: voxel_mesh(&field, 0.006),
                    at: Vec3::new(x, 0.0, 1.2),
                    colour: Color::srgb(0.75, 0.55, 0.35),
                    bounds: field.bounds(),
                    inputs: Vec::new(),
                })
            })
            .collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    })
}

fn bodies(anatomy: &Anatomy) -> Vec<Shown> {
    let opts = BodyFieldOptions { level: LodLevel::Hero, seed: 1, geometric_folds: false, spread: false };
    let field = || BodyField::new(anatomy, &opts);
    let plain = field();
    let body_bounds = plain.bounds();
    let (lo, hi) = (body_bounds.min, body_bounds.max);
    let ball = Ball { centre: lo.lerp(hi, 0.5).with_y(lo.y.lerp(hi.y, 0.72)), radius: 0.3 };
    let ball_bounds = ball.bounds();
    let noise = Noise::new(&NoiseSettings { frequency: 9.0, amplitude: 0.025, ..default() }, 7, "fields");
    let torso = RegionSelector::Or([RegionKind::TorsoFront, RegionKind::TorsoMid, RegionKind::TorsoRear, RegionKind::Belly].map(RegionSelector::Kind).to_vec())
        .mask(anatomy);
    let cell = 0.012;
    let skin = Color::srgb(0.8, 0.6, 0.5);
    let at = |x: f32| Vec3::new(x, 0.0, 0.0);
    std::thread::scope(|s| {
        let jobs = [
            s.spawn(|| Shown { name: "body", bounds: plain.bounds(), mesh: voxel_mesh(&plain, cell), at: at(-1.8), colour: skin, inputs: Vec::new() }),
            s.spawn(|| {
                let f = Intersect { a: field(), b: ball };
                Shown { name: "intersect", bounds: f.bounds(), mesh: voxel_mesh(&f, cell), at: at(-0.6), colour: skin, inputs: vec![body_bounds, ball_bounds] }
            }),
            s.spawn(|| {
                let f = Displace { inner: field(), noise };
                Shown { name: "displace", bounds: f.bounds(), mesh: voxel_mesh(&f, cell), at: at(0.6), colour: skin, inputs: vec![body_bounds] }
            }),
            s.spawn(|| {
                let f = RegionClip { inner: field(), mask: torso };
                Shown { name: "region clip", bounds: f.bounds(), mesh: voxel_mesh(&f, cell), at: at(1.8), colour: Color::srgb(0.35, 0.45, 0.7), inputs: Vec::new() }
            }),
        ];
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    })
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<StandardMaterial>>, mut exit: MessageWriter<AppExit>) {
    let plan = std::env::args().nth(1).unwrap_or_else(|| "mobs/biped.body.ron".into());
    let anatomy = match load_anatomy(&plan) {
        Ok(a) => a,
        Err(e) => {
            error!("{e}");
            exit.write(AppExit::error());
            return;
        }
    };
    let started = std::time::Instant::now();
    let mut shown = sweeps();
    shown.extend(bodies(&anatomy));
    info!("voxelised {} fields in {:.2}s", shown.len(), started.elapsed().as_secs_f32());

    let mut bounds = Bounds::default();
    for s in shown {
        let tris = s.mesh.indices().map_or(0, |i| i.len() / 3);
        let b = s.bounds;
        info!("{:<20} bounds {:.2?}..{:.2?}, {tris} triangles", s.name, Vec3::from(b.min), Vec3::from(b.max));
        commands.spawn((
            Mesh3d(meshes.add(s.mesh)),
            MeshMaterial3d(materials.add(StandardMaterial { base_color: s.colour, perceptual_roughness: 0.8, ..default() })),
            Transform::from_translation(s.at),
        ));
        bounds.0.push((s.at, s.bounds, Color::srgb(1.0, 0.85, 0.2)));
        bounds.0.extend(s.inputs.into_iter().map(|b| (s.at, b, Color::srgb(0.55, 0.55, 0.55))));
    }
    commands.insert_resource(bounds);

    commands.spawn(Camera3d::default());
    spawn_stage(&mut commands, &mut meshes, &mut materials, 20.0, Vec3::new(3.0, 6.0, 4.0));
}

fn draw_bounds(bounds: Option<Res<Bounds>>, mut gizmos: Gizmos) {
    let Some(bounds) = bounds else { return };
    for (at, b, colour) in &bounds.0 {
        let (min, max) = (Vec3::from(b.min), Vec3::from(b.max));
        gizmos.cube(Transform::from_translation(at + (min + max) * 0.5).with_scale(max - min), *colour);
    }
}
