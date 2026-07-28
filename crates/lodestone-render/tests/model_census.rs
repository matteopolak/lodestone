//! D1 measurement: what fraction of real baked block states are full opaque
//! cubes, and does that justify a separate packed-vertex fast path?
//!
//! This bakes every vanilla block state from a fetched `client.jar` (+ Mojang's
//! `generated/reports/blocks.json`) and classifies each with the *same*
//! [`lodestone_render::is_full_cube`] / [`is_packed_cube`] predicates the real
//! renderer uses to route geometry. It is `#[ignore]`d so the default test run
//! stays hermetic; run it with `--ignored`.
//!
//! It reads only from `.cache/mc/<version>/`; it never writes assets and never
//! touches `lodestone-assets`/`lodestone-world`, which other agents own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use lodestone_assets::tint::{TintKind, vanilla_tint_kind};
use lodestone_assets::{
    Atlas, AtlasBuilder, BlockBaker, BlockStates, DisplayTransform, FirstWeight, GuiItemContext,
    GuiLight, IconPart, ItemIconBuilder, ModelResolver, ModelTransform, ResourceLocation,
    ResourceManager, TextureBinding, ZipSource, bake_model,
};
use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};
use lodestone_render::{is_full_cube, is_packed_cube};

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
        if is_packed_cube(&model.quads) {
            packed_cube += 1;
            full_cube += 1;
            entry.0 += 1;
        } else if is_full_cube(&model.quads) {
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
        "      packed (untinted):   {packed_cube}  ({:.1}% of renderable)",
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
