//! The integrated server's world generators.
//!
//! Every world type and dimension the server hosts is built here.
//! [`overworld_chunk_source_of_type`], [`nether_chunk_source`],
//! [`end_chunk_source`] and [`single_biome_chunk_source`] build a
//! [`crate::chunk::Terrain263ChunkSource`] over the bundled 26.3 tables, with
//! structures attached. [`flat_chunk_source`] and [`debug_chunk_source`] serve
//! the two presets that need no noise. [`overworld_chunk_source_override`]
//! picks between them from a saved world's own generator settings.
//!
//! The module also embeds the structure templates, per-biome mob spawn tables
//! and fire-burnout biome list the server reads at run time, and the scope gate
//! ([`bundled_worldgen_serves`], [`overworld_chunk_source_checked`]) a hosting
//! protocol consults before it serves this terrain.
//!
//! The data lives in this crate rather than a version crate because the version
//! crates already depend on `lodestone-server`; the reverse edge would be a
//! dependency cycle.

use std::sync::OnceLock;

use lodestone_data::block::Block;
use lodestone_data::block_states::air_state;
use lodestone_worldgen::density::Resolver;
use lodestone_worldgen::table_resolver::TableResolver;
use serde_json::Value;

use crate::protocol::WorldgenScope;

include!(concat!(env!("OUT_DIR"), "/embedded_worldgen.rs"));
// `EMBEDDED_STRUCTURE_TEMPLATES` — the `.nbt` bytes, sorted by key. See
// `TableResolver::with_structure_templates`.
include!(concat!(env!("OUT_DIR"), "/embedded_structures.rs"));

/// The raw `.nbt` bytes of one bundled structure template, borrowed from rodata.
///
/// The same table `embedded_resolver` serves the worldgen
/// engine from, exposed without its owning `Vec` copy because
/// [`crate::structure_loot`] only reads: it re-parses these bytes for the data
/// markers the engine's own parser drops. Accepts an id with or without the
/// `minecraft:` prefix.
#[must_use]
pub fn embedded_structure_template(id: &str) -> Option<&'static [u8]> {
    let name = id.strip_prefix("minecraft:").unwrap_or(id);
    EMBEDDED_STRUCTURE_TEMPLATES
        .binary_search_by(|(key, _)| (*key).cmp(name))
        .ok()
        .map(|i| EMBEDDED_STRUCTURE_TEMPLATES[i].1)
}

/// Every bundled structure-template id, without the `minecraft:` prefix.
///
/// Exposed for whole-corpus gates: [`crate::structure_loot`]'s self-named-loot pass
/// reads a table id out of each template's own bytes rather than from a
/// per-structure table, so "which of those tables do we actually bundle" is only
/// answerable by walking every template. A gate that scanned a hand-picked list
/// instead could not see a structure whose templates were added later — the same
/// in-scope/out-of-scope hole CLAUDE.md's drift-gate rule names.
#[cfg(test)]
pub fn embedded_structure_template_ids() -> impl Iterator<Item = &'static str> {
    EMBEDDED_STRUCTURE_TEMPLATES.iter().map(|(key, _)| *key)
}

/// The worldgen data scope satisfied by the embedded `assets/worldgen/` bundle.
/// This crate embeds only 26.2 data (protocol 776).
///
/// The version gate is [`bundled_worldgen_serves`] compared against the
/// hosting protocol's own report
/// ([`crate::protocol::ServerProtocol::worldgen_scope`],
/// [`WorldgenScope`](crate::protocol::WorldgenScope)): a family that hosts with
/// anything other than this bundle must not be served this data.
pub const BUNDLED_WORLDGEN_SCOPE: WorldgenScope = WorldgenScope::V26_3;

/// Whether the embedded worldgen bundle can serve a hosting protocol that
/// reports `scope` — the version gate itself.
///
/// True for exactly [`BUNDLED_WORLDGEN_SCOPE`]. A protocol reporting
/// [`WorldgenScope::None`] — no worldgen, or a family whose data this crate
/// does not embed — resolves to false: it must supply its own generator, and
/// must never be handed the 26.2 terrain as a silent default. The consumer is
/// the future hosting path (`integrated.rs`'s chunk source construction, which
/// currently lives behind another agent); until it lands, the gate is pinned
/// by [`tests::bundled_worldgen_gate_serves_v26_2_and_refuses_none`].
#[must_use]
pub fn bundled_worldgen_serves(scope: WorldgenScope) -> bool {
    scope == BUNDLED_WORLDGEN_SCOPE
}

/// Why [`overworld_chunk_source_checked`] refused — the hosting protocol's
/// own reported [`WorldgenScope`] does not match what this crate embeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the embedded worldgen bundle serves only {bundle:?}, but the hosting protocol reports {requested:?}",
    bundle = BUNDLED_WORLDGEN_SCOPE
)]
pub struct WorldgenScopeMismatch {
    /// What the hosting protocol actually reported.
    pub requested: WorldgenScope,
}

/// [`overworld_chunk_source`], gated by [`bundled_worldgen_serves`]: a hosting
/// family whose [`crate::protocol::ServerProtocol::worldgen_scope`] does not
/// match [`BUNDLED_WORLDGEN_SCOPE`] is refused rather than handed terrain it
/// never declared it could serve.
///
/// # Errors
///
/// [`WorldgenScopeMismatch`] when `scope` is not [`BUNDLED_WORLDGEN_SCOPE`].
pub fn overworld_chunk_source_checked(
    scope: WorldgenScope,
    seed: i64,
) -> Result<crate::chunk::Terrain263ChunkSource, WorldgenScopeMismatch> {
    if bundled_worldgen_serves(scope) {
        Ok(overworld_chunk_source(seed))
    } else {
        Err(WorldgenScopeMismatch { requested: scope })
    }
}

/// Builds the production resolver over this crate's generated 26.2 tables.
///
/// This is the one version seam for Overworld, Nether and End generation:
/// `TableResolver` owns every JSON and template lookup, while this crate only
/// supplies its version-specific asset tables and the compiled block-state
/// census that cannot be represented in a datapack. The returned resolver is a
/// cheap clone of one process-wide view whose parsed documents are retained
/// across generator boundaries.
fn embedded_resolver() -> TableResolver<'static> {
    static RESOLVER: OnceLock<TableResolver<'static>> = OnceLock::new();
    RESOLVER
        .get_or_init(|| {
            let base = TableResolver::new(EMBEDDED_WORLDGEN)
                .with_structure_templates(EMBEDDED_STRUCTURE_TEMPLATES)
                .with_block_freeze_facts(freeze_facts)
                .with_block_survival_facts(survival_facts);
            let cache = base.json_cache();
            base.with_json_cache(cache)
        })
        .clone()
}

/// The bundled `terrain_adaptation` of the structure `id` (a resource key).
/// A structure this build does not bundle has none.
#[must_use]
pub(crate) fn bundled_structure_terrain_adjustment(
    id: &str,
) -> lodestone_worldgen::structure::TerrainAdjustment {
    lodestone_worldgen::structure::TerrainAdjustment::parse(
        &embedded_resolver().structure(id)["terrain_adaptation"],
    )
}

/// The legacy-solid column (default answers and state overrides) for the
/// bundled generator's release: the one census fact structure feature
/// placement reads, for the `minecraft:solid` block predicate.
fn freeze_facts() -> &'static Value {
    static FACTS: OnceLock<Value> = OnceLock::new();
    FACTS.get_or_init(|| {
        use lodestone_data::block_solidity;
        let version = lodestone_data::version::GameDataVersion::V26_2;

        type Reader = fn(lodestone_data::block_states::StateId) -> bool;
        const COLUMNS: [(&str, Reader); 1] = [("solid", block_solidity::legacy_solid)];

        let mut default_answers: std::collections::HashMap<&'static str, [bool; COLUMNS.len()]> =
            std::collections::HashMap::new();
        for id in 0..version.state_count() {
            let state = version.state_from_wire(id).expect("bundled state mapping is total");
            if version.default_state(state.block()) != Some(state) {
                continue;
            }
            let name = state.name();
            let answers = std::array::from_fn(|c| COLUMNS[c].1(state));
            default_answers.insert(name, answers);
        }

        let mut defaults: [Vec<&'static str>; COLUMNS.len()] = Default::default();
        let mut overrides: [serde_json::Map<String, Value>; COLUMNS.len()] = Default::default();
        for (name, answers) in &default_answers {
            for (c, &answer) in answers.iter().enumerate() {
                if answer {
                    defaults[c].push(name);
                }
            }
        }
        for id in 0..version.state_count() {
            let state = version.state_from_wire(id).expect("bundled state mapping is total");
            let name = state.name();
            let default = default_answers
                .get(name)
                .unwrap_or_else(|| panic!("block {name} has no default state in the census"));
            for (c, &(_, read)) in COLUMNS.iter().enumerate() {
                let answer = read(state);
                if answer != default[c] {
                    overrides[c].insert(canonical_state(state.raw()), Value::Bool(answer));
                }
            }
        }

        let mut out = serde_json::Map::new();
        for (c, (column, _)) in COLUMNS.iter().enumerate() {
            let mut names: Vec<&str> = defaults[c].clone();
            names.sort_unstable();
            out.insert(
                (*column).to_owned(),
                serde_json::json!({
                    "default": names,
                    "states": Value::Object(overrides[c].clone()),
                }),
            );
        }
        Value::Object(out)
    })
}

/// Builds [`Resolver::block_survival_facts`]'s exact state-predicate document.
///
/// The engine's temporary state handles are private to each generator, so this
/// bridge is deliberately keyed by canonical state spelling: the default answer
/// for each block plus every state that differs. The source arrays remain compact
/// global-state bitsets in `lodestone-data`; the server is the version seam that
/// may translate them into the version-free generator representation.
fn survival_facts() -> &'static Value {
    static FACTS: OnceLock<Value> = OnceLock::new();
    FACTS.get_or_init(|| {
        use lodestone_data::block_survival;
        let version = lodestone_data::version::GameDataVersion::V26_2;

        type Reader = fn(lodestone_data::block_states::StateId) -> bool;
        const COLUMNS: [(&str, Reader); 4] = [
            ("solid_render", block_survival::solid_render),
            ("sturdy_up", block_survival::sturdy_up),
            ("center_support_down", block_survival::center_support_down),
            ("fire_flammable", block_survival::fire_flammable),
        ];
        let mut default_answers: std::collections::HashMap<&'static str, [bool; COLUMNS.len()]> =
            std::collections::HashMap::new();
        for id in 0..version.state_count() {
            let state = version.state_from_wire(id).expect("bundled state mapping is total");
            if version.default_state(state.block()) == Some(state) {
                default_answers.insert(
                    state.name(),
                    std::array::from_fn(|c| COLUMNS[c].1(state)),
                );
            }
        }
        assert_eq!(default_answers.len(), version.block_count() as usize);

        let mut defaults: [Vec<&'static str>; COLUMNS.len()] = Default::default();
        let mut overrides: [serde_json::Map<String, Value>; COLUMNS.len()] = Default::default();
        for (name, answers) in &default_answers {
            for (column, &answer) in answers.iter().enumerate() {
                if answer {
                    defaults[column].push(name);
                }
            }
        }
        for id in 0..version.state_count() {
            let state = version.state_from_wire(id).expect("bundled state mapping is total");
            let default = default_answers
                .get(state.name())
                .expect("every state belongs to a block with a default state");
            for (column, &(_, read)) in COLUMNS.iter().enumerate() {
                let answer = read(state);
                if answer != default[column] {
                    overrides[column].insert(canonical_state(state.raw()), Value::Bool(answer));
                }
            }
        }

        let mut out = serde_json::Map::new();
        for (column, (name, _)) in COLUMNS.iter().enumerate() {
            let mut default = defaults[column].clone();
            default.sort_unstable();
            out.insert(
                (*name).to_owned(),
                serde_json::json!({
                    "default": default,
                    "states": Value::Object(overrides[column].clone()),
                }),
            );
        }
        Value::Object(out)
    })
}

/// The canonical block-state string for `id` — name plus alphabetically-sorted
/// `key=value` properties, the exact spelling
/// `lodestone_worldgen::feature::canon_state` produces and the generator's block
/// field holds.
fn canonical_state(id: u32) -> String {
    use lodestone_data::block_states;
    let name = block_states::block_name(id).expect("every state has a block name");
    let props = block_states::properties(id).expect("every state has a property list");
    if props.is_empty() {
        return name.to_owned();
    }
    // `block_states::properties` already returns a sorted slice (see its doc), so
    // no re-sort is needed — but the join must not assume that silently.
    debug_assert!(
        props.windows(2).all(|w| w[0].0 <= w[1].0),
        "block_states::properties is documented sorted; {name} is not"
    );
    let body: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{name}[{}]", body.join(","))
}

/// Which Overworld noise settings a world uses: the default, Amplified or Large Biomes. All
/// three share the Overworld's biome list and build range. Flat, single-biome and debug worlds
/// are separate generators with their own entry points ([`flat_chunk_source`],
/// [`single_biome_chunk_source`], [`debug_chunk_source`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WorldType {
    #[default]
    Overworld,
    Amplified,
    LargeBiomes,
}

/// Builds the bundled overworld generator for `seed`.
///
/// This is the synchronous direct-call entry point the shell uses to render a
/// real world. It reuses the parsed settings but rebuilds the seed-dependent
/// density/noise state per call, so callers should build it once per world and
/// reuse it across chunks.
/// The seed of the last world source built here. See
/// [`active_world_seed`].
static ACTIVE_WORLD_SEED: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// The world seed of the most recently built bundled generator.
///
/// **This exists because the world seed does not reach the tick loop by any other
/// route, and one thing in there needs it**: `WorldgenRandom.seedSlimeChunk`, so
/// [`crate::natural_spawn::NaturalSpawner`] can tell a slime chunk from an
/// ordinary one. `crate::tick::run_tick_loop` is handed an `Arc<W: ChunkSource>`,
/// and [`ChunkSource`](crate::chunk::ChunkSource) has no `world_seed()` — the
/// generator that knows the number is behind that trait object, built by the
/// *shell* and passed in already erased.
///
/// It is a process-global rather than a parameter deliberately, and the trade is
/// worth writing down:
///
/// * **The right fix is a `ChunkSource::world_seed()` default method**, next to
///   `world_registries()`, which is the same shape of question. That is a
///   one-method addition to `crate::chunk` plus one override on
///   `OverworldChunkSource`, and it would delete this static.
/// * Threading it as a parameter instead means a new argument on
///   `run_tick_loop`, `run_tick_loop_with_weather` and all twelve of their call
///   sites across four files — a much larger diff for the same result.
/// * **The failure mode of the global is confined and benign.** Two worlds open in
///   one process (a `bind` LAN world beside an in-memory one, or two tests) leave
///   the *last* seed here, so a spawn cycle could consult the other world's slime
///   chunks. Nothing else reads it, so the blast radius is "slimes spawn in the
///   wrong chunks in the non-last of two simultaneous worlds" — wrong, but not
///   corrupting, and not reachable from the single-world singleplayer path this
///   server actually ships.
#[must_use]
pub fn active_world_seed() -> i64 {
    ACTIVE_WORLD_SEED.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`bundled_biome_spawners`] indexed by built-in biome, the table the generation-time spawn
/// stage reads.
pub(crate) fn bundled_spawners_by_builtin()
-> &'static [Option<lodestone_worldgen::spawners::BiomeSpawners>; lodestone_data::biomes::BuiltinBiome::COUNT as usize] {
    static TABLE: OnceLock<[Option<lodestone_worldgen::spawners::BiomeSpawners>; lodestone_data::biomes::BuiltinBiome::COUNT as usize]> =
        OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = std::array::from_fn(|_| None);
        for (name, spawners) in bundled_biome_spawners() {
            if let Some(biome) = lodestone_data::biomes::BuiltinBiome::from_name(name) {
                table[biome as usize] = Some(spawners.clone());
            }
        }
        table
    })
}

/// Every bundled biome's parsed spawn settings, biome name to settings —
/// what [`crate::natural_spawn::NaturalSpawner`] consults to answer "what spawns
/// in this biome".
///
/// Parsed once from the embedded `biome/*.json` documents and cached: the spawn
/// lists do not depend on the seed, and the tick loop that needs them holds a
/// [`ChunkSource`](crate::ChunkSource), not a generator.
#[must_use]
pub fn bundled_biome_spawners()
-> &'static std::collections::HashMap<String, lodestone_worldgen::spawners::BiomeSpawners> {
    static TABLE: OnceLock<
        std::collections::HashMap<String, lodestone_worldgen::spawners::BiomeSpawners>,
    > = OnceLock::new();
    TABLE.get_or_init(|| {
        EMBEDDED_WORLDGEN
            .iter()
            .filter_map(|(id, _)| id.strip_prefix("biome/"))
            .filter_map(|name| {
                let document = embedded_resolver().biome_document(name);
                let spawners = lodestone_worldgen::spawners::parse_biome_spawners(&document);
                (!spawners.is_empty()).then(|| (format!("minecraft:{name}"), spawners))
            })
            .collect()
    })
}

/// Whether `biome` (a `minecraft:`-prefixed name) sets the
/// `gameplay/increased_fire_burnout` attribute, read from the bundled biome
/// documents and cached. Fire in such a biome burns out faster and spreads
/// more readily.
#[must_use]
pub fn increased_fire_burnout(biome: &str) -> bool {
    static SET: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        EMBEDDED_WORLDGEN
            .iter()
            .filter_map(|(id, _)| id.strip_prefix("biome/"))
            .filter(|name| {
                embedded_resolver().biome_document(name)["attributes"]
                    ["minecraft:gameplay/increased_fire_burnout"]
                    .as_bool()
                    == Some(true)
            })
            .map(|name| format!("minecraft:{name}"))
            .collect()
    })
    .contains(biome)
}

/// Builds the Overworld [`ChunkSource`](crate::ChunkSource) for `seed` with the default world
/// type.
#[must_use]
pub fn overworld_chunk_source(seed: i64) -> crate::chunk::Terrain263ChunkSource {
    overworld_chunk_source_of_type(seed, WorldType::Overworld)
}

/// Builds the Overworld [`ChunkSource`](crate::ChunkSource) for `seed`: the 26.3 noise
/// fill, surface rules, carvers, biomes and placed-feature decoration
/// (`lodestone_worldgen::terrain263`), the generator a 26.3 world is played in.
///
/// # Panics
/// If the bundled 26.3 data fails to compile, which is a build defect.
#[must_use]
pub fn overworld_chunk_source_of_type(seed: i64, world_type: WorldType) -> crate::chunk::Terrain263ChunkSource {
    // The world's root generator: the Nether and End siblings and the slime-chunk test read
    // the seed from here.
    ACTIVE_WORLD_SEED.store(seed, std::sync::atomic::Ordering::Relaxed);
    let settings = match world_type {
        WorldType::Overworld => "overworld",
        WorldType::Amplified => "amplified",
        WorldType::LargeBiomes => "large_biomes",
    };
    let terrain = lodestone_worldgen::terrain263::Terrain263::with_settings(seed, settings)
        .expect("the bundled 26.3 worldgen data compiles")
        .with_structures(&embedded_resolver());
    crate::chunk::Terrain263ChunkSource::from_terrain(std::sync::Arc::new(terrain), crate::dimension::Dimension::Overworld)
}

/// Builds the Nether [`ChunkSource`](crate::ChunkSource) for `seed`, with its structures.
///
/// # Panics
/// If the bundled 26.3 data fails to compile, which is a build defect.
#[must_use]
pub fn nether_chunk_source(seed: i64) -> crate::chunk::Terrain263ChunkSource {
    dimension_chunk_source(seed, "nether", crate::dimension::Dimension::Nether)
}

/// Builds the End [`ChunkSource`](crate::ChunkSource) for `seed`, with its structures.
///
/// # Panics
/// If the bundled 26.3 data fails to compile, which is a build defect.
#[must_use]
pub fn end_chunk_source(seed: i64) -> crate::chunk::Terrain263ChunkSource {
    dimension_chunk_source(seed, "end", crate::dimension::Dimension::End)
}

fn dimension_chunk_source(seed: i64, settings: &str, dimension: crate::dimension::Dimension) -> crate::chunk::Terrain263ChunkSource {
    let terrain = lodestone_worldgen::terrain263::Terrain263::with_settings(seed, settings)
        .expect("the bundled 26.3 worldgen data compiles")
        .with_structures(&embedded_resolver());
    crate::chunk::Terrain263ChunkSource::from_terrain(std::sync::Arc::new(terrain), dimension)
}

/// Wraps any generated [`crate::ChunkSource`] in the server's normal bounded
/// resident-column cache for `view_radius`.
///
/// This is the public construction seam for tools that generate a finite
/// world area and then encode it through the same retained-source layer as a
/// hosted connection. The opaque return keeps [`crate::chunk_store::ChunkStore`]
/// private: callers need the [`crate::ChunkSource`] behaviour, not cache
/// internals or a second retention implementation.
///
/// `view_radius` uses the hosted capacity policy. Integrated singleplayer uses
/// its own uncapped constructor because it spends the local player's memory.
#[must_use]
pub fn retained_chunk_source_for_view_radius<S: crate::ChunkSource>(
    source: S,
    view_radius: i32,
) -> impl crate::ChunkSource {
    crate::chunk_store::ChunkStore::for_view_radius(source, view_radius)
}

/// `world_preset/single_biome_surface.json`'s embedded overworld
/// `biome_source.biome` — the biome a player gets if they pick this preset
/// without customizing it (`"minecraft:plains"`).
#[must_use]
pub fn world_preset_single_biome_default_biome() -> String {
    let doc = embedded_resolver().document("world_preset/single_biome_surface");
    doc["dimensions"]["minecraft:overworld"]["generator"]["biome_source"]["biome"]
        .as_str()
        .expect("world_preset/single_biome_surface.json must name a fixed biome")
        .to_string()
}

/// Builds the `single_biome_surface` [`ChunkSource`](crate::chunk::ChunkSource) for
/// `seed`: the Overworld's terrain with `biome` (`minecraft:plains`, ...) everywhere.
///
/// # Panics
/// If `biome` names no Overworld biome, or the bundled 26.3 data fails to compile.
#[must_use]
pub fn single_biome_chunk_source(seed: i64, biome: &str) -> crate::chunk::Terrain263ChunkSource {
    ACTIVE_WORLD_SEED.store(seed, std::sync::atomic::Ordering::Relaxed);
    let terrain = lodestone_worldgen::terrain263::Terrain263::with_fixed_biome(seed, biome)
        .expect("the bundled 26.3 worldgen data compiles for a known biome")
        .with_structures(&embedded_resolver());
    crate::chunk::Terrain263ChunkSource::from_terrain(std::sync::Arc::new(terrain), crate::dimension::Dimension::Overworld)
}

/// Parses one of the 9 bundled `flat_level_generator_preset/<id>` documents
/// (id without the `minecraft:` prefix, e.g. `"classic_flat"`, `"the_void"`,
/// `"water_world"`) into a
/// [`FlatLevelGeneratorSettings`](lodestone_worldgen::flat::FlatLevelGeneratorSettings)
/// — the alternate layer stacks vanilla's "Customize" screen offers once a
/// Flat world type is chosen (`assets/worldgen/tags/worldgen/
/// flat_level_generator_preset/visible.json` lists 9 of them in UI order;
/// `overworld` is bundled but excluded from `visible`, matching the jar).
///
/// # Panics
/// Panics if `id` names no bundled `flat_level_generator_preset` document.
#[must_use]
pub fn flat_level_generator_preset_settings(
    id: &str,
) -> lodestone_worldgen::flat::FlatLevelGeneratorSettings {
    let name = id.strip_prefix("minecraft:").unwrap_or(id);
    let doc = embedded_resolver().document(&format!("flat_level_generator_preset/{name}"));
    lodestone_worldgen::flat::FlatLevelGeneratorSettings::from_json(&doc["settings"])
}

/// Parses the overworld dimension's embedded flat settings out of
/// `world_preset/flat` (`all_dimensions == false`) or
/// `world_preset/flat_all_dimensions` (`all_dimensions == true`) — the two
/// "world types" a player actually picks at world creation that need this
/// generator, as opposed to [`flat_level_generator_preset_settings`]'s
/// Customize-screen alternates.
///
/// Scope matches [`WorldType`]: **overworld only**. `flat_all_dimensions`
/// also names flat settings for its own Nether/End dimensions
/// (`world_preset/flat_all_dimensions.json`'s `dimensions` map has all
/// three), which stay unreachable here for the same reason multi-dimension
/// travel is out of scope here.
#[must_use]
pub fn world_preset_flat_settings(
    all_dimensions: bool,
) -> lodestone_worldgen::flat::FlatLevelGeneratorSettings {
    let key = if all_dimensions {
        "world_preset/flat_all_dimensions"
    } else {
        "world_preset/flat"
    };
    let doc = embedded_resolver().document(key);
    lodestone_worldgen::flat::FlatLevelGeneratorSettings::from_json(
        &doc["dimensions"]["minecraft:overworld"]["generator"]["settings"],
    )
}

/// Builds a [`FlatLevelSource`](lodestone_worldgen::flat::FlatLevelSource) for
/// `settings`, over the Overworld's build range.
#[must_use]
pub fn flat_generator(
    settings: lodestone_worldgen::flat::FlatLevelGeneratorSettings,
) -> lodestone_worldgen::flat::FlatLevelSource {
    let overworld = crate::dimension::Dimension::Overworld;
    let (min_y, height) = (overworld.min_y(), overworld.height());
    lodestone_worldgen::flat::FlatLevelSource::new(settings, min_y, height)
}

/// The [`ChunkSource`](crate::chunk::ChunkSource) a superflat world serves —
/// The superflat `ChunkSource` implementation is available for the one preset
/// family whose generator this module now has. It lives here rather than in
/// `chunk.rs` (this crate's other
/// `ChunkSource` implementors' home) because [`lodestone_worldgen::flat`] is
/// this file's dependency to add, not `chunk.rs`'s — the trait itself is
/// public and implementable from any module in this crate, so this needed no
/// change to `chunk.rs` at all.
///
/// Built entirely from [`crate::chunk::ChunkColumn`]'s existing public API
/// (`new` + `set_block` + `set_biome_quarts`) rather than a new
/// `ChunkColumn::from_flat` constructor — same reason.
///
/// A flat world's raw terrain is deterministic and seed-free (see
/// [`lodestone_worldgen::flat`]'s module doc), so unlike
/// [`crate::chunk::OverworldChunkSource`] every generated column before edits
/// is identical; the per-chunk cost here is the 16×16×(layer height) fill
/// loop, not any generation work.
pub struct FlatChunkSource {
    generator: lodestone_worldgen::flat::FlatLevelSource,
    edits: std::sync::Mutex<std::collections::HashMap<(i32, i32), crate::chunk::ChunkColumn>>,
}

impl FlatChunkSource {
    #[must_use]
    pub fn new(generator: lodestone_worldgen::flat::FlatLevelSource) -> Self {
        Self {
            generator,
            edits: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The wrapped generator's own settings — e.g. for a debug screen or a
    /// save-file record of which preset a world was created with.
    #[must_use]
    pub fn generator(&self) -> &lodestone_worldgen::flat::FlatLevelSource {
        &self.generator
    }

    fn generate(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
        let col = self.generator.column(cx, cz);
        let mut out = crate::chunk::ChunkColumn::new(col.min_y(), col.height());
        let biome_quarts: [String; 16] = std::array::from_fn(|_| col.biome().to_string());
        out.set_biome_quarts(&biome_quarts);
        for (row, &state) in col.rows().iter().enumerate() {
            if state == air_state() {
                continue;
            }
            let y = col.min_y() + row as i32;
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    out.set_block_id(
                        lx,
                        y,
                        lz,
                        state,
                    );
                }
            }
        }
        out
    }
}

impl std::fmt::Debug for FlatChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlatChunkSource").finish_non_exhaustive()
    }
}

impl crate::chunk::ChunkSource for FlatChunkSource {
    fn column(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
        let edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        self.generate(cx, cz)
    }

    fn block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> lodestone_data::block_states::StateId {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state_id(lx, y, lz)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: lodestone_data::block_states::StateId,
    ) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate(cx, cz));
        column.set_block_id(lx, y, lz, state);
    }

    fn try_store_resident_edit(
        &self,
        cx: i32,
        cz: i32,
        column: &crate::chunk::ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        let mut edits = match self.edits.try_lock() {
            Ok(edits) => edits,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Some(crate::chunk_store::TryResidentEdit::Busy);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("chunk edit cache lock poisoned")
            }
        };
        edits.insert((cx, cz), column.clone());
        Some(crate::chunk_store::TryResidentEdit::Applied)
    }
}

/// Builds the [`FlatChunkSource`] for `settings` — the server/worldgen
/// boundary a world-creation UI needs for a superflat world: it persists the
/// chosen preset id (or its raw settings) alongside the seed, and at load
/// time calls [`flat_level_generator_preset_settings`] or
/// [`world_preset_flat_settings`] to rebuild `settings`, then this, exactly
/// as [`overworld_chunk_source_of_type`] does for [`WorldType`].
///
/// Called from two places outside this crate: `crates/lodestone-shell/src/net.rs`'s
/// `preset_chunk_source`, for `WorldTypePreset::Flat`/`FlatAllDimensions`'s
/// bundled-default arms, and this file's own
/// [`overworld_chunk_source_override`], for a world whose own
/// `world_gen_settings.dat` stores a customized layer stack — the choice
/// `crate::menu::create_world::CustomizeEditor` (`lodestone-shell`) collects.
#[must_use]
pub fn flat_chunk_source(
    settings: lodestone_worldgen::flat::FlatLevelGeneratorSettings,
) -> FlatChunkSource {
    FlatChunkSource::new(flat_generator(settings))
}

/// Builds the flat/fixed-biome [`ChunkSource`](crate::chunk::ChunkSource)
/// `world_dir`'s own `world_gen_settings.dat` actually specifies — the
/// launch-time half of reading back a "Customize Type" choice.
/// `crate::menu::create_world::CustomizeEditor`
/// (`lodestone-shell`) already writes the player's chosen preset/biome into
/// that file at world creation, via
/// [`lodestone_anvil::world_gen_settings::WorldGenSettings::with_overworld_flat_generator`]/
/// [`with_overworld_fixed_biome_generator`](lodestone_anvil::world_gen_settings::WorldGenSettings::with_overworld_fixed_biome_generator);
/// nothing before this function ever read that field back, so a freshly
/// created customized world generated exactly like an uncustomized one the
/// moment the player pressed Play.
///
/// Returns `Ok(None)` when there is nothing to override: no settings file
/// yet (a throwaway in-memory world, or a world whose first open has not
/// run [`crate::region_source::resolve_world_seed`] yet), or the stored
/// generator is [`OverworldGenerator::Other`](lodestone_anvil::world_gen_settings::OverworldGenerator::Other)
/// — a real `Normal`/`LargeBiomes`/`Amplified` world, whose generator this
/// crate already reconstructs from `seed` alone and needs no on-disk
/// override for.
///
/// # Where this is called from
///
/// `crates/lodestone-shell/src/net.rs`'s singleplayer/LAN open calls this
/// first, whenever a `world_dir` is in scope, and only falls back to
/// `preset_chunk_source`'s bundled-default arms (which still call
/// [`world_preset_flat_settings`]/[`world_preset_single_biome_default_biome`]
/// unconditionally, for the case that reaches them: no stored override, i.e.
/// `Ok(None)` here) when this returns nothing to override. That makes a
/// saved world's own generator win over `WorldTypePreset` — the menu's
/// choice at creation time, which carries none of what a Flat or
/// Single Biome customization collected.
///
/// # Errors
///
/// [`WorldgenScopeMismatch`] if `world_dir` stores a Flat or fixed-biome
/// override but the hosting protocol's declared `scope` does not match what
/// this crate's embedded worldgen bundle serves — the same refusal
/// [`overworld_chunk_source_checked`] and `preset_chunk_source`'s own
/// `refuse_unless_served` apply to every other preset.
///
/// Native only, like [`crate::region_source`]: reads a real file, and
/// `lodestone-anvil` is not a dependency of this crate's `wasm32` build (a
/// browser singleplayer world has no filesystem to have written
/// `world_gen_settings.dat` to in the first place).
#[cfg(not(target_arch = "wasm32"))]
pub fn overworld_chunk_source_override(
    world_dir: &std::path::Path,
    scope: WorldgenScope,
    seed: i64,
) -> Result<Option<(std::sync::Arc<dyn crate::chunk::ChunkSource>, i32, i32)>, WorldgenScopeMismatch>
{
    let path = lodestone_anvil::world_gen_settings::path_in(world_dir);
    let Ok(settings) = lodestone_anvil::world_gen_settings::read_from_file(&path) else {
        // No settings file (or an unreadable one) is not this function's
        // problem to report — `resolve_world_seed` is what makes an
        // unreadable *existing* file a hard error before this ever runs; a
        // missing file (this open is the one creating the world, and
        // creation had not yet written it when this was called) just means
        // "nothing to override yet".
        return Ok(None);
    };
    match settings.overworld_generator() {
        Some(lodestone_anvil::world_gen_settings::OverworldGenerator::Flat {
            layers,
            biome,
            features,
            lakes,
        }) => {
            let flat_settings = lodestone_worldgen::flat::FlatLevelGeneratorSettings {
                biome,
                features,
                lakes,
                layers: layers
                    .into_iter()
                    .map(|layer| lodestone_worldgen::flat::FlatLayer {
                        block: Block::from_name(&layer.block)
                            .map(Block::default_state)
                            .expect("stored flat layer names must be canonical built-in blocks"),
                        // The NBT field is a signed `Int` (matching
                        // `with_overworld_flat_generator`'s own writer);
                        // `FlatLayer::height` is `u32` (row counts are never
                        // negative). Clamped rather than `as u32` so a
                        // corrupt negative value on disk becomes `0` (an
                        // inert, skipped layer) instead of wrapping to a huge
                        // one.
                        height: layer.height.max(0) as u32,
                    })
                    .collect(),
                structure_overrides: lodestone_worldgen::flat::StructureOverrides::Default,
            };
            if !bundled_worldgen_serves(scope) {
                return Err(WorldgenScopeMismatch { requested: scope });
            }
            let source = flat_chunk_source(flat_settings);
            let overworld = crate::dimension::Dimension::Overworld;
            Ok(Some((std::sync::Arc::new(source), overworld.min_y(), overworld.height())))
        }
        Some(lodestone_anvil::world_gen_settings::OverworldGenerator::FixedBiome { biome }) => {
            if !bundled_worldgen_serves(scope) {
                return Err(WorldgenScopeMismatch { requested: scope });
            }
            let source = single_biome_chunk_source(seed, &biome);
            let (min_y, height) = (source.min_y(), source.height());
            Ok(Some((std::sync::Arc::new(source), min_y, height)))
        }
        Some(lodestone_anvil::world_gen_settings::OverworldGenerator::Other) | None => Ok(None),
    }
}

/// Every block state of the hosted game version, as typed ids in that
/// version's global-palette order. Built once per process and shared with the
/// debug-world generator.
///
/// The canonical id space is a union across supported versions, so it is not
/// any one version's registry: the grid must enumerate the hosted version's
/// own wire order or every cell past the first appended state shifts.
fn all_block_states_ordered() -> &'static [lodestone_data::block_states::StateId] {
    static STATES: OnceLock<Vec<lodestone_data::block_states::StateId>> = OnceLock::new();
    STATES.get_or_init(|| {
        let version = lodestone_data::version::GameDataVersion::V26_3;
        (0..version.state_count())
            .map(|wire| {
                version
                    .state_from_wire(wire)
                    .expect("the hosted version's wire table is total")
            })
            .collect()
    })
}

/// Builds the `debug_all_block_states` generator: every registered block state
/// laid out on a fixed grid — see
/// [`lodestone_worldgen::debug`] for the layout. Deterministic and seed-free,
/// like [`flat_generator`], over the Overworld's build range.
#[must_use]
pub fn debug_generator() -> lodestone_worldgen::debug::DebugLevelSource {
    let overworld = crate::dimension::Dimension::Overworld;
    let (min_y, height) = (overworld.min_y(), overworld.height());
    lodestone_worldgen::debug::DebugLevelSource::new(
        all_block_states_ordered().to_vec(),
        min_y,
        height,
    )
}

/// The [`ChunkSource`](crate::chunk::ChunkSource) a `debug_all_block_states`
/// world serves — same shape as [`FlatChunkSource`], built entirely from
/// [`crate::chunk::ChunkColumn`]'s existing public API for the same reason
/// that struct's own doc gives.
pub struct DebugChunkSource {
    generator: lodestone_worldgen::debug::DebugLevelSource,
    edits: std::sync::Mutex<std::collections::HashMap<(i32, i32), crate::chunk::ChunkColumn>>,
}

impl DebugChunkSource {
    #[must_use]
    pub fn new(generator: lodestone_worldgen::debug::DebugLevelSource) -> Self {
        Self {
            generator,
            edits: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn generate(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
        let col = self.generator.column(cx, cz);
        let mut out = crate::chunk::ChunkColumn::new(col.min_y(), col.height());
        let biome_quarts: [String; 16] = std::array::from_fn(|_| col.biome().to_string());
        out.set_biome_quarts(&biome_quarts);
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let barrier = col.block_state(lx, lodestone_worldgen::debug::BARRIER_Y, lz);
                out.set_block_id(
                    lx,
                    lodestone_worldgen::debug::BARRIER_Y,
                    lz,
                    barrier,
                );
                let grid = col.block_state(lx, lodestone_worldgen::debug::GRID_Y, lz);
                if grid != lodestone_data::block::Block::Air.default_state() {
                    out.set_block_id(
                        lx,
                        lodestone_worldgen::debug::GRID_Y,
                        lz,
                        grid,
                    );
                }
            }
        }
        out
    }
}

impl std::fmt::Debug for DebugChunkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DebugChunkSource").finish_non_exhaustive()
    }
}

impl crate::chunk::ChunkSource for DebugChunkSource {
    fn column(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
        let edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        if let Some(edited) = edits.get(&(cx, cz)) {
            return edited.clone();
        }
        drop(edits);
        self.generate(cx, cz)
    }

    fn block_state_id(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> lodestone_data::block_states::StateId {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state_id(lx, y, lz)
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(
        &self,
        x: i32,
        y: i32,
        z: i32,
        state: lodestone_data::block_states::StateId,
    ) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate(cx, cz));
        column.set_block_id(lx, y, lz, state);
    }

    fn try_store_resident_edit(
        &self,
        cx: i32,
        cz: i32,
        column: &crate::chunk::ChunkColumn,
    ) -> Option<crate::chunk_store::TryResidentEdit> {
        let mut edits = match self.edits.try_lock() {
            Ok(edits) => edits,
            Err(std::sync::TryLockError::WouldBlock) => {
                return Some(crate::chunk_store::TryResidentEdit::Busy);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("chunk edit cache lock poisoned")
            }
        };
        edits.insert((cx, cz), column.clone());
        Some(crate::chunk_store::TryResidentEdit::Applied)
    }
}

/// Builds the [`DebugChunkSource`] — the server/worldgen boundary a
/// world-creation UI needs for a `debug_all_block_states` world. Takes no
/// parameters: unlike flat presets, vanilla's debug world has no
/// customization screen (`DebugLevelSource`'s codec names only the fixed
/// `minecraft:plains` biome).
#[must_use]
pub fn debug_chunk_source() -> DebugChunkSource {
    DebugChunkSource::new(debug_generator())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{ChunkColumn, ChunkSource};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};


    #[test]
    fn retained_chunk_source_factory_exposes_residency_without_exposing_the_store() {
        struct Source(Arc<AtomicUsize>);
        impl ChunkSource for Source {
            fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
                self.0.fetch_add(1, Ordering::Relaxed);
                ChunkColumn::new(-64, 384)
            }
            fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> lodestone_data::block_states::StateId {
            air_state()
            }
            fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
                "minecraft:plains".to_owned()
            }
            fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: lodestone_data::block_states::StateId) {}
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let source = retained_chunk_source_for_view_radius(Source(Arc::clone(&calls)), 1);
        assert!(!source.is_column_resident(3, -2), "the wrapped store begins empty");
        let _ = source.column(3, -2);
        assert!(source.is_column_resident(3, -2), "a generated column remains resident");
        let _ = source.column(3, -2);
        assert_eq!(calls.load(Ordering::Relaxed), 1, "the opaque factory must retain rather than regenerate");
    }

    #[test]
    fn bundled_fact_documents_exclude_appended_release_states() {
        for facts in [freeze_facts(), survival_facts()] {
            for (_, column) in facts.as_object().expect("fact columns") {
                let defaults = column["default"].as_array().expect("default answers");
                assert!(!defaults.iter().any(|name| name == "minecraft:poplar_planks"));
                assert!(!column["states"].as_object().expect("state overrides")
                    .keys().any(|state| state.starts_with("minecraft:poplar_")));
            }
        }
        assert!(freeze_facts()["solid"]["default"].as_array().expect("solid defaults")
            .iter().any(|name| name == "minecraft:stone"));
        assert!(survival_facts()["solid_render"]["default"].as_array().expect("solid defaults")
            .iter().any(|name| name == "minecraft:stone"));
    }

    #[test]
    fn bundled_survival_facts_are_complete_and_preserve_state_overrides() {
        let facts = embedded_resolver().block_survival_facts();
        let columns = [
            ("solid_render", 149),
            ("sturdy_up", 3_183),
            ("center_support_down", 4_167),
            ("fire_flammable", 1_217),
        ];
        for (name, overrides) in columns {
            assert!(
                facts[name]["default"].as_array().is_some_and(|v| !v.is_empty()),
                "{name} has no default-state answers"
            );
            assert_eq!(
                facts[name]["states"].as_object().map_or(0, serde_json::Map::len),
                overrides,
                "{name} must include every state differing from its base default"
            );
        }
        assert!(facts["solid_render"]["default"]
            .as_array()
            .is_some_and(|v| v.iter().any(|name| name == "minecraft:stone")));
        assert_eq!(
            facts["fire_flammable"]["states"]
                ["minecraft:oak_fence[east=false,north=false,south=false,waterlogged=true,west=false]"],
            false
        );
    }

    /// The island this resolver's `structure_template` closes: with the trait
    /// default, every template-driven structure lands on the ledger with a
    /// `template '…' unusable` reason and places no blocks at all.
    ///
    /// Asserts on the *ledger*, which is the mechanism's own report rather than
    /// this test's opinion — and the second half is what makes it non-vacuous: a
    /// ledger that is entirely empty would also satisfy the first assertion, and
    /// is not the truth (several structure types still have no piece generator).
    #[test]
    fn no_structure_is_demoted_for_unloadable_templates() {
        let registry =
            lodestone_worldgen::structure::StructureRegistry::new(1234, &embedded_resolver());
        let template_failures: Vec<_> = registry
            .unsupported()
            .iter()
            .filter(|(_, why)| why.starts_with("template "))
            .collect();
        assert!(
            template_failures.is_empty(),
            "structures demoted for unloadable templates: {template_failures:?}"
        );
        assert!(
            !registry.unsupported().is_empty(),
            "an entirely empty ledger means the registry parsed nothing, not that \
             every structure is supported"
        );

        // A template really resolves, by name and to plausible NBT (gzip magic).
        let bytes = embedded_resolver()
            .structure_template("minecraft:shipwreck/with_mast")
            .expect("shipwreck/with_mast is bundled");
        assert_eq!(&bytes[..2], &[0x1f, 0x8b], "structure templates are gzipped NBT");
        assert!(embedded_resolver()
            .structure_template("minecraft:not/a/template")
            .is_none());
    }

    #[test]
    fn embedded_table_is_sorted_and_nonempty() {
        assert!(
            EMBEDDED_WORLDGEN.len() > 90,
            "expected the full shape+surface data subset, got {} files",
            EMBEDDED_WORLDGEN.len()
        );
        assert!(
            EMBEDDED_WORLDGEN.windows(2).all(|w| w[0].0 < w[1].0),
            "embedded table must be sorted for binary_search"
        );
        // The load-bearing entries the generator dereferences by name.
        for key in [
            "noise_settings/overworld",
            "density_function/overworld/sloped_cheese",
            "noise/continentalness",
        ] {
            assert!(
                EMBEDDED_WORLDGEN
                    .binary_search_by(|(id, _)| (*id).cmp(key))
                    .is_ok(),
                "embedded table missing '{key}'"
            );
        }
    }

    /// Every production caller of [`lodestone_worldgen::density::Builder::build`]
    /// (the overworld/nether/end generators, the aquifer, the surface system,
    /// the biome climate sampler, the ore-vein programs) reads its document
    /// from `embedded_resolver` and then `.expect(...)`s the `Result` rather
    /// than propagating it, on the grounds that a document we compiled into
    /// the binary can only fail to parse as a shipping bug, never as
    /// attacker-supplied input. That claim was previously an assumption; this
    /// test makes it a checked gate by walking every embedded
    /// `density_function/*` entry through the same builder those callers use.
    /// A future bundled document that does not build now fails here, at the
    /// data boundary, instead of surfacing later as a panic wherever a
    /// generator happens to get constructed.
    #[test]
    fn every_embedded_density_function_document_builds() {
        use lodestone_worldgen::density::Builder;

        let resolver = super::embedded_resolver();
        let builder = Builder::new(0, &resolver);
        let mut checked = 0usize;
        for &(id, raw) in EMBEDDED_WORLDGEN {
            if !id.starts_with("density_function/") {
                continue;
            }
            let node: Value = serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("embedded '{id}' is not valid JSON: {e}"));
            if let Err(err) = builder.build(&node) {
                panic!("embedded density-function document '{id}' failed to build: {err}");
            }
            checked += 1;
        }
        assert!(
            checked > 0,
            "no 'density_function/*' entries found in the embedded table — this scan \
             would otherwise pass vacuously"
        );
    }

    /// Version gate, driven end to end: a protocol reporting the
    /// 26.2 scope is served the bundled data; a protocol reporting no scope (a
    /// family without worldgen, or one whose data this crate does not embed)
    /// is refused — the refused half is plan §4's load-bearing "`None` means
    /// no world generation, surfaced never routed around".
    #[test]
    fn bundled_worldgen_gate_serves_v26_2_and_refuses_none() {
        use crate::chunk::ChunkColumn;
        use crate::protocol::{ServerBound, ServerDirective, ServerProtocol};
        use lodestone_core::State;
        use uuid::Uuid;

        /// The production v770 declaration, mirrored here because
        /// `lodestone-server` deliberately does not depend on the v770 crate
        /// (that dependency would be the seam collapsing). Every other method
        /// is inert; only `worldgen_scope` differs from the trait default.
        struct V26_2Protocol;
        impl ServerProtocol for V26_2Protocol {
            fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
                ServerBound::Ignored
            }
            fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
                Vec::new()
            }
            fn begin_configuration(&self) -> Vec<ServerDirective> {
                Vec::new()
            }
            fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
                Vec::new()
            }
            fn begin_chunk_batch(&self) -> ServerDirective {
                ServerDirective::None
            }
            fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
                ServerDirective::None
            }
            fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
                ServerDirective::None
            }
            fn worldgen_scope(&self) -> WorldgenScope {
                WorldgenScope::V26_3
            }
        }

        // Served half: the v770-style report resolves the gate true.
        let v26_2: Box<dyn ServerProtocol> = Box::new(V26_2Protocol);
        assert!(
            bundled_worldgen_serves(v26_2.worldgen_scope()),
            "the bundled 26.2 data must serve a protocol that declares the 26.2 scope"
        );

        // Refused half, and the control: `None` — what every other
        // `ServerProtocol` in the workspace reports today, since every test
        // double keeps the trait default — must fail the same gate. If it
        // passed, the gate would be vacuous and a future non-26.2 family would
        // silently be handed 26.2 terrain. The two variants of `WorldgenScope`
        // are exhausted by these two assertions, so the gate is proven exact,
        // not merely "sometimes".
        assert!(
            !bundled_worldgen_serves(WorldgenScope::None),
            "a protocol with no declared worldgen scope must not be served the 26.2 \
             bundle — the version gate exists to make that a refusal, not a silent \
             default to 26.2 terrain"
        );
    }

    /// [`overworld_chunk_source_checked`] is the real consumer
    /// [`bundled_worldgen_serves`]'s own doc says does not exist yet — this
    /// drives it both ways: a matching scope actually returns a working
    /// chunk source (not just `true`), and a mismatched one refuses with
    /// [`WorldgenScopeMismatch`] naming what was requested, rather than
    /// silently constructing the bundle anyway.
    #[test]
    fn overworld_chunk_source_checked_serves_v26_2_and_refuses_everything_else() {
        use crate::chunk::ChunkSource;

        let source = overworld_chunk_source_checked(WorldgenScope::V26_3, 42)
            .expect("the matching scope must be served");
        // Not just "did not error" — the returned source is the real thing,
        // proven by asking it to do the one thing a `ChunkSource` exists
        // for: produce a column.
        let _ = source.column(0, 0);

        let err = overworld_chunk_source_checked(WorldgenScope::None, 42)
            .expect_err("a mismatched scope must refuse rather than silently serving 26.2 terrain");
        assert_eq!(err, WorldgenScopeMismatch { requested: WorldgenScope::None });
    }

    #[test]
    fn worldgen_scope_mismatch_keeps_its_public_diagnostic_without_a_source() {
        let error = WorldgenScopeMismatch { requested: WorldgenScope::None };
        assert_eq!(
            error.to_string(),
            "the embedded worldgen bundle serves only V26_3, but the hosting protocol reports None"
        );
        assert!(std::error::Error::source(&error).is_none());
    }

    /// End-to-end: real biome variety reaches the **served** column (the
    /// column `ServerProtocol::encode_chunk` sends), not just the raw
    /// generator — closing the island CLAUDE.md's rule 1 warns about. Two
    /// adjacent-ish chunks at seed 42 are known (the fixtures above) to
    /// carry different biomes; this proves that variety survives the
    /// `OverworldChunkSource` wrapper the wire encoder actually reads from.
    #[test]
    fn served_chunk_source_carries_real_biome_variety() {
        use crate::ChunkSource;

        let seed = 42;
        let source = overworld_chunk_source(seed);

        // world (0, 0) -> chunk (0,0) local (0,0): dark_forest.
        let a = source.column(0, 0);
        assert_eq!(a.biome_state(0, 0), "minecraft:dark_forest");
        // world (500, 500) -> chunk (31,31) local (4,4): deep_ocean.
        let b = source.column(31, 31);
        assert_eq!(b.biome_state(4, 4), "minecraft:deep_ocean");
    }

    /// The design question `docs/block-edit.md` answers: before edit support,
    /// `OverworldChunkSource::column` called straight through to the
    /// generator on *every* request, so nothing an edit wrote could survive a
    /// later `column()` call — there was nowhere for it to live. This is the
    /// hermetic proof that `set_block`'s retention actually closes that gap,
    /// independent of the slower end-to-end client test
    /// (`crates/protocol/v770/tests/block_edit.rs`), which proves the same
    /// thing through the real wire protocol and a real forget/reload cycle.
    #[test]
    fn set_block_persists_across_repeated_column_calls() {
        use crate::ChunkSource;

        let seed = 1234;
        let source = overworld_chunk_source(seed);

        // World (0, -50, 0) — chunk (0, 0), local (0, 0) — is deep enough
        // that this carver-less generator (`worldgen_data`'s own "no caves"
        // scope note) always fills it: real generated content, not
        // already-air, so an edit applied to existing air could not
        // pass this test by accident.
        let pre = source.block_state_id(0, -50, 0);
        assert_eq!(
            pre.block(),
            Block::Deepslate,
            "test fixture assumption broke: expected solid deepslate at (0,-50,0), found {pre:?}"
        );

        source.set_block(0, -50, 0, air_state());
        assert_eq!(source.block_state_id(0, -50, 0), air_state());

        // Re-fetch the whole column again — simulating the column being
        // forgotten and re-sent, `crate::server`'s `ViewTracker` forget/resend
        // cycle — through a *second, independent* `column()` call. Without
        // retention this would silently regenerate the original deepslate.
        let recolumn = source.column(0, 0);
        assert_eq!(recolumn.block_state_id(0, -50, 0), air_state());

        // The edit must be scoped to exactly the touched cell, not a
        // wholesale wipe of the column: an adjacent, untouched cell in the
        // same column still reads the generator's original content.
        assert_eq!(
            recolumn.block_state_id(1, -50, 0).block(),
            Block::Deepslate,
            "editing (0,-50,0) must not affect its untouched neighbour"
        );
    }



}


/// Bundled generation candidates and real terrain placement, with scalar
/// height checks independent of the compact column's retained summaries.
#[cfg(test)]
mod generation_spawn_reaches_a_real_chunk {
    use lodestone_data::block::Block;
    use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
    use lodestone_data::block_states::StateId;
    use lodestone_data::entity_type::EntityType;

    fn state_id(block: Block, properties: &[(PropertyKey, BuiltinPropertyValue)]) -> StateId {
        let mut properties_set = Properties::empty();
        for &(key, value) in properties {
            properties_set = properties_set
                .with_builtin(key, value)
                .expect("fixture properties belong to the block");
        }
        Properties::state_for_block(block, &properties_set)
            .expect("fixture block state exists in the registry")
    }

    /// The biome the spawn stage draws a chunk's packs from: its minimum corner at the top of
    /// the world, read off the shaped column.
    fn pack_biome(source: &crate::chunk::Terrain263ChunkSource, cx: i32, cz: i32) -> String {
        use crate::ChunkSource as _;
        let column = source.column_at(cx, cz, crate::ChunkGenerationStage::Shaped);
        column.biome_state_at(0, column.min_y + column.height - 1, 0).to_owned()
    }

    #[test]
    fn bounded_real_chunks_propose_grass_supported_animals() {
        use crate::ChunkSource as _;
        let source = super::overworld_chunk_source(12345);
        let mut candidate_count = 0;
        let mut accepted_grass_animals = 0;
        let mut diagnostics = Vec::new();
        let mut selected = Vec::new();
        // Independent 48-bit arithmetic fixes each chunk's first probability gate (the third
        // draw after the decoration seed). Every gate here is under the bundled 0.1, so a chunk
        // whose biome has farm animals proposes at least one pack.
        let witnesses = [
            (20001, -20000, 0.002923727035522461),
            (19999, -20002, 0.04546844959259033),
            (20001, -19997, 0.03898078203201294),
            (20002, -20003, 0.03725546598434448),
            (20003, -19998, 0.048967063426971436),
            (19996, -20002, 0.002508699893951416),
            (19997, -19996, 0.06552106142044067),
            (20005, -20001, 0.03898102045059204),
            (20004, -20004, 0.06438791751861572),
            (20000, -20006, 0.03234684467315674),
            (19994, -19999, 0.018687427043914795),
            (20006, -19998, 0.04547971487045288),
            (-20001, 19998, 0.011465311050415039),
            (-19998, 19999, 0.007927298545837402),
            (-20000, 20004, 0.005354166030883789),
            (-19999, 20004, 0.05988889932632446),
            (19999, 19999, 0.06987500190734863),
            (19998, 20003, 0.05369246006011963),
            (19996, 19998, 0.041105568408966064),
            (19995, 20000, 0.05698889493942261),
            (-20000, -19999, 0.05291450023651123),
            (-20003, -19999, 0.021033167839050293),
            (-20000, -19996, 0.04897868633270264),
            (-19998, -20004, 0.054434239864349365),
        ];
        for (cx, cz, probability_draw) in witnesses {
            let biome = pack_biome(&source, cx, cz);
            let entries = super::bundled_biome_spawners()
                .get(&biome)
                .map(|settings| settings.for_category(lodestone_worldgen::spawners::MobCategory::Creature))
                .unwrap_or(&[]);
            let land_context = entries.iter().any(|entry| matches!(
                entry.entity_type.builtin_or_none(),
                Some(EntityType::Cow | EntityType::Sheep | EntityType::Pig | EntityType::Chicken),
            ));
            diagnostics.push(format!("chunk=({cx},{cz}) gate={probability_draw} biome={biome} creatures={entries:?}"));
            if land_context && selected.len() < 2 {
                selected.push((cx, cz));
            }
        }
        assert!(!selected.is_empty(), "no bounded land context: {diagnostics:#?}");
        let mut validator = crate::natural_spawn::NaturalSpawner::new(super::bundled_biome_spawners().clone(), 0);
        validator.set_day_time(6_000);
        for (tick, (cx, cz)) in selected.into_iter().enumerate() {
            let mut col = source.column(cx, cz);
            let candidates = col.take_generation_spawns();
            candidate_count += candidates.len();
            diagnostics.push(format!("chunk=({cx},{cz}) candidates={}", candidates.len()));
            for candidate in &candidates {
                let (lx, lz) = (candidate.x.rem_euclid(16), candidate.z.rem_euclid(16));
                assert_eq!((candidate.x.div_euclid(16), candidate.z.div_euclid(16)), (cx, cz));
                let canopy = matches!(candidate.entity_type.builtin_or_none(), Some(EntityType::Parrot | EntityType::Ocelot));
                let expected_y = (col.min_y..col.min_y + col.height)
                    .rev()
                    .find(|&y| {
                        let state = col.block_state_id(lx, y, lz);
                        let motion = lodestone_data::block_solidity::blocks_motion(state)
                            || lodestone_data::snow_support::has_fluid_state(state);
                        motion && (canopy || !lodestone_data::tool::builtin_block_tag_contains("minecraft:leaves", state.block()))
                    })
                    .map_or(col.min_y, |y| y + 1);
                assert_eq!(candidate.y, expected_y, "chunk=({cx},{cz}) candidate={candidate:?}");
            }
            let world = std::sync::Arc::new(crate::mobs::ChunkWorld::from_columns([((cx, cz), col)]));
            validator.begin_cycle(world.clone(), tick as u64 + 1, Vec::new());
            for candidate in &candidates {
                let ground = world.block_state_id(candidate.x, candidate.y - 1, candidate.z).block();
                let feet = world.block_state_id(candidate.x, candidate.y, candidate.z).block();
                let decision = validator.classify_generation_spawn(candidate);
                let ordinary_animal = matches!(
                    candidate.entity_type.builtin_or_none(),
                    Some(EntityType::Cow | EntityType::Sheep | EntityType::Pig | EntityType::Chicken),
                );
                if ordinary_animal && matches!(&decision, crate::generation_population::PlacementDecision::Accepted(_)) {
                    assert_eq!(ground, Block::GrassBlock, "candidate={candidate:?} feet={feet:?}");
                    accepted_grass_animals += 1;
                }
                diagnostics.push(format!("candidate={candidate:?} ground={ground:?} feet={feet:?} decision={decision:?}"));
            }
        }
        assert!(candidate_count > 0, "bounded real chunks produced no candidates: {diagnostics:#?}");
        assert!(accepted_grass_animals > 0, "no grass-supported animal passed real placement: {diagnostics:#?}");
    }

    /// Independent probability arithmetic gives a first pack gate of 0.8437569737434387 at
    /// (4,-3), above the bundled 0.1 probability.
    #[test]
    fn real_chunk_with_failed_probability_gate_proposes_nothing() {
        use crate::ChunkSource as _;
        let mut column = super::overworld_chunk_source(12345).column(4, -3);
        assert!(column.take_generation_spawns().is_empty());
    }

    /// A chunk proposes its creatures once per world: generated again, it proposes none.
    #[test]
    fn a_chunk_proposes_its_creatures_only_the_first_time_it_is_generated() {
        use crate::ChunkSource as _;
        let source = super::overworld_chunk_source(12345);
        let (cx, cz) = [(20001, -20000), (19999, -20002), (20001, -19997), (-20001, 19998), (-19998, 19999)]
            .into_iter()
            .find(|&(cx, cz)| source.column(cx, cz).generation_spawn_batch().is_some())
            .expect("one of five chunks with an admitting first gate proposes packs");
        assert_eq!(source.pending_generation_spawn_batches(8).len(), 1, "exactly one batch was published");
        assert!(source.column(cx, cz).generation_spawn_batch().is_none(), "a regenerated chunk proposes none");
        assert_eq!(source.pending_generation_spawn_batches(8).len(), 1, "regenerating publishes nothing more");
    }

    /// `world_preset/flat.json`'s embedded `generator.settings` object, pinned
    /// so a change to the bundled asset is caught here rather than silently
    /// reflected into every consumer.
    #[test]
    fn world_preset_flat_settings_matches_the_bundled_document() {
        let settings = super::world_preset_flat_settings(false);
        assert_eq!(settings.biome, "minecraft:plains");
        assert!(!settings.features);
        assert!(!settings.lakes);
        assert_eq!(settings.total_height(), 4);
    }

    /// `world_preset/flat_all_dimensions.json`'s embedded overworld settings —
    /// a different biome and a taller sandstone stack from `flat`'s, so this
    /// is also the discriminator that the two `all_dimensions` branches are
    /// not accidentally reading the same document.
    #[test]
    fn world_preset_flat_all_dimensions_settings_matches_the_bundled_document() {
        let settings = super::world_preset_flat_settings(true);
        assert_eq!(settings.biome, "minecraft:desert");
        assert!(!settings.features);
        assert!(!settings.lakes);
        assert_eq!(settings.total_height(), 68, "bedrock(1) + sandstone(67)");
    }

    /// `flat_level_generator_preset/the_void.json`'s `structure_overrides` is
    /// an explicit empty array, not an absent field — the same discriminator
    /// `lodestone_worldgen::flat`'s own unit tests check against a hand-built
    /// document; this exercises it against the real embedded asset instead.
    #[test]
    fn flat_level_generator_preset_the_void_structure_overrides_is_explicit_empty() {
        let settings = super::flat_level_generator_preset_settings("the_void");
        assert_eq!(settings.biome, "minecraft:the_void");
        assert_eq!(settings.layers.len(), 1);
        assert_eq!(
            settings.structure_overrides,
            lodestone_worldgen::flat::StructureOverrides::Explicit(Vec::new())
        );
    }

    /// The discriminating assertion asks for: at the same seed and
    /// the same column, [`flat_chunk_source`] must produce `world_preset/flat`'s
    /// exact layer stack, and that stack must differ from what
    /// [`overworld_chunk_source`] produces at the identical column — proving
    /// `FlatChunkSource` is really generating flat terrain, not silently
    /// routing through the default overworld generator under a different name
    /// (the trap this module's own [`WorldType`] doc names).
    ///
    /// The default arm's own values below were **measured**, not guessed: a
    /// throwaway probe (`eprintln!` over `overworld_chunk_source(4242)
    /// column(0, 0)`) read them off
    /// the real generator before this assertion was written. At seed 4242,
    /// chunk (0, 0), local (0, 0), the plain overworld returns
    /// `minecraft:bedrock` at y = -63, -62 and -61, and
    /// `minecraft:deepslate[axis=y]` at y = -60 — ordinary underground
    /// terrain, structurally unable to coincide with a flat world's fixed
    /// layer stack at those same rows. Mismatches are collected rather than
    /// asserted one at a time (CLAUDE.md: "collect mismatches and assert on
    /// the collection").
    #[test]
    fn flat_world_produces_the_exact_layer_stack_and_differs_from_default_overworld_at_the_same_column()
     {
        use crate::chunk::ChunkSource;
        let seed: i64 = 4242;

        let flat = super::flat_chunk_source(super::world_preset_flat_settings(false));
        let overworld = super::overworld_chunk_source(seed);

        let flat_col = flat.column(0, 0);
        let overworld_col = overworld.column(0, 0);

        let mut mismatches: Vec<String> = Vec::new();

        let expected_flat: [(i32, StateId); 5] = [
            (-64, Block::Bedrock.default_state()),
            (-63, Block::Dirt.default_state()),
            (-62, Block::Dirt.default_state()),
            (
                -61,
                state_id(
                    Block::GrassBlock,
                    &[(PropertyKey::Snowy, BuiltinPropertyValue::False)],
                ),
            ),
            (-60, Block::Air.default_state()),
        ];
        for &(y, want) in &expected_flat {
            let got = flat_col.block_state_id(0, y, 0);
            if got != want {
                mismatches.push(format!("flat y={y}: expected {want:?}, got {got:?}"));
            }
        }

        // The default arm's own measured values — the "wrong hypothesis" this
        // gate demonstrably rejects, not merely "differs from an unstated
        // baseline" (CLAUDE.md's *magnitude* species).
        let expected_default: [(i32, StateId); 4] = [
            (-63, Block::Bedrock.default_state()),
            (-62, Block::Bedrock.default_state()),
            (-61, Block::Bedrock.default_state()),
            (
                -60,
                state_id(
                    Block::Deepslate,
                    &[(PropertyKey::Axis, BuiltinPropertyValue::Y)],
                ),
            ),
        ];
        for &(y, want) in &expected_default {
            let got = overworld_col.block_state_id(0, y, 0);
            if got != want {
                mismatches.push(format!(
                    "default overworld y={y}: expected {want:?} (re-derive rather \
                     than editing this if the plain overworld's own output moved \
                     at this seed and column), got {got:?}"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "layer-stack mismatches:\n{mismatches:#?}"
        );

        // The load-bearing comparison: at every row both arms cover, the flat
        // world's fixed layer stack must not equal the default's ordinary
        // underground terrain.
        for y in [-63, -62, -61] {
            assert_ne!(
                flat_col.block_state_id(0, y, 0),
                overworld_col.block_state_id(0, y, 0),
                "flat and default overworld agree at y={y}; FlatChunkSource may be \
                 silently routing through the default generator — the exact \
                 failure mode this gate exists to catch"
            );
        }

        // A flat world has no per-column variation: a second, distant chunk
        // must report the identical stack.
        let far = flat.column(500, -500);
        for &(y, want) in &expected_flat {
            assert_eq!(far.block_state_id(3, y, 11), want, "y={y} at a distant chunk");
        }

        assert_eq!(flat_col.biome_state(0, 0), "minecraft:plains");
    }

    /// A `set_block` edit through [`FlatChunkSource`] must be visible on a
    /// later `column`/`block_state` read for the same chunk, and must not
    /// leak into a neighbouring, unedited chunk — the same edit-cache contract
    /// [`crate::chunk::OverworldChunkSource`] provides.
    #[test]
    fn flat_chunk_source_set_block_persists_and_stays_chunk_local() {
        use crate::chunk::ChunkSource;
        let flat = super::flat_chunk_source(super::world_preset_flat_settings(false));

        assert_eq!(
            flat.block_state_id(0, -61, 0),
            state_id(
                Block::GrassBlock,
                &[(PropertyKey::Snowy, BuiltinPropertyValue::False)],
            )
        );
        flat.set_block(0, -61, 0, Block::DiamondBlock.default_state());
        assert_eq!(
            flat.block_state_id(0, -61, 0),
            Block::DiamondBlock.default_state()
        );

        // A different column, never edited, still reads the generated stack.
        assert_eq!(
            flat.block_state_id(16, -61, 0),
            state_id(
                Block::GrassBlock,
                &[(PropertyKey::Snowy, BuiltinPropertyValue::False)],
            )
        );
    }

    fn tempdir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lodestone-worldgen-data-693-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch world dir");
        dir
    }

    /// **The full chain, on disk.** Writes a `world_gen_settings.dat` the way
    /// `crate::saves::create_world_in` (`lodestone-shell`) does when the
    /// player picks a *non-default* Flat preset in `CustomizeEditor` — not
    /// Classic Flat, so a wrong implementation that silently fell back to the
    /// bundled default would still produce plausible-looking terrain instead
    /// of visibly failing — then calls
    /// [`overworld_chunk_source_override`] exactly as `net.rs`'s
    /// `preset_chunk_source` would, and asserts the **exact block stack**
    /// the generated column reports: the value is predicted (a specific
    /// block per row), not merely "some terrain came back" or "differs from
    /// a baseline".
    #[test]
    fn overworld_chunk_source_override_builds_the_customized_flat_world_from_disk() {
        use crate::chunk::ChunkSource;
        let dir = tempdir("flat");
        let path = lodestone_anvil::world_gen_settings::path_in(&dir);
        let layers = [
            lodestone_anvil::world_gen_settings::FlatLayer { block: "minecraft:bedrock", height: 1 },
            lodestone_anvil::world_gen_settings::FlatLayer { block: "minecraft:sandstone", height: 4 },
            lodestone_anvil::world_gen_settings::FlatLayer { block: "minecraft:sand", height: 2 },
        ];
        let settings = lodestone_anvil::world_gen_settings::WorldGenSettings::from_seed(7)
            .with_overworld_flat_generator(&layers, "minecraft:desert", false, false);
        lodestone_anvil::world_gen_settings::write_to_file(&settings, &path)
            .expect("writes the settings file");

        let (source, min_y, _height) =
            super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_3, 7)
                .expect("scope matches the bundle")
                .expect("a Flat generator was stored on disk");

        assert_eq!(min_y, -64, "flat/debug share the bundled overworld's own min_y");
        let expected: [(i32, StateId); 8] = [
            (-64, Block::Bedrock.default_state()),
            (-63, Block::Sandstone.default_state()),
            (-62, Block::Sandstone.default_state()),
            (-61, Block::Sandstone.default_state()),
            (-60, Block::Sandstone.default_state()),
            (-59, Block::Sand.default_state()),
            (-58, Block::Sand.default_state()),
            (-57, Block::Air.default_state()),
        ];
        let mut mismatches = Vec::new();
        for &(y, want) in &expected {
            let got = source.block_state_id(0, y, 0);
            if got != want {
                mismatches.push(format!("y={y}: expected {want:?}, got {got:?}"));
            }
        }
        assert!(mismatches.is_empty(), "layer-stack mismatches:\n{mismatches:#?}");
        // `biome_state` (2-D, surface quarts), not `biome_state_at` (the 3-D
        // grid): `FlatChunkSource` only populates the surface quarts a flat
        // world actually has a use for (`flat_world_produces_the_exact_
        // layer_stack_and_differs_from_default_overworld_at_the_same_column`,
        // this same module's own pre-existing gate, checks the identical way)
        // — its 3-D grid is left at `ChunkColumn::new`'s default, so
        // `biome_state_at` would report that default rather than the chosen
        // biome, which is not this test's subject.
        assert_eq!(source.column(0, 0).biome_state(0, 0), "minecraft:desert");

        // The absence control this repo's own standards require: the
        // *default* Classic Flat stack (what the old, unfixed behaviour
        // would have produced regardless of this file's contents) must not
        // appear at the same rows.
        assert_ne!(
            source.block_state_id(0, -63, 0).block(),
            Block::Dirt,
            "control: the bundled default's own layer must not appear — a \
             wrong implementation that ignored world_gen_settings.dat and \
             fell back to the bundled preset would still pass every \
             assertion above it if this one were missing"
        );
    }

    /// The Single Biome half of the same chain — a chosen biome id that is
    /// not the bundled default, so a wrong implementation that always
    /// reported the default would still pass a test using the default.
    #[test]
    fn overworld_chunk_source_override_builds_the_customized_single_biome_world_from_disk() {
        use crate::chunk::ChunkSource;
        let dir = tempdir("single-biome");
        let path = lodestone_anvil::world_gen_settings::path_in(&dir);
        let settings = lodestone_anvil::world_gen_settings::WorldGenSettings::from_seed(7)
            .with_overworld_fixed_biome_generator("minecraft:jungle");
        lodestone_anvil::world_gen_settings::write_to_file(&settings, &path)
            .expect("writes the settings file");

        let (source, _min_y, _height) =
            super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_3, 7)
                .expect("scope matches the bundle")
                .expect("a fixed-biome generator was stored on disk");

        // Every column must report the chosen biome — `single_biome_chunk_source`'s
        // own documented contract (`FixedBiomeSource`), re-checked here through
        // the disk-driven entry point rather than assumed to carry over.
        assert_eq!(source.biome_state_at(0, 80, 0), "minecraft:jungle");
        assert_eq!(source.biome_state_at(512, 80, -512), "minecraft:jungle");
        assert_ne!(
            source.biome_state_at(0, 80, 0),
            super::world_preset_single_biome_default_biome(),
            "control: the bundled default biome must not appear — a wrong \
             implementation that ignored the chosen biome and fell back to \
             the bundled default would still pass a test that only checked \
             for *a* biome"
        );
    }

    /// **The absence control.** A settings file with no `dimensions`
    /// compound at all — what a settings file looks like the moment
    /// [`crate::region_source::resolve_world_seed`] creates it, before any
    /// customization lands — must resolve to `Ok(None)`: nothing to
    /// override, defer to whatever the caller would otherwise build. Proves
    /// the detector distinguishes "no override" from "override present"
    /// rather than treating every settings file as a Flat/FixedBiome one.
    #[test]
    fn overworld_chunk_source_override_is_none_for_an_uncustomized_world() {
        let dir = tempdir("normal");
        let path = lodestone_anvil::world_gen_settings::path_in(&dir);
        let settings = lodestone_anvil::world_gen_settings::WorldGenSettings::from_seed(7);
        lodestone_anvil::world_gen_settings::write_to_file(&settings, &path)
            .expect("writes the settings file");

        // `Arc<dyn ChunkSource>` is neither `Debug` nor `PartialEq`, so the
        // `Ok(Some(..))` arm cannot be spelled in an `assert_eq!` — matching
        // is the only way to assert "definitely `Ok(None)`", not "definitely
        // not `Err`" (which `is_ok_and` alone would understate).
        match super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_3, 7) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("expected Ok(None) for an uncustomized world, got Ok(Some(..))"),
            Err(e) => panic!("expected Ok(None) for an uncustomized world, got Err({e})"),
        }
    }

    /// A world directory with no settings file at all (never opened) is the
    /// same "nothing to override" case, not an error — `net.rs` calls this
    /// after `resolve_world_seed` has already run, but a caller checking
    /// earlier, or a throwaway path, must not panic or error.
    #[test]
    fn overworld_chunk_source_override_is_none_with_no_settings_file() {
        let dir = tempdir("missing");
        match super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_3, 7) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("expected Ok(None) for a missing settings file, got Ok(Some(..))"),
            Err(e) => panic!("expected Ok(None) for a missing settings file, got Err({e})"),
        }
    }

    /// The same uniform scope refusal every other preset in
    /// `preset_chunk_source` gets (see [`overworld_chunk_source_override`]'s
    /// own doc) — a stored Flat override does not bypass it.
    #[test]
    fn overworld_chunk_source_override_refuses_a_mismatched_scope() {
        let dir = tempdir("scope-mismatch");
        let path = lodestone_anvil::world_gen_settings::path_in(&dir);
        let settings = lodestone_anvil::world_gen_settings::WorldGenSettings::from_seed(7)
            .with_overworld_flat_generator(
                &[lodestone_anvil::world_gen_settings::FlatLayer { block: "minecraft:stone", height: 1 }],
                "minecraft:plains",
                false,
                false,
            );
        lodestone_anvil::world_gen_settings::write_to_file(&settings, &path)
            .expect("writes the settings file");

        // `Result::expect_err` requires the `Ok` side to be `Debug`, which
        // `Arc<dyn ChunkSource>` is not — matching is the only way to pull
        // the error out.
        let err = match super::overworld_chunk_source_override(&dir, super::WorldgenScope::None, 7) {
            Err(e) => e,
            Ok(_) => panic!("a bundle-less host must be refused, not silently served"),
        };
        assert_eq!(err, super::WorldgenScopeMismatch { requested: super::WorldgenScope::None });
    }
}

/// `single_biome_surface` and `debug_all_block_states` each have a
/// discriminating gate below. Both follow the
/// pattern `flat_world_produces_the_exact_layer_stack_and_differs_from_default_overworld_at_the_same_column`:
/// a specific, re-derived value from each arm,
/// asserted to differ from the other arm's own measured value at the
/// identical seed/column, not a bare "is different" (CLAUDE.md's *magnitude*
/// species).
#[cfg(test)]
mod single_biome_and_debug_world_selection {
    use crate::chunk::ChunkSource;
    use lodestone_data::block::Block;
    use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
    use lodestone_data::block_states::StateId;

    fn state_id(block: Block, properties: &[(PropertyKey, BuiltinPropertyValue)]) -> StateId {
        let mut properties_set = Properties::empty();
        for &(key, value) in properties {
            properties_set = properties_set
                .with_builtin(key, value)
                .expect("fixture properties belong to the block");
        }
        Properties::state_for_block(block, &properties_set)
            .expect("fixture block state exists in the registry")
    }

    /// Scans down from the top of the dimension for the first non-air block
    /// — a small local helper since [`crate::chunk::ChunkColumn`] (unlike the
    /// generator-level `GeneratedColumn`/`FlatColumn`) has no `top_non_air_y`
    /// of its own.
    fn top_non_air(col: &crate::chunk::ChunkColumn, x: i32, z: i32) -> (i32, StateId) {
        for y in (-64..320).rev() {
            let state = col.block_state_id(x, y, z);
            if state != StateId::AIR {
                return (y, state);
            }
        }
        (-65, StateId::AIR)
    }

    /// `world_preset/single_biome_surface.json`'s default biome, pinned so a
    /// change to the bundled asset is caught here.
    #[test]
    fn world_preset_single_biome_default_biome_matches_the_bundled_document() {
        assert_eq!(super::world_preset_single_biome_default_biome(), "minecraft:plains");
    }

    #[test]
    fn sulfur_cave_surface_rule_uses_the_underground_biome_cell() {
        let source = super::single_biome_chunk_source(42, "minecraft:sulfur_caves");
        let column = source.column(-500, -500);
        assert_eq!(
            column.block_state_id(10, -21, 12).block(),
            Block::Sulfur,
            "the external surface oracle reports sulfur at this fixed-biome point"
        );
        assert_eq!(
            column.block_state_id(13, -11, 5).block(),
            Block::Cinnabar,
            "the external surface oracle reports cinnabar at this fixed-biome point"
        );
    }




    #[test]
    fn surface_rules_choose_the_zoomed_biome_not_the_raw_packet_cell() {
        let source = super::overworld_chunk_source(42);
        let column = source.column(-500, -500);
        assert_eq!(
            column.biome_state_at(10, -21, 12),
            "minecraft:plains",
            "the captured reference packet has plains in this raw quart cell"
        );
        assert_eq!(
            column.block_state_id(10, -21, 12).block(),
            Block::Sulfur,
            "the independently captured surface result selects sulfur from a nearby cave-biome cell"
        );
    }

    /// The discriminating assertion for `single_biome_surface`: at the same
    /// seed, [`super::single_biome_chunk_source`]`(seed, "minecraft:desert")`
    /// must report `minecraft:desert` as its biome at *every* sampled column
    /// — including one where the default multi-noise overworld reports a
    /// *different* biome (`minecraft:plains`, not `minecraft:desert`) — and
    /// its surface material must differ from the default arm's own measured
    /// output at the identical column. A biome whose surface is grass (e.g.
    /// plains) would leave a wrong-biome-source bug indistinguishable from
    /// correct at many columns (CLAUDE.md); desert's sand surface cannot
    /// coincide with default overworld's terrain by chance.
    ///
    /// The desert values are the preset's own rule: a desert surface is sand,
    /// and at seed 4242 the three sampled chunks sit at sea level (y=63).
    #[test]
    fn single_biome_desert_reports_desert_everywhere_and_differs_from_default_overworld() {
        let seed: i64 = 4242;
        let desert = super::single_biome_chunk_source(seed, "minecraft:desert");
        let overworld = super::overworld_chunk_source(seed);

        let cases: [(i32, i32, i32, StateId); 3] = [
            (0, 0, 63, Block::Sand.default_state()),
            (5, -3, 63, Block::Sand.default_state()),
            (20, 20, 63, Block::Sand.default_state()),
        ];
        let mut mismatches: Vec<String> = Vec::new();
        for &(cx, cz, want_y, want_state) in &cases {
            let col = desert.column(cx, cz);
            if col.biome_state(0, 0) != "minecraft:desert" {
                mismatches.push(format!(
                    "chunk ({cx},{cz}): expected biome minecraft:desert, got {:?}",
                    col.biome_state(0, 0)
                ));
            }
            let (y, state) = top_non_air(&col, 0, 0);
            if (y, state) != (want_y, want_state) {
                mismatches.push(format!(
                    "chunk ({cx},{cz}): expected top ({want_y}, {want_state:?}), got ({y}, {state:?})"
                ));
            }
        }
        assert!(mismatches.is_empty(), "single-biome desert mismatches:\n{mismatches:#?}");

        // The default arm at the identical seed and columns is the wrong
        // hypothesis: it must be neither desert nor a sand surface.
        for &(cx, cz, _, _) in &cases {
            let col = overworld.column(cx, cz);
            let (_, state) = top_non_air(&col, 0, 0);
            assert!(
                col.biome_state(0, 0) != "minecraft:desert" && state.block() != Block::Sand,
                "chunk ({cx},{cz}): the default overworld is desert-like here, so it cannot \
                 tell the single-biome arm from the default ({:?}, {state:?})",
                col.biome_state(0, 0)
            );
        }

        // The load-bearing comparison: at every sampled chunk, desert's biome
        // and surface must not equal the default arm's own answer at the
        // identical column.
        for &(cx, cz, _, _) in &cases {
            let d = desert.column(cx, cz);
            let o = overworld.column(cx, cz);
            assert_ne!(
                d.biome_state(0, 0),
                o.biome_state(0, 0),
                "chunk ({cx},{cz}): desert and default overworld report the same biome — \
                 single_biome_chunk_source may be silently routing through the default \
                 per-column table, the exact failure mode this gate exists to catch"
            );
        }
    }

    /// `all_block_states_ordered`'s size and a few pinned entries — the
    /// vanilla global-palette order [`lodestone_worldgen::debug::DebugLevelSource`]
    /// depends on. Index 0 is `minecraft:air` (air is the first registered
    /// block, matching vanilla's own `ALL_BLOCKS[0]`); index 1 is
    /// `minecraft:stone`.
    #[test]
    fn all_block_states_ordered_matches_the_real_registry_count_and_head() {
        let states = super::all_block_states_ordered();
        assert_eq!(states.len(), 35_723, "the 26.3 registry's state count");
        assert_eq!(states[0], Block::Air.default_state());
        assert_eq!(states[1], Block::Stone.default_state());
    }

    /// `DebugLevelSource.GRID_WIDTH`/`GRID_HEIGHT`'s vanilla formula
    /// (`ceil(sqrt(n))` / `ceil(n / GRID_WIDTH)`) at the real 35,723-state
    /// count, re-derived rather than assumed equal on both sides.
    #[test]
    fn debug_generator_grid_dimensions_match_the_vanilla_formula_at_the_real_state_count() {
        let n = 35_723f64;
        let expected_width = n.sqrt().ceil() as i32;
        let expected_height = (n / f64::from(expected_width)).ceil() as i32;
        let debug = super::debug_generator();
        assert_eq!(debug.grid_width(), expected_width);
        assert_eq!(debug.grid_height(), expected_height);
    }

    /// The discriminating assertion for `debug_all_block_states`: a real
    /// [`super::DebugChunkSource`] must place the exact predicted barrier
    /// floor and block-state grid, and that grid must differ from the
    /// default overworld's own output at the identical column — proving the
    /// generator is really laying out the registry, not silently producing
    /// ordinary terrain under the preset's name.
    ///
    /// From the 26.3 server's block report: world `(1, 1)` (chunk
    /// `(0, 0)`, local `(1, 1)`) halves to grid cell `(0, 0)`, index `0` —
    /// `minecraft:air`, matching vanilla's own `ALL_BLOCKS[0]`. World
    /// `(17, 17)` (chunk `(1, 1)`, local `(1, 1)`) halves to `(8, 8)`, index
    /// `8 * 190 + 8 = 1528` — `minecraft:note_block[instrument=trumpet,note=24,
    /// powered=true]` in the 26.3 server's own block report, a real multi-property state,
    /// which is the whole point: the grid enumerates actual registered
    /// states, not just base block ids.
    #[test]
    fn debug_world_places_the_exact_predicted_grid_and_differs_from_default_overworld() {
        let debug = super::debug_chunk_source();
        let overworld = super::overworld_chunk_source(4242);

        let mut mismatches: Vec<String> = Vec::new();

        // Barrier floor at every (local_x, local_z) in chunk (0, 0).
        let origin = debug.column(0, 0);
        for lx in 0..16i32 {
            for lz in 0..16i32 {
                let got = origin.block_state_id(lx, 60, lz);
                if got.block() != Block::Barrier {
                    mismatches.push(format!("barrier ({lx},{lz}): got {got:?}"));
                }
            }
        }
        if origin.block_state_id(1, 70, 1) != StateId::AIR {
            mismatches.push(format!(
                "world (1,1) grid row: expected air, got {:?}",
                origin.block_state_id(1, 70, 1)
            ));
        }

        let far = debug.column(1, 1);
        let want_far = state_id(
            Block::NoteBlock,
            &[
                (PropertyKey::Instrument, BuiltinPropertyValue::Trumpet),
                (PropertyKey::Note, BuiltinPropertyValue::Value24),
                (PropertyKey::Powered, BuiltinPropertyValue::True),
            ],
        );
        if far.block_state_id(1, 70, 1) != want_far {
            mismatches.push(format!(
                "world (17,17) grid row: expected {want_far:?}, got {:?}",
                far.block_state_id(1, 70, 1)
            ));
        }
        if origin.biome_state(0, 0) != "minecraft:plains" {
            mismatches.push(format!(
                "debug biome: expected minecraft:plains, got {:?}",
                origin.biome_state(0, 0)
            ));
        }

        assert!(mismatches.is_empty(), "debug-world mismatches:\n{mismatches:#?}");

        // The load-bearing comparison: the default overworld's own measured
        // output at chunk (1, 1), local (1, 1) is ordinary terrain, not a
        // note_block and not a barrier — see `single_biome_desert_...`'s doc
        // for the same-seed default-arm measurement convention.
        let default_col = overworld.column(1, 1);
        assert_ne!(
            default_col.block_state_id(1, 70, 1),
            far.block_state_id(1, 70, 1),
            "default overworld and debug world agree at (17,17) y=70 — \
             DebugChunkSource may be silently routing through the default \
             generator, the exact failure mode this gate exists to catch"
        );
        assert_ne!(
            default_col.block_state_id(1, 60, 1),
            far.block_state_id(1, 60, 1),
            "default overworld and debug world agree at (17,17) y=60"
        );
    }

    /// A `set_block` edit through [`super::DebugChunkSource`] must be
    /// visible on a later read for the same chunk, and must not leak into a
    /// neighbouring, unedited chunk — the same contract
    /// `flat_chunk_source_set_block_persists_and_stays_chunk_local` checks
    /// for [`super::FlatChunkSource`].
    #[test]
    fn debug_chunk_source_set_block_persists_and_stays_chunk_local() {
        let debug = super::debug_chunk_source();
        assert_eq!(debug.block_state_id(1, 60, 1).block(), Block::Barrier);
        debug.set_block(1, 60, 1, Block::DiamondBlock.default_state());
        assert_eq!(
            debug.block_state_id(1, 60, 1),
            Block::DiamondBlock.default_state()
        );
        // A different column, never edited, still reads the generated grid.
        assert_eq!(debug.block_state_id(17, 60, 1).block(), Block::Barrier);
    }
}
