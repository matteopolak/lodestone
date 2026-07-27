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

use lodestone_assets::{
    Atlas, AtlasBuilder, BlockBaker, BlockStates, FirstWeight, ModelResolver, ResourceLocation,
    ResourceManager, TextureBinding, ZipSource,
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

fn full_block_atlas(manager: &ResourceManager, resolver: &ModelResolver) -> Atlas {
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
    let mut builder = AtlasBuilder::new();
    for loc in &textures {
        let _ = builder.load(manager, loc);
    }
    builder.build().expect("build atlas")
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
    let atlas = full_block_atlas(&manager, &resolver);
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
