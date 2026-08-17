//! D1 measurement: what fraction of real baked block states are full opaque
//! cubes, and does that justify a separate packed-vertex fast path?
//!
//! This bakes every vanilla block state from a fetched `client.jar` (+ Mojang's
//! `generated/reports/blocks.json`) and classifies each with
//! [`lodestone_render::is_full_cube`] / [`is_packed_cube`] — the predicates
//! `lodestone-render`'s own packed/model split is designed around. **Not**,
//! today, the predicates `lodestone-shell`'s live path actually routes
//! through: `crates/lodestone-shell/src/mesher.rs`'s `mesh_one` sends every
//! live (`ShellClassifier::Vanilla`) block through the wide per-quad model
//! path regardless of cube-ness, so `is_packed_cube` currently has no
//! production caller there — see `crates/lodestone-render/src/models.rs`'s
//! module doc for the verified wiring status. This census still measures a
//! real fact about the baked model set (see below), just not (yet) a fact
//! about what the live game draws. It is `#[ignore]`d so the default test run
//! stays hermetic; run it with `--ignored`.
//!
//! It reads only from `.cache/mc/<version>/`; it never writes assets and never
//! touches `lodestone-assets`/`lodestone-world`, which other agents own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lodestone_assets::tint::{TintKind, vanilla_tint_kind};
use lodestone_assets::{
    Atlas, AtlasBuilder, BakedQuad, BlockBaker, BlockStates, DisplayTransform, FirstWeight,
    GuiItemContext, GuiLight, IconPart, ItemIconBuilder, ModelResolver, ModelTransform,
    ResourceLocation, ResourceManager, TextureBinding, ZipSource, bake_model,
};
use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};
use lodestone_render::{RenderLayer, is_full_cube, is_packed_cube};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// Test-support registry from Mojang's data-generator `blocks.json`, mirroring
/// the harness `lodestone-assets` uses. Read-only; never written.
#[derive(Debug)]
struct BlocksReport {
    entries: Vec<Option<(Identifier, BTreeMap<String, String>)>>,
}

impl BlocksReport {
    fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let root: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let obj = root.as_object()?;
        let mut states = Vec::new();
        let mut max_id = 0u32;
        for (name, block) in obj {
            let id: Identifier = name.parse().ok()?;
            let Some(arr) = block.get("states").and_then(|s| s.as_array()) else {
                continue;
            };
            for state in arr {
                let sid = state.get("id").and_then(serde_json::Value::as_u64)? as u32;
                let mut props = BTreeMap::new();
                if let Some(p) = state.get("properties").and_then(|p| p.as_object()) {
                    for (k, v) in p {
                        if let Some(v) = v.as_str() {
                            props.insert(k.clone(), v.to_string());
                        }
                    }
                }
                max_id = max_id.max(sid);
                states.push((sid, id.clone(), props));
            }
        }
        let mut entries = vec![None; max_id as usize + 1];
        for (sid, id, props) in states {
            entries[sid as usize] = Some((id, props));
        }
        Some(Self { entries })
    }
}

impl BlockStateRegistry for BlocksReport {
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
        let (block, properties) = self.entries.get(id as usize)?.as_ref()?;
        Some(ResolvedBlockState { block, properties })
    }
    fn state_count(&self) -> u32 {
        self.entries.len() as u32
    }
}

/// The stitched atlas of every texture reachable from a blockstate, plus the
/// textures of `extra_models` (used to measure item-model coverage with and
/// without the item seeding `BlockModels::build` performs).
fn full_block_atlas(
    manager: &ResourceManager,
    resolver: &ModelResolver,
    extra_models: &[ResourceLocation],
) -> Atlas {
    let mut textures: BTreeSet<ResourceLocation> = BTreeSet::new();
    for path in manager.list("assets/minecraft/blockstates/") {
        let Some(bytes) = manager.read(&path) else {
            continue;
        };
        let Ok(bs) = BlockStates::parse(&bytes) else {
            continue;
        };
        for r in bs.model_refs() {
            if let Ok(model) = resolver.resolve(&r.model) {
                for binding in model.textures.values() {
                    if let TextureBinding::Resolved(loc) = binding {
                        textures.insert(loc.clone());
                    }
                }
            }
        }
    }
    for model in extra_models {
        if let Ok(resolved) = resolver.resolve(model) {
            for binding in resolved.textures.values() {
                if let TextureBinding::Resolved(loc) = binding {
                    textures.insert(loc.clone());
                }
            }
        }
    }
    let mut builder = AtlasBuilder::new();
    for loc in &textures {
        let _ = builder.load(manager, loc);
    }
    builder.build().expect("build atlas")
}

/// One item whose inventory icon is a 3-D model, as `BlockModels::build`
/// discovers it.
struct ItemModelPart {
    item: ResourceLocation,
    model: ResourceLocation,
    transform: DisplayTransform,
    gui_light: GuiLight,
}

/// Every `assets/<ns>/items/<id>.json` in the pack, sorted.
fn item_ids(manager: &ResourceManager) -> Vec<ResourceLocation> {
    let mut ids = BTreeSet::new();
    for path in manager.list("assets/") {
        let Some(rest) = path.strip_prefix("assets/") else {
            continue;
        };
        let Some((namespace, tail)) = rest.split_once('/') else {
            continue;
        };
        let Some(item_path) = tail
            .strip_prefix("items/")
            .and_then(|p| p.strip_suffix(".json"))
        else {
            continue;
        };
        if let Ok(loc) = ResourceLocation::parse(&format!("{namespace}:{item_path}")) {
            ids.insert(loc);
        }
    }
    ids.into_iter().collect()
}

/// The `IconPart::Model` of every item that has one, resolved under
/// [`GuiItemContext`] exactly as `BlockModels::build` does.
fn item_model_parts(manager: &ResourceManager) -> (Vec<ItemModelPart>, usize) {
    let builder = ItemIconBuilder::new(manager);
    let mut parts = Vec::new();
    let mut items = 0usize;
    for id in item_ids(manager) {
        items += 1;
        let Ok(icon) = builder.icon_with(&id, &GuiItemContext) else {
            continue;
        };
        for part in &icon.parts {
            if let IconPart::Model {
                model,
                transform,
                gui_light,
                ..
            } = part
            {
                parts.push(ItemModelPart {
                    item: id.clone(),
                    model: model.clone(),
                    transform: *transform,
                    gui_light: *gui_light,
                });
            }
        }
    }
    (parts, items)
}

/// A quad's `RenderLayer`, sampled directly from the stitched atlas's real
/// per-texel alpha over the quad's UV footprint — the same rule
/// `crate::translucency::RenderLayer::from_sprite_alpha` codifies and
/// `lodestone-render`'s own `block_layer` uses to classify a baked state, kept
/// independent here rather than imported so this census measures the *data*,
/// not a shared helper that could itself be wrong.
fn quad_layer(atlas: &Atlas, quad: &BakedQuad) -> RenderLayer {
    let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for uv in &quad.uvs {
        umin = umin.min(uv[0]);
        umax = umax.max(uv[0]);
        vmin = vmin.min(uv[1]);
        vmax = vmax.max(uv[1]);
    }
    let x0 = (umin * atlas.width as f32).floor().max(0.0) as u32;
    let x1 = (umax * atlas.width as f32).ceil().min(atlas.width as f32) as u32;
    let y0 = (vmin * atlas.height as f32).floor().max(0.0) as u32;
    let y1 = (vmax * atlas.height as f32).ceil().min(atlas.height as f32) as u32;
    let mut alphas = Vec::new();
    for y in y0..y1.max(y0 + 1).min(atlas.height) {
        for x in x0..x1.max(x0 + 1).min(atlas.width) {
            let idx = ((y * atlas.width + x) * 4 + 3) as usize;
            if let Some(&a) = atlas.rgba.get(idx) {
                alphas.push(a);
            }
        }
    }
    RenderLayer::from_sprite_alpha(&alphas)
}

/// A baked state's `RenderLayer`: the most transparent layer across its quads
/// (`Solid < Cutout < Translucent`), matching `block_layer`'s "any translucent
/// face drags the whole block" rule.
fn state_layer(atlas: &Atlas, quads: &[BakedQuad]) -> RenderLayer {
    quads
        .iter()
        .map(|q| quad_layer(atlas, q))
        .max()
        .unwrap_or(RenderLayer::Solid)
}

#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn full_cube_fraction_over_all_baked_states() {
    // Fail closed: #[ignore]d means running this is an explicit opt-in, so a
    // missing jar/registry is an environment failure, not a silent skip.
    let jar = require_client_jar();
    let report_path = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver, &[]);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let report = BlocksReport::load(&report_path).expect("load blocks.json");

    let total = report.state_count();
    let mut baked_ok = 0usize;
    let mut empty = 0usize; // air / fluid / block-entity-only: zero quads
    let mut full_cube = 0usize; // geometry is a full cube (may be tinted)
    let mut packed_cube = 0usize; // full cube AND untinted -> packed fast path
    let mut tinted_cube = 0usize; // full cube but tinted -> wide path
    let mut noncube = 0usize; // renderable but not a full cube

    // Per-block roll-up: is every state of this block a packed cube?
    let mut block_states: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // (packed, total)

    for id in 0..total {
        let Some(resolved) = report.resolve(id) else {
            continue;
        };
        let Ok(model) = baker.bake_state(&report, id, &FirstWeight) else {
            continue;
        };
        baked_ok += 1;
        let name = resolved.block.to_string();
        let entry = block_states.entry(name).or_insert((0, 0));
        entry.1 += 1;
        if model.quads.is_empty() {
            empty += 1;
            continue;
        }
        let layer = state_layer(&atlas, &model.quads);
        if is_packed_cube(&model.quads, layer) {
            packed_cube += 1;
            full_cube += 1;
            entry.0 += 1;
        } else if is_full_cube(&model.quads) {
            // Full-cube geometry that still fails `is_packed_cube`: tinted
            // (grass, leaves) or a real non-`Solid` layer (stained glass, ice,
            // tinted glass, slime, honey) — both must take the wide path.
            full_cube += 1;
            tinted_cube += 1;
        } else {
            noncube += 1;
        }
    }

    let renderable = baked_ok - empty;
    let blocks_all_packed = block_states
        .values()
        .filter(|(p, t)| *t > 0 && p == t)
        .count();
    let blocks_total = block_states.len();

    eprintln!("=== D1 full-cube census (real vanilla bake) ===");
    eprintln!("states in registry:        {total}");
    eprintln!("baked ok:                  {baked_ok}");
    eprintln!("  empty (air/fluid/etc):   {empty}");
    eprintln!("  renderable:              {renderable}");
    eprintln!(
        "    full-cube geometry:    {full_cube}  ({:.1}% of renderable)",
        100.0 * full_cube as f64 / renderable as f64
    );
    eprintln!(
        "      packed (untinted, Solid layer): {packed_cube}  ({:.1}% of renderable)",
        100.0 * packed_cube as f64 / renderable as f64
    );
    eprintln!("      tinted cube:         {tinted_cube}  (wide path)");
    eprintln!(
        "    non-cube:              {noncube}  ({:.1}% of renderable)",
        100.0 * noncube as f64 / renderable as f64
    );
    eprintln!("distinct blocks:           {blocks_total}");
    eprintln!(
        "  all-states-packed-cube:  {blocks_all_packed}  ({:.1}% of blocks)",
        100.0 * blocks_all_packed as f64 / blocks_total as f64
    );

    // Sanity: stone-family basics must classify as packed cubes.
    assert!(
        packed_cube > 0,
        "no packed cubes found — predicate is broken"
    );
    assert!(
        full_cube >= packed_cube,
        "tinted-cube accounting inconsistent"
    );
}

/// Item-model coverage: of the items whose inventory icon is a **3-D model**,
/// how many bake cleanly against the block atlas — and which do not, by name.
///
/// This is the cheap guard against a silent regression in the GUI item path.
/// Two atlases are measured so the report separates "the block atlas already
/// covers this" from "the item seeding in `build_complete_atlas` is what covers
/// it": a texture that only the item seeding reaches is exactly the class a
/// resource pack will reintroduce, and a `BakeError::SpriteMissing` for it must
/// be *counted*, never fatal.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn item_model_coverage() {
    let jar = require_client_jar();
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let resolver = ModelResolver::new(&manager);

    let (parts, items) = item_model_parts(&manager);
    let models: Vec<ResourceLocation> = parts.iter().map(|p| p.model.clone()).collect();

    // Atlas A: blockstate coverage only, i.e. what the atlas held before this
    // work. Atlas B: plus every item model's textures.
    let blockstate_only = full_block_atlas(&manager, &resolver, &[]);
    let with_items = full_block_atlas(&manager, &resolver, &models);

    let bake_all = |atlas: &Atlas| -> (usize, Vec<String>) {
        let mut ok = 0usize;
        let mut misses = Vec::new();
        for part in &parts {
            match resolver
                .resolve(&part.model)
                .map_err(|e| e.to_string())
                .and_then(|m| {
                    bake_model(&m, atlas, ModelTransform::default()).map_err(|e| e.to_string())
                }) {
                Ok(_) => ok += 1,
                Err(e) => misses.push(format!("{} ({}): {e}", part.item, part.model)),
            }
        }
        (ok, misses)
    };

    let (ok_before, misses_before) = bake_all(&blockstate_only);
    let (ok_after, misses_after) = bake_all(&with_items);

    // Tint census: how many item models carry a raw `tintindex`, and how many of
    // those resolve to a live tint kind. Carrying an index is not the same as
    // being tinted — `vanilla_tint_kind` returns `None` for e.g. `cherry_leaves`.
    let no_props = BTreeMap::new();
    let mut carry_tint = 0usize;
    let mut live_tint = 0usize;
    let mut live_names: Vec<String> = Vec::new();
    let mut dead_names: Vec<String> = Vec::new();
    let mut gui_side = 0usize;
    let mut gui_front = 0usize;
    let mut identity_pose = 0usize;
    for part in &parts {
        match part.gui_light {
            GuiLight::Side => gui_side += 1,
            GuiLight::Front => gui_front += 1,
        }
        if part.transform == DisplayTransform::default() {
            identity_pose += 1;
        }
        let Ok(resolved) = resolver.resolve(&part.model) else {
            continue;
        };
        let Ok(quads) = bake_model(&resolved, &with_items, ModelTransform::default()) else {
            continue;
        };
        let tints: Vec<i32> = quads.iter().filter_map(|q| q.tint_index).collect();
        if tints.is_empty() {
            continue;
        }
        carry_tint += 1;
        let block: Identifier = match part.item.to_string().parse() {
            Ok(b) => b,
            Err(_) => continue,
        };
        if tints
            .iter()
            .any(|&raw| vanilla_tint_kind(&block, raw, &no_props) != TintKind::None)
        {
            live_tint += 1;
            live_names.push(part.item.to_string());
        } else {
            dead_names.push(part.item.to_string());
        }
    }

    // `parts` counts icon *parts*; a `composite` icon can contribute several for
    // one item. `BlockModels` is keyed by item, so both numbers matter.
    let distinct_items: BTreeSet<String> = parts.iter().map(|p| p.item.to_string()).collect();
    let composites = parts.len() - distinct_items.len();
    let mut per_item: BTreeMap<String, usize> = BTreeMap::new();
    for p in &parts {
        *per_item.entry(p.item.to_string()).or_default() += 1;
    }
    let multi: Vec<(&String, &usize)> = per_item.iter().filter(|(_, n)| **n > 1).collect();

    eprintln!("=== item-model GUI geometry census (real vanilla bake) ===");
    eprintln!("item definitions:          {items}");
    eprintln!("  with a Model icon part:  {}", distinct_items.len());
    eprintln!("  total Model parts:       {}", parts.len());
    eprintln!("    extra composite parts: {composites}  {multi:?}");
    eprintln!("baked against the blockstate-only atlas:");
    eprintln!("  ok:                      {ok_before}");
    eprintln!("  missing sprite:          {}", misses_before.len());
    for m in &misses_before {
        eprintln!("    - {m}");
    }
    eprintln!("baked against the atlas WITH item textures seeded:");
    eprintln!("  ok:                      {ok_after}");
    eprintln!("  missing sprite:          {}", misses_after.len());
    for m in &misses_after {
        eprintln!("    - {m}");
    }
    eprintln!("gui_light:                 side {gui_side}, front {gui_front}");
    eprintln!("identity display.gui pose: {identity_pose}");
    eprintln!("tintindex carried:         {carry_tint}");
    eprintln!("  live (vanilla_tint_kind):{live_tint}  {live_names:?}");
    eprintln!(
        "  index but no tint:       {}  {dead_names:?}",
        dead_names.len()
    );

    assert!(
        distinct_items.len() > 700,
        "expected ~752 model items in 26.2, found {}",
        distinct_items.len()
    );
    // The seeding must be a strict improvement, and must leave nothing behind:
    // every item model has to reach a real sprite once its textures are stitched.
    assert!(
        ok_after >= ok_before,
        "seeding item textures must not lose coverage ({ok_after} < {ok_before})"
    );
    assert!(
        misses_after.is_empty(),
        "every item model must bake once its textures are seeded; missing: {misses_after:?}"
    );
}

/// **Vegetation bakes real geometry** — the located suspect from issue #478,
/// tested rather than assumed.
///
/// `mesh_models` skips any cell whose `quads_at` is empty
/// (`lodestone-render/src/models.rs`), and `BlockModels::quads` falls back to an
/// empty model for an id it has no entry for, so a coverage gap in plants would
/// be a **silent** skip: grass and flowers would simply not draw, with nothing
/// anywhere reporting a problem. That is the same shape as the bug it was
/// proposed to explain, which is exactly why it needed a measurement instead of
/// an argument.
///
/// The whole-registry census above already reports 32366 of 32366 states baking
/// with 1377 empty, but that aggregate cannot say *which* states are the empty
/// ones — a plant among them would be invisible inside a 4% bucket that is
/// legitimately full of air, fluids and block-entity-only blocks. This names the
/// population instead, so a regression points at a block rather than at a
/// fraction.
///
/// Cross-shaped plants and cutout leaves are precisely the blocks whose geometry
/// is *not* a full cube, so they exercise the non-cube path rather than the
/// packed fast path.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn vegetation_states_bake_non_empty_geometry() {
    let jar = require_client_jar();
    let report_path = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver, &[]);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let report = BlocksReport::load(&report_path).expect("load blocks.json");

    // The blocks #478's worldgen sweep reported placing, plus the leaf and log
    // species a tree is actually built from.
    let wanted: BTreeSet<&str> = [
        "minecraft:short_grass",
        "minecraft:tall_grass",
        "minecraft:fern",
        "minecraft:large_fern",
        "minecraft:dandelion",
        "minecraft:poppy",
        "minecraft:oak_sapling",
        "minecraft:birch_sapling",
        "minecraft:oak_leaves",
        "minecraft:birch_leaves",
        "minecraft:spruce_leaves",
        "minecraft:acacia_leaves",
        "minecraft:jungle_leaves",
        "minecraft:oak_log",
        "minecraft:birch_log",
        "minecraft:acacia_log",
        "minecraft:dead_bush",
        "minecraft:sugar_cane",
        "minecraft:cactus",
    ]
    .into_iter()
    .collect();

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut empty_states: Vec<(String, u32)> = Vec::new();
    let mut bake_failures: Vec<(String, u32)> = Vec::new();
    let mut checked = 0usize;

    for id in 0..report.state_count() {
        let Some(resolved) = report.resolve(id) else {
            continue;
        };
        let name = resolved.block.to_string();
        if !wanted.contains(name.as_str()) {
            continue;
        }
        seen.insert(name.clone());
        checked += 1;
        match baker.bake_state(&report, id, &FirstWeight) {
            Err(_) => bake_failures.push((name, id)),
            Ok(model) if model.quads.is_empty() => empty_states.push((name, id)),
            Ok(_) => {}
        }
    }

    eprintln!("=== #478 vegetation model coverage ===");
    eprintln!("blocks requested: {}", wanted.len());
    eprintln!("blocks found:     {}", seen.len());
    eprintln!("states checked:   {checked}");
    eprintln!("bake failures:    {}", bake_failures.len());
    eprintln!("empty geometry:   {}", empty_states.len());

    // Premise check: a typo in `wanted` would make every assertion below pass
    // over an empty set. This is the failure mode that let the *deleted*
    // `VegGrid` gate's surviving doc comment read as coverage — an assertion
    // whose subject does not exist looks identical to one that holds.
    let missing: Vec<&&str> = wanted
        .iter()
        .filter(|w| !seen.contains(**w))
        .collect();
    assert!(
        missing.is_empty(),
        "these block names resolved to no state at all, so they asserted \
         nothing — fix the names, do not delete them: {missing:?}"
    );
    assert!(
        checked > 40,
        "expected many states across these blocks (leaves alone carry several \
         each), got {checked} — the registry walk is not reaching them"
    );

    assert!(
        bake_failures.is_empty(),
        "vegetation states failed to bake: {bake_failures:?}"
    );
    assert!(
        empty_states.is_empty(),
        "these vegetation states baked ZERO quads, so `mesh_models`' \
         `quads.is_empty()` skip drops them and they are invisible in game with \
         nothing reporting it: {empty_states:?}"
    );
}
