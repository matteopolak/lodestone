//! The datapack lookups generation reads: biome documents, configured and
//! placed features, block and biome tags, structure sets, structures, jigsaw
//! template pools, processor lists and structure templates, plus the two
//! compiled block-state censuses that no datapack carries.
//!
//! Every method has a "no data supplied" default (`Value::Null`, an empty list
//! or `None`), so a fixture resolver implements only what its test reads.
//! [`crate::table_resolver::TableResolver`] is the production implementation.

use serde_json::Value;

/// Resolves datapack documents by resource id.
pub trait Resolver {
    /// Identifies an immutable asset bundle suitable for sharing parsed
    /// products across generators. Dynamic resolvers return `None`.
    fn asset_fingerprint(&self) -> Option<u64> {
        None
    }

    /// The full `worldgen/biome/<name>.json` document for one biome
    /// (`spawners`, `effects`, `attributes`, `carvers`, `features`).
    /// Default: `Value::Null`, the "no data supplied" convention every
    /// optional method here follows.
    fn biome_document(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/configured_feature/<name>.json`. Default: `Value::Null`.
    fn configured_feature(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/placed_feature/<name>.json`. Default: `Value::Null`.
    fn placed_feature(&self, _id: &str) -> Value {
        Value::Null
    }

    /// Compiled per-block-state predicates, as a JSON object of columns:
    ///
    /// ```json
    /// { "solid": { "default": ["minecraft:stone", ...], "states": {"...": false} } }
    /// ```
    ///
    /// Each column is "the answer for every block's default state" plus an
    /// override for **every** state that disagrees with its own default — a
    /// complete, exact encoding, not a curated subset.
    ///
    /// Default: `Value::Null`, which parses to empty predicates. This is *not*
    /// datapack data: it is a census of the game's own compiled behaviour, so
    /// a resolver that wants these facts supplies them from its canonical
    /// block-state census rather than from a JSON asset. See
    /// `lodestone_server::worldgen_data`'s implementation.
    fn block_freeze_facts(&self) -> Value {
        Value::Null
    }

    /// The exact state predicates simple-block survival needs. The document has
    /// the same complete default-plus-override shape as
    /// [`Self::block_freeze_facts`], with `solid_render`, `sturdy_up`,
    /// `center_support_down`, and `fire_flammable` columns. Default:
    /// [`Value::Null`], so fixture resolvers need not carry a version-specific
    /// compiled-state census.
    fn block_survival_facts(&self) -> Value {
        Value::Null
    }

    /// `tags/block/<name>.json` (the raw tag document, `{"values": [...]}`,
    /// with sub-tag references as `"#minecraft:..."` entries needing their
    /// own recursive lookup).
    /// Default: `Value::Null`, which resolves to an empty tag (no member
    /// blocks) rather than panicking.
    fn block_tag(&self, _id: &str) -> Value {
        Value::Null
    }

    /// Every `worldgen/structure_set/*.json` id this resolver can serve, e.g.
    /// `["minecraft:villages", "minecraft:shipwrecks", …]`.
    ///
    /// This is the *entry point* to the whole structure engine: vanilla's own
    /// chunk-generator structure-state "create for normal" iterates the structure-set
    /// registry, so a resolver that returns nothing here places no structures at
    /// all — the same "no data supplied" convention as
    /// [`biome_document`](Self::biome_document), and the reason
    /// `lodestone_worldgen::structure` is inert for every fixture resolver in
    /// this workspace without any of them changing.
    ///
    /// **Order is not significant and callers must not depend on it.**
    /// `lodestone_worldgen::structure::StructureRegistry` re-orders whatever it
    /// gets into vanilla's own bootstrap order (its own structure-sets
    /// bootstrap), which
    /// is the order its own structure-creation walk uses.
    fn structure_set_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// `worldgen/structure_set/<name>.json` — `{placement: {...}, structures: [...]}`.
    /// Default: `Value::Null` ("no such set").
    fn structure_set(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/structure/<name>.json` — the structure's `type`, `biomes`
    /// holder-set, `step`, `terrain_adaptation` and type-specific config.
    /// Default: `Value::Null`.
    fn structure(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `tags/worldgen/biome/<name>.json`, the raw tag document
    /// (`{"values": [...]}`, with `"#minecraft:..."` entries needing their own
    /// recursive lookup, exactly like [`block_tag`](Self::block_tag)).
    ///
    /// Needed because **every** bundled structure spells its `biomes` field as a
    /// single tag reference (`"#minecraft:has_structure/shipwreck"`) rather than
    /// an inline list, so without this the biome predicate of every structure is
    /// empty and no start is ever valid. Default: `Value::Null` (empty tag).
    fn biome_tag(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `structure/<path>.nbt` — one NBT **structure template**, as the raw file
    /// bytes. `minecraft:shipwreck/with_mast` means
    /// `assets/structure/shipwreck/with_mast.nbt`.
    ///
    /// Returned **exactly as shipped**, gzip wrapper included:
    /// `lodestone_worldgen::structure::template::StructureTemplate::parse` handles
    /// both gzipped and bare NBT, so a resolver never has to know which. Handing
    /// over the bytes rather than a parsed document is what keeps the NBT schema
    /// in one place instead of once per resolver.
    ///
    /// Default: `None` ("no such template"), the same no-data-supplied convention
    /// as [`biome_document`](Self::biome_document). A structure whose
    /// templates are missing is demoted to unsupported and named in
    /// `StructureRegistry::unsupported` — it never silently places nothing.
    fn structure_template(&self, _id: &str) -> Option<Vec<u8>> {
        None
    }

    /// `worldgen/template_pool/<name>.json` — one **jigsaw template pool**
    /// (`{fallback, elements: [{element, weight}]}`).
    ///
    /// The third entry point to the structure engine, after
    /// [`structure_set_ids`](Self::structure_set_ids) and
    /// [`structure_template`](Self::structure_template): a jigsaw structure names
    /// a `start_pool` and every jigsaw block inside a placed element names the
    /// next pool, so a resolver that supplies none of them makes every jigsaw
    /// structure (the five villages, `pillager_outpost`, `ancient_city`,
    /// `trail_ruins`, `trial_chambers`, the bastion) demote to `Unsupported` and
    /// appear in `StructureRegistry::unsupported` — placed, but with no blocks.
    ///
    /// Default: `Value::Null` ("no such pool"), the same no-data-supplied
    /// convention as [`biome_document`](Self::biome_document).
    fn template_pool(&self, _id: &str) -> Value {
        Value::Null
    }

    /// `worldgen/processor_list/<name>.json` — one named
    /// **structure-processor list** (`{"processors": [...]}`).
    ///
    /// A pool element spells its `processors` field either inline (an object) or
    /// as a reference to one of these 40 documents, so the reference form needs a
    /// lookup. Default: `Value::Null`, which resolves to an empty processor
    /// chain — an element then places its template unfiltered rather than not at
    /// all, so this one degrades quietly and is recorded on the ledger by name.
    fn processor_list(&self, _id: &str) -> Value {
        Value::Null
    }
}
