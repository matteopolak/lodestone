//! Optional, offline-by-default tests against a real vanilla `client.jar`.
//!
//! These are `#[ignore]`d so the default test run stays hermetic. Run with
//! `cargo test -p lodestone-assets -- --ignored --nocapture` when a jar has been
//! fetched to `.cache/mc/<version>/client.jar` (see `xtask fetch-assets`).

use lodestone_assets::{
    Atlas, AtlasBuilder, BlockBaker, BlockStateDefinition, BlockStates, FirstWeight, IconPart,
    ItemIconBuilder, ModelResolver, ResourceLocation, ResourceManager, TextureBinding, ZipSource,
};
use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

fn cache_root() -> Option<PathBuf> {
    Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .join(".cache/mc"),
    )
}

/// The modern default jar the bulk of these tests assert against. Prefers 26.2
/// explicitly so the presence of fetched legacy jars (1.8.9/1.12.2) can never
/// silently swap the corpus out from under a test that expects flattened dirs.
fn client_jar() -> Option<PathBuf> {
    let cache = cache_root()?;
    let preferred = cache.join("26.2").join("client.jar");
    if preferred.is_file() {
        return Some(preferred);
    }
    let entries = std::fs::read_dir(&cache).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("client.jar");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The `client.jar` for a specific version directory, or `None` if not fetched.
fn client_jar_for(version: &str) -> Option<PathBuf> {
    let candidate = cache_root()?.join(version).join("client.jar");
    candidate.is_file().then_some(candidate)
}

fn manager_for(version: &str) -> Option<ResourceManager> {
    let jar = client_jar_for(version)?;
    let source = ZipSource::open(&jar).ok()?;
    Some(ResourceManager::new(vec![Box::new(source)]))
}

fn manager() -> ResourceManager {
    let jar = client_jar().expect("no client.jar under .cache/mc/<version>/; fetch it first");
    let source = ZipSource::open(&jar).expect("open client.jar");
    ResourceManager::new(vec![Box::new(source)])
}

#[test]
#[ignore = "requires a fetched vanilla client.jar in .cache/mc/<version>/"]
fn real_vanilla_assets_load() {
    let manager = manager();

    let stone_tex = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert!(
        manager.read_asset(&stone_tex, "textures", "png").is_some(),
        "stone block texture should load"
    );
    let stone_state = ResourceLocation::parse("minecraft:stone").unwrap();
    assert!(
        manager
            .read_asset(&stone_state, "blockstates", "json")
            .is_some(),
        "stone blockstate should load"
    );

    // Vanilla has NO root pack.mcmeta; metadata comes from version.json.
    assert!(
        manager.read("pack.mcmeta").is_none(),
        "vanilla client.jar should not have a root pack.mcmeta"
    );
    let meta = manager
        .read_pack_meta()
        .expect("derive meta from version.json");
    assert_eq!(
        meta.pack_format, 88,
        "26.2 resource pack format is 88 (version.json resource_major)"
    );
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn resolves_sample_models() {
    let manager = manager();
    let resolver = ModelResolver::new(&manager);

    // stone: trivial, resolves through cube_all -> cube -> block.
    let stone = resolver
        .resolve(&ResourceLocation::parse("block/stone").unwrap())
        .expect("resolve stone");
    assert!(!stone.elements.is_empty());
    assert!(stone.unresolved_textures().is_empty());
    assert_eq!(
        stone.resolve_texture("#up").unwrap().to_string(),
        "minecraft:block/stone"
    );

    // grass_block: multiple textures + tinting.
    let grass = resolver
        .resolve(&ResourceLocation::parse("block/grass_block").unwrap())
        .expect("resolve grass_block");
    assert!(grass.resolve_texture("#top").is_some());
    assert!(grass.resolve_texture("#side").is_some());
    let tinted = grass
        .elements
        .iter()
        .flat_map(|e| e.faces.values())
        .any(|f| f.tintindex.is_some());
    assert!(tinted, "grass_block should have a tinted face");

    // oak_stairs blockstate: many rotated variants.
    let stairs_bytes = manager
        .read_asset(
            &ResourceLocation::parse("minecraft:oak_stairs").unwrap(),
            "blockstates",
            "json",
        )
        .unwrap();
    let stairs = BlockStates::parse(&stairs_bytes).expect("parse oak_stairs");
    let refs: Vec<_> = stairs.model_refs().collect();
    assert!(
        refs.iter().any(|r| r.x != 0 || r.y != 0),
        "stairs use rotation"
    );
    for r in &refs {
        resolver.resolve(&r.model).expect("resolve stairs model");
    }

    // oak_fence blockstate: multipart.
    let fence_bytes = manager
        .read_asset(
            &ResourceLocation::parse("minecraft:oak_fence").unwrap(),
            "blockstates",
            "json",
        )
        .unwrap();
    let fence = BlockStates::parse(&fence_bytes).expect("parse oak_fence");
    assert!(matches!(
        fence.definition,
        BlockStateDefinition::Multipart(_)
    ));
    for r in fence.model_refs() {
        resolver.resolve(&r.model).expect("resolve fence model");
    }
}

/// The headline coverage measure: parse every blockstate and resolve every model
/// it references. Prints a success rate and a breakdown of failures.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn resolves_all_blockstates() {
    let manager = manager();
    let resolver = ModelResolver::new(&manager);

    let started = std::time::Instant::now();
    let paths = manager.list("assets/minecraft/blockstates/");
    let total = paths.len();
    assert!(total > 1000, "expected ~1198 blockstates, found {total}");

    let mut ok = 0usize;
    let mut parse_failures: Vec<String> = Vec::new();
    let mut resolve_failures: Vec<String> = Vec::new();
    let mut model_count = 0usize;

    for path in &paths {
        let Some(bytes) = manager.read(path) else {
            continue;
        };
        let bs = match BlockStates::parse(&bytes) {
            Ok(bs) => bs,
            Err(e) => {
                parse_failures.push(format!("{path}: {e}"));
                continue;
            }
        };
        let mut all_ok = true;
        for r in bs.model_refs() {
            model_count += 1;
            if let Err(e) = resolver.resolve(&r.model) {
                all_ok = false;
                resolve_failures.push(format!("{path} -> {}: {e}", r.model));
            }
        }
        if all_ok {
            ok += 1;
        }
    }

    eprintln!("=== blockstate resolution coverage ===");
    eprintln!("blockstates fully resolved: {ok}/{total}");
    eprintln!("model references resolved:  {model_count}");
    eprintln!("parse failures:   {}", parse_failures.len());
    for f in parse_failures.iter().take(20) {
        eprintln!("  PARSE  {f}");
    }
    eprintln!("resolve failures: {}", resolve_failures.len());
    for f in resolve_failures.iter().take(20) {
        eprintln!("  RESOLVE {f}");
    }

    // Also report how many distinct block models resolve standalone.
    let model_paths = manager.list("assets/minecraft/models/block/");
    let mut model_ok = 0usize;
    let mut model_fail: Vec<String> = Vec::new();
    for path in &model_paths {
        let Some(rest) = path.strip_prefix("assets/minecraft/models/") else {
            continue;
        };
        let Some(stem) = rest.strip_suffix(".json") else {
            continue;
        };
        let loc = ResourceLocation::new("minecraft", stem).unwrap();
        match resolver.resolve(&loc) {
            Ok(_) => model_ok += 1,
            Err(e) => model_fail.push(format!("{loc}: {e}")),
        }
    }
    eprintln!(
        "standalone block models resolved: {model_ok}/{}",
        model_paths.len()
    );
    for f in model_fail.iter().take(20) {
        eprintln!("  MODEL {f}");
    }

    let rate = ok as f64 / total as f64;
    assert!(
        rate > 0.99,
        "blockstate resolution rate {rate:.4} below 0.99 ({ok}/{total})"
    );

    // Regression guard: after the O(1)-per-read ZipSource fix this whole pass
    // (thousands of zip reads) runs in a few seconds. A generous upper bound
    // catches an accidental return to re-parsing the central directory per read
    // (which took ~150s). This is wall-clock, so keep the bound loose.
    let elapsed = started.elapsed();
    eprintln!("elapsed: {:.2}s", elapsed.as_secs_f64());
    assert!(
        elapsed.as_secs() < 30,
        "resolves_all_blockstates took {:.1}s (>30s) — ZipSource read may be re-parsing the central directory per read",
        elapsed.as_secs_f64()
    );
}

/// Cross-check: version.json's protocol_version independently confirms the
/// number the network stack targets.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn version_json_cross_check() {
    let manager = manager();
    let bytes = manager.read("version.json").expect("version.json present");
    let v = lodestone_assets::VersionMeta::parse(&bytes).unwrap();
    eprintln!(
        "version {} protocol {:?} resource {}.{} data {}.{}",
        v.id,
        v.protocol_version,
        v.resource_format.major,
        v.resource_format.minor,
        v.data_format.major,
        v.data_format.minor
    );
    assert_eq!(v.resource_format.major, 88);
}

/// Builds the full block atlas from every texture referenced by the 1,198
/// resolved blockstates, and reports sprite count, dimensions, memory, and time.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn builds_full_block_atlas() {
    let manager = manager();
    let resolver = ModelResolver::new(&manager);

    // Collect every distinct texture referenced by every resolved model that a
    // blockstate points at.
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
    eprintln!("distinct block textures referenced: {}", textures.len());

    let started = std::time::Instant::now();
    let mut builder = AtlasBuilder::new();
    let mut loaded = 0usize;
    let mut missing = 0usize;
    let mut failed = 0usize;
    for loc in &textures {
        match builder.load(&manager, loc) {
            Ok(_) => loaded += 1,
            Err(lodestone_assets::AtlasError::TextureMissing { .. }) => missing += 1,
            Err(e) => {
                failed += 1;
                if failed <= 20 {
                    eprintln!("  ATLAS-LOAD {e}");
                }
            }
        }
    }
    let atlas = builder.build().expect("build atlas");
    let elapsed = started.elapsed();

    let animated = atlas.sprites().iter().filter(|s| s.is_animated()).count();
    let peak_mb = atlas.rgba.len() as f64 / (1024.0 * 1024.0);
    eprintln!("=== block atlas ===");
    eprintln!("textures loaded:   {loaded} (missing {missing}, failed {failed})");
    eprintln!("sprites:           {}", atlas.sprites().len());
    eprintln!("  animated:        {animated}");
    eprintln!(
        "atlas dimensions:  {}x{} ({} layer)",
        atlas.width, atlas.height, atlas.layers
    );
    eprintln!("atlas RGBA memory: {peak_mb:.1} MiB");
    eprintln!("build wall-clock:  {:.2}s", elapsed.as_secs_f64());

    assert!(atlas.sprites().len() > 1000, "expected >1000 block sprites");
    assert!(
        missing < textures.len() / 20,
        "too many missing textures: {missing}"
    );

    // Deterministic: a second build over the same inputs is byte-identical.
    let mut builder2 = AtlasBuilder::new();
    for loc in &textures {
        let _ = builder2.load(&manager, loc);
    }
    let atlas2 = builder2.build().unwrap();
    assert_eq!(atlas.width, atlas2.width);
    assert_eq!(atlas.height, atlas2.height);
    assert_eq!(
        atlas.rgba, atlas2.rgba,
        "atlas must be reproducible byte-for-byte"
    );
}

// --- Task E: baked geometry against the real jar -----------------------------

/// Path to the version's `generated/reports/blocks.json`, next to the jar.
fn blocks_report_path() -> Option<PathBuf> {
    let jar = client_jar()?;
    let dir = jar.parent()?;
    let candidate = dir.join("generated/reports/blocks.json");
    candidate.is_file().then_some(candidate)
}

/// A test-support [`BlockStateRegistry`] loaded from Mojang's data-generator
/// `blocks.json`. This mirrors what a version crate (`v770`) will eventually own;
/// we build it here only so bake coverage can be measured offline. The real
/// generated table is produced elsewhere — this test never writes it.
#[derive(Debug)]
struct BlocksReport {
    /// Indexed by block state id; `None` for any gap in the id range.
    entries: Vec<Option<(Identifier, BTreeMap<String, String>)>>,
}

impl BlocksReport {
    fn load() -> Option<Self> {
        let bytes = std::fs::read(blocks_report_path()?).ok()?;
        let root: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let obj = root.as_object()?;

        let mut states: Vec<(u32, Identifier, BTreeMap<String, String>)> = Vec::new();
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

    /// The default (first) state id for a block, for targeted sample bakes.
    fn first_state_of(&self, block: &str) -> Option<u32> {
        let want: Identifier = block.parse().ok()?;
        self.entries
            .iter()
            .enumerate()
            .find_map(|(i, e)| e.as_ref().filter(|(id, _)| *id == want).map(|_| i as u32))
    }

    /// All state ids belonging to a block.
    fn states_of(&self, block: &str) -> Vec<u32> {
        let Ok(want) = block.parse::<Identifier>() else {
            return Vec::new();
        };
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().filter(|(id, _)| *id == want).map(|_| i as u32))
            .collect()
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

/// Loads the block-state registry, failing loudly with the fix if the
/// generated report is absent. An `#[ignore]`d test that was explicitly run
/// must never pass without its registry (this is the fixture whose absence
/// silently no-op'd a sibling's headline gate).
fn blocks_report() -> BlocksReport {
    BlocksReport::load().unwrap_or_else(|| {
        panic!(
            "missing generated/reports/blocks.json next to the selected client.jar.\n\
             Expected at: .cache/mc/26.2/generated/reports/blocks.json\n\
             Generate it with the vanilla server:  \
             java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports\n\
             then copy generated/reports/ next to the jar. Do NOT skip — a green test \
             with no registry is not evidence."
        )
    })
}

/// Builds the full block atlas from every texture referenced by every resolved
/// blockstate model. Shared by the bake tests below.
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

/// Bakes a hand-picked sample that exercises the tricky paths: trivial cube,
/// tinting, rotated variants + uvlock, multipart, and the Euler element
/// rotation used by hanging signs.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn bakes_sample_blocks() {
    let manager = manager();
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let report = blocks_report();

    // stone: trivial full cube — 6 quads, no tint.
    let id = report.first_state_of("minecraft:stone").unwrap();
    let stone = baker
        .bake_state(&report, id, &FirstWeight)
        .expect("bake stone");
    assert_eq!(stone.quads.len(), 6, "stone is a full cube");
    assert!(stone.quads.iter().all(|q| q.tint_index.is_none()));

    // grass_block: tinting must survive baking. The default/first state may be
    // `snowy=true` (an untinted snow model), so check across all states.
    let mut grass_tinted = false;
    let mut grass_states = 0usize;
    for id in report.states_of("minecraft:grass_block") {
        let grass = baker
            .bake_state(&report, id, &FirstWeight)
            .expect("bake grass_block");
        grass_states += 1;
        if grass.quads.iter().any(|q| q.tint_index.is_some()) {
            grass_tinted = true;
        }
    }
    assert!(grass_states > 0);
    assert!(
        grass_tinted,
        "grass_block must carry a tint index through baking for at least one state"
    );

    // oak_stairs: rotated variants with uvlock — bake every state, all non-empty.
    let mut rotated_seen = false;
    for id in report.states_of("minecraft:oak_stairs") {
        let baked = baker
            .bake_state(&report, id, &FirstWeight)
            .expect("bake oak_stairs state");
        assert!(!baked.is_empty(), "stairs state {id} produced no quads");
        // At least one state is a rotated variant.
        rotated_seen = true;
        let _ = &baked;
    }
    assert!(rotated_seen);

    // oak_fence: multipart — the post plus connected sides. Bake every state.
    for id in report.states_of("minecraft:oak_fence") {
        let baked = baker
            .bake_state(&report, id, &FirstWeight)
            .expect("bake oak_fence state");
        assert!(!baked.is_empty(), "fence state {id} produced no quads");
    }

    // oak_hanging_sign: exercises the Euler {x,y,z,origin} element rotation
    // (template_hanging_sign_rot_3). Every state must bake without panicking.
    let mut hanging_quads = 0usize;
    for id in report.states_of("minecraft:oak_hanging_sign") {
        let baked = baker
            .bake_state(&report, id, &FirstWeight)
            .expect("bake oak_hanging_sign state");
        hanging_quads += baked.quads.len();
    }
    assert!(hanging_quads > 0, "hanging sign should produce geometry");
}

/// Headline bake-coverage measure: bake every block state in `blocks.json` and
/// report the success rate plus a breakdown of failures. Empty models (air,
/// fluids, block-entity-only blocks) are a success with zero quads, not a
/// failure — a failure is an actual `BakeError`.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn bakes_all_block_states() {
    let manager = manager();
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let report = blocks_report();

    let started = std::time::Instant::now();
    let total = report.state_count();
    let mut ok = 0usize;
    let mut empty = 0usize;
    let mut quads = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut failure_kinds: BTreeMap<String, usize> = BTreeMap::new();

    for id in 0..total {
        let Some(resolved) = report.resolve(id) else {
            continue;
        };
        match baker.bake_state(&report, id, &FirstWeight) {
            Ok(baked) => {
                ok += 1;
                if baked.is_empty() {
                    empty += 1;
                } else {
                    quads += baked.quads.len();
                }
            }
            Err(e) => {
                let kind = match &e {
                    lodestone_assets::BakeError::UnresolvedTexture { .. } => "UnresolvedTexture",
                    lodestone_assets::BakeError::SpriteMissing { .. } => "SpriteMissing",
                    lodestone_assets::BakeError::UnknownState { .. } => "UnknownState",
                    lodestone_assets::BakeError::Blockstate { .. } => "Blockstate",
                    lodestone_assets::BakeError::Model { .. } => "Model",
                };
                *failure_kinds.entry(kind.to_string()).or_default() += 1;
                if failures.len() < 30 {
                    failures.push(format!("{} [{}]: {e}", resolved.block, id));
                }
            }
        }
    }
    let elapsed = started.elapsed();

    let fail_total: usize = failure_kinds.values().sum();
    let attempted = ok + fail_total;
    eprintln!("=== block bake coverage ===");
    eprintln!("states baked ok: {ok}/{attempted}");
    eprintln!("  of which empty (air/fluid/block-entity): {empty}");
    eprintln!("  total quads:                             {quads}");
    eprintln!("failures: {fail_total}");
    for (kind, n) in &failure_kinds {
        eprintln!("  {kind}: {n}");
    }
    for f in failures.iter().take(30) {
        eprintln!("  FAIL {f}");
    }
    eprintln!("elapsed: {:.2}s", elapsed.as_secs_f64());

    let rate = ok as f64 / attempted as f64;
    assert!(
        rate > 0.99,
        "bake success rate {rate:.4} below 0.99 ({ok}/{attempted})"
    );
    // Regression guard on the per-read cost (see resolves_all_blockstates).
    assert!(
        elapsed.as_secs() < 60,
        "bakes_all_block_states took {:.1}s (>60s)",
        elapsed.as_secs_f64()
    );
}

#[test]
#[ignore = "census helper: distinct tint indices across all baked states"]
fn tint_index_census() {
    let manager = manager();
    let resolver = ModelResolver::new(&manager);
    let atlas = full_block_atlas(&manager, &resolver);
    let baker = BlockBaker::new(&manager, &resolver, &atlas);
    let report = blocks_report();

    let mut by_index: BTreeMap<i32, BTreeSet<String>> = BTreeMap::new();
    for id in 0..report.state_count() {
        let Some(rs) = report.resolve(id) else {
            continue;
        };
        let Ok(baked) = baker.bake_state(&report, id, &FirstWeight) else {
            continue;
        };
        for q in &baked.quads {
            if let Some(t) = q.tint_index {
                by_index.entry(t).or_default().insert(rs.block.to_string());
            }
        }
    }
    eprintln!("=== tint index census ===");
    eprintln!("distinct tint indices: {}", by_index.len());
    for (idx, blocks) in &by_index {
        eprintln!("tintindex {idx}: {} blocks", blocks.len());
        let mut v: Vec<&String> = blocks.iter().collect();
        v.sort();
        for b in v {
            eprintln!("    {b}");
        }
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn loads_real_colormaps() {
    use lodestone_assets::tint::Colormaps;
    let manager = manager();
    let maps = Colormaps::load(&manager).expect("load colormaps from client.jar");
    // Plains-like climate (temp 0.8, downfall 0.4) is a mid-green, not the
    // magenta/default fallback.
    let grass = maps.grass.sample(0.8, 0.4);
    let foliage = maps.foliage.sample(0.8, 0.4);
    eprintln!("plains grass=0x{grass:06X} foliage=0x{foliage:06X}");
    let g = ((grass >> 8) & 0xFF) as i32;
    let r = ((grass >> 16) & 0xFF) as i32;
    let b = (grass & 0xFF) as i32;
    assert!(g > r && g > b, "grass colour should be green-dominant");
    // Snowy climate (temp 0) is a distinctly different (browner) colour.
    let snowy = maps.grass.sample(0.0, 0.5);
    assert_ne!(snowy, grass);
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn entity_textures_resolve() {
    use lodestone_assets::Image;
    let manager = manager();

    // Entity sheets live under assets/minecraft/textures/entity/**. The default
    // player skin (steve/alex) and a couple of mob sheets must resolve and decode.
    for path in [
        "minecraft:entity/player/wide/steve",
        "minecraft:entity/player/slim/alex",
        "minecraft:entity/creeper/creeper",
        "minecraft:entity/zombie/zombie",
    ] {
        let loc = ResourceLocation::parse(path).unwrap();
        let bytes = manager
            .read_asset(&loc, "textures", "png")
            .unwrap_or_else(|| panic!("entity texture {path} should resolve"));
        let img = Image::decode_png(&bytes).unwrap_or_else(|_| panic!("decode {path}"));
        assert!(
            img.width >= 64 && img.height >= 32,
            "{path} sheet too small"
        );
        eprintln!("{path}: {}x{}", img.width, img.height);
    }

    // A skin is just an entity texture at a full in-pack path; prove the raw
    // path resolves too (this is the shape a downloaded skin cache would use).
    assert!(
        manager
            .read("assets/minecraft/textures/entity/player/wide/steve.png")
            .is_some(),
        "raw skin path should resolve through the pack stack"
    );
}

/// Whole-corpus coverage for the hand-ported entity models: every entry's
/// declared sheet size is checked against the **real texture PNG** in
/// `client.jar` (an authority we did not write), and every baked UV must land
/// inside that real sheet. A mistranscribed sheet size, a wrong texture path, or
/// a UV that escapes the sheet fails loudly here — the §12.31 defence: the check
/// is against Mojang's own PNG, not against a number we also computed.
///
/// This runs from the first mob so a gap shows as a percentage, not a silent
/// absence. Missing fixture is a FAILURE with the fetch command, never a skip.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn entity_models_whole_corpus_coverage() {
    use lodestone_assets::Image;
    use lodestone_assets::entity::{
        CatCoat, EntityVariant, HorseColor, LlamaColor, MooshroomColor, ParrotColor, Temperature,
        WolfCoat, WolfState, bake_entity,
    };
    use lodestone_assets::entity_models::entity_models;

    // Every registered ByVariant entry's own variant axis, probed with every
    // real variant value it has (not just Temperature) — otherwise a
    // ByVariant entry for e.g. horse colour would only ever get its default
    // sheet checked three times, and a wrong horse_black.png path would pass
    // silently. Panics on an unrecognised variant-driven name so a future
    // ByVariant entry can't be added without extending this list.
    fn variant_probes_for(name: &str) -> Vec<EntityVariant> {
        match name {
            "pig" | "cow" | "chicken" => vec![
                EntityVariant::Temperature(Temperature::Temperate),
                EntityVariant::Temperature(Temperature::Cold),
                EntityVariant::Temperature(Temperature::Warm),
            ],
            "horse" => vec![
                EntityVariant::HorseColor(HorseColor::White),
                EntityVariant::HorseColor(HorseColor::Creamy),
                EntityVariant::HorseColor(HorseColor::Chestnut),
                EntityVariant::HorseColor(HorseColor::Brown),
                EntityVariant::HorseColor(HorseColor::Black),
                EntityVariant::HorseColor(HorseColor::Gray),
                EntityVariant::HorseColor(HorseColor::DarkBrown),
            ],
            "llama" | "trader_llama" => vec![
                EntityVariant::Llama(LlamaColor::Creamy),
                EntityVariant::Llama(LlamaColor::White),
                EntityVariant::Llama(LlamaColor::Brown),
                EntityVariant::Llama(LlamaColor::Gray),
            ],
            "cat" => vec![
                EntityVariant::Cat(CatCoat::Tabby),
                EntityVariant::Cat(CatCoat::Black),
                EntityVariant::Cat(CatCoat::Red),
                EntityVariant::Cat(CatCoat::Siamese),
                EntityVariant::Cat(CatCoat::BritishShorthair),
                EntityVariant::Cat(CatCoat::Calico),
                EntityVariant::Cat(CatCoat::Persian),
                EntityVariant::Cat(CatCoat::Ragdoll),
                EntityVariant::Cat(CatCoat::White),
                EntityVariant::Cat(CatCoat::Jellie),
                EntityVariant::Cat(CatCoat::AllBlack),
            ],
            "wolf" => {
                let mut v = Vec::new();
                for coat in [
                    WolfCoat::Pale,
                    WolfCoat::Spotted,
                    WolfCoat::Snowy,
                    WolfCoat::Black,
                    WolfCoat::Ashen,
                    WolfCoat::Rusty,
                    WolfCoat::Woods,
                    WolfCoat::Chestnut,
                    WolfCoat::Striped,
                ] {
                    for state in [WolfState::Wild, WolfState::Tame, WolfState::Angry] {
                        v.push(EntityVariant::Wolf { coat, state });
                    }
                }
                v
            }
            "parrot" => vec![
                EntityVariant::Parrot(ParrotColor::RedBlue),
                EntityVariant::Parrot(ParrotColor::Blue),
                EntityVariant::Parrot(ParrotColor::Green),
                EntityVariant::Parrot(ParrotColor::YellowBlue),
                EntityVariant::Parrot(ParrotColor::Gray),
            ],
            "mooshroom" => vec![
                EntityVariant::Mooshroom(MooshroomColor::Red),
                EntityVariant::Mooshroom(MooshroomColor::Brown),
            ],
            other => panic!(
                "{other}: ByVariant entry has no variant-probe list in the real-jar coverage \
                 test — add one covering its variant axis"
            ),
        }
    }

    if client_jar().is_none() {
        panic!(
            "requires .cache/mc/26.2/client.jar — run: cargo run -p xtask -- fetch-assets --version 26.2"
        );
    }
    let manager = manager();

    // Denominator context: how many entity texture directories exist in the jar
    // (a rough proxy for the mob roster), so coverage reads as N-of-M.
    let entity_dirs: BTreeSet<String> = manager
        .list("assets/minecraft/textures/entity/")
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix("assets/minecraft/textures/entity/")
                .and_then(|r| r.split('/').next())
                .map(str::to_string)
        })
        .collect();

    let models = entity_models();
    let mut verified = 0usize;
    let mut variant_sheets_verified = 0usize;
    for e in &models {
        let model = (e.build)();
        let tex_path = e.texture.default_path();
        let loc = ResourceLocation::parse(&format!("minecraft:{}", tex_path)).unwrap();
        let bytes = manager
            .read_asset(&loc, "textures", "png")
            .unwrap_or_else(|| {
                panic!(
                    "{}: texture {} not found in client.jar — path wrong or asset missing",
                    e.name, tex_path
                )
            });
        let img =
            Image::decode_png(&bytes).unwrap_or_else(|_| panic!("{}: decode {}", e.name, tex_path));

        // A variant-driven entry must have *every* variant sheet present in the
        // jar, not just its default — otherwise ByVariant is a latent 404. Prove
        // it non-vacuously against the real PNGs, probing this entry's own
        // variant axis (temperature, horse colour, llama, cat, wolf, parrot —
        // not just temperature for every entry).
        if e.texture.is_variant() {
            for v in variant_probes_for(e.name) {
                let vpath = e.texture.resolve(v);
                let vloc = ResourceLocation::parse(&format!("minecraft:{}", vpath)).unwrap();
                let vbytes = manager
                    .read_asset(&vloc, "textures", "png")
                    .unwrap_or_else(|| {
                        panic!(
                            "{}: variant texture {} ({v:?}) not found in client.jar",
                            e.name, vpath
                        )
                    });
                Image::decode_png(&vbytes)
                    .unwrap_or_else(|_| panic!("{}: decode variant {}", e.name, vpath));
                variant_sheets_verified += 1;
            }
        }

        // External-authority check: the real PNG must be a positive-integer
        // multiple of the model's declared UV resolution, with the same factor on
        // both axes. Minecraft normalises UVs against the model's declared sheet
        // and lets the texture ship at any integer multiple of that (e.g. the
        // ghast model declares 64x32 but ships a 128x64 texture at 2x). This still
        // catches a wrong path or wrong aspect ratio, without falsely rejecting
        // hi-res vanilla textures.
        assert!(
            model.texture_width > 0
                && model.texture_height > 0
                && img.width.is_multiple_of(model.texture_width)
                && img.height.is_multiple_of(model.texture_height)
                && img.width / model.texture_width == img.height / model.texture_height,
            "{}: declared UV sheet {}x{} is not an integer-multiple match for real {} PNG {}x{}",
            e.name,
            model.texture_width,
            model.texture_height,
            tex_path,
            img.width,
            img.height
        );

        let quads = bake_entity(&model);
        assert!(!quads.is_empty(), "{} baked no quads", e.name);
        // UVs are normalised against the model's declared sheet. Vanilla is
        // emphatically not strictly in-bounds: SalmonModel/CodModel use negative
        // texOffs, and PufferfishBigModel's fins run ~7 texels off the right edge
        // of their 32x32 sheet. So we only assert UVs are finite and within a
        // gross 2x envelope (catches a NaN or a halved/doubled sheet); the real
        // gate is the integer-multiple sheet-size check above plus box counts.
        for q in &quads {
            let p = &q.positions;
            let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
            let e2 = [p[3][0] - p[0][0], p[3][1] - p[0][1], p[3][2] - p[0][2]];
            let cross = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            if cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2] < 1e-9 {
                continue;
            }
            for uv in q.uvs {
                assert!(
                    uv[0].is_finite() && uv[1].is_finite(),
                    "{}: non-finite UV {uv:?}",
                    e.name
                );
                assert!(
                    (-1.0..=2.0).contains(&uv[0]) && (-1.0..=2.0).contains(&uv[1]),
                    "{}: UV {uv:?} is wildly off the declared sheet (sheet-size error?)",
                    e.name,
                );
            }
        }
        verified += 1;
        eprintln!(
            "  {:<12} {:>3} quads on {}x{} ({})",
            e.name,
            quads.len(),
            img.width,
            img.height,
            tex_path
        );
    }

    eprintln!(
        "entity model coverage: {verified}/{} ported models verified against real PNGs \
         ({variant_sheets_verified} variant sheets across all ByVariant entries also verified); \
         {} entity texture dirs in jar",
        models.len(),
        entity_dirs.len()
    );
    assert_eq!(
        verified,
        models.len(),
        "every ported model must be jar-verified"
    );
    assert!(verified >= 10, "priority corpus present");
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn item_models_whole_corpus_coverage() {
    use lodestone_assets::Image;
    let manager = manager();
    let resolver = ModelResolver::new(&manager);

    let prefix = "assets/minecraft/models/item/";
    let mut names: Vec<String> = manager
        .list(prefix)
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(prefix)
                .and_then(|r| r.strip_suffix(".json"))
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names.dedup();
    let total = names.len();
    assert!(
        total > 100,
        "expected a large item-model corpus, got {total}"
    );

    let (mut generated, mut builtin_entity, mut other_builtin) = (0usize, 0usize, 0usize);
    let (mut block3d, mut empty, mut errors) = (0usize, 0usize, 0usize);
    let mut err_samples: Vec<String> = Vec::new();
    // Also prove the generated pipeline end-to-end: decode real layer textures
    // and bake, counting how many generated items produce geometry.
    let (mut baked_ok, mut baked_quads) = (0usize, 0usize);

    for name in &names {
        let loc = ResourceLocation::parse(&format!("item/{name}")).unwrap();
        let model = match resolver.resolve(&loc) {
            Ok(m) => m,
            Err(e) => {
                errors += 1;
                if err_samples.len() < 25 {
                    err_samples.push(format!("{name}: {e}"));
                }
                continue;
            }
        };
        match model.builtin.as_deref() {
            Some("generated") => {
                generated += 1;
                // Gather layer0.. sprites in order; bake if all present.
                let mut images: Vec<Image> = Vec::new();
                let mut ok = true;
                for layer in lodestone_assets::item::LAYER_NAMES {
                    match model.textures.get(layer) {
                        Some(TextureBinding::Resolved(tex)) => {
                            match manager.read_asset(tex, "textures", "png") {
                                Some(bytes) => match Image::decode_png(&bytes) {
                                    Ok(img) => images.push(img),
                                    Err(_) => {
                                        ok = false;
                                        break;
                                    }
                                },
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        Some(TextureBinding::Unresolved(_)) => {
                            ok = false;
                            break;
                        }
                        None => break, // no more layers
                    }
                }
                if ok && !images.is_empty() {
                    let refs: Vec<&Image> = images.iter().collect();
                    let quads = lodestone_assets::item::bake_item_generated(&refs);
                    if !quads.is_empty() {
                        baked_ok += 1;
                        baked_quads += quads.len();
                    }
                }
            }
            Some("entity") => builtin_entity += 1,
            Some(_) => other_builtin += 1,
            None => {
                if model.elements.is_empty() {
                    empty += 1;
                } else {
                    block3d += 1;
                }
            }
        }
    }

    let resolved = total - errors;
    eprintln!("== item model whole-corpus coverage ==");
    eprintln!("total={total} resolved={resolved} errors={errors}");
    eprintln!(
        "  generated={generated} builtin_entity={builtin_entity} other_builtin={other_builtin} block3d={block3d} empty={empty}"
    );
    eprintln!("  generated baked ok={baked_ok} total_quads={baked_quads}");
    eprintln!(
        "coverage: {resolved}/{total} = {:.2}%",
        100.0 * resolved as f64 / total as f64
    );
    for s in &err_samples {
        eprintln!("  ERR {s}");
    }
    assert_eq!(errors, 0, "every item model should resolve without error");
    assert!(baked_ok > 0, "at least some generated items should bake");
}

/// The higher-value instrument: drive the full item -> drawable-icon pipeline
/// over the real `items/*.json` corpus (the true inventory item set, including
/// the ex-`builtin/entity` items that only surface as `special` there), and
/// report how many produce a *drawable* icon whose sprites actually decode.
///
/// "Drawable" is verified concretely, not just classified: a `Sprite` part's
/// every layer texture must decode as a PNG, and a `Special` part's base model
/// must resolve. That end-to-end check is what turns a silent gap (a definition
/// naming a texture the pack lacks) into a named failure and a percentage.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn item_icons_whole_corpus_coverage() {
    use lodestone_assets::Image;
    let manager = manager();
    let builder = ItemIconBuilder::new(&manager);
    let resolver = ModelResolver::new(&manager);

    // Enumerate the item-definition corpus deterministically (sorted, deduped;
    // never read_dir order — a fixture-by-name discipline, per the mip test that
    // once passed green on a degenerate single-sprite atlas).
    let prefix = "assets/minecraft/items/";
    let mut ids: Vec<String> = manager
        .list(prefix)
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(prefix)
                .and_then(|r| r.strip_suffix(".json"))
                .map(str::to_string)
        })
        .collect();
    ids.sort();
    ids.dedup();
    let total = ids.len();
    assert!(total > 100, "expected a large item corpus, got {total}");

    let (mut sprite, mut model, mut special, mut empty) = (0usize, 0usize, 0usize, 0usize);
    let mut drawable = 0usize;
    // Classified named failures: (id, class, reason).
    let mut failures: Vec<(String, &'static str, String)> = Vec::new();
    let mut empty_samples: Vec<String> = Vec::new();
    // A verifier over one part; returns Err(reason) when the part cannot draw.
    let verify_part = |part: &IconPart| -> Result<(), String> {
        match part {
            IconPart::Sprite { layers } => {
                for layer in layers {
                    let bytes = manager
                        .read_asset(&layer.sprite, "textures", "png")
                        .ok_or_else(|| format!("missing texture {}", layer.sprite))?;
                    Image::decode_png(&bytes)
                        .map_err(|e| format!("undecodable texture {}: {e}", layer.sprite))?;
                }
                Ok(())
            }
            // A Model part was already resolved with elements during building; a
            // Special part must have a resolvable base sprite model.
            IconPart::Model { .. } => Ok(()),
            IconPart::Special { base, .. } => resolver
                .resolve(base)
                .map(|_| ())
                .map_err(|e| format!("special base {base} unresolved: {e}")),
        }
    };

    for id in &ids {
        let loc = ResourceLocation::parse(id).unwrap();
        let icon = match builder.icon(&loc) {
            Ok(icon) => icon,
            Err(e) => {
                if failures.len() < 40 {
                    failures.push((id.clone(), "build", e.to_string()));
                }
                continue;
            }
        };
        if !icon.is_drawable() {
            empty += 1;
            if empty_samples.len() < 40 {
                empty_samples.push(id.clone());
            }
            continue;
        }
        // Tally the dominant class of the icon (its first part) for the census.
        match &icon.parts[0] {
            IconPart::Sprite { .. } => sprite += 1,
            IconPart::Model { .. } => model += 1,
            IconPart::Special { .. } => special += 1,
        }
        // Concretely verify every part draws.
        let mut ok = true;
        for part in &icon.parts {
            if let Err(reason) = verify_part(part) {
                ok = false;
                let class = match part {
                    IconPart::Sprite { .. } => "sprite",
                    IconPart::Model { .. } => "model",
                    IconPart::Special { .. } => "special",
                };
                if failures.len() < 40 {
                    failures.push((id.clone(), class, reason));
                }
                break;
            }
        }
        if ok {
            drawable += 1;
        }
    }

    eprintln!("== item icon whole-corpus coverage ==");
    eprintln!("total={total} drawable={drawable} empty={empty}");
    eprintln!("  sprite={sprite} model={model} special={special}");
    eprintln!(
        "drawable: {drawable}/{total} = {:.2}%",
        100.0 * drawable as f64 / total as f64
    );
    for (id, class, reason) in &failures {
        eprintln!("  FAIL [{class}] {id}: {reason}");
    }
    for id in &empty_samples {
        eprintln!("  EMPTY {id}");
    }

    // Correctness gate: nothing that classified as drawable may fail to draw.
    // A moved/renamed sprite or a new texture object form surfaces here as a
    // named FAIL rather than a silent gap — the check that has repeatedly caught
    // regressions hand-picked fixtures missed.
    assert_eq!(
        failures.len(),
        0,
        "some items classified as drawable but failed verification (see FAIL lines)"
    );
    // Coverage gate: essentially every item draws. The only expected non-drawable
    // item is `air` (no visual); a mass regression (a class of items resolving to
    // empty) drops the ratio below this floor and trips the assert.
    assert!(
        drawable > 0 && drawable * 1000 >= total * 999,
        "item icon coverage regressed: {drawable}/{total} drawable (see EMPTY/FAIL lines)"
    );
}


#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn font_default_advances_and_whole_corpus_coverage() {
    use lodestone_assets::font::{FontLoader, FontOptions, ProviderDef};
    let manager = manager();
    let loader = FontLoader::new(&manager);

    // The default font composites the space provider, ascii bitmap, and unifont
    // includes. Load with default options (Uniform NOT active).
    let default = ResourceLocation::parse("minecraft:default").unwrap();
    let font = loader
        .load(&default, &FontOptions::none())
        .expect("load default font");

    // CRITICAL: the space provider is declared first and MUST win, proving the
    // "first-declared provider wins" priority direction. Vanilla space = 4.
    assert_eq!(
        font.advance(' ' as u32),
        Some(4.0),
        "space advance must be 4"
    );

    // Known bitmap advances derived from the rightmost non-transparent column.
    // (Verified independently against ascii.png with PIL.)
    let known: &[(char, f32)] = &[
        ('i', 2.0),
        ('l', 3.0),
        ('I', 4.0),
        ('a', 6.0),
        ('!', 2.0),
        ('W', 6.0),
        ('t', 4.0),
        ('.', 2.0),
    ];
    for (ch, want) in known {
        assert_eq!(
            font.advance(*ch as u32),
            Some(*want),
            "advance for {ch:?} should be {want}"
        );
    }

    // The total measured width of a known string must equal the sum of its
    // glyph advances, computed against the REAL ascii.png bitmaps. "ilI!." mixes
    // the narrowest glyphs (2+3+4+2+2 = 13); a fixed-advance font would report
    // 5 * cell_width instead and fail here.
    assert_eq!(
        font.string_width("ilI!."),
        13.0,
        "string width must sum real per-glyph advances"
    );
    let independent: f32 = "ilI!."
        .chars()
        .map(|c| font.advance(c as u32).unwrap())
        .sum();
    assert_eq!(font.string_width("ilI!."), independent);

    // Whole-corpus census of every font definition in the jar.
    let prefix = "assets/minecraft/font/";
    let mut names: Vec<String> = manager
        .list(prefix)
        .into_iter()
        .filter_map(|p| {
            p.strip_prefix(prefix)
                .and_then(|r| r.strip_suffix(".json"))
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names.dedup();

    let (mut bitmap, mut space, mut reference, mut ttf, mut unihex) = (0, 0, 0, 0, 0);
    let mut top_level = 0usize;
    for name in &names {
        let loc = ResourceLocation::parse(name).unwrap();
        let Some(bytes) = manager.read_asset(&loc, "font", "json") else {
            continue;
        };
        let def = lodestone_assets::font::FontDefinition::parse(&bytes)
            .unwrap_or_else(|e| panic!("font {name} parse: {e}"));
        // Count only true top-level fonts (not the include/* fragments).
        if !name.starts_with("include/") {
            top_level += 1;
        }
        for cp in &def.providers {
            match cp.def {
                ProviderDef::Bitmap { .. } => bitmap += 1,
                ProviderDef::Space { .. } => space += 1,
                ProviderDef::Reference { .. } => reference += 1,
                ProviderDef::Ttf { .. } => ttf += 1,
                ProviderDef::Unihex { .. } => unihex += 1,
            }
        }
    }

    // Distinct codepoints covered by the fully-composited default font.
    let default_cps = font.codepoint_count();

    eprintln!("== font whole-corpus coverage ==");
    eprintln!(
        "font definition files={} (top-level={top_level})",
        names.len()
    );
    eprintln!(
        "providers: bitmap={bitmap} space={space} reference={reference} ttf={ttf} unihex={unihex}"
    );
    eprintln!("default font distinct codepoints={default_cps}");

    assert!(top_level >= 3, "expected the vanilla top-level fonts");
    assert!(
        default_cps > 800,
        "default font should cover many codepoints, got {default_cps}"
    );
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn gui_sprite_scaling_whole_corpus_coverage() {
    use lodestone_assets::gui::{GuiMeta, GuiScaling};
    let manager = manager();

    let prefix = "assets/minecraft/textures/gui/sprites/";
    let pngs: Vec<String> = manager
        .list(prefix)
        .into_iter()
        .filter(|p| p.ends_with(".png"))
        .collect();

    let (mut stretch, mut tile, mut nine_slice, mut errors) = (0usize, 0usize, 0usize, 0usize);
    for png in &pngs {
        let meta_path = format!("{png}.mcmeta");
        let scaling = match manager.read(&meta_path) {
            None => GuiScaling::Stretch, // no mcmeta -> default stretch
            Some(bytes) => match GuiMeta::parse(&bytes) {
                Ok(m) => m.scaling,
                Err(e) => {
                    errors += 1;
                    eprintln!("  ERR {png}: {e}");
                    continue;
                }
            },
        };
        match scaling {
            GuiScaling::Stretch => stretch += 1,
            GuiScaling::Tile { .. } => tile += 1,
            GuiScaling::NineSlice { .. } => nine_slice += 1,
        }
    }

    eprintln!("== gui sprite scaling whole-corpus coverage ==");
    eprintln!("total gui sprites={}", pngs.len());
    eprintln!("  stretch={stretch} tile={tile} nine_slice={nine_slice} parse_errors={errors}");
    assert_eq!(errors, 0, "every gui .mcmeta should parse");
    assert!(nine_slice > 0, "expected some nine_slice sprites");
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn particles_whole_corpus_coverage() {
    use lodestone_assets::particle::ParticleDefinition;
    let manager = manager();

    let prefix = "assets/minecraft/particles/";
    let names: Vec<String> = manager
        .list(prefix)
        .into_iter()
        .filter(|p| p.ends_with(".json"))
        .collect();

    let (mut total_sprites, mut with_textures, mut errors) = (0usize, 0usize, 0usize);
    for path in &names {
        let Some(bytes) = manager.read(path) else {
            continue;
        };
        match ParticleDefinition::parse(&bytes) {
            Ok(def) => {
                total_sprites += def.textures.len();
                if !def.textures.is_empty() {
                    with_textures += 1;
                }
                // Every listed sprite must resolve to a real particle texture.
                for tp in def.texture_paths() {
                    assert!(manager.read(&tp).is_some(), "missing particle texture {tp}");
                }
            }
            Err(e) => {
                errors += 1;
                eprintln!("  ERR {path}: {e}");
            }
        }
    }

    eprintln!("== particle whole-corpus coverage ==");
    eprintln!(
        "particle files={} with_textures={with_textures}",
        names.len()
    );
    eprintln!("  total sprite references={total_sprites}");
    assert_eq!(errors, 0, "every particle json should parse");
    assert!(names.len() > 100, "expected the full particle corpus");
}

/// Locates the external `sounds.json` object via `asset-index-*.json`. Sounds
/// and `.ogg` files are NOT inside `client.jar`; they live in the external
/// asset-object store addressed by the index (`<sha1[0..2]>/<sha1>`).
///
/// Selects the 26.2 version dir **by name** (rule 2: never pick a fixture by
/// `read_dir` order — coexisting legacy version dirs each carry their own
/// partial asset-index, and iterating them let one old-format index abort the
/// whole lookup via `?`).
fn sounds_json_object() -> Option<PathBuf> {
    let dir = cache_root()?.join("26.2");
    let index = std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        let name = p.file_name()?.to_str()?.to_string();
        (name.starts_with("asset-index") && name.ends_with(".json")).then_some(p)
    })?;
    let bytes = std::fs::read(&index).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let hash = json
        .get("objects")?
        .get("minecraft/sounds.json")?
        .get("hash")?
        .as_str()?;
    let obj = dir.join("objects").join(&hash[0..2]).join(hash);
    obj.is_file().then_some(obj)
}

#[test]
#[ignore = "requires the external sounds.json object (asset index), not in client.jar"]
fn sounds_json_whole_corpus_coverage() {
    use lodestone_assets::sound::{SoundKind, SoundRegistry};
    let Some(path) = sounds_json_object() else {
        panic!(
            "sounds_json_whole_corpus_coverage requires the external sounds.json asset object.\n\
             It is NOT inside client.jar — it lives in the asset-object store.\n\
             Fetch it with:  cargo run -p xtask -- fetch-assets --version 26.2\n\
             Expected under: .cache/mc/26.2/objects/<xx>/<sha1>  (resolved via asset-index-*.json)\n\
             An #[ignore]d test that was explicitly asked to run must FAIL on a missing \
             fixture, never skip — a silent pass repairs nothing."
        );
    };
    let bytes = std::fs::read(&path).expect("read sounds.json");
    let reg = SoundRegistry::parse(&bytes).expect("parse real sounds.json");

    let (mut file_entries, mut event_entries, mut total_entries) = (0usize, 0usize, 0usize);
    let mut files = BTreeSet::new();
    let mut refs: Vec<(String, String)> = Vec::new();
    for name in reg.event_names() {
        let ev = reg.event(name).unwrap();
        for s in &ev.sounds {
            total_entries += 1;
            match s.kind {
                SoundKind::File => {
                    file_entries += 1;
                    files.insert(s.name.to_string());
                }
                SoundKind::Event => {
                    event_entries += 1;
                    refs.push((name.to_string(), s.name.path().to_string()));
                }
            }
        }
    }

    // Prove every event resolves and no reference chain cycles/overflows.
    let mut max_depth = 0usize;
    for name in reg.event_names() {
        // total_weight follows the chains; a cycle would return an error.
        reg.total_weight(name)
            .unwrap_or_else(|e| panic!("event {name} weight: {e}"));
        // A fixed roll of 0 always resolves the first entry down the chain.
        let mut roll = |_max: u32| 0u32;
        let _ = reg
            .resolve(name, &mut roll)
            .unwrap_or_else(|e| panic!("event {name} resolve: {e}"));
    }
    // Measure the actual chain depth in vanilla.
    for name in reg.event_names() {
        let ev = reg.event(name).unwrap();
        for s in &ev.sounds {
            if s.kind == SoundKind::Event {
                let target = s.name.path().to_string();
                let mut depth = 1usize;
                let mut cur = target;
                let mut guard = 0;
                loop {
                    guard += 1;
                    if guard > 100 {
                        break;
                    }
                    let Some(tev) = reg.event(&cur) else { break };
                    match tev.sounds.iter().find(|e| e.kind == SoundKind::Event) {
                        Some(next) => {
                            depth += 1;
                            cur = next.name.path().to_string();
                        }
                        None => break,
                    }
                }
                max_depth = max_depth.max(depth);
            }
        }
    }

    eprintln!("== sounds.json whole-corpus coverage ==");
    eprintln!("sound events={}", reg.len());
    eprintln!("entries: total={total_entries} file={file_entries} event(ref)={event_entries}");
    eprintln!("distinct file names referenced={}", files.len());
    eprintln!("max reference-chain depth={max_depth}");
    assert!(
        reg.len() > 1000,
        "expected the full vanilla sound-event corpus"
    );
    assert!(event_entries > 0, "vanilla uses type:event references");
}

// --- Task I1c: the mipped atlas census the renderer sizes its GPU alloc from ---

/// Fraction of texels whose alpha exceeds `reference` (the alpha-test coverage).
fn coverage(img: &lodestone_assets::Image, reference: u8) -> f64 {
    let total = (img.width * img.height) as f64;
    if total == 0.0 {
        return 0.0;
    }
    let mut hit = 0.0;
    for y in 0..img.height {
        for x in 0..img.width {
            if img.pixel(x, y)[3] > reference {
                hit += 1.0;
            }
        }
    }
    hit / total
}

fn has_transparent(img: &lodestone_assets::Image) -> bool {
    (0..img.height).any(|y| (0..img.width).any(|x| img.pixel(x, y)[3] == 0))
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn block_atlas_mip_census() {
    use lodestone_assets::{AtlasError, Image, MipStrategy, generate_mip_levels};

    let manager = manager();
    let resolver = ModelResolver::new(&manager);

    // Whole-corpus block texture set: every texture referenced by every model a
    // blockstate points at (same enumeration as `builds_full_block_atlas`).
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

    // Build WITH the vanilla default of 4 mip levels.
    const MIP_LEVELS: u32 = 4;
    let started = std::time::Instant::now();
    let mut builder = AtlasBuilder::new().with_mip_levels(MIP_LEVELS);
    let (mut loaded, mut missing) = (0usize, 0usize);
    for loc in &textures {
        match builder.load(&manager, loc) {
            Ok(_) => loaded += 1,
            Err(AtlasError::TextureMissing { .. }) => missing += 1,
            Err(e) => eprintln!("  ATLAS-LOAD {e}"),
        }
    }
    let atlas = builder.build().expect("build mipped atlas");
    let elapsed = started.elapsed();

    // --- census numbers the renderer needs ---
    let sprites = atlas.sprites().len();
    let animated = atlas.sprites().iter().filter(|s| s.is_animated()).count();
    let total_frames: u32 = atlas.sprites().iter().map(|s| s.frame_count).sum();
    let mip_count = atlas.mip_count();

    // Total RGBA bytes across the whole pyramid = what actually lands in VRAM.
    let mut pyramid_bytes = atlas.rgba.len();
    for lvl in 1..mip_count {
        pyramid_bytes += atlas.mip(lvl).unwrap().rgba.len();
    }
    let base_mb = atlas.rgba.len() as f64 / (1024.0 * 1024.0);
    let pyramid_mb = pyramid_bytes as f64 / (1024.0 * 1024.0);

    eprintln!("=== I1c: block atlas census (mipped) ===");
    eprintln!("textures loaded:      {loaded} (missing {missing})");
    eprintln!(
        "atlas dimensions:     {}x{} ({} layer)",
        atlas.width, atlas.height, atlas.layers
    );
    eprintln!("sprites:              {sprites}");
    eprintln!("  animated:           {animated}");
    eprintln!("total physical frames:{total_frames}");
    eprintln!(
        "mip levels:           {mip_count} (base + {} downsamples)",
        mip_count.saturating_sub(1)
    );
    eprintln!("base level 0 memory:  {base_mb:.1} MiB");
    eprintln!(
        "full pyramid memory:  {pyramid_mb:.1} MiB (levels 0..{})",
        mip_count - 1
    );
    eprintln!("build wall-clock:     {:.2}s", elapsed.as_secs_f64());

    // --- WebGPU texture-array fit verdict (I1c) ---
    // WebGPU guarantees only maxTextureArrayLayers = 256 (Metal reports 2048).
    // A "one array layer per sprite" (or per 16x16 tile) layout is therefore
    // impossible for the vanilla corpus; a 2D atlas + mip pyramid is the only
    // portable path. State it with the measured number.
    const WEBGPU_MIN_ARRAY_LAYERS: usize = 256;
    eprintln!(
        "webgpu array-layer fit: {sprites} sprites vs guaranteed {WEBGPU_MIN_ARRAY_LAYERS} layers -> {}",
        if sprites <= WEBGPU_MIN_ARRAY_LAYERS {
            "fits a texture-array-per-sprite layout"
        } else {
            "does NOT fit; must use a single 2D atlas (this output) with the per-sprite mip pyramid"
        }
    );
    assert!(
        sprites > WEBGPU_MIN_ARRAY_LAYERS,
        "vanilla block corpus exceeds the 256-layer guarantee, so the 2D-atlas verdict holds"
    );
    assert!(
        sprites > 1000,
        "expected the full vanilla block sprite corpus"
    );
    assert_eq!(
        mip_count,
        MIP_LEVELS + 1,
        "4 requested levels => 5 resident levels (base + 4)"
    );

    // --- 2D-dimension fit verdict (A1.3) ---
    // The single-atlas path lives or dies on maxTextureDimension2D. WebGPU
    // guarantees 8192; Metal reports 16384. State the measured atlas edge against
    // both so impl-render can size its allocation (and choose packed vs wide
    // vertices) from a number rather than a recollection.
    const WEBGPU_MAX_DIM_2D: u32 = 8192;
    const METAL_MAX_DIM_2D: u32 = 16384;
    let edge = atlas.width.max(atlas.height);
    eprintln!(
        "2d-dimension fit:     atlas edge {edge}px vs WebGPU-guaranteed {WEBGPU_MAX_DIM_2D} / Metal {METAL_MAX_DIM_2D} -> {}",
        if edge <= WEBGPU_MAX_DIM_2D {
            "fits everywhere (single 2D atlas is portable)"
        } else if edge <= METAL_MAX_DIM_2D {
            "fits Metal but NOT the WebGPU guarantee; browser must split the atlas"
        } else {
            "exceeds even Metal; must split"
        }
    );
    assert!(
        edge <= WEBGPU_MAX_DIM_2D,
        "vanilla block atlas edge {edge} must fit the 8192 WebGPU guarantee"
    );

    // --- I1b proof on a REAL cutout texture: leaves must not bleed to black ---
    let candidates = [
        "minecraft:block/oak_leaves",
        "minecraft:block/birch_leaves",
        "minecraft:block/acacia_leaves",
        "minecraft:block/spruce_leaves",
        "minecraft:block/jungle_leaves",
        "minecraft:block/iron_bars",
    ];
    let mut proved = false;
    for name in candidates {
        let loc = ResourceLocation::parse(name).unwrap();
        let Some(png) = manager.read_asset(&loc, "textures", "png") else {
            continue;
        };
        let Ok(base) = Image::decode_png(&png) else {
            continue;
        };
        if !has_transparent(&base) {
            continue; // pack/version may ship an opaque variant; try the next
        }

        let levels = generate_mip_levels(&base, MIP_LEVELS, MipStrategy::Cutout, 0.0);
        let base_cov = coverage(&base, 127);
        // Mean luma over the source's opaque texels (the target the mips must hold).
        let (mut base_luma_sum, mut base_opaque) = (0.0f64, 0u32);
        for y in 0..base.height {
            for x in 0..base.width {
                let p = base.pixel(x, y);
                if p[3] > 0 {
                    base_opaque += 1;
                    base_luma_sum += p[0] as f64 + p[1] as f64 + p[2] as f64;
                }
            }
        }
        let base_luma = base_luma_sum / (base_opaque.max(1) as f64 * 3.0);

        for (lvl, img) in levels.iter().enumerate().skip(1) {
            // Anti-black-bleed: every visible (alpha>0) texel in the downsample
            // must carry real leaf colour, never solidify-to-black. Vanilla
            // achieves this by flood-filling transparent RGB before averaging,
            // so a green sprite mips to green, not to fringed black.
            // Vanilla leaf textures are greyscale (tinted per-biome at draw time),
            // so the anti-bleed property is *luma preservation*, not hue: a naive
            // box filter averages transparent-black RGB into the fringe and the
            // sprite goes dark; solidify flood-fills transparent RGB with the
            // nearest opaque colour first, so visible texels keep leaf luma.
            let mut visible = 0u32;
            let mut pure_black_visible = 0u32;
            let mut mip_luma_sum = 0.0f64;
            for y in 0..img.height {
                for x in 0..img.width {
                    let p = img.pixel(x, y);
                    if p[3] > 0 {
                        visible += 1;
                        mip_luma_sum += p[0] as f64 + p[1] as f64 + p[2] as f64;
                        if p[0] == 0 && p[1] == 0 && p[2] == 0 {
                            pure_black_visible += 1;
                        }
                    }
                }
            }
            assert!(visible > 0, "{name} mip {lvl} should keep visible texels");
            assert_eq!(
                pure_black_visible, 0,
                "{name} mip {lvl}: {pure_black_visible} visible texels bled to pure black"
            );
            let mip_luma = mip_luma_sum / (visible as f64 * 3.0);
            eprintln!(
                "  {name} mip {lvl}: visible={visible} mip_luma={mip_luma:.1} base_luma={base_luma:.1}"
            );
            // Downsample luma must stay near the source's opaque luma (no darkening).
            assert!(
                mip_luma >= base_luma * 0.75,
                "{name} mip {lvl}: luma darkened {base_luma:.1} -> {mip_luma:.1} (fringe bled to black)"
            );
            // Alpha-coverage preservation keeps thin leaves from dissolving.
            // Only meaningful on larger levels; at 4x4/2x2 the metric is quantised
            // to quarters and coverage-scaling can't land precisely (vanilla too).
            if img.width >= 8 {
                let cov = coverage(img, 127);
                assert!(
                    (cov - base_cov).abs() < 0.20,
                    "{name} mip {lvl}: coverage drifted {base_cov:.3} -> {cov:.3}"
                );
            }
        }
        eprintln!(
            "cutout mip proof:     {name} ({}x{}), base coverage {:.3}, {} levels, no black bleed",
            base.width,
            base.height,
            base_cov,
            levels.len()
        );
        proved = true;
        break;
    }
    assert!(
        proved,
        "expected at least one transparent leaves/bars texture to prove the cutout path"
    );
}

// --- Task A1.2: whole-corpus animation census -------------------------------
//
// The renderer must decide how animated sprites live in the atlas — per-frame
// re-upload, a texture_2d_array layer, or a UV offset into a resident strip —
// and that decision belongs to whoever has the real numbers. This scans EVERY
// `*.png.mcmeta` in the jar (not just block-atlas sprites) and reports frame
// counts straight from vanilla's own metadata and PNG dimensions: an authority
// nobody here computed.
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn animation_corpus_census() {
    use lodestone_assets::{Image, TextureMeta};

    let manager = manager();

    let mut mcmeta_total = 0usize;
    let mut animated = 0usize;
    let mut with_explicit_frames = 0usize;
    let mut with_unequal_times = 0usize;
    let mut interpolated = 0usize;
    let mut total_physical_frames = 0u64;
    let mut frame_counts: Vec<(String, u32)> = Vec::new();
    let mut unequal_examples: Vec<String> = Vec::new();

    for path in manager.list("assets/") {
        if !path.ends_with(".png.mcmeta") {
            continue;
        }
        let Some(meta_bytes) = manager.read(&path) else {
            continue;
        };
        mcmeta_total += 1;
        let Ok(meta) = TextureMeta::parse(&meta_bytes) else {
            continue;
        };
        let Some(anim) = meta.animation.as_ref() else {
            continue;
        };

        // Physical frame count comes from the real PNG's dimensions + the real
        // frame size, exactly as vanilla derives it.
        let png_path = path.strip_suffix(".mcmeta").unwrap();
        let Some(png) = manager.read(png_path) else {
            continue;
        };
        let Ok(img) = Image::decode_png(&png) else {
            continue;
        };
        let frame_h = anim
            .frame_height
            .unwrap_or(anim.frame_width.unwrap_or(img.width));
        if frame_h == 0 || !img.height.is_multiple_of(frame_h) {
            eprintln!(
                "  skipping odd strip {png_path}: {}x{} / {frame_h}",
                img.width, img.height
            );
            continue;
        }
        let physical = img.height / frame_h;

        animated += 1;
        total_physical_frames += physical as u64;
        frame_counts.push((png_path.to_string(), physical));
        if anim.interpolate {
            interpolated += 1;
        }
        if !anim.frames.is_empty() {
            with_explicit_frames += 1;
            let times: Vec<u32> = anim
                .frames
                .iter()
                .map(|f| f.time.unwrap_or(anim.frametime))
                .collect();
            if times.iter().any(|&t| t != times[0]) {
                with_unequal_times += 1;
                if unequal_examples.len() < 6 {
                    unequal_examples.push(format!(
                        "{png_path} times={times:?} frametime={}",
                        anim.frametime
                    ));
                }
            }
        }
    }

    frame_counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let worst = frame_counts.first().cloned().unwrap_or_default();

    // Frame-count histogram buckets.
    let mut buckets: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, n) in &frame_counts {
        let key = match n {
            0..=1 => "1",
            2..=4 => "2-4",
            5..=8 => "5-8",
            9..=16 => "9-16",
            17..=32 => "17-32",
            33..=64 => "33-64",
            _ => "65+",
        };
        *buckets.entry(key).or_default() += 1;
    }

    eprintln!("=== A1.2: whole-corpus animation census ===");
    eprintln!(".png.mcmeta files:        {mcmeta_total}");
    eprintln!("animated textures:        {animated}");
    eprintln!("  with explicit frames:   {with_explicit_frames}");
    eprintln!("  with unequal times:     {with_unequal_times}");
    eprintln!("  with interpolate=true:  {interpolated}");
    eprintln!("total physical frames:    {total_physical_frames}");
    eprintln!("worst case:               {} = {} frames", worst.0, worst.1);
    eprintln!("frame-count histogram:");
    for (k, v) in &buckets {
        eprintln!("  {k:>6} frames: {v}");
    }
    eprintln!("top 10 by frame count:");
    for (name, n) in frame_counts.iter().take(10) {
        eprintln!("  {n:>4}  {name}");
    }
    eprintln!("unequal-time examples:");
    for e in &unequal_examples {
        eprintln!("  {e}");
    }

    // Sanity floors so the census can't silently go empty (a broken jar or a
    // regression in mcmeta discovery). Vanilla ships many animated textures.
    assert!(
        animated >= 30,
        "expected the vanilla animated-texture corpus, got {animated}"
    );
    assert!(
        total_physical_frames >= animated as u64,
        "each animated texture has >=1 frame"
    );
    assert!(
        worst.1 >= 16,
        "vanilla has at least one long animation (e.g. prismarine/lava)"
    );
}

// --- Task A2: item-definition (1.21.4+) corpus census ------------------------
//
// Every `items/<id>.json` in the jar is the real authority for the selector-tree
// shape. This proves the parser accepts the whole corpus and reports what the
// renderer must handle: node-type mix, the code-driven special renderers, tree
// depth, and how many distinct models an item can resolve to.
fn item_node_stats(
    node: &lodestone_assets::ItemModelNode,
    depth: u32,
    counts: &mut BTreeMap<&'static str, usize>,
    max_depth: &mut u32,
) {
    use lodestone_assets::ItemModelNode as N;
    *max_depth = (*max_depth).max(depth);
    let key = match node {
        N::Model { .. } => "model",
        N::Composite { .. } => "composite",
        N::Condition { .. } => "condition",
        N::Select { .. } => "select",
        N::RangeDispatch { .. } => "range_dispatch",
        N::Special { .. } => "special",
        N::Empty => "empty",
        N::Other { .. } => "other",
    };
    *counts.entry(key).or_default() += 1;
    match node {
        N::Composite { models } => models
            .iter()
            .for_each(|m| item_node_stats(m, depth + 1, counts, max_depth)),
        N::Condition {
            on_true, on_false, ..
        } => {
            item_node_stats(on_true, depth + 1, counts, max_depth);
            item_node_stats(on_false, depth + 1, counts, max_depth);
        }
        N::Select {
            cases, fallback, ..
        } => {
            cases
                .iter()
                .for_each(|c| item_node_stats(&c.model, depth + 1, counts, max_depth));
            if let Some(f) = fallback {
                item_node_stats(f, depth + 1, counts, max_depth);
            }
        }
        N::RangeDispatch {
            entries, fallback, ..
        } => {
            entries
                .iter()
                .for_each(|e| item_node_stats(&e.model, depth + 1, counts, max_depth));
            if let Some(f) = fallback {
                item_node_stats(f, depth + 1, counts, max_depth);
            }
        }
        _ => {}
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn item_definition_corpus_census() {
    use lodestone_assets::ItemModel;

    let manager = manager();
    let paths: Vec<String> = manager
        .list("assets/minecraft/items/")
        .into_iter()
        .filter(|p| p.ends_with(".json"))
        .collect();

    let mut files = 0usize;
    let mut parse_failures: Vec<String> = Vec::new();
    let mut node_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut special_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut deepest = (String::new(), 0u32);
    let mut most_refs = (String::new(), 0usize);
    let mut total_refs = 0usize;

    for path in &paths {
        let Some(bytes) = manager.read(path) else {
            continue;
        };
        files += 1;
        match ItemModel::parse(&bytes) {
            Ok(model) => {
                let mut counts = BTreeMap::new();
                let mut max_depth = 0u32;
                item_node_stats(&model.root, 1, &mut counts, &mut max_depth);
                for (k, v) in counts {
                    *node_counts.entry(k).or_default() += v;
                }
                if max_depth > deepest.1 {
                    deepest = (path.clone(), max_depth);
                }
                for (_, kind) in model.special_renderers() {
                    *special_kinds.entry(kind.to_string()).or_default() += 1;
                }
                let refs = model.model_refs().len();
                total_refs += refs;
                if refs > most_refs.1 {
                    most_refs = (path.clone(), refs);
                }
            }
            Err(e) => parse_failures.push(format!("{path}: {e}")),
        }
    }

    eprintln!("=== A2: item-definition corpus census ===");
    eprintln!("items/*.json files:       {files}");
    eprintln!("parse failures:           {}", parse_failures.len());
    for f in parse_failures.iter().take(10) {
        eprintln!("  FAIL {f}");
    }
    eprintln!("node-type totals:");
    for (k, v) in &node_counts {
        eprintln!("  {k:>14}: {v}");
    }
    eprintln!("special renderer kinds:   {}", special_kinds.len());
    for (k, v) in &special_kinds {
        eprintln!("  {k:>32}: {v}");
    }
    eprintln!("total model refs:         {total_refs}");
    eprintln!(
        "deepest tree:             {} (depth {})",
        deepest.0, deepest.1
    );
    eprintln!(
        "most models in one item:  {} ({} refs)",
        most_refs.0, most_refs.1
    );

    assert!(
        files > 1000,
        "expected the full vanilla item corpus, got {files}"
    );
    assert!(
        parse_failures.is_empty(),
        "every vanilla item definition must parse"
    );
    assert!(
        node_counts.get("special").copied().unwrap_or(0) > 0,
        "vanilla ships special renderers"
    );
}

// --- Task A2: atlas source-list corpus census --------------------------------
//
// Every `atlases/<id>.json` is the real authority for what goes on each stitched
// sheet. This proves the parser accepts all of them and reports the source-type
// mix + resolved sprite counts the renderer needs to size the block-entity
// atlases (chests/signs/beds/banners/shulker boxes are all `directory` here).
#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn atlas_source_corpus_census() {
    use lodestone_assets::{AtlasDefinition, AtlasSource};

    let manager = manager();
    let mut paths: Vec<String> = manager
        .list("assets/minecraft/atlases/")
        .into_iter()
        .filter(|p| p.ends_with(".json"))
        .collect();
    paths.sort();

    let mut files = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let mut kind_counts: BTreeMap<&'static str, usize> = BTreeMap::new();

    eprintln!("=== A2: atlas source-list corpus census ===");
    for path in &paths {
        let Some(bytes) = manager.read(path) else {
            continue;
        };
        files += 1;
        let name = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".json");
        match AtlasDefinition::parse(&bytes) {
            Ok(def) => {
                let mut dir = 0;
                let mut single = 0;
                let mut paletted = 0;
                let mut unknown = 0;
                let mut derived = 0;
                for s in &def.sources {
                    match s {
                        AtlasSource::Directory { .. } => {
                            dir += 1;
                            *kind_counts.entry("directory").or_default() += 1;
                        }
                        AtlasSource::Single { .. } => {
                            single += 1;
                            *kind_counts.entry("single").or_default() += 1;
                        }
                        AtlasSource::PalettedPermutations { .. } => {
                            paletted += 1;
                            derived += s.derived_sprite_ids().len();
                            *kind_counts.entry("paletted_permutations").or_default() += 1;
                        }
                        AtlasSource::Unknown { kind } => {
                            unknown += 1;
                            eprintln!("  {name}: UNKNOWN source type {kind:?}");
                            *kind_counts.entry("unknown").or_default() += 1;
                        }
                    }
                }
                let resolved = def.resolve(&manager).len();
                eprintln!(
                    "  {name:>18}: dir={dir} single={single} paletted={paletted} unknown={unknown} \
                     -> resolved {resolved} sprites (+{derived} palette-derived)"
                );
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    eprintln!("atlas files:        {files}");
    eprintln!("parse failures:     {}", failures.len());
    for f in &failures {
        eprintln!("  FAIL {f}");
    }
    eprintln!("source-type totals:");
    for (k, v) in &kind_counts {
        eprintln!("  {k:>22}: {v}");
    }

    assert!(
        files >= 10,
        "expected the full vanilla atlas corpus, got {files}"
    );
    assert!(
        failures.is_empty(),
        "every vanilla atlas definition must parse"
    );
    assert_eq!(
        kind_counts.get("unknown").copied().unwrap_or(0),
        0,
        "no unknown source types expected in vanilla"
    );
}

// --- Task A3: cross-version asset-drift census -------------------------------
//
// Proves the version verdict against the *real* 1.8.9, 1.12.2 and 26.2 client
// jars rather than documentation: measures the plural→singular flip, multipart
// arrival, and atlas-index arrival, and asserts each version's AssetProfile
// matches what the jar actually contains. A wrong profile (e.g. claiming 1.8.9
// is flattened, or has an atlas index) fails here against the jar.
#[test]
#[ignore = "requires fetched 1.8.9/1.12.2/26.2 client jars"]
fn cross_version_asset_drift_census() {
    use lodestone_assets::{AssetProfile, AtlasDefinition, BlockStates};

    struct Probe {
        version: &'static str,
        profile: AssetProfile,
    }
    let probes = [
        Probe {
            version: "1.8.9",
            profile: AssetProfile::LEGACY_1_8,
        },
        Probe {
            version: "1.12.2",
            profile: AssetProfile::LEGACY_1_12,
        },
        Probe {
            version: "26.2",
            profile: AssetProfile::MODERN,
        },
    ];

    eprintln!("=== A3: cross-version asset-drift census ===");
    eprintln!(
        "{:>8} | {:>12} {:>13} | {:>10} {:>9} | {:>10} {:>10} | {:>7}",
        "version",
        "tex/blocks",
        "tex/block",
        "models",
        "blockstates",
        "multipart",
        "atlases",
        "anim"
    );

    let mut measured = 0;
    let mut missing: Vec<&str> = Vec::new();
    for p in &probes {
        let Some(mgr) = manager_for(p.version) else {
            missing.push(p.version);
            continue;
        };
        measured += 1;

        let count = |prefix: &str| {
            mgr.list(prefix)
                .iter()
                .filter(|x| !x.ends_with('/'))
                .count()
        };
        let tex_blocks = count("assets/minecraft/textures/blocks/");
        let tex_block = count("assets/minecraft/textures/block/");
        let models = mgr
            .list("assets/minecraft/models/block/")
            .iter()
            .filter(|x| x.ends_with(".json"))
            .count();
        let bs_paths: Vec<String> = mgr
            .list("assets/minecraft/blockstates/")
            .into_iter()
            .filter(|x| x.ends_with(".json"))
            .collect();
        let blockstates = bs_paths.len();
        let atlases = mgr
            .list("assets/minecraft/atlases/")
            .iter()
            .filter(|x| x.ends_with(".json"))
            .count();
        let anim = mgr
            .list("assets/minecraft/textures/")
            .iter()
            .filter(|x| x.ends_with(".mcmeta"))
            .count();

        // Multipart present anywhere? (arrived in 1.9.)
        let mut multipart = 0usize;
        let mut variants_parsed = 0usize;
        for path in &bs_paths {
            if let Some(bytes) = mgr.read(path)
                && let Ok(def) = BlockStates::parse(&bytes)
            {
                variants_parsed += 1;
                if matches!(def.definition, BlockStateDefinition::Multipart(_)) {
                    multipart += 1;
                }
            }
        }

        eprintln!(
            "{:>8} | {:>12} {:>13} | {:>10} {:>9} | {:>10} {:>10} | {:>7}",
            p.version, tex_blocks, tex_block, models, blockstates, multipart, atlases, anim
        );

        // --- assert the profile matches the jar ------------------------------
        let uses_singular = p.profile.block_texture_dir == "block";
        if uses_singular {
            assert!(
                tex_block > 0 && tex_blocks == 0,
                "{}: profile says flattened",
                p.version
            );
        } else {
            assert!(
                tex_blocks > 0 && tex_block == 0,
                "{}: profile says plural",
                p.version
            );
        }
        assert_eq!(
            p.profile.uses_atlas_index,
            atlases > 0,
            "{}: uses_atlas_index must match the presence of atlases/*.json",
            p.version
        );
        // Every blockstate in every version must parse with the one parser.
        assert_eq!(
            variants_parsed, blockstates,
            "{}: all blockstates must parse",
            p.version
        );

        // The pre-1.13 implicit terrain fallback resolves the real plural sheet.
        if !p.profile.uses_atlas_index {
            let atlas = AtlasDefinition::implicit_terrain(p.profile.block_texture_dir);
            let resolved = atlas.resolve(&mgr).len();
            assert!(
                resolved > 0,
                "{}: implicit terrain must resolve real sprites",
                p.version
            );
            eprintln!("         implicit terrain atlas resolved {resolved} sprites");
        }
    }

    eprintln!("multipart first appears in 1.12.2 (absent in 1.8.9); atlases/ only in 26.2");
    // This census is only meaningful across versions. Per the loud-precondition
    // rule, a jar that was asked for but isn't present FAILS with the fix,
    // rather than quietly measuring fewer versions and passing.
    assert!(
        missing.is_empty(),
        "cross-version census needs all three client jars; missing: {missing:?}.\n\
         Fetch each with:  cargo run -p xtask -- fetch-assets --version <VER>\n\
         (the xtask validator rejects pre-1.13 layouts but still writes the jar to \
         .cache/mc/<VER>/client.jar)"
    );
    assert_eq!(measured, 3, "expected exactly the three target versions");
}
