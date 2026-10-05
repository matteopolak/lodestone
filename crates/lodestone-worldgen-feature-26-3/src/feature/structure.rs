//! `template` and `fossil`: stamp bundled structure templates into the world.
//!
//! The template feature picks a weighted template and a rotation, centres it on the origin and
//! places it. The fossil feature digs far below the surface: it picks a rotation and one of
//! several skeleton/overlay pairs, drops the pair to just under the lowest surface cell of its
//! footprint, refuses spots whose box corners are mostly open space, and places the skeleton and
//! then its ore overlay under the feature random's block-rot processors.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::Dir;
use crate::env::{Env, Heightmap};
use crate::json::{Res, array, get, int, int_or, string};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::template::{Processors, Rotation, Settings, Template, place_in_world};

fn rotation_named(name: &str) -> Option<Rotation> {
    Some(match name.strip_prefix("minecraft:").unwrap_or(name) {
        "none" => Rotation::None,
        "clockwise_90" => Rotation::Cw90,
        "180" | "clockwise_180" => Rotation::Cw180,
        "counterclockwise_90" => Rotation::Ccw90,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
struct Entry {
    template: Arc<Template>,
    rotations: Vec<Rotation>,
    weight: i32,
}

#[derive(Clone, Debug)]
pub struct TemplateConfig {
    entries: Vec<Entry>,
    processors: Processors,
}

impl TemplateConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let mut entries = Vec::new();
        for e in array(v, "templates", ctx)? {
            let data = get(e, "data", ctx)?;
            let rotations = match data.get("rotations") {
                Some(r) => r
                    .as_array()
                    .ok_or_else(|| format!("{ctx}: rotations"))?
                    .iter()
                    .map(|n| n.as_str().and_then(rotation_named).ok_or_else(|| format!("{ctx}: rotation")))
                    .collect::<Res<Vec<_>>>()?,
                None => Rotation::ALL.to_vec(),
            };
            entries.push(Entry {
                template: Arc::new(Template::load(env, string(data, "id", ctx)?)?),
                rotations,
                weight: int_or(e, "weight", 1, ctx)?,
            });
        }
        let processors = match v.get("processors") {
            Some(p) => Processors::parse(env, p, ctx)?,
            None => Processors::default(),
        };
        Ok(Self { entries, processors })
    }

    pub fn unsupported(&self) -> &[String] {
        &self.processors.unsupported
    }
}

pub fn place_template(cfg: &TemplateConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !cfg.processors.unsupported.is_empty() {
        return false;
    }
    let total: i32 = cfg.entries.iter().map(|e| e.weight).sum();
    let mut pick = rng.next_int_bounded(total);
    let entry = cfg
        .entries
        .iter()
        .find(|e| {
            pick -= e.weight;
            pick < 0
        })
        .expect("a weighted pick lands on an entry");
    let rotation = entry.rotations[rng.next_int_bounded(entry.rotations.len() as i32) as usize];
    let (sx, _, sz) = entry.template.raw_size();
    let (wx, _, wz) = rotation.rotate_dir(Dir::West).step();
    let (nx, _, nz) = rotation.rotate_dir(Dir::North).step();
    let pos = origin.offset(wx * (sx / 2) + nx * (sz / 2), 0, wz * (sx / 2) + nz * (sz / 2));
    place_in_world(level, rng, &entry.template, pos, &cfg.processors, Settings { rotation, bounds: None })
}

#[derive(Clone, Debug)]
pub struct FossilConfig {
    fossils: Vec<Arc<Template>>,
    overlays: Vec<Arc<Template>>,
    fossil_processors: Processors,
    overlay_processors: Processors,
    max_empty_corners: i32,
}

impl FossilConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let load = |key: &str| -> Res<Vec<Arc<Template>>> {
            array(v, key, ctx)?
                .iter()
                .map(|n| Template::load(env, n.as_str().unwrap_or_default()).map(Arc::new))
                .collect()
        };
        Ok(Self {
            fossils: load("fossil_structures")?,
            overlays: load("overlay_structures")?,
            fossil_processors: Processors::parse(env, get(v, "fossil_processors", ctx)?, ctx)?,
            overlay_processors: Processors::parse(env, get(v, "overlay_processors", ctx)?, ctx)?,
            max_empty_corners: int(v, "max_empty_corners_allowed", ctx)?,
        })
    }

    pub fn unsupported(&self) -> Vec<String> {
        self.fossil_processors.unsupported.iter().chain(&self.overlay_processors.unsupported).cloned().collect()
    }
}

pub fn place_fossil(cfg: &FossilConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !cfg.unsupported().is_empty() {
        return false;
    }
    let rotation = Rotation::ALL[rng.next_int_bounded(4) as usize];
    let index = rng.next_int_bounded(cfg.fossils.len() as i32) as usize;
    let (base, overlay) = (&cfg.fossils[index], &cfg.overlays[index]);
    let (cx, cz) = (origin.x >> 4, origin.z >> 4);
    let (lo, hi) = (Pos::new(cx * 16 - 16, level.min_y, cz * 16 - 16), Pos::new(cx * 16 + 15 + 16, level.max_y(), cz * 16 + 15 + 16));
    let settings = Settings { rotation, bounds: Some((lo, hi)) };
    let (sx, _, sz) = base.size(rotation);
    let low_corner = origin.offset(-sx / 2, 0, -sz / 2);
    let mut lowest = origin.y;
    for xs in 0..sx {
        for zs in 0..sz {
            lowest = lowest.min(level.height(Heightmap::OceanFloorWg, low_corner.x + xs, low_corner.z + zs));
        }
    }
    let target_y = (lowest - 15 - rng.next_int_bounded(10)).max(level.min_y + 10);
    let target = base.zero_position_with_transform(low_corner.at_y(target_y), rotation);
    let blocks = &level.env.blocks;
    let water = blocks.block_by_name("water");
    let lava = blocks.block_by_name("lava");
    let empty_corners = base
        .corners(target, rotation)
        .into_iter()
        .filter(|p| {
            let s = level.get(p.x, p.y, p.z);
            blocks.is_air(s) || Some(blocks.block_of(s)) == lava || Some(blocks.block_of(s)) == water
        })
        .count() as i32;
    if empty_corners > cfg.max_empty_corners {
        return false;
    }
    place_in_world(level, rng, base, target, &cfg.fossil_processors, settings);
    place_in_world(level, rng, overlay, target, &cfg.overlay_processors, settings);
    true
}
