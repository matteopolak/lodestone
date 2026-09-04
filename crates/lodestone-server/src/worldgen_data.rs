//! Bundled singleplayer overworld generator.
//!
//! Closes the worldgen island: the verified [`lodestone_worldgen`] pipeline is
//! version-free and holds no data, so *something* must supply the vanilla noise
//! settings, density functions and noises. This module embeds the 26.2 shape +
//! surface data (see `build.rs`) and exposes a synchronous
//! [`overworld_generator`] the shell's local world can call directly — no async
//! runtime, no files, no network.
//!
//! # Where this belongs long-term, and why it does not move to a version crate
//!
//! An earlier version of this doc said the data "eventually lives in the
//! version crate": it moves `assets/worldgen/` into
//! `crates/protocol/v770`). That was checked against the tree and does not
//! fit — not as a style preference, as a hard `cargo` cycle:
//! `crates/protocol/v770/Cargo.toml` already depends on `lodestone-server`
//! (`V770ServerProtocol` implements [`crate::protocol::ServerProtocol`]), so
//! `lodestone-server` depending back on `lodestone-v26-2` for its worldgen
//! data would be the reverse edge of an existing dependency — cargo refuses
//! a cycle outright, regardless of feature-gating it as optional. This data
//! is the same category of thing `lodestone-data`'s own extraction already
//! settled for the *other* 26.2 censuses (block collision, entity
//! hitboxes, …): 26.2-specific, but not a *protocol* question, so it stays
//! here rather than moving into the one crate a version-family split would
//! put it in.
//!
//! What genuinely was missing, and is now real: a Cargo-level
//! acknowledgement that this bundle is version-specific
//! (`bundled-worldgen-v26_2` in this crate's own `[features]`, default on —
//! see that feature's own doc comment for the honest limit of what it buys
//! today), and a *checked* construction entry point,
//! [`overworld_chunk_source_checked`], that actually consults
//! [`bundled_worldgen_serves`] instead of leaving it "pinned by tests" with
//! no caller. Both are real. What neither reaches on its own is the one
//! production call site that would make the check *matter*:
//! `crates/lodestone-shell/src/net.rs`'s `Origin::Integrated` handling still
//! calls [`overworld_chunk_source`] directly and unconditionally — that file
//! is out of this session's ownership, so [`overworld_chunk_source_checked`]
//! is built, tested, and ready for whoever next owns that call site to
//! adopt in one line.
//!
//! # Honest scope
//!
//! [`OverworldGenerator`] composes shape + the **real** aquifer + surface
//! rules + real multi-noise biome assignment + real carvers
//! and ore features (the real 3×3 block-write-radius-1
//! driver) + grass/flower/tree vegetal decoration, exercised by a 3×3
//! driver and a JVM oracle, with remaining gaps enumerated per biome in
//! [`KNOWN_VEGETATION_GAPS`] and
//! `lodestone_worldgen::feature::vegetation`'s module documentation) + snow
//! layers and surface ice (`freeze_top_layer` — bit-exact against
//! the real server at four fixtures; see `docs/worldgen-freeze-top-layer.md`
//! and the `top_layer_parity` module below) — real terrain shape, surface,
//! biome variety,
//! caves/ravines, and now vegetation, block-for-block verified where a JVM
//! oracle exists for the stage (`docs/worldgen-parity.md`'s harness
//! measures the composed subset directly; vegetation has no such oracle
//! yet — see that module's doc). Structures are still unbuilt anywhere in
//! this repository.

use std::sync::OnceLock;

use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use serde_json::Value;

use crate::protocol::WorldgenScope;

include!(concat!(env!("OUT_DIR"), "/embedded_worldgen.rs"));
// `EMBEDDED_STRUCTURE_TEMPLATES` — the `.nbt` bytes, sorted by key. See
// `EmbeddedResolver::structure_template`.
include!(concat!(env!("OUT_DIR"), "/embedded_structures.rs"));

/// The raw `.nbt` bytes of one bundled structure template, borrowed from rodata.
///
/// The same table [`EmbeddedResolver::structure_template`] serves the worldgen
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
#[must_use]
pub fn embedded_structure_template_ids() -> impl Iterator<Item = &'static str> {
    EMBEDDED_STRUCTURE_TEMPLATES.iter().map(|(key, _)| *key)
}

/// The fallback biome [`OverworldGenerator`] would use if [`EmbeddedResolver`]
/// supplied no biome-parameter table. [`EmbeddedResolver::biome_parameters`]
/// supplies one, so real per-column biome variety is what this generator
/// actually produces; these two constants only document "what it used to
/// always be" and are the value a future resolver with no biome data still
/// gets. Plains has snow disabled, matching `cold_enough_to_snow == false`.
const DEFAULT_BIOME: &str = "minecraft:plains";
const DEFAULT_BIOME_SNOWS: bool = false;

/// The worldgen data scope satisfied by the embedded `assets/worldgen/` bundle.
/// This crate embeds only 26.2 data (protocol 776).
///
/// The version gate is [`bundled_worldgen_serves`] compared against the
/// hosting protocol's own report
/// ([`crate::protocol::ServerProtocol::worldgen_scope`],
/// [`WorldgenScope`](crate::protocol::WorldgenScope)) — a family that hosts
/// with anything other than the 26.2 bundle must not be served this data.
/// Today the only production host is v770, which reports
/// [`WorldgenScope::V26_2`]; a future v340-style host reports
/// [`WorldgenScope::None`] until its own generator (plan §4's `ChunkSource`
/// seam) exists. This is the version-free crate's *declaration* of what its
/// data is; the protocol-side report is the other half of the same gate.
pub const BUNDLED_WORLDGEN_SCOPE: WorldgenScope = WorldgenScope::V26_2;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldgenScopeMismatch {
    /// What the hosting protocol actually reported.
    pub requested: WorldgenScope,
}

impl std::fmt::Display for WorldgenScopeMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the embedded worldgen bundle serves only {BUNDLED_WORLDGEN_SCOPE:?}, but the hosting \
             protocol reports {:?}",
            self.requested
        )
    }
}

impl std::error::Error for WorldgenScopeMismatch {}

/// [`overworld_chunk_source`], gated by [`bundled_worldgen_serves`] — the
/// Checked construction requires a hosting family
/// whose [`crate::protocol::ServerProtocol::worldgen_scope`] does not match
/// [`BUNDLED_WORLDGEN_SCOPE`] is refused here rather than silently handed
/// 26.2 terrain it never declared it could serve.
///
/// See this module's own doc for why nothing in *production* calls this
/// yet: the one real construction site is a single unconditional call to
/// [`overworld_chunk_source`] in `crates/lodestone-shell/src/net.rs`, a
/// file this crate cannot reach into. `v770` — today's only
/// [`crate::protocol::ServerProtocol`] implementor — reports
/// [`WorldgenScope::V26_2`] unconditionally, so even once wired the refusal
/// branch stays unreachable in production until a second hosting family
/// exists; that is the same "not urgent, but no longer merely declared"
/// status [`bundled_worldgen_serves`]'s own doc already states.
///
/// # Errors
///
/// [`WorldgenScopeMismatch`] when `scope` is not [`BUNDLED_WORLDGEN_SCOPE`].
pub fn overworld_chunk_source_checked(
    scope: WorldgenScope,
    seed: i64,
) -> Result<crate::chunk::OverworldChunkSource, WorldgenScopeMismatch> {
    if bundled_worldgen_serves(scope) {
        Ok(overworld_chunk_source(seed))
    } else {
        Err(WorldgenScopeMismatch { requested: scope })
    }
}

/// A [`Resolver`] backed by the embedded worldgen table.
///
/// Parsed `Value`s are cached so repeated references to the same density
/// function (the router tree revisits shared nodes heavily) parse once.
#[derive(Debug, Default)]
struct EmbeddedResolver;

impl EmbeddedResolver {
    fn raw(&self, key: &str) -> &'static str {
        // Binary search: the table is sorted by id in `build.rs`.
        EMBEDDED_WORLDGEN
            .binary_search_by(|(id, _)| (*id).cmp(key))
            .map(|i| EMBEDDED_WORLDGEN[i].1)
            .unwrap_or_else(|_| panic!("embedded worldgen data missing '{key}'"))
    }

    fn json(&self, key: &str) -> Value {
        serde_json::from_str(self.raw(key))
            .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
    }

    /// Like [`Self::raw`], but a missing key returns `None` instead of
    /// panicking during composition lookups
    /// (`biome_document`/`configured_carver`/`configured_feature`/
    /// `placed_feature`/`block_tag`), where a name absent from the embedded
    /// table (e.g. a `mineable/*` tool tag never bundled, or a biome id the
    /// parameter table names that this bundle didn't ship) is expected and
    /// should resolve to "no data" per `Resolver`'s own documented default,
    /// not abort chunk generation.
    fn try_raw(&self, key: &str) -> Option<&'static str> {
        EMBEDDED_WORLDGEN
            .binary_search_by(|(id, _)| (*id).cmp(key))
            .ok()
            .map(|i| EMBEDDED_WORLDGEN[i].1)
    }

    fn try_json(&self, key: &str) -> Value {
        self.try_raw(key).map_or(Value::Null, |raw| {
            serde_json::from_str(raw)
                .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
        })
    }
}

impl Resolver for EmbeddedResolver {
    fn density_function(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.json(&format!("density_function/{name}"))
    }

    fn noise(&self, id: &str) -> NoiseParams {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let v = self.json(&format!("noise/{name}"));
        NoiseParams {
            first_octave: v["firstOctave"]
                .as_i64()
                .unwrap_or_else(|| panic!("noise '{name}' missing firstOctave"))
                as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .unwrap_or_else(|| panic!("noise '{name}' missing amplitudes"))
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }

    /// Real multi-noise biome assignment. Overriding this
    /// (default is an empty array, per [`Resolver::biome_parameters`]'s own
    /// doc) is what switches [`OverworldGenerator`] from its old
    /// single-fixed-biome behaviour to real per-column variety — see
    /// `biome_parameters/overworld.json`'s own header for provenance
    /// (`scripts/worldgen-oracle/BiomeOracle.java`, `table` mode, 7594 rows).
    fn biome_parameters(&self) -> Value {
        self.json("biome_parameters/overworld")
    }

    /// Per-biome `temperature`, used to derive `cold_enough_to_snow` per
    /// sampled column (`biome_parameters/overworld_temperature.json`, read
    /// directly from vanilla's own `data/minecraft/worldgen/biome/*.json`
    /// files — no oracle needed for this one, see that file's own header).
    fn biome_temperatures(&self) -> Value {
        self.json("biome_parameters/overworld_temperature")
    }

    /// Full `worldgen/biome/<name>.json` documents for composition:
    /// carvers + `UNDERGROUND_ORES` feature lists, for
    /// `crate::worldgen_data`'s bundled generator to compose carvers into
    /// [`OverworldGenerator::column`]. 66 files, copied verbatim from
    /// `.cache/mc/26.2/src/data/minecraft/worldgen/biome/` (Mojang's own
    /// generated data, the repository's primary data source).
    fn biome_document(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("biome/{name}"))
    }

    /// `worldgen/configured_carver/<name>.json` — 4 files (`cave`,
    /// `cave_extra_underground`, `canyon`, `nether_cave`; only the first
    /// three are ever referenced by an overworld biome).
    fn configured_carver(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("configured_carver/{name}"))
    }

    /// `worldgen/configured_feature/<name>.json` — 226 bundled documents for
    /// ore composition, vegetation, and other feature families. The resolver
    /// keeps every document rather than filtering to currently executed ones.
    fn configured_feature(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("configured_feature/{name}"))
    }

    /// `worldgen/placed_feature/<name>.json` — 262 files, same provenance as
    /// [`Self::configured_feature`].
    fn placed_feature(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("placed_feature/{name}"))
    }

    /// `tags/block/<name>.json` — 261 files, needed to resolve
    /// `#overworld_carver_replaceables`' recursive closure, and
    /// `#cannot_support_snow_layer`/`#support_override_snow_layer` for
    /// `freeze_top_layer`.
    fn block_tag(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("tags/block/{name}"))
    }

    /// Every bundled `worldgen/structure_set/*.json` id.
    ///
    /// **This method is the entry point to the whole structure engine.** The
    /// default is an empty list, and a resolver returning nothing here places no
    /// structures at all — which is exactly the state the integrated server was
    /// in while the placement engine sat fully built and unreachable. Every
    /// fixture resolver in the workspace still returns nothing, deliberately, so
    /// the parity fixtures stay byte-identical; this is the one resolver that
    /// opts production in.
    ///
    /// Derived from the embedded table rather than a hand-written list, so adding
    /// a `structure_set/*.json` to `assets/worldgen/` is the whole change. Order
    /// is not significant — `StructureRegistry` re-orders into vanilla's
    /// `StructureSets.bootstrap` order.
    fn structure_set_ids(&self) -> Vec<String> {
        EMBEDDED_WORLDGEN
            .iter()
            .filter_map(|(id, _)| id.strip_prefix("structure_set/"))
            .map(|name| format!("minecraft:{name}"))
            .collect()
    }

    /// `worldgen/structure_set/<name>.json` — 20 files.
    fn structure_set(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("structure_set/{name}"))
    }

    /// `worldgen/structure/<name>.json` — 34 files.
    fn structure(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("structure/{name}"))
    }

    /// The raw `structure/<name>.nbt` bytes for one template (the S2 unit).
    ///
    /// **The second entry point to the structure engine**, and it fails the same
    /// quiet way [`structure_set_ids`](Self::structure_set_ids) does: the trait
    /// default is `None`, and a resolver taking it makes the worldgen side
    /// **demote** every template-driven structure to `Unsupported` and record it
    /// in the ledger — the start is placed, the blocks are not. Shipwrecks, ocean
    /// ruins and igloos reached zero blocks in the served world for exactly that
    /// reason.
    ///
    /// Served from a `binary_search_by` over the generated table, which `build.rs`
    /// sorts by key for this reason (an unsorted table would silently miss rather
    /// than fail). Owned `Vec` because the trait is version-free and cannot name
    /// this crate's `&'static` table.
    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        EMBEDDED_STRUCTURE_TEMPLATES
            .binary_search_by(|(key, _)| (*key).cmp(name))
            .ok()
            .map(|i| EMBEDDED_STRUCTURE_TEMPLATES[i].1.to_vec())
    }

    /// `worldgen/template_pool/<name>.json` — 188 files.
    ///
    /// **An entry point to the structure engine**, and it fails exactly the
    /// way the other two do: the trait default is `Value::Null`, and a resolver
    /// taking it makes every *jigsaw* structure demote to `Unsupported` and land
    /// on the ledger — the five villages, `pillager_outpost`, `ancient_city`,
    /// `trail_ruins`, `trial_chambers` and the bastion, i.e. every structure whose
    /// terrain adaptation S3's beardifier exists to apply.
    fn template_pool(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("template_pool/{name}"))
    }

    /// `worldgen/processor_list/<name>.json` — 40 files. A pool element's
    /// `processors` field is either an inline object or a reference to one of
    /// these; only the reference form reaches this method.
    fn processor_list(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("processor_list/{name}"))
    }

    /// `tags/worldgen/biome/<name>.json`. Load-bearing rather than a nicety:
    /// every bundled structure spells its `biomes` field as a single tag
    /// reference (`"#minecraft:has_structure/shipwreck"`), so without this every
    /// structure's biome predicate is empty and no start is ever valid — the
    /// engine would run and place nothing, which looks identical to it not
    /// running.
    fn biome_tag(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("tags/worldgen/biome/{name}"))
    }

    /// The five per-block-state predicates `freeze_top_layer` needs, built from
    /// [`lodestone_data`]'s jar-dumped censuses rather
    /// than from an embedded JSON asset.
    ///
    /// **This is deliberately not a datapack asset**, unlike every other method
    /// on this impl. `blocks_motion`, fluid presence and collision UP-face
    /// fullness are properties of the game's *compiled behaviour*, not of any
    /// JSON file — `blocks.json` has no geometry at all — so the authoritative
    /// source is `lodestone_data::{block_solidity, snow_support}`, which are
    /// themselves dumps of the real 26.2 server. Routing them through the
    /// `Resolver` seam rather than making `lodestone-worldgen` depend on
    /// `lodestone-data` is what keeps the engine version-free (see
    /// `docs/plans/worldgen-parity.md` §4 — the engine takes *all* its data
    /// through `Resolver` by construction).
    ///
    /// Built once per process, not per generator: it is a pure function of two
    /// static tables.
    fn block_freeze_facts(&self) -> Value {
        freeze_facts().clone()
    }
}

/// Builds [`Resolver::block_freeze_facts`]'s document by walking all 32,366
/// block states once.
///
/// The output shape is "the answer for each block's **default** state, by base
/// name" plus "an override for every state that disagrees with its own default".
/// That is exact — the override list is produced by a full registry walk, never
/// curated — and it is two orders of magnitude smaller than a per-state map,
/// which matters because this document is parsed into `HashSet`/`HashMap`s at
/// generator construction.
///
/// The default-state half is load-bearing rather than an optimisation:
/// `lodestone-worldgen` emits fluids without their `level` property
/// (`docs/worldgen-parity.md`'s "Known representation gap"), so a generated
/// column's water reads as `minecraft:water`, and
/// `snow_support::is_water_source_liquid_block` is true for exactly *one* water
/// state. Without the default-state fallback no ocean would ever freeze.
fn freeze_facts() -> &'static Value {
    static FACTS: OnceLock<Value> = OnceLock::new();
    FACTS.get_or_init(|| {
        use lodestone_data::{block_solidity, block_states, snow_support};

        type Reader = fn(lodestone_data::block_states::StateId) -> bool;
        fn blocks_motion(id: lodestone_data::block_states::StateId) -> bool {
            block_solidity::blocks_motion(id.raw())
                .expect("validated state is present in the solidity census")
        }
        const COLUMNS: [(&str, Reader); 5] = [
            ("blocks_motion", blocks_motion),
            ("has_fluid_state", snow_support::has_fluid_state),
            ("water_source", snow_support::is_water_source_liquid_block),
            ("face_full_up", snow_support::face_full_up),
            ("snowy_property", snow_support::has_snowy_property),
        ];
        assert_eq!(
            snow_support::STATE_COUNT,
            block_solidity::STATE_COUNT,
            "the two censuses must share one state-id space"
        );

        // Pass 1: each block's default-state answer per column. `is_default_state`
        // sets exactly one bit per block (asserted in `lodestone-data`'s
        // `tests/snow_support.rs`), so one walk suffices.
        let mut default_answers: std::collections::HashMap<&'static str, [bool; COLUMNS.len()]> =
            std::collections::HashMap::new();
        for id in 0..snow_support::STATE_COUNT {
            let state = block_states::StateId::new(id)
                .expect("generated state-table index is valid");
            if !state.is_default() {
                continue;
            }
            let name = block_states::block_name(id).expect("every state has a block name");
            let answers = std::array::from_fn(|c| COLUMNS[c].1(state));
            default_answers.insert(name, answers);
        }

        // Pass 2: every state that disagrees with its own block's default.
        let mut defaults: [Vec<&'static str>; COLUMNS.len()] = Default::default();
        let mut overrides: [serde_json::Map<String, Value>; COLUMNS.len()] = Default::default();
        for (name, answers) in &default_answers {
            for (c, &answer) in answers.iter().enumerate() {
                if answer {
                    defaults[c].push(name);
                }
            }
        }
        for id in 0..snow_support::STATE_COUNT {
            let state = block_states::StateId::new(id)
                .expect("generated state-table index is valid");
            let name = block_states::block_name(id).expect("every state has a block name");
            let default = default_answers
                .get(name)
                .unwrap_or_else(|| panic!("block {name} has no default state in the census"));
            for (c, &(_, read)) in COLUMNS.iter().enumerate() {
                let answer = read(state);
                if answer != default[c] {
                    overrides[c].insert(canonical_state(id), Value::Bool(answer));
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

/// The Nether's [`Resolver`]: [`EmbeddedResolver`] with **one** method changed.
///
/// `biome_parameters` is the whole difference. `NetherGenerator::new` parses that
/// document as the dimension's own 5-row multi-noise table and *asserts* it is
/// non-empty — deliberately, because temperature and vegetation are the entire
/// Nether biome layout and a fallback would produce a uniform `nether_wastes`
/// that looks plausible in a screenshot. Handing it the overworld table would be
/// worse still: it parses, so nothing fails, and every Nether column gets an
/// overworld biome name whose surface rules and carver list do not exist here.
///
/// Everything else — density functions, noises, biome documents, carvers, block
/// tags, structure sets and templates — is dimension-agnostic lookup by id, so it
/// delegates. `biome_temperatures` delegates too: it feeds
/// `cold_enough_to_snow`, which only [`OverworldGenerator`] consults, and the
/// Nether has no `biome_parameters/nether_temperature` asset to point at.
///
/// A newtype rather than a discriminant field on [`EmbeddedResolver`] so that no
/// existing `&EmbeddedResolver` call site changes shape.
#[derive(Debug, Default)]
struct NetherResolver(EmbeddedResolver);

impl Resolver for NetherResolver {
    /// The one override — see the struct doc.
    fn biome_parameters(&self) -> Value {
        self.0.json("biome_parameters/nether")
    }

    fn density_function(&self, id: &str) -> Value {
        self.0.density_function(id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        self.0.noise(id)
    }
    fn biome_temperatures(&self) -> Value {
        self.0.biome_temperatures()
    }
    fn biome_document(&self, id: &str) -> Value {
        self.0.biome_document(id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.0.configured_carver(id)
    }
    fn configured_feature(&self, id: &str) -> Value {
        self.0.configured_feature(id)
    }
    fn placed_feature(&self, id: &str) -> Value {
        self.0.placed_feature(id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.0.block_tag(id)
    }
    fn structure_set_ids(&self) -> Vec<String> {
        self.0.structure_set_ids()
    }
    fn structure_set(&self, id: &str) -> Value {
        self.0.structure_set(id)
    }
    fn structure(&self, id: &str) -> Value {
        self.0.structure(id)
    }
    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        self.0.structure_template(id)
    }
    fn template_pool(&self, id: &str) -> Value {
        self.0.template_pool(id)
    }
    fn processor_list(&self, id: &str) -> Value {
        self.0.processor_list(id)
    }
    fn biome_tag(&self, id: &str) -> Value {
        self.0.biome_tag(id)
    }
    fn block_freeze_facts(&self) -> Value {
        self.0.block_freeze_facts()
    }
}

/// Which bundled overworld `noise_settings` + density functions a generator
/// uses. `Overworld` is the default and is exactly what every
/// pre-existing call site ([`overworld_generator`]/[`overworld_chunk_source`])
/// still gets — nothing changes for them.
///
/// `Amplified` and `LargeBiomes` need no new engine code: their
/// `noise_settings/*.json` and `density_function/overworld_amplified/*` /
/// `overworld_large_biomes/*` documents are already bundled, and
/// [`EmbeddedResolver::density_function`] already resolves any dotted id
/// under `density_function/`, so `minecraft:overworld_amplified/depth` (as
/// referenced by `noise_settings/amplified.json`'s own `noise_router`)
/// resolves the same way `minecraft:overworld/depth` always has. Both
/// presets' own `world_preset/*.json` select
/// `biome_source.preset: "minecraft:overworld"`, so
/// [`EmbeddedResolver::biome_parameters`]'s hardcoded `biome_parameters/
/// overworld` table is the *correct* table for them too, not a stand-in —
/// their biome variety instead comes from `noise_settings/{amplified,
/// large_biomes}.json`'s own `temperature`/`vegetation` router entries
/// (`large_biomes` points those at `noise/temperature_large` and
/// `noise/vegetation_large`, both bundled), which [`OverworldGenerator::new`]
/// already builds its [`ClimateSampler`](lodestone_worldgen::biome) from
/// per-call. So selecting a [`WorldType`] is the entire gap; no
/// `Resolver::biome_parameters` widening is needed for either preset.
///
/// `single_biome_surface` and `debug_all_block_states` are also **not** a
/// `WorldType` variant, but for a different reason: both have
/// their own entry points ([`single_biome_generator`]/[`debug_generator`]),
/// yet neither reuses
/// `overworld_generator_of_type`'s `noise_settings`-keyed shape.
/// `single_biome_surface` turned out to need no new generator at all —
/// [`OverworldGenerator::new`]'s existing fixed-biome fallback (`dynamic_biome:
/// None`, see that constructor's own doc) *is* vanilla's `FixedBiomeSource`;
/// what was missing was a resolver that deliberately withholds
/// `biome_parameters` to select it, plus a caller-chosen biome — see
/// [`SingleBiomeResolver`]. `debug_all_block_states` is a structurally
/// different, seed-free generator, exactly like `flat` below — see
/// [`lodestone_worldgen::debug`].
///
/// `flat`/`flat_all_dimensions` are **not** a `WorldType` variant even though
/// their generator exists ([`lodestone_worldgen::flat::FlatLevelSource`]) —
/// they are not parameter variants of
/// `Amplified`/`LargeBiomes`. Both of those are still [`OverworldGenerator`]s,
/// just parameterised by a different `noise_settings` document; a flat world
/// is a structurally different generator (no noise router, no seed, no
/// carvers — see that module's own doc), so it needs its own entry point
/// rather than a new arm here. See [`flat_generator`]/[`FlatChunkSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorldType {
    #[default]
    Overworld,
    Amplified,
    LargeBiomes,
}

impl WorldType {
    /// The embedded `noise_settings/<id>` asset key for this world type.
    const fn settings_asset(self) -> &'static str {
        match self {
            WorldType::Overworld => "noise_settings/overworld",
            WorldType::Amplified => "noise_settings/amplified",
            WorldType::LargeBiomes => "noise_settings/large_biomes",
        }
    }
}

/// The parsed noise settings for `world_type` (parsed once per type, reused
/// across seeds and worlds — one `OnceLock` per [`WorldType`] variant rather
/// than a keyed map, since the variant set is small and fixed).
fn settings_for(world_type: WorldType) -> &'static Value {
    static OVERWORLD: OnceLock<Value> = OnceLock::new();
    static AMPLIFIED: OnceLock<Value> = OnceLock::new();
    static LARGE_BIOMES: OnceLock<Value> = OnceLock::new();
    let lock = match world_type {
        WorldType::Overworld => &OVERWORLD,
        WorldType::Amplified => &AMPLIFIED,
        WorldType::LargeBiomes => &LARGE_BIOMES,
    };
    lock.get_or_init(|| {
        let key = world_type.settings_asset();
        let raw = EmbeddedResolver.raw(key);
        serde_json::from_str(raw).unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
    })
}

/// Builds the bundled overworld generator for `seed`.
///
/// This is the synchronous direct-call entry point the shell uses to render a
/// real world. It reuses the parsed settings but rebuilds the seed-dependent
/// density/noise state per call, so callers should build it once per world and
/// reuse it across chunks.
/// The last world seed [`overworld_generator`] was asked for. See
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

#[must_use]
pub fn overworld_generator(seed: i64) -> OverworldGenerator {
    overworld_generator_of_type(seed, WorldType::Overworld)
}

/// Builds the bundled overworld generator for `seed`, using `world_type`'s
/// noise settings and density functions in place of the plain overworld's
/// This is the parameter [`overworld_generator`] uses to
/// [`WorldType::Overworld`]; a world-creation UI selecting Amplified or Large
/// Biomes calls this instead, threading the choice through to persistence the
/// same way it already threads a seed.
#[must_use]
pub fn overworld_generator_of_type(seed: i64, world_type: WorldType) -> OverworldGenerator {
    ACTIVE_WORLD_SEED.store(seed, std::sync::atomic::Ordering::Relaxed);
    OverworldGenerator::new(
        seed,
        settings_for(world_type),
        &EmbeddedResolver,
        DEFAULT_BIOME,
        DEFAULT_BIOME_SNOWS,
    )
}

/// Every bundled biome's parsed `MobSpawnSettings`, biome name to settings —
/// what [`crate::natural_spawn::NaturalSpawner`] consults to answer "what spawns
/// in this biome".
///
/// Parsed straight from the embedded `biome/*.json` documents and cached, rather
/// than read off an [`OverworldGenerator`]: the spawn lists are **seed-independent
/// bundled data**, and the tick loop that needs them holds a
/// [`ChunkSource`](crate::ChunkSource), not a generator. Building a whole
/// generator to reach a constant table would cost the full ~54-document settings
/// parse per world.
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
                let document = EmbeddedResolver.biome_document(name);
                let spawners = lodestone_worldgen::spawners::parse_biome_spawners(&document);
                (!spawners.is_empty()).then(|| (format!("minecraft:{name}"), spawners))
            })
            .collect()
    })
}

/// Builds the bundled overworld [`ChunkSource`](crate::ChunkSource) for `seed`.
///
/// This is the terrain source the **integrated server** serves to a real client
/// (and the path `ServerProtocol::encode_chunk` drives). It wraps the same
/// [`overworld_generator`] the shell calls directly, so both the direct
/// singleplayer path and the loopback-server path produce identical, verified
/// block states — no simplified second generator lives one layer in.
#[must_use]
pub fn overworld_chunk_source(seed: i64) -> crate::chunk::OverworldChunkSource {
    overworld_chunk_source_of_type(seed, WorldType::Overworld)
}

/// Builds the bundled overworld [`ChunkSource`](crate::ChunkSource) for `seed`
/// using `world_type` — the server/worldgen boundary where a
/// world-creation UI needs: it persists a [`WorldType`] alongside the seed and
/// passes it here (and to [`overworld_generator_of_type`]) instead of calling
/// the `Overworld`-only entry points.
#[must_use]
pub fn overworld_chunk_source_of_type(
    seed: i64,
    world_type: WorldType,
) -> crate::chunk::OverworldChunkSource {
    crate::chunk::OverworldChunkSource::new(overworld_generator_of_type(seed, world_type))
}

/// [`Resolver`] for `single_biome_surface`:
/// identical to [`EmbeddedResolver`] except it does **not** override
/// [`Resolver::biome_parameters`]/[`Resolver::biome_temperatures`], so it
/// takes the trait's own empty defaults instead of the real 7594-row overworld
/// table. [`OverworldGenerator::new`] treats an empty `biome_parameters()` as
/// "no real biome variety supplied" and falls back to its fixed-biome path
/// (`dynamic_biome: None`) — vanilla's `FixedBiomeSource`, already built into
/// the engine and never before deliberately selected in production (see
/// [`WorldType`]'s own doc). `biome_temperatures` is likewise left at the
/// default: the fixed-biome path never consults it —
/// [`single_biome_generator`] derives `cold_enough_to_snow` from
/// [`EmbeddedResolver`]'s real temperature table before construction instead.
///
/// A newtype over [`EmbeddedResolver`], same shape as [`NetherResolver`].
#[derive(Debug, Default)]
struct SingleBiomeResolver(EmbeddedResolver);

impl Resolver for SingleBiomeResolver {
    fn density_function(&self, id: &str) -> Value {
        self.0.density_function(id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        self.0.noise(id)
    }
    // biome_parameters / biome_temperatures: intentionally not overridden —
    // see the struct doc.
    fn biome_document(&self, id: &str) -> Value {
        self.0.biome_document(id)
    }
    fn configured_carver(&self, id: &str) -> Value {
        self.0.configured_carver(id)
    }
    fn configured_feature(&self, id: &str) -> Value {
        self.0.configured_feature(id)
    }
    fn placed_feature(&self, id: &str) -> Value {
        self.0.placed_feature(id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.0.block_tag(id)
    }
    fn structure_set_ids(&self) -> Vec<String> {
        self.0.structure_set_ids()
    }
    fn structure_set(&self, id: &str) -> Value {
        self.0.structure_set(id)
    }
    fn structure(&self, id: &str) -> Value {
        self.0.structure(id)
    }
    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        self.0.structure_template(id)
    }
    fn template_pool(&self, id: &str) -> Value {
        self.0.template_pool(id)
    }
    fn processor_list(&self, id: &str) -> Value {
        self.0.processor_list(id)
    }
    fn biome_tag(&self, id: &str) -> Value {
        self.0.biome_tag(id)
    }
    fn block_freeze_facts(&self) -> Value {
        self.0.block_freeze_facts()
    }
}

/// `world_preset/single_biome_surface.json`'s embedded overworld
/// `biome_source.biome` — the biome a player gets if they pick this preset
/// without customizing it (`"minecraft:plains"`).
#[must_use]
pub fn world_preset_single_biome_default_biome() -> String {
    let doc = EmbeddedResolver.json("world_preset/single_biome_surface");
    doc["dimensions"]["minecraft:overworld"]["generator"]["biome_source"]["biome"]
        .as_str()
        .expect("world_preset/single_biome_surface.json must name a fixed biome")
        .to_string()
}

/// Builds the `single_biome_surface` generator:
/// every column answers `biome`, vanilla's `FixedBiomeSource` selected
/// deliberately rather than as a degradation — see [`SingleBiomeResolver`].
///
/// Reuses [`OverworldGenerator`] rather than a new type: shape, surface
/// rules, carvers, ore features and vegetation are all already per-biome data
/// lookups keyed off whichever biome id the fixed-biome path reports for a
/// column (`OverworldGenerator`'s own `fallback_biome` field), so a
/// non-default `biome` (e.g. `"minecraft:desert"`) already drives the correct
/// surface material through the same [`crate::worldgen_data`] data this
/// module's overworld path uses — nothing about surface selection is
/// hardcoded to `"minecraft:plains"`.
///
/// `cold_enough_to_snow` is derived from [`EmbeddedResolver`]'s real
/// `biome_parameters/overworld_temperature` table via
/// [`lodestone_worldgen::biome::cold_enough_to_snow`], not hardcoded, so an
/// unusually warm or cold fixed biome still gets the right answer.
///
/// # Panics
/// Panics if `biome` is not `minecraft:`-prefixed (matching every other
/// biome id this module handles).
#[must_use]
pub fn single_biome_generator(seed: i64, biome: &str) -> OverworldGenerator {
    ACTIVE_WORLD_SEED.store(seed, std::sync::atomic::Ordering::Relaxed);
    let temperatures =
        lodestone_worldgen::biome::parse_temperatures(&EmbeddedResolver.biome_temperatures());
    let cold_enough_to_snow = lodestone_worldgen::biome::cold_enough_to_snow(&temperatures, biome);
    OverworldGenerator::new(
        seed,
        settings_for(WorldType::Overworld),
        &SingleBiomeResolver::default(),
        biome,
        cold_enough_to_snow,
    )
}

/// Builds the `single_biome_surface` [`ChunkSource`](crate::chunk::ChunkSource)
/// for `seed`/`biome` — the server/worldgen boundary a world-creation UI
/// needs: it persists the chosen biome id alongside the seed and calls this
/// at load time, exactly as [`overworld_chunk_source_of_type`] does for
/// [`WorldType`].
#[must_use]
pub fn single_biome_chunk_source(seed: i64, biome: &str) -> crate::chunk::OverworldChunkSource {
    crate::chunk::OverworldChunkSource::new(single_biome_generator(seed, biome))
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
    let doc = EmbeddedResolver.json(&format!("flat_level_generator_preset/{name}"));
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
    let doc = EmbeddedResolver.json(key);
    lodestone_worldgen::flat::FlatLevelGeneratorSettings::from_json(
        &doc["dimensions"]["minecraft:overworld"]["generator"]["settings"],
    )
}

/// Builds a [`FlatLevelSource`](lodestone_worldgen::flat::FlatLevelSource) for
/// `settings`, using the bundled overworld `noise_settings`' own `min_y`/
/// `height` for the vertical bounds — the dimension a flat overworld world
/// occupies, read from data already embedded rather than re-hardcoded
/// (`-64`/`384` for 26.2, but this way a future data bump cannot desync the
/// two).
#[must_use]
pub fn flat_generator(
    settings: lodestone_worldgen::flat::FlatLevelGeneratorSettings,
) -> lodestone_worldgen::flat::FlatLevelSource {
    let noise = &settings_for(WorldType::Overworld)["noise"];
    let min_y = noise["min_y"].as_i64().unwrap_or(-64) as i32;
    let height = noise["height"].as_i64().unwrap_or(384) as i32;
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
        for (row, state) in col.rows().iter().enumerate() {
            if state == "minecraft:air" {
                continue;
            }
            let y = col.min_y() + row as i32;
            for lz in 0..16i32 {
                for lx in 0..16i32 {
                    out.set_block(lx, y, lz, state);
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

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate(cx, cz));
        column.set_block(lx, y, lz, name);
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
                        block: layer.block,
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
            let source = flat_chunk_source(flat_settings);
            // Same "throwaway `OverworldChunkSource` purely to read its
            // bounds" move `preset_chunk_source`'s own doc already
            // documents for `Flat`/`FlatAllDimensions`/`DebugAllBlockStates`
            // — the flat/debug generators read `min_y`/`height` off this
            // exact overworld noise-settings document, so the two are
            // guaranteed equal without hardcoding `(-64, 384)` here too.
            let bounds = overworld_chunk_source_checked(scope, seed)?;
            Ok(Some((std::sync::Arc::new(source), bounds.min_y(), bounds.height())))
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

/// Every block state's canonical string, id `0..STATE_COUNT`, in the vanilla
/// global-palette order — [`lodestone_worldgen::debug::DebugLevelSource`]'s
/// `ALL_BLOCKS`. Built once per process from [`lodestone_data::block_states`]
/// (whose ids are documented as that exact wire/global-palette order) via the
/// same [`canonical_state`] this module already uses for
/// [`Resolver::block_freeze_facts`]'s override table.
fn all_block_states_ordered() -> &'static [String] {
    static STATES: OnceLock<Vec<String>> = OnceLock::new();
    STATES.get_or_init(|| {
        (0..lodestone_data::block_states::STATE_COUNT)
            .map(canonical_state)
            .collect()
    })
}

/// Builds the `debug_all_block_states` generator: every registered block state
/// laid out on a fixed grid — see
/// [`lodestone_worldgen::debug`] for the layout. Deterministic and seed-free,
/// like [`flat_generator`]; uses the bundled overworld `noise_settings`' own
/// `min_y`/`height` for the vertical bounds, same reasoning as that function.
#[must_use]
pub fn debug_generator() -> lodestone_worldgen::debug::DebugLevelSource {
    let noise = &settings_for(WorldType::Overworld)["noise"];
    let min_y = noise["min_y"].as_i64().unwrap_or(-64) as i32;
    let height = noise["height"].as_i64().unwrap_or(384) as i32;
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
                out.set_block(lx, lodestone_worldgen::debug::BARRIER_Y, lz, barrier);
                let grid = col.block_state(lx, lodestone_worldgen::debug::GRID_Y, lz);
                if grid != "minecraft:air" {
                    out.set_block(lx, lodestone_worldgen::debug::GRID_Y, lz, grid);
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

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        let mut edits = self.edits.lock().expect("chunk edit cache lock poisoned");
        let column = edits
            .entry((cx, cz))
            .or_insert_with(|| self.generate(cx, cz));
        column.set_block(lx, y, lz, name);
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

/// The parsed Nether noise settings (parsed once, reused across seeds).
fn nether_settings() -> &'static Value {
    static SETTINGS: OnceLock<Value> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let raw = EmbeddedResolver.raw("noise_settings/nether");
        serde_json::from_str(raw).expect("parsing embedded nether noise settings")
    })
}

/// Builds the bundled Nether generator for `seed`.
///
/// **Does not touch [`active_world_seed`]**, unlike [`overworld_generator`]. That
/// static answers "which world's slime chunks", a question only the overworld
/// asks, and storing the Nether's seed there would point `crate::natural_spawn`
/// at the wrong world for the rest of the process — the two generators share one
/// world seed today, so the store would be a no-op *by coincidence*, which is the
/// worst kind of correct.
#[must_use]
pub fn nether_generator(seed: i64) -> lodestone_worldgen::nether::NetherGenerator {
    lodestone_worldgen::nether::NetherGenerator::new(seed, nether_settings(), &NetherResolver::default())
}

/// Builds the bundled Nether [`ChunkSource`](crate::ChunkSource) for `seed` — the
/// terrain a player who walks through a portal is served.
#[must_use]
pub fn nether_chunk_source(seed: i64) -> crate::chunk::NetherChunkSource {
    crate::chunk::NetherChunkSource::new(nether_generator(seed))
}

/// The parsed End noise settings (parsed once, reused across seeds).
fn end_settings() -> &'static Value {
    static SETTINGS: OnceLock<Value> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let raw = EmbeddedResolver.raw("noise_settings/end");
        serde_json::from_str(raw).expect("parsing embedded end noise settings")
    })
}

/// Builds the bundled End generator for `seed`.
///
/// **Takes the plain [`EmbeddedResolver`]**, unlike [`nether_generator`]'s
/// [`NetherResolver`]: `EndGenerator::new` never calls `Resolver::biome_parameters`
/// at all (`EndBiomeSource` — see `lodestone_worldgen::end`'s module doc — is
/// built from the seed alone, not from a multi-noise parameter table), so there is
/// no method to override and nothing that could resolve to the wrong dimension's
/// table the way an unoverridden Nether resolver would.
///
/// **Does not touch [`active_world_seed`]**, for the same reason
/// [`nether_generator`] does not: that static answers "which world's slime
/// chunks", a question only the overworld's spawner asks.
#[must_use]
pub fn end_generator(seed: i64) -> lodestone_worldgen::end::EndGenerator {
    lodestone_worldgen::end::EndGenerator::new(seed, end_settings(), &EmbeddedResolver)
}

/// Builds the bundled End [`ChunkSource`](crate::ChunkSource) for `seed` — the
/// terrain a player who steps through a completed end-portal-frame ring would be
/// served, once something *triggers* that trip. `crate::integrated`'s
/// `with_nether` already constructs one of these on demand for
/// `Dimension::End`; see `crate::dimension`'s module doc for what still has no
/// caller.
#[must_use]
pub fn end_chunk_source(seed: i64) -> crate::chunk::EndChunkSource {
    crate::chunk::EndChunkSource::new(end_generator(seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkSource;

    /// The wiring-layer discriminator asks for: at the same seed and
    /// the same chunk coordinates, [`end_chunk_source`] must produce terrain that
    /// actually differs from [`overworld_chunk_source`]'s — an implementation
    /// that silently routed both through the same generator (or built
    /// [`crate::chunk::EndChunkSource`] over the overworld's settings by mistake)
    /// would still pass any test that merely asserted "chunks were generated".
    ///
    /// The expectation comes from each dimension's own `default_block` record
    /// (`noise_settings/{overworld,end}.json`), read here rather than assumed, so
    /// this is a claim about the data as well as about the generator: the
    /// overworld's is `minecraft:stone` and must never place `end_stone`; the
    /// End's is `minecraft:end_stone`, and per `lodestone_worldgen::end`'s module
    /// doc the End has no water and no grass at all.
    #[test]
    fn end_terrain_differs_from_overworld_terrain_at_the_same_seed_and_coordinates() {
        let seed: i64 = -195_764_831;
        assert_eq!(settings_for(WorldType::Overworld)["default_block"]["Name"], "minecraft:stone");
        assert_eq!(end_settings()["default_block"]["Name"], "minecraft:end_stone");

        let overworld = overworld_chunk_source(seed);
        let end = end_chunk_source(seed);

        // All three chunks sit well inside the End's main island
        // (chunkX^2 + chunkZ^2 <= 4096 = radius 64), so every one is guaranteed
        // solid ground rather than open water between small islands.
        //
        // Each chunk is generated **once** per source via `column`, then read
        // from the returned `ChunkColumn`'s own cheap local lookup — going
        // through `ChunkSource::block_state` per cell instead would regenerate
        // the whole column on every single call (see that trait method's own
        // doc), turning this sweep into a multi-minute run for no reason.
        let mut overworld_end_stone = 0usize;
        let mut end_end_stone = 0usize;
        let mut differing = 0usize;
        let mut total = 0usize;
        for &(cx, cz) in &[(0, 0), (5, -3), (-12, 20)] {
            let ow_col = overworld.column(cx, cz);
            let en_col = end.column(cx, cz);
            for x in 0..16usize {
                for z in 0..16usize {
                    for y in 0..64i32 {
                        let ow = ow_col.block_state(x as i32, y, z as i32);
                        let en = en_col.block_state(x as i32, y, z as i32);
                        if ow.starts_with("minecraft:end_stone") {
                            overworld_end_stone += 1;
                        }
                        if en.starts_with("minecraft:end_stone") {
                            end_end_stone += 1;
                        }
                        if ow != en {
                            differing += 1;
                        }
                        total += 1;
                    }
                }
            }
        }
        assert_eq!(overworld_end_stone, 0, "the overworld generator must never place end_stone");
        assert!(end_end_stone > 0, "the End generator must place end_stone somewhere in this sweep");
        assert!(
            differing > total / 2,
            "{differing}/{total} cells differ between the overworld and the End at the same \
             seed and coordinates; a majority is required so a generator that silently fell \
             back to producing overworld terrain cannot pass by coincidental agreement in a \
             minority of cells"
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
            lodestone_worldgen::structure::StructureRegistry::new(1234, &EmbeddedResolver);
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
        let bytes = EmbeddedResolver
            .structure_template("minecraft:shipwreck/with_mast")
            .expect("shipwreck/with_mast is bundled");
        assert_eq!(&bytes[..2], &[0x1f, 0x8b], "structure templates are gzipped NBT");
        assert!(EmbeddedResolver.structure_template("minecraft:not/a/template").is_none());
    }

    /// Coordinate sweep used to *choose* the `freeze_top_layer` fixtures rather
    /// than guess them. `#[ignore]`d: it is a several-minute
    /// release-profile scan, and its output is a report, not an assertion.
    ///
    /// ```text
    /// cargo test --release -p lodestone-server --lib freeze_coordinate_sweep -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "multi-minute coordinate sweep; a report, not an assertion"]
    fn freeze_coordinate_sweep() {
        let env = |key: &str, fallback: i32| -> i32 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(fallback)
        };
        let seed = i64::from(env("SWEEP_SEED", 42));
        let extent = env("SWEEP_EXTENT", 240);
        let step = env("SWEEP_STEP", 80).max(1) as usize;
        let generator = overworld_generator(seed);
        let mut rows = Vec::new();
        for cx in (-extent..=extent).step_by(step) {
            for cz in (-extent..=extent).step_by(step) {
                let column = generator.column(cx, cz);
                let mut snow = 0usize;
                let mut ice = 0usize;
                let mut snowy = 0usize;
                let mut min_top = i32::MAX;
                let mut max_top = i32::MIN;
                for lx in 0..16usize {
                    for lz in 0..16usize {
                        let top = column.top_non_air_y(lx, lz);
                        min_top = min_top.min(top);
                        max_top = max_top.max(top);
                    }
                }
                for lx in 0..16usize {
                    for lz in 0..16usize {
                        for y in generator.min_y()..(generator.min_y() + generator.height()) {
                            let state = column.block_state(lx, y, lz);
                            if state.starts_with("minecraft:snow[") {
                                snow += 1;
                            } else if state == "minecraft:ice" {
                                ice += 1;
                            } else if state.contains("snowy=true") {
                                snowy += 1;
                            }
                        }
                    }
                }
                let biomes: std::collections::BTreeSet<&str> = (0..16)
                    .map(|i| column.biome_state((i % 4) * 4, (i / 4) * 4))
                    .collect();
                rows.push(format!(
                    "({cx:>5},{cz:>5}) top {min_top:>4}..{max_top:<4} \
                     snow={snow:<4} ice={ice:<4} snowy={snowy:<4} biomes={biomes:?}"
                ));
            }
        }
        for row in &rows {
            println!("{row}");
        }
    }

    /// Release-profile cost of the `TOP_LAYER_MODIFICATION` stage as a share of
    /// the whole composed `column` call.
    ///
    /// **This is the first release-profile figure for the composed pipeline.**
    /// `docs/plans/worldgen-parity.md` §6 records that every number on file for
    /// it — the 144-chunk sweep at ~68 s pre-ore and 700.57 s after, the ore
    /// sweep — is **debug** profile, so there is no release baseline to compare
    /// against. Debug timings are ordering evidence only; run this with
    /// `--release` or the answer means nothing.
    ///
    /// ```text
    /// cargo test --release -p lodestone-server --lib freeze_stage_release_timing \
    ///     -- --ignored --nocapture
    /// ```
    ///
    /// The split comes from `OverworldGenerator::column_timed`'s own
    /// `StageTimes.top_layer` field rather than from an A/B of two runs. That is
    /// deliberate: an A/B needs a fresh generator per arm (the staged store is
    /// per-generator and retains 512 entries, so
    /// a reused generator makes the second arm recompute nothing and report a
    /// fabricated delta — the vacuity `049c603` had to fix in two determinism
    /// gates), and even then it measures two different process states. One
    /// instrumented pass measures the stage where it actually runs.
    #[test]
    #[ignore = "release-profile timing; a measurement, not an assertion"]
    fn freeze_stage_release_timing() {
        // Snowy, frozen and warm coordinates together, so the figure is not taken
        // only from columns where the step short-circuits on temperature.
        const CHUNKS: [(i32, i32); 8] = [
            (-1200, -2400),
            (-1201, -2400),
            (1200, 600),
            (1201, 600),
            (2400, -600),
            (-600, 0),
            (0, 240),
            (-160, -240),
        ];
        let generator = overworld_generator(42);
        let mut top_layer = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let mut wall = std::time::Duration::ZERO;
        for (cx, cz) in CHUNKS {
            let start = lodestone_time::Instant::now();
            let (column, times) = generator.column_timed(cx, cz);
            wall += start.elapsed();
            assert!(column.non_air_count() > 0);
            top_layer += times.top_layer;
            total += times.total();
        }
        let n = CHUNKS.len() as u32;
        let share = top_layer.as_secs_f64() / total.as_secs_f64() * 100.0;
        println!(
            "release, {n} chunks: wall {wall:?} ({:?}/chunk), staged total {total:?}, \
             top_layer {top_layer:?} ({:?}/chunk) = {share:.3}% of composed column cost",
            wall / n,
            top_layer / n,
        );
        assert!(
            top_layer > std::time::Duration::ZERO,
            "the freeze stage measured as exactly zero, so this is timing a no-op rather \
             than the stage — check that the fixtures' biomes list freeze_top_layer"
        );
        // The prediction from `docs/plans/worldgen-parity.md` §6: a new decoration
        // step is "<5 % each of composed column cost". Asserted as a ceiling so a
        // regression fails here rather than being absorbed — the specific shape to
        // guard against is a per-column `ClimateNoise::new()`, which would be
        // ~780 RNG draws per column instead of per generator.
        assert!(
            share < 5.0,
            "the freeze stage is {share:.3}% of composed column cost, above the 5% \
             prediction (top_layer {top_layer:?}, total {total:?})"
        );
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
    /// from [`EmbeddedResolver`] and then `.expect(...)`s the `Result` rather
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

        let resolver = EmbeddedResolver;
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
                WorldgenScope::V26_2
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

        let source = overworld_chunk_source_checked(WorldgenScope::V26_2, 42)
            .expect("the matching scope must be served");
        // Not just "did not error" — the returned source is the real thing,
        // proven by asking it to do the one thing a `ChunkSource` exists
        // for: produce a column.
        let _ = source.column(0, 0);

        let err = overworld_chunk_source_checked(WorldgenScope::None, 42)
            .expect_err("a mismatched scope must refuse rather than silently serving 26.2 terrain");
        assert_eq!(err, WorldgenScopeMismatch { requested: WorldgenScope::None });
    }

    /// The exact, named vegetal-decoration gap surface for every biome
    /// reachable through the overworld biome-parameter table. Each entry is
    /// `(biome, sorted deduplicated reasons)`, where each reason is emitted by
    /// `lodestone_worldgen::feature::vegetation::ConfiguredFeature::Unsupported`
    /// for a feature reached by that biome's `VEGETAL_DECORATION` step.
    /// `multiface_growth` is present in many biomes; other possible reasons
    /// cover unsupported tree configuration or feature forms such as coral,
    /// bamboo, large mushrooms, root systems, cave vegetation, and simple
    /// block placement. The table is generated by running every reachable biome
    /// through `lodestone_worldgen::compose::build_biome_vegetation` and
    /// `vegetation::collect_unsupported`. See
    /// [`vegetation_placer_gaps_are_named_not_silent`] below: its failure output
    /// prints the complete per-biome list for updating this table.
    ///
    /// Implemented feature forms are deliberately absent from this table.
    /// `kelp`, `seagrass`, `sea_pickle`, `vines`, `vegetation_patch` and
    /// `random_boolean_selector` are all implemented now and dropped off. But
    /// `minecraft:mushroom_fields` **gained** `huge_brown_mushroom` and
    /// `huge_red_mushroom`: its gap used to read `random_boolean_selector`,
    /// and resolving that selector is what finally exposed the two branches
    /// underneath it. A row growing here is the gate working, not a
    /// regression — the previous entry was hiding two features behind one
    /// unparsed wrapper.
    ///
    /// **Fancy oak and the fallen-tree family (`dc637859`) closed
    /// `fallen_tree`, plus most of the surviving `"tree: unsupported..."`
    /// rows, in one pass.** `fallen_tree` (`FallenTreeFeature`) no longer
    /// occurs anywhere — it was never a fancy-oak-shaped gap, so its own
    /// close is unrelated to the other one, just coincident in the same
    /// commit. `"tree: unsupported..."` closed for bamboo_jungle/dark_forest/
    /// jungle/sparse_jungle (the `fancy_oak_checked`/`fancy_oak_leaf_litter`
    /// branch every RandomSelector those biomes use also carries) and for
    /// birch_forest/forest/plains/… (oak's own `fancy_oak` branch, drawn
    /// directly rather than through a selector).
    ///
    /// **The last two, `minecraft:cherry_grove` and `minecraft:mangrove_swamp`,
    /// closed too**: `TrunkPlacerCfg::Cherry`/`FoliagePlacerCfg::Cherry` (cherry's
    /// own `CherryTrunkPlacer`/`CherryFoliagePlacer`) and
    /// `TrunkPlacerCfg::UpwardsBranching`/`FoliagePlacerCfg::RandomSpread` plus a
    /// new `RootPlacerCfg::Mangrove` (`UpwardsBranchingTrunkPlacer`/
    /// `RandomSpreadFoliagePlacer`/`MangroveRootPlacer`) close both. Neither was a
    /// fancy-oak case — both species' placers were genuinely unported vanilla
    /// classes, not a `RandomSelector` branch already closed by something else.
    /// (`docs/worldgen-vegetation.md`'s "What it is" section carries the port detail.)
    ///
    /// **A floor, not a ceiling.** [`vegetation_gap_mismatches`] fails loudly
    /// in BOTH directions: a biome producing a reason NOT listed here (a new
    /// silent gap — the exact failure mode this gate exists to catch) or a
    /// listed biome/reason no longer occurring (this table gone stale after
    /// a stale entry is removed rather than retained. If cacti or sugar-cane
    /// column placement were unsupported, `minecraft:desert`'s own entry
    /// here would have needed `"block_column: unsupported layer/direction
    /// /predicate"` in addition to `multiface_growth` — this table's job is
    /// to force that kind of entry to be written down, not to auto-shrink.
    const KNOWN_VEGETATION_GAPS: &[(&str, &[&str])] = &[
        ("minecraft:badlands", &["multiface_growth"]),
        // bamboo_jungle/jungle/sparse_jungle's `"tree: unsupported..."` entry
        // is gone: fancy oak (`d102eb1d`) closed the 10%-weight
        // `fancy_oak_checked` branch every jungle variant's RandomSelector
        // also carries, on top of `mega-jungle trunk configuration`/
        // `FoliagePlacerCfg::MegaJungle` (mega_jungle_tree, 33.3% of
        // trees_jungle) and `FoliagePlacerCfg::Bush` (jungle_bush, 50%).
        ("minecraft:bamboo_jungle", &["bamboo", "multiface_growth"]),
        ("minecraft:beach", &["multiface_growth"]),
        ("minecraft:birch_forest", &["multiface_growth"]),
        ("minecraft:cherry_grove", &["multiface_growth"]),
        ("minecraft:cold_ocean", &["multiface_growth"]),
        // dark_forest's `"tree: unsupported..."` entry is gone: fancy oak
        // (`d102eb1d`) closed the 10%-weight `fancy_oak_leaf_litter` branch,
        // on top of `dark-oak trunk configuration`/`dark-oak foliage configuration`
        // for the 66.7%-weight dark_oak branch.
        ("minecraft:dark_forest", &["huge_brown_mushroom", "huge_red_mushroom", "multiface_growth"]),
        ("minecraft:deep_cold_ocean", &["multiface_growth"]),
        ("minecraft:deep_dark", &["multiface_growth"]),
        ("minecraft:deep_frozen_ocean", &["multiface_growth"]),
        ("minecraft:deep_lukewarm_ocean", &["multiface_growth"]),
        ("minecraft:deep_ocean", &["multiface_growth"]),
        ("minecraft:desert", &["multiface_growth"]),
        ("minecraft:dripstone_caves", &["multiface_growth"]),
        ("minecraft:eroded_badlands", &["multiface_growth"]),
        ("minecraft:flower_forest", &["multiface_growth", "simple_block: unsupported to_place"]),
        ("minecraft:forest", &["multiface_growth"]),
        ("minecraft:frozen_ocean", &["multiface_growth"]),
        ("minecraft:frozen_peaks", &["multiface_growth"]),
        ("minecraft:frozen_river", &["multiface_growth"]),
        ("minecraft:grove", &["multiface_growth"]),
        ("minecraft:ice_spikes", &["multiface_growth"]),
        ("minecraft:jagged_peaks", &["multiface_growth"]),
        ("minecraft:jungle", &["bamboo", "multiface_growth"]),
        ("minecraft:lukewarm_ocean", &["multiface_growth"]),
        ("minecraft:lush_caves", &["block_column: unsupported layer/direction/predicate", "multiface_growth", "root_system"]),
        // mangrove_swamp's `"tree: unsupported..."` entry is gone:
        // `TrunkPlacerCfg::UpwardsBranching`/`FoliagePlacerCfg::RandomSpread`/
        // `RootPlacerCfg::Mangrove` close mangrove/tall_mangrove's own trunk,
        // foliage and root placers.
        ("minecraft:mangrove_swamp", &["multiface_growth"]),
        ("minecraft:meadow", &["multiface_growth", "simple_block: unsupported to_place"]),
        ("minecraft:mushroom_fields", &["huge_brown_mushroom", "huge_red_mushroom", "multiface_growth"]),
        ("minecraft:ocean", &["multiface_growth"]),
        ("minecraft:old_growth_birch_forest", &["multiface_growth"]),
        // old_growth_pine_taiga/old_growth_spruce_taiga's `mega_pine`/
        // `mega_spruce` configured features use `giant_trunk_placer` +
        // `mega_pine_foliage_placer` — `giant trunk configuration`/
        // `FoliagePlacerCfg::MegaPine` close the "tree: unsupported..."
        // entry for both; the fallen-tree family (`dc637859`) closed
        // `fallen_tree`.
        ("minecraft:old_growth_pine_taiga", &["multiface_growth"]),
        ("minecraft:old_growth_spruce_taiga", &["multiface_growth"]),
        // pale_garden's `"tree: unsupported..."` entry closed with the same
        // change — pale_oak/pale_oak_creaking reuse the dark oak
        // trunk/foliage placers with their own providers.
        ("minecraft:pale_garden", &["multiface_growth"]),
        ("minecraft:plains", &["multiface_growth"]),
        ("minecraft:river", &["multiface_growth"]),
        // savanna/savanna_plateau/windswept_savanna all resolve through
        // trees_savanna's RandomSelector (oak_checked default, acacia_checked
        // 80%, fallen_oak_tree 1.25%) — `forking trunk configuration`/
        // `FoliagePlacerCfg::Acacia` closes the "tree: unsupported..." entry
        // for all three; the fallen-tree family (`dc637859`) closed
        // `fallen_tree`.
        ("minecraft:savanna", &["multiface_growth"]),
        ("minecraft:savanna_plateau", &["multiface_growth"]),
        ("minecraft:snowy_beach", &["multiface_growth"]),
        ("minecraft:snowy_plains", &["multiface_growth"]),
        ("minecraft:snowy_slopes", &["multiface_growth"]),
        ("minecraft:snowy_taiga", &["multiface_growth"]),
        ("minecraft:sparse_jungle", &["multiface_growth"]),
        ("minecraft:stony_peaks", &["multiface_growth"]),
        ("minecraft:stony_shore", &["multiface_growth"]),
        ("minecraft:sulfur_caves", &["multiface_growth"]),
        ("minecraft:sunflower_plains", &["multiface_growth"]),
        ("minecraft:swamp", &["multiface_growth"]),
        ("minecraft:taiga", &["multiface_growth"]),
        ("minecraft:warm_ocean", &["coral_claw", "coral_mushroom", "coral_tree", "multiface_growth"]),
        ("minecraft:windswept_forest", &["multiface_growth"]),
        ("minecraft:windswept_gravelly_hills", &["multiface_growth"]),
        ("minecraft:windswept_hills", &["multiface_growth"]),
        ("minecraft:windswept_savanna", &["multiface_growth"]),
        ("minecraft:wooded_badlands", &["multiface_growth"]),
    ];

    /// Diffs a measured `biome -> sorted, deduped reasons` map against
    /// [`KNOWN_VEGETATION_GAPS`], both directions. Standalone (no
    /// `EmbeddedResolver` needed) specifically so
    /// [`vegetation_gap_mismatches_fires_on_an_undeclared_gap`] can exercise
    /// it with a synthetic map — CLAUDE.md's "absence assertions need a
    /// control proving the detector fires."
    fn vegetation_gap_mismatches(actual: &std::collections::BTreeMap<String, Vec<String>>) -> Vec<String> {
        let known: std::collections::BTreeMap<&str, &[&str]> =
            KNOWN_VEGETATION_GAPS.iter().copied().collect();
        let mut mismatches = Vec::new();
        for (biome, reasons) in actual {
            let expected: &[&str] = known.get(biome.as_str()).copied().unwrap_or(&[]);
            if reasons.iter().map(String::as_str).ne(expected.iter().copied()) {
                mismatches.push(format!(
                    "{biome}: KNOWN_VEGETATION_GAPS says {expected:?}, measured {reasons:?}"
                ));
            }
        }
        for biome in known.keys() {
            if !actual.contains_key(*biome) {
                mismatches.push(format!(
                    "{biome}: listed in KNOWN_VEGETATION_GAPS but no longer a reachable overworld biome"
                ));
            }
        }
        mismatches
    }

    /// Measures the real gap surface once (via `EmbeddedResolver`, the same
    /// data the bundled generator serves) and asserts it matches
    /// [`KNOWN_VEGETATION_GAPS`] exactly, in both directions. This is the
    /// The gate fails when a biome's declared `VEGETAL_DECORATION` step
    /// includes a placer this crate doesn't implement, and which isn't
    /// already named above, now fails a required check instead of quietly
    /// generating a biome with fewer trees than vanilla.
    #[test]
    fn vegetation_placer_gaps_are_named_not_silent() {
        use std::collections::BTreeMap;
        let table = lodestone_worldgen::biome::parse_table(&EmbeddedResolver.biome_parameters());
        let table = lodestone_worldgen::biome::usable_overworld_table(table);
        let mut names: Vec<String> = table.into_iter().map(|p| p.biome).collect();
        names.sort_unstable();
        names.dedup();
        assert!(
            names.len() >= 50,
            "expected the real ~55-biome reachable overworld set, got {}",
            names.len()
        );

        let mut actual: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for biome in names {
            let list = lodestone_worldgen::compose::build_biome_vegetation(&EmbeddedResolver, &biome);
            let mut reasons: Vec<String> = list
                .iter()
                .flat_map(|(_, placed)| lodestone_worldgen::feature::vegetation::collect_unsupported(placed))
                .collect();
            reasons.sort();
            reasons.dedup();
            actual.insert(biome, reasons);
        }

        let mismatches = vegetation_gap_mismatches(&actual);
        assert!(
            mismatches.is_empty(),
            "vegetation placer gap surface drifted from KNOWN_VEGETATION_GAPS — either a NEW \
             silent gap appeared (implement the placer or add it here, named) or a listed gap \
             was fixed (prune the entry):\n{}",
            mismatches.join("\n")
        );
    }

    /// Control for the gate above: an unsupported reason for a biome that
    /// ISN'T in [`KNOWN_VEGETATION_GAPS]` at all (`minecraft:desert` here,
    /// deliberately given a reason it doesn't really have) must be caught,
    /// proving [`vegetation_gap_mismatches`] actually fires rather than
    /// vacuously passing because nothing in-scope ever changes.
    #[test]
    fn vegetation_gap_mismatches_fires_on_an_undeclared_gap() {
        let mut actual = std::collections::BTreeMap::new();
        actual.insert(
            "minecraft:desert".to_string(),
            vec!["brand_new_unimplemented_placer".to_string(), "multiface_growth".to_string()],
        );
        let mismatches = vegetation_gap_mismatches(&actual);
        assert!(
            mismatches.iter().any(|m| m.contains("minecraft:desert") && m.contains("brand_new_unimplemented_placer")),
            "an undeclared new gap must be caught: {mismatches:?}"
        );
    }

    /// Second half of the same control: a biome/reason pair that's listed
    /// but no longer measured (i.e. the gap got fixed) must ALSO be caught —
    /// this is what keeps `KNOWN_VEGETATION_GAPS` from silently going stale
    /// in the direction that hides a real improvement.
    #[test]
    fn vegetation_gap_mismatches_fires_on_a_gap_that_was_fixed() {
        let mut actual = std::collections::BTreeMap::new();
        // `minecraft:desert`'s real entry is `["multiface_growth"]`; report
        // it as fully clean instead, simulating "multiface_growth got fixed".
        actual.insert("minecraft:desert".to_string(), Vec::<String>::new());
        let mismatches = vegetation_gap_mismatches(&actual);
        assert!(
            mismatches.iter().any(|m| m.contains("minecraft:desert")),
            "a listed gap that no longer measures must be caught: {mismatches:?}"
        );
    }

    /// The magnitude gate checks that the number of positions `patch_grass_plain`
    /// pushes into its own trailing survivability predicate must equal the
    /// product of the constants in **its own bundled placement JSON**, not
    /// merely be "more than zero".
    ///
    /// # The prediction, derived from `placed_feature/patch_grass_plain.json`
    ///
    /// That file's `placement` array is, in order:
    ///
    /// | modifier | positions out, per position in |
    /// |---|---|
    /// | `noise_threshold_count` (`below_noise: 5`, `above_noise: 10`) | **5 or 10** |
    /// | `in_square` | 1 |
    /// | `heightmap: WORLD_SURFACE_WG` | 1 (any column that is not all air) |
    /// | `biome` | 1 |
    /// | `count: 32` | **32** |
    /// | `random_offset` | 1 |
    /// | `block_predicate_filter` | terrain-dependent — **not** predictable |
    ///
    /// So the count arriving *at* the filter is exactly `n * 32` for
    /// `n ∈ {5, 10}` — `160` or `320`, and nothing else. Which of the two is
    /// chosen depends on `Biome.BIOME_INFO_NOISE` at the source origin, and
    /// this gate deliberately does **not** compute that: predicting the branch
    /// with our own noise implementation would make the expected value
    /// originate inside the code under test. Both branch values are admitted;
    /// every wrong hypothesis below falls outside the pair either way.
    ///
    /// # The wrong hypotheses this excludes, each computed from the same JSON
    ///
    /// | hypothesis | predicted value | in `{160, 320}`? |
    /// |---|---|---|
    /// | correct | `n * 32` | yes |
    /// | `count` silently dropped (`VegPlacement::try_parse` returned `None` and `filter_map` ate it) | `n` = 5 or 10 | no |
    /// | `count` read as its own `type` field rather than `value` | 1 | no |
    /// | `noise_threshold_count` dropped | `32` | no |
    /// | both count modifiers dropped | `1` | no |
    /// | `noise_threshold_count` reading `noise_level` as the count | `-0` → 0 | no |
    ///
    /// The dropped-modifier row is the one that matters: `parse_placed_feature_doc`
    /// builds its pipeline with `.filter_map(VegPlacement::try_parse)`, so an
    /// unrecognised modifier `type` is **removed from the pipeline** rather than
    /// making the feature inert. That is a silent 32× under-placement, and no
    /// `cargo check` and no "is it non-zero" assertion can see it.
    ///
    /// Terrain here is a synthetic flat grass plane rather than a generated
    /// column, so the `heightmap` row above is exactly 1 by construction and the
    /// product stays a product of JSON constants. The production seam — real
    /// embedded data, real generated terrain, the 3×3 driver, the fold back into
    /// a `GeneratedColumn` — is
    /// [`vegetation_reaches_real_blocks_over_a_production_sweep`]'s job.
    #[test]
    fn plains_grass_patch_attempt_count_matches_the_placement_json() {
        use lodestone_worldgen::feature::vegetation::{
            apply_vegetal_decoration_step, build_veg_tags, census, resolve_placed_feature_ref,
            VegGrid,
        };
        use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};

        const MIN_Y: i32 = -64;
        const HEIGHT: i32 = 384;
        const SURFACE_Y: i32 = 64;
        /// `count: 32` — read from `placed_feature/patch_grass_plain.json`.
        const COUNT: usize = 32;
        /// `noise_threshold_count`'s `below_noise` / `above_noise`.
        const NOISE_BELOW: usize = 5;
        const NOISE_ABOVE: usize = 10;

        // Guard the prediction against the data moving under it: if a future
        // 26.2+ drop changes these constants, fail here naming the field rather
        // than failing the arithmetic below with no explanation.
        let doc = EmbeddedResolver.placed_feature("minecraft:patch_grass_plain");
        let placement = doc["placement"].as_array().expect("patch_grass_plain placement array");
        let ntc = placement
            .iter()
            .find(|m| m["type"] == "minecraft:noise_threshold_count")
            .expect("patch_grass_plain must still carry a noise_threshold_count");
        assert_eq!(ntc["below_noise"].as_u64(), Some(NOISE_BELOW as u64));
        assert_eq!(ntc["above_noise"].as_u64(), Some(NOISE_ABOVE as u64));
        let count = placement
            .iter()
            .find(|m| m["type"] == "minecraft:count")
            .expect("patch_grass_plain must still carry a count");
        assert_eq!(count["count"].as_u64(), Some(COUNT as u64));

        let tags = build_veg_tags(&EmbeddedResolver);
        let placed = resolve_placed_feature_ref(
            &EmbeddedResolver,
            &Value::String("minecraft:patch_grass_plain".to_owned()),
        );

        // A flat grass plane over dirt, filling chunk (0,0)'s own footprint, so
        // `heightmap: WORLD_SURFACE_WG` resolves to exactly one position per
        // column (`SURFACE_Y + 1`) for every column `in_square` can pick.
        let mut grid = VegGrid::new(MIN_Y, HEIGHT, 0, 0);
        for lz in 0..16 {
            for lx in 0..16 {
                for y in MIN_Y..=SURFACE_Y {
                    let state = if y == SURFACE_Y {
                        "minecraft:grass_block"
                    } else {
                        "minecraft:dirt"
                    };
                    grid.seed(lx, y, lz, state.to_owned());
                }
            }
        }

        census::reset();
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        // Step index 5 is `patch_grass_plain`'s real position in plains'
        // `VEGETAL_DECORATION` array (see `biome/plains.json`); it only feeds
        // `setFeatureSeed`, so it changes *which* positions are drawn, never how
        // many.
        apply_vegetal_decoration_step(&mut random, 42, 0, 0, &mut grid, &tags, &[(5, placed)]);
        let c = census::snapshot();

        let expected_low = NOISE_BELOW * COUNT;
        let expected_high = NOISE_ABOVE * COUNT;
        assert!(
            c.block_predicate_filter_in == expected_low
                || c.block_predicate_filter_in == expected_high,
            "patch_grass_plain pushed {} positions into its trailing predicate; its own \
             placement JSON predicts exactly noise_threshold_count x count = \
             {NOISE_BELOW}x{COUNT}={expected_low} or {NOISE_ABOVE}x{COUNT}={expected_high}. \
             Wrong hypotheses and their values: count modifier dropped => {NOISE_BELOW} or \
             {NOISE_ABOVE}; noise_threshold_count dropped => {COUNT}; both dropped => 1. \
             Full census: {c:?}",
            c.block_predicate_filter_in
        );

        // The count alone does not prove blocks were written — a pipeline that
        // produces every position and then writes nowhere is exactly the
        // `VegGrid` absolute-vs-local regression documented above.
        assert!(
            c.simple_block > 0 && c.writes > 0,
            "positions reached the predicate but no short_grass was written \
             (simple_block={}, writes={}, rejected={}, unsupported_ground={}) — the \
             pipeline ran and reached zero blocks",
            c.simple_block,
            c.writes,
            c.writes_rejected,
            c.simple_block_unsupported_ground
        );
        // `patch_grass_plain`'s configured feature is `minecraft:grass`
        // (`simple_block`), which this engine implements — so nothing on this
        // path may land in the unmodelled bucket.
        assert!(
            c.unsupported.is_empty(),
            "patch_grass_plain reached an unmodelled feature type: {:?}",
            c.unsupported
        );
    }

    /// Control for the gate above, proving its subject is the pipeline and not
    /// the harness: feeding the SAME grid and RNG a placed feature whose
    /// `placement` array has had the `count` modifier removed must land on the
    /// dropped-`count` hypothesis (`5` or `10`) and therefore fail the pair
    /// assertion. Without this, "the value was 160" is consistent with a
    /// harness that would have printed 160 no matter what pipeline ran.
    #[test]
    fn grass_patch_attempt_count_control_fires_when_the_count_modifier_is_removed() {
        use lodestone_worldgen::feature::vegetation::{
            apply_vegetal_decoration_step, build_veg_tags, census, resolve_placed_feature_ref,
            VegGrid,
        };
        use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};

        const MIN_Y: i32 = -64;
        const HEIGHT: i32 = 384;
        const SURFACE_Y: i32 = 64;

        let mut doc = EmbeddedResolver.placed_feature("minecraft:patch_grass_plain");
        let before = doc["placement"].as_array().expect("placement array").len();
        doc["placement"] = Value::Array(
            doc["placement"]
                .as_array()
                .expect("placement array")
                .iter()
                .filter(|m| m["type"] != "minecraft:count")
                .cloned()
                .collect(),
        );
        let after = doc["placement"].as_array().expect("placement array").len();
        assert_eq!(
            after,
            before - 1,
            "the control must actually remove exactly one modifier, else it measures nothing"
        );

        let tags = build_veg_tags(&EmbeddedResolver);
        let placed = resolve_placed_feature_ref(&EmbeddedResolver, &doc);

        let mut grid = VegGrid::new(MIN_Y, HEIGHT, 0, 0);
        for lz in 0..16 {
            for lx in 0..16 {
                for y in MIN_Y..=SURFACE_Y {
                    let state = if y == SURFACE_Y {
                        "minecraft:grass_block"
                    } else {
                        "minecraft:dirt"
                    };
                    grid.seed(lx, y, lz, state.to_owned());
                }
            }
        }

        census::reset();
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        apply_vegetal_decoration_step(&mut random, 42, 0, 0, &mut grid, &tags, &[(5, placed)]);
        let c = census::snapshot();

        assert!(
            c.block_predicate_filter_in == 5 || c.block_predicate_filter_in == 10,
            "removing `count` must collapse the attempt count to the bare \
             noise_threshold_count value (5 or 10), proving the gate above measures the \
             `count` modifier rather than a constant; got {}",
            c.block_predicate_filter_in
        );
        assert!(
            c.block_predicate_filter_in != 160 && c.block_predicate_filter_in != 320,
            "the control must FAIL the real gate's pair assertion; it did not"
        );
    }

    /// Every vegetation block state [`overworld_generator`] can emit, sorted.
    /// Not a curated allow-list — it is the measured output of the sweep below,
    /// and the sweep asserts every entry it finds is either in here or matched by
    /// [`is_vegetation_state`]'s substring rule, so a newly-implemented placer
    /// shows up as a failure telling you to add its blocks rather than being
    /// silently absorbed.
    const VEGETATION_SUBSTRINGS: &[&str] = &[
        "_log", "leaves", "short_grass", "tall_grass", "bush", "flower", "poppy", "dandelion",
        "mushroom", "sugar_cane", "pumpkin", "azure", "oxeye", "cornflower", "tulip", "daisy",
        "allium", "sapling", "fern", "leaf_litter", "cactus", "lichen", "wildflowers",
    ];

    fn is_vegetation_state(base: &str) -> bool {
        VEGETATION_SUBSTRINGS.iter().any(|s| base.contains(s))
    }

    /// **island** gate: the composed production pipeline
    /// (`EmbeddedResolver`'s bundled data -> real generated terrain -> the 3x3
    /// vegetal-decoration driver -> the fold back into a `GeneratedColumn`) must
    /// put real vegetation blocks into served columns.
    ///
    /// # Why this exists as a separate gate, and why it was missing
    ///
    /// [`plains_grass_patch_attempt_count_matches_the_placement_json`] proves the
    /// *pipeline arithmetic* against a synthetic grid.
    /// [`vegetation_placer_gaps_are_named_not_silent`] proves the *resolve* step
    /// names every unimplemented placer. Neither runs `OverworldGenerator::column`,
    /// so neither can see the failure this crate has already shipped once: the
    /// absolute-vs-local `VegGrid` coordinate bug recorded in
    /// `lodestone_worldgen::feature::vegetation::VegGrid`'s own doc comment, where
    /// composition ran, resolution was clean, every hermetic test was green, and
    /// **every served chunk got zero vegetation** because the write path compared
    /// absolute coordinates against a local bound.
    ///
    /// That doc comment still names the gate that caught it —
    /// `diagnostic_vegetation_counts_over_plains_sweep` — but the gate itself was
    /// deleted at some point before `074b5e9` and only the reference survived. So
    /// for an unknown span this crate had a written record of a regression and
    /// nothing watching for its return. This is that gate, restored with a
    /// predicted magnitude instead of a diagnostic print.
    ///
    /// # Coordinates, chosen before any number was known
    ///
    /// A fixed stride-9 5x5 lattice from chunk `(-40, -40)` at seed 42 — a rule
    /// stated as a rule, not a set of coordinates picked after seeing which ones
    /// had trees (CLAUDE.md's evidence standard on cherry-picked coordinates).
    /// Stride 9 rather than 1 so no two centres share a 3x3 neighbourhood, and 25
    /// chunks so the lattice spans ~600 blocks and cannot be entirely one biome.
    ///
    /// # The floor, and what it excludes
    ///
    /// The dominant historical failure mode of this class is **exactly zero**, so
    /// a floor's real job is to also exclude the *quiet* version: a silently
    /// dropped placement modifier. The gate above measures that ratio directly —
    /// removing `patch_grass_plain`'s `count` took its attempt count from 320 to
    /// 10, a factor of 32, purely from the JSON. [`VEGETATION_FLOOR`] is set well
    /// below the measured healthy total but more than 32x above zero, so a
    /// single-modifier drop anywhere in the common grass/tree path fails here too,
    /// not just a total blackout. The absolute anchor comes from the engine that
    /// `lodestone_worldgen::tests::vegetation_parity` validates block-for-block
    /// against `scripts/worldgen-oracle/VegetationOracle.java`; the *ratio* is
    /// JSON-derived. That split is the honest description — do not restate the
    /// floor as an independently predicted absolute.
    ///
    /// Failure prints the per-biome breakdown, not just the total: a gate
    /// reporting one aggregate cannot distinguish "uniformly thin" from "one
    /// biome contributes everything" (CLAUDE.md: measure by location, never by
    /// frame average).
    #[test]
    fn vegetation_reaches_real_blocks_over_a_production_sweep() {
        use std::collections::BTreeMap;

        /// See this test's doc comment. Measured healthy total over this exact
        /// lattice is **3269** blocks (observed by raising this constant until
        /// the assertion fired, so the failure path is exercised, not assumed);
        /// 300 sits ~11x under that, and ~3x above what a single dropped
        /// placement modifier (a 32x cut, measured) would leave of it.
        const VEGETATION_FLOOR: usize = 300;
        const STRIDE: i32 = 9;
        const SIDE: i32 = 5;
        const ORIGIN: i32 = -40;

        let generator = overworld_generator(42);
        // biome -> (chunks, vegetation blocks, per-state counts)
        let mut per_biome: BTreeMap<String, (usize, usize, BTreeMap<String, usize>)> =
            BTreeMap::new();
        let mut total = 0usize;

        for i in 0..SIDE {
            for j in 0..SIDE {
                let (cx, cz) = (ORIGIN + i * STRIDE, ORIGIN + j * STRIDE);
                let col = generator.column(cx, cz);
                let biome = col.biome_state(8, 8).to_owned();
                let mut counts: BTreeMap<String, usize> = BTreeMap::new();
                let mut chunk_total = 0usize;
                for lz in 0..16 {
                    for lx in 0..16 {
                        // Only the band around the surface can carry vegetation,
                        // and scanning the full 384 rows for 25 columns is the
                        // difference between a ~30s gate and a ~90s one.
                        let top = col.top_non_air_y(lx, lz);
                        let lo = (top - 8).max(col.min_y());
                        let hi = (top + 40).min(col.min_y() + col.height() - 1);
                        for y in lo..=hi {
                            let state = col.block_state(lx, y, lz);
                            let base = state.split('[').next().unwrap_or(state);
                            if is_vegetation_state(base) {
                                *counts.entry(base.to_owned()).or_default() += 1;
                                chunk_total += 1;
                            }
                        }
                    }
                }
                total += chunk_total;
                let entry = per_biome.entry(biome).or_insert((0, 0, BTreeMap::new()));
                entry.0 += 1;
                entry.1 += chunk_total;
                for (state, n) in counts {
                    *entry.2.entry(state).or_default() += n;
                }
            }
        }

        let breakdown = per_biome
            .iter()
            .map(|(biome, (chunks, veg, states))| {
                format!("  {biome}: {chunks} chunks, {veg} veg blocks, {states:?}")
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            total >= VEGETATION_FLOOR,
            "the composed production pipeline put {total} vegetation blocks into a \
             {SIDE}x{SIDE} stride-{STRIDE} lattice from chunk ({ORIGIN},{ORIGIN}) at seed 42; \
             at least {VEGETATION_FLOOR} is required. Zero means vegetal decoration reached \
             no served block at all (the `VegGrid` coordinate regression); a value one to two \
             orders of magnitude short means a placement modifier is being silently dropped \
             from a pipeline (see \
             `plains_grass_patch_attempt_count_matches_the_placement_json`, which measures \
             that ratio as 32x for `patch_grass_plain`). Per-biome breakdown:\n{breakdown}"
        );

        // Trees specifically: grass and flowers are `simple_block`, one write
        // each, and would carry the total on their own. A tree exercises
        // `TreeConfig` -- trunk placer, foliage placer, leaf-distance update --
        // an entirely separate code path whose absence is precisely what issue
        // guards against.
        let logs: usize = per_biome
            .values()
            .flat_map(|(_, _, states)| states.iter())
            .filter(|(state, _)| state.ends_with("_log"))
            .map(|(_, n)| *n)
            .sum();
        assert!(
            logs > 0,
            "no tree logs anywhere in the lattice — grass may be placing while the \
             `ConfiguredFeature::Tree` path reaches zero blocks, which is the specific \
             symptom issue #478 reported. Per-biome breakdown:\n{breakdown}"
        );
    }

    #[test]
    fn generator_builds_and_produces_real_terrain() {
        let generator = overworld_generator(42);
        let col = generator.column(0, 0);
        // Anti-vacuity: a real column is neither all air nor all one block.
        let non_air = col.non_air_count();
        assert!(
            non_air > 16 * 16 * 10,
            "bundled generator produced near-empty column ({non_air} non-air)"
        );
        let mut kinds = std::collections::BTreeSet::new();
        for lz in 0..16 {
            for lx in 0..16 {
                for y in col.min_y()..col.min_y() + col.height() {
                    let b = col.block_state(lx, y, lz);
                    kinds.insert(b.split('[').next().unwrap_or(b).to_string());
                }
            }
        }
        assert!(
            kinds.len() >= 3,
            "expected shape+fluid+surface variety, got only {kinds:?}"
        );
    }

    /// The integrated server's chunk source must serve the **real** generator
    /// block-for-block — no simplified terrain one layer in. This diffs the
    /// [`crate::ChunkSource`] output against the generator over a whole column
    /// and floors on fluid + surface presence so it can't pass on empty air.
    #[test]
    fn chunk_source_serves_generator_block_for_block() {
        use crate::ChunkSource;

        let seed = 42; // chunk (0,0) is a submerged ocean column at this seed.
        let generator = overworld_generator(seed);
        let source = overworld_chunk_source(seed);
        let expected = generator.column(0, 0);
        let served = source.column(0, 0);

        assert_eq!(served.min_y, expected.min_y());
        assert_eq!(served.height, expected.height());

        let mut checked = 0usize;
        let mut water = 0usize;
        let mut surface = 0usize; // non-stone solid: grass/dirt/sand/gravel/…
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for y in served.min_y..served.min_y + served.height {
                    let want = expected.block_state(lx as usize, y, lz as usize);
                    let got = served.block_state(lx, y, lz);
                    assert_eq!(got, want, "served/generated mismatch at ({lx},{y},{lz})");
                    checked += 1;
                    match got.split('[').next().unwrap_or(got) {
                        "minecraft:water" => water += 1,
                        "minecraft:air"
                        | "minecraft:cave_air"
                        | "minecraft:void_air"
                        | "minecraft:lava"
                        | "minecraft:stone"
                        | "minecraft:bedrock" => {}
                        _ => surface += 1,
                    }
                }
            }
        }
        // The comparison loop covered the whole column (not a short-circuit).
        assert_eq!(checked, 16 * 16 * served.height as usize);
        // Fluid fill survived into the served chunk (this ocean column is wet).
        assert!(water > 0, "served ocean chunk has no water — fluid stage lost");
        // Surface rules survived too (gravel/dirt on the ocean floor).
        assert!(
            surface > 0,
            "served chunk has no surface material — surface stage lost"
        );
    }

    /// Exact biome-id parity against vanilla's own `RandomState.sampler()` +
    /// `MultiNoiseBiomeSourceParameterList.findValueBruteForce`.
    ///
    /// Ground truth: `scripts/worldgen-oracle/BiomeOracle.java` `sample`
    /// mode, seed 42, at each column's own quart-aligned corner and its own
    /// generated terrain surface height (`y` rounded down to a multiple of 4
    /// — see [`lodestone_worldgen::overworld::OverworldGenerator::biome_stage`]'s
    /// doc comment for why *both* axes need quart-rounding, found the hard
    /// way: getting either wrong flips a real dark_forest/river boundary at
    /// world `(0, 0)`, one of the fixtures below). This is a *predicted
    /// value*, not a "some variety appeared" check — CLAUDE.md's "predict
    /// the value, not the sign": a climate-band-boundary-off-by-one bug would
    /// still show *some* biome, so only an exact match against vanilla's own
    /// answer catches it.
    #[test]
    fn biome_matches_vanilla_at_known_coordinates_seed_42() {
        let seed = 42;
        let generator = overworld_generator(seed);

        // (world x, world z, vanilla's own answer at that column's quart
        // corner and generated surface height).
        let cases: &[(i32, i32, &str)] = &[
            (0, 0, "minecraft:dark_forest"),
            (8, 8, "minecraft:river"),
            (-8, 8, "minecraft:dark_forest"),
            (500, 500, "minecraft:deep_ocean"),
            (-500, 500, "minecraft:beach"),
            (2000, -1500, "minecraft:swamp"),
            (10000, 10000, "minecraft:deep_ocean"),
            (300, -800, "minecraft:plains"),
            (-4000, 100, "minecraft:lukewarm_ocean"),
            (1000, 0, "minecraft:deep_cold_ocean"),
            (0, 1000, "minecraft:beach"),
            (5000, 5000, "minecraft:warm_ocean"),
            (-10000, -10000, "minecraft:plains"),
            (120, 4564, "minecraft:river"),
            (776, -780, "minecraft:frozen_peaks"),
            (64, 64, "minecraft:beach"),
            (-2500, 3200, "minecraft:savanna"),
        ];

        let mut distinct = std::collections::BTreeSet::new();
        for &(x, z, want) in cases {
            let cx = x.div_euclid(16);
            let cz = z.div_euclid(16);
            let lx = x.rem_euclid(16) as usize;
            let lz = z.rem_euclid(16) as usize;
            let col = generator.column(cx, cz);
            let got = col.biome_state(lx, lz);
            assert_eq!(got, want, "biome mismatch at world ({x}, {z})");
            distinct.insert(got.to_string());
        }
        // Anti-vacuity floor, per CLAUDE.md's "magnitude" vacuous-test
        // species: a table/search bug that happened to return one constant
        // biome for every probed *coordinate* would still fail the loop
        // above at 16/17 cases — but a bug that returns one constant biome
        // whenever asked (ignoring climate entirely) needs a *count* check
        // to catch, since it could theoretically pass every exact-match
        // assertion if all 17 fixtures shared their expected biome (they do
        // not, by construction — this asserts that fact rather than
        // assuming it). 10 is derived from this exact probe set's own
        // distinct answers above, not guessed.
        assert!(
            distinct.len() >= 10,
            "expected wide biome variety across the probe set, got only {distinct:?}"
        );
    }

    /// Control for [`biome_matches_vanilla_at_known_coordinates_seed_42`]'s
    /// implicit claim that the search can return *different* answers for
    /// different inputs: run it and watch a single chunk (0, 0) — which
    /// straddles the `dark_forest`/`river` boundary the fixture above
    /// already names — actually produce both biomes across its 16 quarts,
    /// not one biome copy-pasted 16 times.
    #[test]
    fn a_single_chunk_can_carry_more_than_one_biome() {
        let generator = overworld_generator(42);
        let col = generator.column(0, 0);
        assert!(
            col.distinct_biome_count() >= 2,
            "chunk (0,0) at seed 42 is known (BiomeOracle) to straddle a \
             dark_forest/river boundary; got only one biome across all 16 quarts"
        );
    }

    /// **Superseded property, inverted.** This test used to assert served
    /// columns *never* resolve to badlands/eroded_badlands/wooded_badlands,
    /// because `lodestone_worldgen::biome::usable_overworld_table` used to
    /// exclude them (their surface rule reached an unported
    /// `SurfaceSystem.getBand`, which would panic). `3cf523c` ported
    /// `getBand` (`crate::surface::Rule::Bandlands`) and made
    /// `usable_overworld_table` a pass-through — see that function's own doc
    /// comment, which names this exact test as needing this update. The old
    /// assertion's premise is gone: a column can resolve to any of the three
    /// again, so asserting it never does is now testing a stale invariant,
    /// not a real one.
    ///
    /// Re-verified before rewriting, per `CLAUDE.md`'s "re-verify before
    /// routing around": running the *old* assertion against this tree
    /// (`cargo test -p lodestone-server … served_columns_never_carry_an_unported_badlands_variant
    /// -- --nocapture`) passed — the 12×12 sweep at seed 42 happens not to
    /// cross a badlands boundary in this exact window, so the old test was a
    /// time bomb (would fail the moment correct code touched badlands
    /// climate here), not a test that was actually red on `main` right now.
    ///
    /// That finding is exactly why the sweep alone is insufficient evidence
    /// for the *new* property too: scanning only `-6..6` would find zero
    /// badlands cells and pass vacuously, proving nothing (`CLAUDE.md`'s
    /// "assertions of an absence need a control proving the detector
    /// works" — the mirror image applies to an assertion of *presence*).
    /// `docs/worldgen-parity.md`'s own measured finding — chunk
    /// `(-120,-120)`'s real vanilla biome is badlands/eroded_badlands — is
    /// added to the coordinate list for exactly that reason, so
    /// `badlands_cells > 0` below is asserted, not merely hoped for.
    ///
    /// The predicted value set is not "some badlands block": vanilla's own `SurfaceSystem
    /// .generateBands` (lines 286-316 of its decompiled source)
    /// and this port's `generate_bands`
    /// (`crates/lodestone-worldgen/src/surface/mod.rs:170-209`) can only ever
    /// emit exactly these seven blocks: base `minecraft:terracotta`
    /// (java:287-288, rust:171), `minecraft:orange_terracotta`
    /// (java:292-293, rust:184), `minecraft:yellow_terracotta` (java:297,
    /// rust:189), `minecraft:brown_terracotta` (java:298, rust:190),
    /// `minecraft:red_terracotta` (java:299, rust:191),
    /// `minecraft:white_terracotta` (java:303-304, rust:197) and
    /// `minecraft:light_gray_terracotta` (java:306/310, rust:199/202) — no
    /// other block can ever come back from `Rule::Bandlands`/`getBand`
    /// (vanilla's own `SurfaceSystem`, lines 332-334). These are the only blocks this test's
    /// terracotta scan can match, so a false positive from an unrelated
    /// block is not possible.
    #[test]
    fn badlands_columns_when_present_carry_terracotta_bands() {
        const TERRACOTTA_BAND_BLOCKS: [&str; 7] = [
            "minecraft:terracotta",
            "minecraft:orange_terracotta",
            "minecraft:yellow_terracotta",
            "minecraft:brown_terracotta",
            "minecraft:red_terracotta",
            "minecraft:white_terracotta",
            "minecraft:light_gray_terracotta",
        ];

        let generator = overworld_generator(42);

        // Same 12×12 sweep the old test used, plus the one coordinate
        // `docs/worldgen-parity.md` already measured as real-vanilla
        // badlands at this seed — without it, this test's core claim would
        // never actually fire against this window (see doc comment above).
        let mut coords: Vec<(i32, i32)> = Vec::new();
        for cx in -6..6 {
            for cz in -6..6 {
                coords.push((cx, cz));
            }
        }
        coords.push((-120, -120));

        let mut badlands_cells = 0usize;
        let mut band_hits = 0usize;
        for (cx, cz) in coords {
            let col = generator.column(cx, cz);
            let min_y = col.min_y();
            let height = col.height();
            for lz in 0..16usize {
                for lx in 0..16usize {
                    let biome = col.biome_state(lx, lz);
                    if !lodestone_worldgen::biome::UNSUPPORTED_SURFACE_BIOMES.contains(&biome) {
                        continue;
                    }
                    badlands_cells += 1;
                    for y in min_y..min_y + height {
                        let state = col.block_state(lx, y, lz);
                        let base = state.split('[').next().unwrap_or(state);
                        if TERRACOTTA_BAND_BLOCKS.contains(&base) {
                            band_hits += 1;
                        }
                    }
                }
            }
        }

        assert!(
            badlands_cells > 0,
            "test's own premise failed: expected at least one badlands/eroded_badlands/\
             wooded_badlands cell across the 12×12 sweep plus the known-badlands chunk \
             (-120,-120), found none — this test would otherwise pass vacuously"
        );
        assert!(
            band_hits > 0,
            "found {badlands_cells} badlands cell(s) across {} columns but none carried any of \
             the 7 possible terracotta band blocks — SurfaceSystem.getBand \
             (vanilla's own SurfaceSystem, lines 332-334) / Rule::Bandlands is not reaching them",
            12 * 12 + 1
        );
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
        let pre = source.block_state(0, -50, 0);
        assert_eq!(
            pre.split('[').next(),
            Some("minecraft:deepslate"),
            "test fixture assumption broke: expected solid deepslate at (0,-50,0), found {pre}"
        );

        source.set_block(0, -50, 0, "minecraft:air");
        assert_eq!(source.block_state(0, -50, 0), "minecraft:air");

        // Re-fetch the whole column again — simulating the column being
        // forgotten and re-sent, `crate::server`'s `ViewTracker` forget/resend
        // cycle — through a *second, independent* `column()` call. Without
        // retention this would silently regenerate the original deepslate.
        let recolumn = source.column(0, 0);
        assert_eq!(recolumn.block_state(0, -50, 0), "minecraft:air");

        // The edit must be scoped to exactly the touched cell, not a
        // wholesale wipe of the column: an adjacent, untouched cell in the
        // same column still reads the generator's original content.
        assert_eq!(
            recolumn.block_state(1, -50, 0).split('[').next(),
            Some("minecraft:deepslate"),
            "editing (0,-50,0) must not affect its untouched neighbour"
        );
    }

    /// **Diagnostic control** for `crate::chunk::tests
    /// ::parallel_generation_is_deterministic_and_matches_serial` (issue
    /// which distinguishes value determinism from palette-order determinism. That
    /// test compares serialised bytes (palette order included) across
    /// independent `column()` calls, so a byte mismatch could mean either
    /// "the actual blocks differ" or "the same blocks, a different palette
    /// assignment order" — this isolates which, for the exact chunks that
    /// test uses, with no threading involved at all.
    ///
    /// **Made vacuous by `6509a97`'s pre-ore memoisation cache, now fixed.**
    /// `OverworldGenerator::store` (`crates/lodestone-worldgen/src/overworld/mod.rs`,
    /// with the store itself in `overworld/store.rs`) is a field on the generator
    /// instance, keyed by exact `(cx, cz)`. This
    /// test used to call `column()` twice on *one* `generator`, so the
    /// second call was served straight out of the first call's cache entry
    /// — literally the same `Arc<PreOreResult>` — which guarantees identical
    /// bytes by **pointer identity**, not by `column()` being deterministic.
    /// A regression that reintroduced the historical palette-order bug (see
    /// `crate::overworld::OverworldGenerator::materialize_world`'s own doc
    /// comment — iterating a `surface_diff` `HashMap` directly instead of a
    /// fixed-order point lookup) would still pass this test, because both
    /// calls would still hit the one cached value.
    ///
    /// Fixed by building **two independently-constructed generators** —
    /// each gets its own empty cache, its own `HashMap<String, f32>`
    /// temperature table, its own everything — so a byte match here again
    /// means real determinism, not a shared cache entry. This is also the
    /// property a server restart actually needs: two separate process
    /// lifetimes must generate the same chunk from the same seed.
    ///
    /// If this passes, `OverworldGenerator::column` is a pure function of
    /// `(self.seed_and_settings, cx, cz)` as designed and the failure
    /// is not a value-determinism bug in ore composition itself.
    #[test]
    fn column_is_byte_identical_across_two_independently_constructed_generators() {
        let generator_a = overworld_generator(42);
        let generator_b = overworld_generator(42);
        for &(cx, cz) in &[(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (2, -1)] {
            let a = generator_a.column(cx, cz);
            let b = generator_b.column(cx, cz);
            let (a_min_y, a_height, a_palette, a_blocks, a_biomes) = a.into_raw();
            let (b_min_y, b_height, b_palette, b_blocks, b_biomes) = b.into_raw();
            assert_eq!(
                a_min_y, b_min_y,
                "chunk ({cx},{cz}) min_y differs between two independently constructed generators"
            );
            assert_eq!(
                a_height, b_height,
                "chunk ({cx},{cz}) height differs between two independently constructed generators"
            );
            assert_eq!(
                a_palette, b_palette,
                "chunk ({cx},{cz}) palette differs between two independently constructed generators \
                 — a non-determinism bug or a palette-assignment-order difference, not threading"
            );
            assert_eq!(
                a_blocks, b_blocks,
                "chunk ({cx},{cz}) block indices differ between two independently constructed generators"
            );
            assert_eq!(
                a_biomes, b_biomes,
                "chunk ({cx},{cz}) biome quarts differ between two independently constructed generators"
            );
        }
    }

    /// End-to-end: real vegetation reaches the **served** column for a
    /// known plains chunk — closing the exact island CLAUDE.md's rule 1
    /// warns about, and the specific coordinate-translation failure it catches:
    /// `crate::feature::vegetation::VegGrid` used to store *and expose*
    /// chunk-local coordinates while every position the placement engine
    /// computes is absolute — so vegetation composed at construction time,
    /// ran without erroring, and placed **zero** blocks in every chunk
    /// except `(0, 0)` (`in_bounds`/`get` compared an absolute world
    /// coordinate against a `0..16` bound that was essentially always
    /// false). `crate::feature::vegetation`'s own hermetic unit tests never
    /// caught this because every one of them happened to use `origin =
    /// BlockPos { x: 8, y: 70, z: 8 }` — coincidentally already "local".
    /// Chunk `(18, -50)` (world `(300, -800)`) is the same known-plains
    /// fixture `biome_matches_vanilla_at_known_coordinates_seed_42` already
    /// names, so this isn't a freshly-picked coordinate chosen to make the
    /// test pass.
    #[test]
    fn vegetation_reaches_a_known_plains_chunk() {
        let generator = overworld_generator(42);
        let col = generator.column(18, -50);
        assert_eq!(col.biome_state(12, 0), "minecraft:plains");

        let mut grass = 0usize;
        for lz in 0..16usize {
            for lx in 0..16usize {
                for y in col.min_y()..col.min_y() + col.height() {
                    if col.block_state(lx, y, lz) == "minecraft:short_grass" {
                        grass += 1;
                    }
                }
            }
        }
        assert!(
            grass > 0,
            "a plains chunk composed through the served pipeline must carry grass"
        );
    }

    /// An aggregate-statistics gate checks tree count per biome against an
    /// expected band. The band is predicted from the embedded placement JSON
    /// rather than a live
    /// vanilla dump (no JVM oracle for vegetation exists yet — see
    /// `crate::feature::vegetation`'s module doc). Two independent
    /// predictions, both computed *before* looking at the measured numbers
    /// (recorded here so a future reader can see the reasoning, not just
    /// the assertion):
    ///
    /// - **Grass upper bound**: `patch_grass_plain.json`'s outer
    ///   `noise_threshold_count` yields 5 or 10 attempts per chunk, each
    ///   feeding an inner `count: 32` — so at most `10 * 32 = 320` candidate
    ///   `short_grass` placements per chunk, before the final
    ///   `block_predicate_filter` (air) and `canSurvive` (support-block)
    ///   checks reject most of them. Measured must be `> 0` and comfortably
    ///   under `320 * chunk_count`.
    /// - **Oak logs**: `trees_plains.json`'s outer count is
    ///   `weighted_list{0: 19, 1: 1}`
    ///   (`IntProvider::expected_value() == 0.05`), and the `oak`
    ///   configured-feature branch of `trees_plains`'s `RandomSelector`
    ///   survives with probability `(1 - 0.33333334) * (1 - 0.0125) ≈
    ///   0.6579` (the `fancy_oak`/`fallen_oak` branches are
    ///   `ConfiguredFeature::Unsupported` — see module doc). A successful
    ///   straight oak trunk places `base_height=4` to `4+2=6` logs. So the
    ///   *isolated, single-chunk* expected oak-log count per chunk is
    ///   `0.05 * 0.6579 * (4..6) ≈ 0.132..0.197`, i.e. **not zero, and not
    ///   large** — over a 64-chunk sweep, `8.4..12.6` logs. Measured under
    /// a single-chunk simulation baseline: `12`, inside that band.
    ///
    /// **The 3×3 driver measures `6` logs — a real drop,
    ///   not a regression.** The isolated prediction above assumes each
    ///   swept chunk's tree placement reads only its OWN terrain; the real
    ///   3×3 driver now lets an edge-adjacent tree's space-check
    ///   (`place_tree`'s `getMaxFreeTreeHeight`-equivalent scan) read the
    ///   TRUE neighbour terrain at the tree's own absolute height instead of
    ///   the old clamped approximation (which just re-read the centre's own
    ///   nearest in-bounds column — usually open air above a similar
    ///   surface height, so it almost always reported "free"). Real terrain
    ///   height genuinely varies chunk to chunk; when a neighbour's surface
    ///   is taller than the centre's at the probed offset, the scan now sees
    ///   real solid ground where the old approximation saw air, and the tree
    ///   is correctly rejected instead of spuriously placed. Confirmed to be
    ///   this mechanism, not an unrelated defect, by re-running this exact
    ///   sweep with `LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1` (the debug escape
    ///   hatch in `OverworldGenerator::vegetation_stage` that reverts to the
    /// single-source-only control): that reproduces `12`, exactly
    ///   the old measurement, with no other code changed — the entire delta
    ///   is attributable to the 3×3 driver's real neighbour reads, per
    ///   CLAUDE.md's evidence standard ("a control's premise" — here, that
    ///   flipping only the 3×3-vs-single-source toggle recovers the old
    ///   number — "proving the detector/mechanism actually fired").
    ///   This is an internal-consistency check against the engine's own
    ///   inputs, not vanilla parity (named explicitly, per
    ///   `crate::feature::vegetation`'s own module doc and this crate's
    ///   evidence standard) — the isolated band remains documented above as
    ///   a floor on what single-chunk-only placement alone would produce,
    ///   but the assertion below now widens to also accept the real,
    ///   measured 3×3 reduction rather than asserting a number this
    ///   docstring cannot re-derive analytically (real terrain height
    ///   variance has no closed form here) as if it could.
    #[test]
    fn plains_vegetation_counts_are_predicted_and_measured() {
        let generator = overworld_generator(42);
        // (18, -50) is world (300, -800), a known plains chunk
        // (`biome_matches_vanilla_at_known_coordinates_seed_42`).
        let base_cx = 18;
        let base_cz = -50;
        let sweep_chunks = 8 * 8;
        let mut grass = 0usize;
        let mut flowers = 0usize;
        let mut logs = 0usize;
        let mut leaves = 0usize;
        let mut plains_touching_chunks = 0usize;
        for dcx in 0..8 {
            for dcz in 0..8 {
                let cx = base_cx + dcx;
                let cz = base_cz + dcz;
                let col = generator.column(cx, cz);
                let mut any_plains = false;
                for lz in 0..16usize {
                    for lx in 0..16usize {
                        if col.biome_state(lx, lz) == "minecraft:plains" {
                            any_plains = true;
                        }
                        for y in col.min_y()..col.min_y() + col.height() {
                            let b = col.block_state(lx, y, lz);
                            let base = b.split('[').next().unwrap_or(b);
                            match base {
                                "minecraft:short_grass" => grass += 1,
                                "minecraft:dandelion" | "minecraft:poppy" | "minecraft:azure_bluet"
                                | "minecraft:oxeye_daisy" | "minecraft:cornflower"
                                | "minecraft:orange_tulip" | "minecraft:red_tulip"
                                | "minecraft:pink_tulip" | "minecraft:white_tulip" => flowers += 1,
                                "minecraft:oak_log" => logs += 1,
                                "minecraft:oak_leaves" => leaves += 1,
                                _ => {}
                            }
                        }
                    }
                }
                if any_plains {
                    plains_touching_chunks += 1;
                }
            }
        }
        // Anti-vacuity floor per CLAUDE.md's "world" vacuous-test species:
        // the sweep must actually contain plains, or every assertion below
        // would pass by both sides being empty.
        assert!(
            plains_touching_chunks > 0,
            "test's own premise failed: the 8x8 sweep from chunk ({base_cx},{base_cz}) contains \
             no plains — pick a different anchor before trusting anything below"
        );

        // Grass: measured must be positive, and bounded well under the
        // structural upper bound (10 outer * 32 inner = 320 candidates per
        // chunk, before survival checks).
        assert!(grass > 0, "measured zero grass over a plains-touching sweep");
        assert!(
            grass < 320 * sweep_chunks,
            "measured grass ({grass}) exceeds the structural upper bound \
             (320 candidates/chunk * {sweep_chunks} chunks) — the placement \
             pipeline is over-counting, not merely dense"
        );

        // Oak logs: predicted band from the JSON's own IntProvider, not a
        // guessed number — see this test's own doc comment for the
        // derivation. `0.05 * 0.6579 * 4 = 0.1316`, `* 6 = 0.1974`, times 64
        // chunks.
        let isolated_min = 0.05 * 0.6579 * 4.0 * sweep_chunks as f64;
        let isolated_max = 0.05 * 0.6579 * 6.0 * sweep_chunks as f64;
        // The 3×3 driver's edge-adjacent space-check
        // reads TRUE neighbour terrain (see this test's own doc comment for
        // the mechanism and the `LODESTONE_VEG_SINGLE_SOURCE_DEBUG=1`
        // control that isolated it), which can legitimately reject a tree
        // the old clamped approximation always let through — measured `6`,
        // half the single-source control's measurement of `12`. The floor is loosened to
        // `0.25x` the isolated-model's own minimum (not lowered to the bare
        // `> 0` anti-vacuity floor above, which would make this assertion
        // vacuous against a real regression that drove logs to near-zero)
        // rather than re-centred on `6` itself, since `6` is one sample from
        // one real-terrain sweep, not a value with a closed-form derivation
        // this docstring could defend the way the isolated band's `8.4..
        // 12.6` is defended.
        let min = isolated_min * 0.25;
        let max = isolated_max * 1.5;
        assert!(
            (min..=max).contains(&(logs as f64)),
            "measured oak logs ({logs}) over {sweep_chunks} chunks is outside the band \
             [{min:.1}, {max:.1}] — the isolated single-chunk model predicts \
             [{isolated_min:.1}, {isolated_max:.1}] (trees_plains.json's own weighted_list \
             count and RandomSelector branch chances), widened downward for issue #427's real \
             3x3 driver rejecting more edge-adjacent trees against true neighbour terrain (see \
             this test's own doc comment) and upward for sampling noise across which of the \
             swept chunks actually resolve to plains at their own carver-source corner"
        );
        // A tree with logs must also carry leaves (the "not enough room"
        // gate and the log/leaf presence check in `place_tree` both require
        // this — see `crate::feature::vegetation::place_tree`).
        assert!(
            logs == 0 || leaves > 0,
            "measured {logs} oak logs but zero leaves — a real straight-trunk tree always \
             carries both"
        );
        // Flowers are gated behind a rarer noise_threshold_count + a
        // rarity_filter(32) on top — expect them present but sparse
        // relative to grass.
        assert!(flowers > 0, "measured zero flowers over a plains-touching sweep");
        assert!(
            flowers < grass,
            "flowers ({flowers}) should be sparser than grass ({grass}) given \
             flower_plains.json's extra rarity_filter(32) the grass pipeline lacks"
        );
    }

    /// `build_biome_vegetation` must resolve plains' real `trees_plains`/
    /// `flower_plains`/`patch_grass_plain` entries into the concrete
    /// [`ConfiguredFeature`](lodestone_worldgen::feature::vegetation::ConfiguredFeature)
    /// variants this engine actually implements — a construction-time
    /// regression control for the composition step
    /// [`plains_vegetation_counts_are_predicted_and_measured`] depends on:
    /// if any of these three silently degraded to `Unsupported`, that test
    /// would still measure *some* output from the other plains entries and
    /// could mask the regression.
    #[test]
    fn build_biome_vegetation_resolves_plains_grass_flower_and_tree() {
        use lodestone_worldgen::feature::vegetation::{BlockStateProvider, ConfiguredFeature};

        let list = lodestone_worldgen::compose::build_biome_vegetation(
            &EmbeddedResolver,
            "minecraft:plains",
        );
        assert!(!list.is_empty(), "plains must have a non-empty vegetal-decoration list");

        let grass_resolved = list.iter().any(|(_, p)| {
            matches!(
                &*p.feature,
                ConfiguredFeature::SimpleBlock(BlockStateProvider::Simple(s))
                    if s == "minecraft:short_grass"
            )
        });
        assert!(
            grass_resolved,
            "patch_grass_plain must resolve to SimpleBlock(Simple(\"minecraft:short_grass\")), \
             not Unsupported — entries: {list:?}"
        );

        let tree = list
            .iter()
            .find(|(_, p)| matches!(*p.feature, ConfiguredFeature::RandomSelector { .. }))
            .expect("trees_plains must resolve to a RandomSelector");
        if let ConfiguredFeature::RandomSelector { default, .. } = &*tree.1.feature {
            assert!(
                matches!(*default.feature, ConfiguredFeature::Tree(_)),
                "trees_plains' default branch must resolve to a real Tree, not Unsupported"
            );
        }
    }

    /// Regression control for the tag closures
    /// [`crate::feature::vegetation::place_simple_block`]'s `canSurvive`
    /// check and [`crate::feature::vegetation::place_tree`]'s space-check
    /// depend on — if `#minecraft:supports_vegetation`'s nested
    /// `#substrate_overworld` -> `#grass_blocks` chain ever stopped
    /// resolving (a tag file renamed, a resolver regression), every grass/
    /// flower placement in the real embedded data would silently reject at
    /// the `canSurvive` check, exactly as the coordinate-translation failure
    /// described above did. This test exists
    /// so *that* failure mode has a direct, fast-failing check instead of
    /// only being visible through a 64-chunk sweep's aggregate count.
    #[test]
    fn embedded_veg_tags_resolve_grass_block_as_supporting_vegetation() {
        let tags = lodestone_worldgen::feature::vegetation::build_veg_tags(&EmbeddedResolver);
        assert!(
            tags.supports_vegetation.contains("minecraft:grass_block"),
            "supports_vegetation must include grass_block via \
             #supports_vegetation -> #substrate_overworld -> #grass_blocks"
        );
        assert!(!tags.replaceable_by_trees.is_empty());
        assert!(!tags.logs.is_empty());
        assert!(!tags.cannot_replace_below_tree_trunk.is_empty());
    }

    /// **Regression gate:** dark_forest must decorate with its
    /// OWN vegetation feature list, not lush_caves'.
    ///
    /// # What it catches, and why the other gates could not
    ///
    /// When the behavior was incorrect, the vegetation stage
    /// (`lodestone_worldgen`) resolved each source chunk's feature list
    /// through `biome_for_carver_source`, which samples climate at **y = 0** —
    /// the `crate::biome` module doc's "y = 0 trap": at y=0 the `depth`
    /// gradient is already ≈ +1.0, solidly underground climate, so every
    /// surface dark_forest chunk resolved as lush_caves and decorated with
    /// lush_caves' feature list (vines, vegetation_patch, root_system — all
    /// silent no-ops). dark_forest produced ~zero grass and ~zero trees even
    /// though the dark-oak tree placer is supported (66.7%-weight
    /// `dark_oak_leaf_litter` branch never dispatched because the wrong
    /// biome's list was chosen). The resolve-side gates
    /// ([`vegetation_placer_gaps_are_named_not_silent`],
    /// [`vegetation_reaches_real_blocks_over_a_production_sweep`]) stayed
    /// green because neither inspects dark_forest at runtime — this gate does.
    ///
    /// # Coordinates and anti-vacuity
    ///
    /// Fixed stride-9 5x5 lattices at seed 42 cover chunks (-40,-40) and a
    /// mirrored (0,0)-origin lattice, providing two probes. Stride 9 ensures
    /// no two centres share a 3x3
    /// neighbourhood. The gate first asserts the lattice actually CONTAINS
    /// dark_forest chunks — CLAUDE.md's "world" vacuous-test species: a lattice
    /// with zero dark_forest chunks would pass every "> 0" assertion by both
    /// sides being empty, and the biome band boundaries at this seed are
    /// exactly what an input-coordinate bug could silently move.
    ///
    /// # The floors
    ///
    /// Measured after the corrected biome selection: 5 dark_forest chunks over the two
    /// lattices, **714 dark_oak_log** and **3000** total vegetation blocks
    /// (2304 + 696, dominated by dark_oak_leaves). A configuration that selects
    /// the wrong biome feature list yields **0 tree logs and 3 veg blocks**. The
    /// `dark_oak_log > 0` assertion is the load-bearing one — a plain "some
    /// vegetation" count would be satisfied by grass alone, but the 66.7%-weight
    /// dark oak branch only runs when dark_forest's OWN step is selected. The
    /// `VEGETATION_FLOOR` sits ~170x above the broken state and ~6x below the
    /// healthy total, so the quiet failure (step runs, but dark_forest's
    /// grass/flower entries lost) fails here too. Both numbers are
    /// deterministic for this fixed lattice/seed.
    #[test]
    fn dark_forest_runs_its_own_vegetation_step_not_lush_caves() {
        let generator = overworld_generator(42);
        const VEGETATION_FLOOR: usize = 500;
        let lattices: &[(i32, i32)] = &[(-40, -40), (0, 0)];
        let mut dark_chunks = 0usize;
        let mut veg = 0usize;
        let mut dark_oak_logs = 0usize;
        for (ox, oz) in lattices {
            for i in 0..5 {
                for j in 0..5 {
                    let (cx, cz) = (ox + i * 9, oz + j * 9);
                    let col = generator.column(cx, cz);
                    if col.biome_state(8, 8) != "minecraft:dark_forest" {
                        continue;
                    }
                    dark_chunks += 1;
                    for lz in 0..16 {
                        for lx in 0..16 {
                            // Only the band around the surface can carry
                            // vegetation — scanning the full 384 rows for 50
                            // columns is the difference between a ~30s gate and
                            // a ~90s one (same band `vegetation_reaches_real_blocks...`
                            // uses).
                            let top = col.top_non_air_y(lx, lz);
                            let lo = (top - 8).max(col.min_y());
                            let hi = (top + 40).min(col.min_y() + col.height() - 1);
                            for y in lo..=hi {
                                let state = col.block_state(lx, y, lz);
                                let base = state.split('[').next().unwrap_or(state);
                                if is_vegetation_state(base) {
                                    veg += 1;
                                }
                                if base == "minecraft:dark_oak_log" {
                                    dark_oak_logs += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            dark_chunks > 0,
            "test premise failed: neither lattice at seed 42 contains a dark_forest chunk — \
             the input cannot exercise dark_forest, so every assertion below is vacuous"
        );
        assert!(
            dark_oak_logs > 0,
            "zero dark_oak_log over {dark_chunks} dark_forest chunks — the 66.7%-weight \
             dark oak branch never dispatched, which is the exact issue #480 symptom: \
             vegetation_stage ran the wrong biome's feature list (lush_caves at y=0)"
        );
        assert!(
            veg >= VEGETATION_FLOOR,
            "{veg} vegetation blocks over {dark_chunks} dark_forest chunks is below the \
             {VEGETATION_FLOOR} floor (measured healthy: 3000; broken state: 3)"
        );
    }
}

/// Bit-exact `freeze_top_layer` parity against the real 26.2 server.
///
/// # Why a *worldgen* parity gate lives in `lodestone-server`
///
/// This gate needs two things at once: `lodestone-worldgen`'s engine, and the
/// jar-sourced per-block-state facts that engine runs on. Only this crate has
/// both — the engine is version-free by construction and takes all its data
/// through [`Resolver`], so it cannot reach `lodestone_data` itself. Putting the
/// gate here also means it drives [`freeze_facts`] and [`EmbeddedResolver`]
/// **directly**, i.e. the exact production data rather than a re-derivation that
/// could quietly diverge from it. The fixtures live with the other worldgen
/// oracle dumps in `crates/lodestone-worldgen/tests/support/`, reached with one
/// extra `../` (the same cross-crate fixture arrangement `lodestone-data`'s
/// `tests/collision_shapes.rs` already uses).
///
/// # What the fixtures are
///
/// `scripts/worldgen-oracle/TopLayerOracle.java` boots the real 26.2 server,
/// runs vanilla's own `doFill` + `buildSurface` + `applyCarvers` +
/// `UNDERGROUND_ORES` + `VEGETAL_DECORATION` over a 3×3 neighbourhood, and then
/// runs the real `TOP_LAYER_MODIFICATION` step through
/// `PlacedFeature.placeWithBiomeCheck` — vanilla's `SnowAndFreezeFeature`, not a
/// reimplementation. It dumps the centre chunk's pre-step field (`base.`, RLE),
/// vanilla's own `getHeight(MOTION_BLOCKING)` per column (`top.`), and every cell
/// the step changed (`freeze.`).
///
/// So the gate is: **load vanilla's own post-vegetation field, run our engine on
/// it, and require the same writes at the same coordinates.** Nothing upstream of
/// this step is involved, which is why a residual here can only be this step's.
///
/// # Fixture choice, and why each one can fail
///
/// | fixture | biome | what it can catch |
/// |---|---|---|
/// | `snowy_plains` | temp 0.0 | 250 snow + 250 `snowy` flips: the ordinary path, and the flip |
/// | `frozen_ocean` | temp 0.0, modifier `frozen` | 36 ice, **0 snow**: the write order, and `TemperatureModifier.FROZEN`'s ice patches |
/// | `windswept_hills` | temp 0.2 | 115 snow: **the height-adjusted temperature** |
/// | `desert` | temp 2.0, no precipitation | **0 cells**: the negative fixture |
///
/// `windswept_hills` is the one that discriminates the trap. Its declared
/// temperature `0.2` is *above* the `0.15` rain threshold, so a port reading the
/// flat biome temperature places **zero** snow there — while vanilla places 115.
/// A snowy-biome fixture cannot see that error (temperature `0.0` snows under
/// either reading), which is exactly the *world* species of vacuous test.
///
/// It also turned out to discriminate more than intended. The snowed and bare
/// columns' `top.` heights **fully overlap** (119–125 for both), because the
/// `TEMPERATURE_NOISE` term contributes `±8` against a `(y − 120) / 800`
/// altitude term — so `freeze_top_layer` produces a *speckle* at every height,
/// not a snow line. A port that thresholded on altitude alone would also fail
/// here.
#[cfg(test)]
mod top_layer_parity {
    use std::collections::{BTreeMap, HashMap, HashSet};

    use lodestone_worldgen::dense_grid::DenseBlockGrid;
    use lodestone_worldgen::feature::top_layer::{
        self, BiomeClimate, FreezeCounts, SnowSupport,
    };
    use lodestone_worldgen::noise::ClimateNoise;

    use super::{EmbeddedResolver, Resolver, freeze_facts};

    struct Fixture {
        biome: String,
        chunk_x: i32,
        chunk_z: i32,
        min_y: i32,
        height: i32,
        sea_level: i32,
        step_index: i32,
        /// `base.` runs, expanded into one state per `(lx, y, lz)`.
        base: BTreeMap<(i32, i32, i32), String>,
        /// `freeze.` cells: `(lx, y, lz) -> state`.
        freeze: BTreeMap<(i32, i32, i32), String>,
        /// `top.` — vanilla's own `getHeight(MOTION_BLOCKING, x, z)`.
        top: BTreeMap<(i32, i32), i32>,
        snow: usize,
        ice: usize,
        snowy: usize,
        /// `meta.proxyCall` — the oracle's own record of which `WorldGenLevel`
        /// methods the feature actually reached. Read by
        /// [`the_oracle_actually_exercised_the_feature`] so a fixture cannot be a
        /// dump from a proxy that silently returned defaults.
        proxy_calls: Vec<String>,
    }

    fn load(name: &str) -> Fixture {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lodestone-worldgen/tests/support")
            .join(format!("top_layer_{name}_jvm.txt"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

        let mut biome = None;
        let mut chunk_x = None;
        let mut chunk_z = None;
        let mut min_y = None;
        let mut height = None;
        let mut sea_level = None;
        let mut step_index = None;
        let mut base = BTreeMap::new();
        let mut freeze = BTreeMap::new();
        let mut top = BTreeMap::new();
        let mut snow = None;
        let mut ice = None;
        let mut snowy = None;
        let mut proxy_calls = Vec::new();
        let mut done = false;

        for line in text.lines() {
            let Some((key, rest)) = line.split_once(' ') else {
                continue;
            };
            // The oracle's own JVM log noise precedes the data; any line whose
            // key is not one of ours is skipped. `meta.done` being required
            // below is what stops a truncated dump reading as a small one.
            match key {
                "meta.biome" => biome = Some(rest.to_owned()),
                "meta.chunkX" => chunk_x = Some(rest.parse().expect("chunkX")),
                "meta.chunkZ" => chunk_z = Some(rest.parse().expect("chunkZ")),
                "meta.minY" => min_y = Some(rest.parse().expect("minY")),
                "meta.height" => height = Some(rest.parse().expect("height")),
                "meta.seaLevel" => sea_level = Some(rest.parse().expect("seaLevel")),
                "meta.stepIndex" => step_index = Some(rest.parse().expect("stepIndex")),
                "meta.freezeSnow" => snow = Some(rest.parse().expect("freezeSnow")),
                "meta.freezeIce" => ice = Some(rest.parse().expect("freezeIce")),
                "meta.freezeSnowy" => snowy = Some(rest.parse().expect("freezeSnowy")),
                "meta.proxyCall" => proxy_calls.push(rest.to_owned()),
                "meta.done" => done = true,
                _ => {
                    if let Some(coords) = key.strip_prefix("base.") {
                        let (lx, lz) = split2(coords);
                        let mut parts = rest.split(' ');
                        let start: i32 = parts.next().expect("base y").parse().expect("y");
                        let run: i32 = parts.next().expect("base run").parse().expect("run");
                        let state = parts.next().expect("base state");
                        assert!(parts.next().is_none(), "base line has 3 fields: {line}");
                        for y in start..start + run {
                            let previous = base.insert((lx, y, lz), state.to_owned());
                            assert!(previous.is_none(), "base overlaps at ({lx},{y},{lz})");
                        }
                    } else if let Some(coords) = key.strip_prefix("freeze.") {
                        let (lx, y, lz) = split3(coords);
                        let previous = freeze.insert((lx, y, lz), rest.to_owned());
                        assert!(previous.is_none(), "duplicate freeze at ({lx},{y},{lz})");
                    } else if let Some(coords) = key.strip_prefix("top.") {
                        let (lx, lz) = split2(coords);
                        let previous = top.insert((lx, lz), rest.parse().expect("top y"));
                        assert!(previous.is_none(), "duplicate top at ({lx},{lz})");
                    }
                }
            }
        }

        assert!(done, "{name}: fixture has no meta.done — truncated dump");
        let fixture = Fixture {
            biome: biome.expect("meta.biome"),
            chunk_x: chunk_x.expect("meta.chunkX"),
            chunk_z: chunk_z.expect("meta.chunkZ"),
            min_y: min_y.expect("meta.minY"),
            height: height.expect("meta.height"),
            sea_level: sea_level.expect("meta.seaLevel"),
            step_index: step_index.expect("meta.stepIndex"),
            base,
            freeze,
            top,
            snow: snow.expect("meta.freezeSnow"),
            ice: ice.expect("meta.freezeIce"),
            snowy: snowy.expect("meta.freezeSnowy"),
            proxy_calls,
        };
        // Shape checks that make every later assertion non-vacuous: a fixture
        // missing columns would let a comparison "pass" over the part it has.
        assert_eq!(
            fixture.top.len(),
            256,
            "{name}: every one of the 16x16 columns needs a `top.` row"
        );
        assert_eq!(
            fixture.base.len(),
            256 * fixture.height as usize,
            "{name}: `base.` must expand to the whole 16x{}x16 box",
            fixture.height
        );
        assert_eq!(
            fixture.step_index,
            top_layer::STEP_TOP_LAYER_MODIFICATION,
            "{name}: fixture is not from the TOP_LAYER_MODIFICATION step"
        );
        assert_eq!(
            fixture.sea_level, 63,
            "{name}: overworld sea level is 63; snowLevel = seaLevel + 17 depends on it"
        );
        fixture
    }

    fn split2(s: &str) -> (i32, i32) {
        let (a, b) = s.split_once(',').expect("two comma-separated ints");
        (a.parse().expect("int"), b.parse().expect("int"))
    }

    fn split3(s: &str) -> (i32, i32, i32) {
        let mut it = s.split(',');
        let a = it.next().expect("3 ints").parse().expect("int");
        let b = it.next().expect("3 ints").parse().expect("int");
        let c = it.next().expect("3 ints").parse().expect("int");
        assert!(it.next().is_none(), "exactly 3 ints");
        (a, b, c)
    }

    /// The production `SnowSupport`: [`freeze_facts`]'s real document plus the two
    /// real tag closures out of the embedded datapack.
    fn support() -> SnowSupport {
        let s = top_layer::build_snow_support(&EmbeddedResolver);
        assert!(
            !s.is_empty(),
            "the production block_freeze_facts document is empty, so every assertion \
             below would be comparing against a no-op engine"
        );
        // The two tags are load-bearing in opposite directions (see
        // `lodestone_data::snow_support`), so both must be non-empty and must
        // actually contain the members the engine branches on.
        assert!(
            s.cannot_support_snow_layer.contains("minecraft:ice"),
            "ice must be in cannot_support_snow_layer or frozen oceans get snow on ice"
        );
        assert!(
            s.support_override_snow_layer.contains("minecraft:mud"),
            "mud must be in support_override_snow_layer"
        );
        let _ = freeze_facts();
        s
    }

    fn climates(biome: &str) -> HashMap<String, BiomeClimate> {
        let document = EmbeddedResolver.biome_document(biome);
        let climate = top_layer::parse_biome_climate(&document)
            .unwrap_or_else(|| panic!("no ClimateSettings in the embedded {biome} document"));
        let mut map = HashMap::new();
        map.insert(biome.to_owned(), climate);
        map
    }

    /// The fixture's `base.` field as an absolute-coordinate grid, exactly the
    /// shape [`top_layer::apply_freeze_top_layer`] expects.
    fn grid_from(fixture: &Fixture) -> DenseBlockGrid {
        let base_x = fixture.chunk_x * 16;
        let base_z = fixture.chunk_z * 16;
        let mut grid = DenseBlockGrid::new(
            base_x,
            fixture.min_y,
            base_z,
            16,
            fixture.height,
            16,
            "minecraft:air",
        );
        for ((lx, y, lz), state) in &fixture.base {
            grid.set(base_x + lx, *y, base_z + lz, state);
        }
        grid
    }

    /// Runs the engine and returns `(counts, actual diff)` keyed the same way the
    /// fixture's `freeze.` cells are.
    fn run(
        fixture: &Fixture,
        support: &SnowSupport,
        sea_level: i32,
    ) -> (FreezeCounts, BTreeMap<(i32, i32, i32), String>) {
        let base_x = fixture.chunk_x * 16;
        let base_z = fixture.chunk_z * 16;
        let mut grid = grid_from(fixture);
        let biome = fixture.biome.clone();
        let biome_at = |_lx: i32, _lz: i32| -> &str { &biome };
        let counts = top_layer::apply_freeze_top_layer(
            &mut grid,
            fixture.chunk_x,
            fixture.chunk_z,
            fixture.min_y,
            fixture.height,
            sea_level,
            &biome_at,
            &climates(&fixture.biome),
            support,
            &ClimateNoise::new(),
        );
        let mut diff = BTreeMap::new();
        for ((lx, y, lz), before) in &fixture.base {
            let after = grid.get(base_x + lx, *y, base_z + lz);
            if after != before {
                diff.insert((*lx, *y, *lz), after.to_owned());
            }
        }
        (counts, diff)
    }

    const FIXTURES: [&str; 4] = ["snowy_plains", "frozen_ocean", "windswept_hills", "desert"];

    // -----------------------------------------------------------------------
    // The main gate: exact block states at exact coordinates
    // -----------------------------------------------------------------------

    #[test]
    fn every_fixture_is_bit_exact() {
        let support = support();
        for name in FIXTURES {
            let fixture = load(name);
            let (counts, diff) = run(&fixture, &support, fixture.sea_level);

            // Both directions, and the *state* not just the position: a snow
            // layer with the wrong `layers` value, or a `snowy=false` left
            // unflipped, has to fail here.
            let missing: Vec<_> = fixture
                .freeze
                .iter()
                .filter(|(k, v)| diff.get(*k) != Some(*v))
                .map(|(k, v)| (*k, v.clone(), diff.get(k).cloned()))
                .collect();
            let extra: Vec<_> = diff
                .iter()
                .filter(|(k, _)| !fixture.freeze.contains_key(*k))
                .map(|(k, v)| (*k, v.clone()))
                .collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{name}: {} vanilla cells wrong/absent, {} cells we wrote that vanilla did not.\n\
                 first 8 wrong (coord, vanilla, ours): {:?}\n\
                 first 8 extra (coord, ours): {:?}",
                missing.len(),
                extra.len(),
                &missing[..missing.len().min(8)],
                &extra[..extra.len().min(8)],
            );
            assert_eq!(diff.len(), fixture.freeze.len(), "{name}: cell count");

            // And the counters, reached a second independent way (the engine's
            // own tally vs the JVM's).
            assert_eq!(counts.snow, fixture.snow, "{name}: snow count");
            assert_eq!(counts.ice, fixture.ice, "{name}: ice count");
            assert_eq!(counts.snowy_flips, fixture.snowy, "{name}: snowy-flip count");
        }
    }

    /// The `MOTION_BLOCKING` heightmap on its own, against vanilla's own
    /// `getHeight` per column — 1,024 comparisons across the four fixtures.
    ///
    /// This is separate from the write gate on purpose. The height the feature
    /// reads is the product of two cancelling `±1`s (`Heightmap` stores
    /// `topY + 1`, `getHighestTaken` subtracts one, `WorldGenRegion.getHeight`
    /// adds it back), and dropping either puts *every* snow layer one block out.
    /// A single combined gate would report that as "the writes are wrong"
    /// without saying why.
    #[test]
    fn motion_blocking_heightmap_matches_vanilla_per_column() {
        let support = support();
        let mut compared = 0usize;
        for name in FIXTURES {
            let fixture = load(name);
            let grid = grid_from(&fixture);
            let base_x = fixture.chunk_x * 16;
            let base_z = fixture.chunk_z * 16;
            for ((lx, lz), &expected) in &fixture.top {
                let got = top_layer::motion_blocking_first_free(
                    &grid,
                    &support,
                    base_x + lx,
                    base_z + lz,
                    fixture.min_y,
                    fixture.height,
                );
                assert_eq!(
                    got, expected,
                    "{name} column ({lx},{lz}): vanilla getHeight(MOTION_BLOCKING) is \
                     {expected}, ours is {got}"
                );
                compared += 1;
            }
        }
        assert_eq!(compared, 4 * 256, "every column of every fixture was compared");
    }

    // -----------------------------------------------------------------------
    // Non-vacuity: what each fixture actually contains
    // -----------------------------------------------------------------------

    /// Each fixture must contain the content it exists to test — the *world*
    /// species of vacuous test is the one that cannot be seen by reading the
    /// test, so the contents are asserted rather than assumed.
    #[test]
    fn fixtures_contain_the_content_they_exist_for() {
        let snowy = load("snowy_plains");
        assert_eq!(snowy.snow, 250, "snowy_plains snow cells");
        assert_eq!(snowy.snowy, 250, "every snowy_plains snow sits on a grass block");
        assert_eq!(snowy.ice, 0, "snowy_plains has no water surface to freeze");

        let ocean = load("frozen_ocean");
        assert_eq!(ocean.ice, 36, "frozen_ocean ice cells");
        assert_eq!(
            ocean.snow, 0,
            "frozen_ocean must have ZERO snow: the ice is written at belowPos first and \
             minecraft:ice is in cannot_support_snow_layer, so nothing survives on top of it"
        );
        // The ice must sit exactly one below the column top, which is the
        // `belowPos` the feature writes to.
        for ((lx, y, lz), state) in &ocean.freeze {
            assert_eq!(state, "minecraft:ice", "frozen_ocean writes only ice");
            assert_eq!(
                ocean.top[&(*lx, *lz)],
                y + 1,
                "ice at ({lx},{y},{lz}) is not at top-1"
            );
        }
        assert!(
            ocean.ice < 256,
            "frozen_ocean froze every column, so TemperatureModifier.FROZEN's warm ice \
             patches are not being exercised by this fixture"
        );

        let windswept = load("windswept_hills");
        assert_eq!(windswept.snow, 115, "windswept_hills snow cells");
        // The finding this fixture produced: the snowed and bare columns' heights
        // OVERLAP, so the split is a noise speckle rather than an altitude line.
        // A port that thresholded on Y alone would pass a line-shaped fixture and
        // fail this one.
        let snowed: HashSet<(i32, i32)> = windswept
            .freeze
            .keys()
            .filter(|(lx, y, lz)| {
                windswept.freeze[&(*lx, *y, *lz)].starts_with("minecraft:snow[")
            })
            .map(|(lx, _, lz)| (*lx, *lz))
            .collect();
        assert_eq!(snowed.len(), 115, "115 distinct columns snowed");
        let hs: Vec<i32> = snowed.iter().map(|c| windswept.top[c]).collect();
        let bare: Vec<i32> = windswept
            .top
            .iter()
            .filter(|(c, _)| !snowed.contains(*c))
            .map(|(_, y)| *y)
            .collect();
        let (smin, smax) = (*hs.iter().min().unwrap(), *hs.iter().max().unwrap());
        let (bmin, bmax) = (*bare.iter().min().unwrap(), *bare.iter().max().unwrap());
        assert!(
            smin <= bmax && bmin <= smax,
            "windswept_hills' snowed heights [{smin},{smax}] and bare heights [{bmin},{bmax}] \
             do not overlap — this fixture would then be satisfiable by an altitude threshold, \
             which is not what freeze_top_layer computes"
        );

        let desert = load("desert");
        assert!(
            desert.freeze.is_empty(),
            "desert must freeze nothing (has_precipitation false, temperature 2.0), got {} cells",
            desert.freeze.len()
        );
        // The detector control: the same emptiness check on a fixture that DOES
        // have content must fail, or "desert is empty" measures nothing.
        assert!(
            !snowy.freeze.is_empty(),
            "the emptiness detector reports the snowy_plains fixture as empty too, so \
             desert's zero is a broken reader rather than a climate gate"
        );
    }

    /// The oracle's own record of which `WorldGenLevel` methods the feature
    /// reached. `TopLayerOracle`'s proxy **throws** on an unmodelled method
    /// rather than returning a default, which is the fix for the precedent that
    /// cost this repo a whole vegetation gate: `VegetationOracle`'s proxy lacked
    /// `isStateAtPosition`, its default arm returned `false`, and no tree ever
    /// placed a block through it while the harness reported success.
    ///
    /// This asserts the surface that must have been exercised for the fixture to
    /// mean anything — in particular `getBrightness` and `isInsideBuildHeight`,
    /// whose plausible wrong defaults (`0` is right, `false` is not) would each
    /// have produced an entirely empty, entirely believable dump.
    #[test]
    fn the_oracle_actually_exercised_the_feature() {
        for name in ["snowy_plains", "frozen_ocean", "windswept_hills"] {
            let fixture = load(name);
            let calls: HashSet<&str> = fixture.proxy_calls.iter().map(String::as_str).collect();
            for required in [
                "getHeight",
                "getBiome",
                "getSeaLevel",
                "getBlockState",
                "getBrightness",
                "isInsideBuildHeight",
                "setBlock",
            ] {
                assert!(
                    calls.contains(required),
                    "{name}: the oracle never called {required}, so its dump cannot be \
                     evidence about freeze_top_layer. Calls seen: {:?}",
                    fixture.proxy_calls
                );
            }
        }
        // Desert is the interesting one: it reaches NEITHER `getBrightness` nor
        // `isInsideBuildHeight`, because `has_precipitation: false` and
        // temperature 2.0 short-circuit in `warmEnoughToRain`/`getPrecipitationAt`
        // first. That is an independent proof its zero comes from the climate
        // gate rather than from a proxy that skipped the work.
        let desert = load("desert");
        let calls: HashSet<&str> = desert.proxy_calls.iter().map(String::as_str).collect();
        assert!(
            calls.contains("getHeight") && calls.contains("getBiome"),
            "desert must still have read the heightmap and the biome"
        );
        assert!(
            !calls.contains("getBrightness"),
            "desert reached the block-light gate, so its zero is not purely climatic"
        );
    }

    // -----------------------------------------------------------------------
    // Controls, run and observed to fail — not described
    // -----------------------------------------------------------------------

    /// **Control: the step disabled.** With `freeze_top_layer` not run at all,
    /// the very assertion [`every_fixture_is_bit_exact`] makes must fail. Without
    /// this, that assertion could be satisfied by a fixture whose `freeze.`
    /// section this reader silently dropped.
    #[test]
    fn control_not_running_the_step_fails_the_parity_assertion() {
        for name in ["snowy_plains", "frozen_ocean", "windswept_hills"] {
            let fixture = load(name);
            // "Disabled" = the grid untouched, so the diff is empty.
            let empty: BTreeMap<(i32, i32, i32), String> = BTreeMap::new();
            assert_ne!(
                empty.len(),
                fixture.freeze.len(),
                "{name}: with the step disabled the diff still matched the fixture, so the \
                 parity assertion is vacuous for this fixture"
            );
        }
        // And desert genuinely cannot distinguish them — which is why desert is a
        // negative fixture and never the only one.
        let desert = load("desert");
        assert_eq!(
            desert.freeze.len(),
            0,
            "desert is the one fixture the disabled-step control cannot separate"
        );
    }

    /// **Control: the flat biome temperature, i.e. the trap.** Pushing
    /// `sea_level` far above the terrain makes `y > seaLevel + 17` false
    /// everywhere, so [`top_layer::height_adjusted_temperature`] returns the
    /// biome's declared value unmodified — exactly the port this unit was warned
    /// against, reproduced without touching the engine.
    ///
    /// Observed: `windswept_hills` (declared `0.2`, above the `0.15` threshold)
    /// drops from 115 snow cells to **0**, and the parity assertion fails.
    /// `snowy_plains` (declared `0.0`) is **unaffected** — which is the whole
    /// argument for the `windswept_hills` fixture existing.
    #[test]
    fn control_flat_temperature_loses_the_windswept_snow_and_not_the_snowy_plains_snow() {
        let support = support();
        // `sea_level` chosen so `sea_level + 17` is above the build limit, making
        // the height-adjustment branch unreachable for every column.
        let flat = 10_000;

        let windswept = load("windswept_hills");
        let (real, _) = run(&windswept, &support, windswept.sea_level);
        let (flattened, flat_diff) = run(&windswept, &support, flat);
        assert_eq!(real.snow, 115, "the real reading places 115 snow cells");
        assert_eq!(
            flattened.snow, 0,
            "a flat 0.2 temperature is above the 0.15 threshold, so it must place NO snow — \
             got {}",
            flattened.snow
        );
        assert_ne!(
            flat_diff.len(),
            windswept.freeze.len(),
            "the flat-temperature control still matched vanilla, so this gate cannot \
             detect the flat-temperature error at all"
        );

        let snowy = load("snowy_plains");
        let (snowy_real, _) = run(&snowy, &support, snowy.sea_level);
        let (snowy_flat, _) = run(&snowy, &support, flat);
        assert_eq!(
            snowy_real.snow, snowy_flat.snow,
            "snowy_plains' declared temperature is 0.0, below the threshold at every \
             altitude, so the two readings must agree — this is why a snowy-biome fixture \
             alone cannot catch the trap"
        );
    }

    /// **Control: `cannot_support_snow_layer` emptied.** With `ice` no longer in
    /// the tag, `frozen_ocean`'s ice columns become snow-covered ice, and the
    /// parity assertion fails. This is the branch-order control: the tag check has
    /// to run before the geometry check, and `minecraft:ice` genuinely *has* a
    /// full UP collision face (measured in `lodestone_data::snow_support`), so
    /// nothing else stops the snow.
    #[test]
    fn control_dropping_the_cannot_support_tag_puts_snow_on_frozen_ocean_ice() {
        let mut support = support();
        support.cannot_support_snow_layer = HashSet::new();
        let ocean = load("frozen_ocean");
        let (counts, diff) = run(&ocean, &support, ocean.sea_level);
        assert!(
            counts.snow > 0,
            "with the tag dropped, snow must appear on the ice — if it does not, the tag \
             check is not what keeps frozen oceans bare and this control proves nothing"
        );
        assert_ne!(
            diff.len(),
            ocean.freeze.len(),
            "the tag-dropped control still matched vanilla"
        );
    }
}

/// The `SPAWN` stage runs against the **real** bundled generator, not a
/// hand-built fixture: the biome document -> `BiomeSpawners`
/// -> `SPAWN` stage -> `GeneratedColumn` link, exercised end to end with
/// procedurally-placed terrain rather than a synthetic column.
///
/// Chunk (4, -3) at seed 12345 was found by an exhaustive scan of `cx`/`cz` in
/// `-4..=4` (the only two chunks in that whole 9x9 area that proposed
/// anything) and is not cherry-picked beyond "the search found it" — see
/// `dark_forest.json`'s own `spawners.creature` list for the numbers this test
/// predicts from, independently of running the code:
/// `[(sheep, w12, 4-4), (pig, w10, 4-4), (chicken, w10, 4-4), (cow, w8, 4-4)]`.
/// Every entry's pack is a fixed `4`, so **whichever species the weighted pick
/// lands on, the predicted pack size is exactly 4** — the one thing this
/// fixture lets a hand-derived prediction pin down without re-deriving the RNG
/// stream. `dark_forest` was chosen from the scan's own output, not selected
/// to make this true.
#[cfg(test)]
mod generation_spawn_reaches_a_real_chunk {
    #[test]
    fn dark_forest_chunk_proposes_a_full_pack_of_one_species() {
        let generator = super::overworld_generator(12345);
        let col = generator.column(4, -3);
        assert_eq!(
            col.biome_state(8, 8),
            "minecraft:dark_forest",
            "this fixture's own prediction depends on the biome being dark_forest; \
             re-derive the expected species/pack if the generator ever changes this"
        );
        let candidates = col.spawn_candidates();
        assert_eq!(
            candidates.len(),
            4,
            "dark_forest's creature list is entirely fixed 4-4 packs — every entry \
             predicts exactly 4 regardless of which one the weighted pick lands on"
        );
        let species = &candidates[0].entity_type;
        assert!(
            ["minecraft:sheep", "minecraft:pig", "minecraft:chicken", "minecraft:cow"]
                .contains(&species.as_str()),
            "{species} is not one of dark_forest's own four creature entries"
        );
        assert!(
            candidates.iter().all(|c| &c.entity_type == species),
            "one weighted pick names one species for the whole pack, not a mix"
        );
        for c in candidates {
            assert!(
                (4 * 16..4 * 16 + 16).contains(&c.x),
                "x={} outside chunk (4, -3)'s own 16x16",
                c.x
            );
            assert!(
                (-3 * 16..-3 * 16 + 16).contains(&c.z),
                "z={} outside chunk (4, -3)'s own 16x16",
                c.z
            );
        }
    }

    /// Negative control at the real-generator level: a chunk this test does
    /// **not** predict anything for (the 9x9 scan around it found nothing) must
    /// itself carry no candidates — proving the positive result above is
    /// biome-driven and not "every chunk gets something".
    #[test]
    fn a_chunk_the_scan_found_nothing_for_proposes_nothing() {
        let generator = super::overworld_generator(12345);
        let col = generator.column(0, 0);
        assert!(
            col.spawn_candidates().is_empty(),
            "chunk (0, 0) at seed 12345 was not one of the two chunks the 9x9 scan \
             found a candidate in; a non-empty result here means either the scan was \
             stale or every chunk now proposes something regardless of biome"
        );
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

        let expected_flat: [(i32, &str); 5] = [
            (-64, "minecraft:bedrock"),
            (-63, "minecraft:dirt"),
            (-62, "minecraft:dirt"),
            (-61, "minecraft:grass_block[snowy=false]"),
            (-60, "minecraft:air"),
        ];
        for &(y, want) in &expected_flat {
            let got = flat_col.block_state(0, y, 0);
            if got != want {
                mismatches.push(format!("flat y={y}: expected {want:?}, got {got:?}"));
            }
        }

        // The default arm's own measured values — the "wrong hypothesis" this
        // gate demonstrably rejects, not merely "differs from an unstated
        // baseline" (CLAUDE.md's *magnitude* species).
        let expected_default: [(i32, &str); 4] = [
            (-63, "minecraft:bedrock"),
            (-62, "minecraft:bedrock"),
            (-61, "minecraft:bedrock"),
            (-60, "minecraft:deepslate[axis=y]"),
        ];
        for &(y, want) in &expected_default {
            let got = overworld_col.block_state(0, y, 0);
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
                flat_col.block_state(0, y, 0),
                overworld_col.block_state(0, y, 0),
                "flat and default overworld agree at y={y}; FlatChunkSource may be \
                 silently routing through the default generator — the exact \
                 failure mode this gate exists to catch"
            );
        }

        // A flat world has no per-column variation: a second, distant chunk
        // must report the identical stack.
        let far = flat.column(500, -500);
        for &(y, want) in &expected_flat {
            assert_eq!(far.block_state(3, y, 11), want, "y={y} at a distant chunk");
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

        assert_eq!(flat.block_state(0, -61, 0), "minecraft:grass_block[snowy=false]");
        flat.set_block(0, -61, 0, "minecraft:diamond_block");
        assert_eq!(flat.block_state(0, -61, 0), "minecraft:diamond_block");

        // A different column, never edited, still reads the generated stack.
        assert_eq!(flat.block_state(16, -61, 0), "minecraft:grass_block[snowy=false]");
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
            super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_2, 7)
                .expect("scope matches the bundle")
                .expect("a Flat generator was stored on disk");

        assert_eq!(min_y, -64, "flat/debug share the bundled overworld's own min_y");
        let expected: [(i32, &str); 8] = [
            (-64, "minecraft:bedrock"),
            (-63, "minecraft:sandstone"),
            (-62, "minecraft:sandstone"),
            (-61, "minecraft:sandstone"),
            (-60, "minecraft:sandstone"),
            (-59, "minecraft:sand"),
            (-58, "minecraft:sand"),
            (-57, "minecraft:air"),
        ];
        let mut mismatches = Vec::new();
        for &(y, want) in &expected {
            let got = source.block_state(0, y, 0);
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
            source.block_state(0, -63, 0),
            "minecraft:dirt",
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
            super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_2, 7)
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
        match super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_2, 7) {
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
        match super::overworld_chunk_source_override(&dir, super::WorldgenScope::V26_2, 7) {
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

    /// Scans down from the top of the dimension for the first non-air block
    /// — a small local helper since [`crate::chunk::ChunkColumn`] (unlike the
    /// generator-level `GeneratedColumn`/`FlatColumn`) has no `top_non_air_y`
    /// of its own.
    fn top_non_air(col: &crate::chunk::ChunkColumn, x: i32, z: i32) -> (i32, String) {
        for y in (-64..320).rev() {
            let s = col.block_state(x, y, z);
            if s != "minecraft:air" {
                return (y, s.to_string());
            }
        }
        (-65, "minecraft:air".to_string())
    }

    /// `world_preset/single_biome_surface.json`'s default biome, pinned so a
    /// change to the bundled asset is caught here.
    #[test]
    fn world_preset_single_biome_default_biome_matches_the_bundled_document() {
        assert_eq!(super::world_preset_single_biome_default_biome(), "minecraft:plains");
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
    /// All six values below were measured by running the real generator (a
    /// throwaway probe), not
    /// predicted: at seed 4242, `single_biome_chunk_source(seed,
    /// "minecraft:desert")` reports biome `minecraft:desert` and surface
    /// `minecraft:sand` at y=63 at all three sampled chunks; the default
    /// `overworld_chunk_source(seed)` reports `minecraft:snowy_plains`/
    /// `minecraft:snow[layers=1]` at y=64 (chunk (0,0)) and y=67 (chunk
    /// (5,-3)), and `minecraft:plains`/`minecraft:grass_block[snowy=false]`
    /// at y=63 (chunk (20,20)) — three distinct answers from real per-column
    /// biome variety, none of them `minecraft:desert`.
    #[test]
    fn single_biome_desert_reports_desert_everywhere_and_differs_from_default_overworld() {
        let seed: i64 = 4242;
        let desert = super::single_biome_chunk_source(seed, "minecraft:desert");
        let overworld = super::overworld_chunk_source(seed);

        let cases: [(i32, i32, i32, &str); 3] = [
            (0, 0, 63, "minecraft:sand"),
            (5, -3, 63, "minecraft:sand"),
            (20, 20, 63, "minecraft:sand"),
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
            if (y, state.as_str()) != (want_y, want_state) {
                mismatches.push(format!(
                    "chunk ({cx},{cz}): expected top ({want_y}, {want_state:?}), got ({y}, {state:?})"
                ));
            }
        }
        assert!(mismatches.is_empty(), "single-biome desert mismatches:\n{mismatches:#?}");

        // The default arm's own measured values at the identical seed and
        // columns — the "wrong hypothesis" this gate demonstrably rejects.
        let default_cases: [(i32, i32, &str, i32, &str); 3] = [
            (0, 0, "minecraft:snowy_plains", 64, "minecraft:snow[layers=1]"),
            (5, -3, "minecraft:snowy_plains", 67, "minecraft:snow[layers=1]"),
            (20, 20, "minecraft:plains", 63, "minecraft:grass_block[snowy=false]"),
        ];
        let mut default_mismatches: Vec<String> = Vec::new();
        for &(cx, cz, want_biome, want_y, want_state) in &default_cases {
            let col = overworld.column(cx, cz);
            let got_biome = col.biome_state(0, 0);
            let (y, state) = top_non_air(&col, 0, 0);
            if got_biome != want_biome || (y, state.as_str()) != (want_y, want_state) {
                default_mismatches.push(format!(
                    "chunk ({cx},{cz}): expected ({want_biome}, {want_y}, {want_state:?}) \
                     (re-derive rather than editing the desert assertion if the plain \
                     overworld's own output moved at this seed and column), got \
                     ({got_biome:?}, {y}, {state:?})"
                ));
            }
        }
        assert!(default_mismatches.is_empty(), "default overworld mismatches:\n{default_mismatches:#?}");

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
        assert_eq!(states.len(), lodestone_data::block_states::STATE_COUNT as usize);
        assert_eq!(states[0], "minecraft:air");
        assert_eq!(states[1], "minecraft:stone");
    }

    /// `DebugLevelSource.GRID_WIDTH`/`GRID_HEIGHT`'s vanilla formula
    /// (`ceil(sqrt(n))` / `ceil(n / GRID_WIDTH)`) at the real 32,366-state
    /// count, re-derived rather than assumed equal on both sides.
    #[test]
    fn debug_generator_grid_dimensions_match_the_vanilla_formula_at_the_real_state_count() {
        let n = lodestone_data::block_states::STATE_COUNT as f64;
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
    /// Measured (a throwaway probe, since deleted): world `(1, 1)` (chunk
    /// `(0, 0)`, local `(1, 1)`) halves to grid cell `(0, 0)`, index `0` —
    /// `minecraft:air`, matching vanilla's own `ALL_BLOCKS[0]`. World
    /// `(17, 17)` (chunk `(1, 1)`, local `(1, 1)`) halves to `(8, 8)`, index
    /// `8 * 180 + 8 = 1448` — `minecraft:note_block[instrument=
    /// trumpet_exposed,note=8,powered=false]`, a real multi-property state,
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
                let got = origin.block_state(lx, 60, lz);
                if got != "minecraft:barrier" {
                    mismatches.push(format!("barrier ({lx},{lz}): got {got:?}"));
                }
            }
        }
        if origin.block_state(1, 70, 1) != "minecraft:air" {
            mismatches.push(format!(
                "world (1,1) grid row: expected air, got {:?}",
                origin.block_state(1, 70, 1)
            ));
        }

        let far = debug.column(1, 1);
        let want_far = "minecraft:note_block[instrument=trumpet_exposed,note=8,powered=false]";
        if far.block_state(1, 70, 1) != want_far {
            mismatches.push(format!(
                "world (17,17) grid row: expected {want_far:?}, got {:?}",
                far.block_state(1, 70, 1)
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
            default_col.block_state(1, 70, 1),
            far.block_state(1, 70, 1),
            "default overworld and debug world agree at (17,17) y=70 — \
             DebugChunkSource may be silently routing through the default \
             generator, the exact failure mode this gate exists to catch"
        );
        assert_ne!(
            default_col.block_state(1, 60, 1),
            far.block_state(1, 60, 1),
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
        assert_eq!(debug.block_state(1, 60, 1), "minecraft:barrier");
        debug.set_block(1, 60, 1, "minecraft:diamond_block");
        assert_eq!(debug.block_state(1, 60, 1), "minecraft:diamond_block");
        // A different column, never edited, still reads the generated grid.
        assert_eq!(debug.block_state(17, 60, 1), "minecraft:barrier");
    }
}
