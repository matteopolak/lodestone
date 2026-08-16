//! Loot-table loading and rolling (issue #337, server-plumbing epic #339).
//!
//! # What it is
//!
//! The version-free server-side loot system: parses Mojang's datapack loot-table
//! JSON (the same format `net.minecraft.world.level.storage.loot` reads), then
//! "rolls" a table with a deterministic RNG to produce `Vec<ItemStack>` — the
//! data that becomes a chest fill, a mob drop, or a block drop. This is the
//! server half of the client's `lodestone-game` recipe loader: both consume
//! vanilla datapack JSON behind a version seam, neither names a protocol.
//!
//! # How it works
//!
//! A table is `pools` (each a weighted `entries` list, a `rolls` count, and
//! optional `conditions`/`functions`); an entry is a weighted leaf (`item`,
//! `empty`, `loot_table`) or a composite (`alternatives`, `group`,
//! `sequence`). [`LootTable::roll_with`] walks the structure exactly as
//! `LootPool.addRandomItems`/`LootPool.addRandomItem` (`LootPool.java:97-95`)
//! do:
//!
//! 1. A pool whose conditions all pass emits `rolls` rolls. A roll expands the
//!    pool's entry tree into weighted leaves (an `alternatives` stops at the
//!    first child that expands, a `group`/`sequence` expands every child), sums
//!    the *luck-adjusted* weights (`max(floor(weight + quality·luck), 0)`),
//!    draws `nextInt(totalWeight)`, and emits the leaf the draw lands on.
//! 2. A selected leaf applies its entry functions, the pool's functions, and
//!    the table's functions in that order (the same `LootItemFunction.decorate`
//!    nesting as vanilla).
//! 3. Nested `minecraft:loot_table` entries resolve through the
//!    [`LootTableResolver`] supplied to [`LootTable::roll_with`], with vanilla's
//!    visited-set recursion guard against table cycles
//!    (`LootContext.pushVisitedElement`).
//!
//! ## The empty loot context
//!
//! Issue #337's starting point is the **empty** context — no entity, no level,
//! no tool, no block state, no explosion — carrying only `luck`. Every
//! condition/function this module understands has a *defined* empty-context
//! value, so a table with zero unsupported features rolls **correctly** here:
//!
//! | feature | empty-context value | vanilla source |
//! |---|---|---|
//! | `random_chance` | `nextFloat() < chance` | `LootItemRandomChanceCondition` |
//! | `random_chance_with_enchanted_bonus` | uses `unenchanted_chance` (no attacker → level 0) | `…Condition.java:41-45` |
//! | `killed_by_player` | `false` (no `LAST_DAMAGE_PLAYER` param) | `…Condition.java:26-28` |
//! | `survives_explosion` | `true` (no `EXPLOSION_RADIUS` param) | `ExplosionCondition.test` |
//! | `match_tool` | `false` (no tool). **With a tool it is fully evaluated** against `ItemPredicate` | `MatchTool` |
//! | `block_state_property` | `false` (no state). **With a state it is fully evaluated** against `StatePropertiesPredicate` | `LootItemBlockStatePropertyCondition` |
//! | `entity_properties` / `damage_source_properties` / `location_check` | `false` (no entity/source/level) | `…EntityProperty…`, `…DamageSourceProperties…`, `LocationCheck` |
//! | `table_bonus` | `chances[0]` (fortune level 0) | `BonusLevelTableCondition.java:37-42` |
//! | `set_count` | uniform/constant/binomial rolled | `SetItemCountFunction` |
//! | `enchanted_count_increase` | no-op (no attacker → level 0) | `…Function.java:74-89` |
//! | `apply_bonus` | no-op (no tool) | `ApplyBonusCount.java:63-72` |
//! | `explosion_decay` | no-op (no `EXPLOSION_RADIUS`) | `ApplyExplosionDecay.run` |
//! | `furnace_smelt` | smelted via [`crate::furnace::recipe_for`] | `SmeltItemFunction` |
//!
//! ## The explosion context
//!
//! [`LootContext::explosion_radius`] is the one parameter whose *absence* is
//! dangerous rather than merely incomplete. With it unset, `survives_explosion`
//! returns `true` with no draw and `explosion_decay` is a no-op, so a blast
//! rolling an empty context drops **every** destroyed block at full rate — the
//! reason `crate::explosion_blocks` deliberately dropped nothing until this
//! existed. Set it and both become real: one draw per stack for the condition,
//! one draw **per item** for the decay function.
//!
//! ## The block-state context
//!
//! [`LootContext::block_state`] is the parameter whose absence was *silently*
//! wrong rather than merely incomplete, and it is worth stating why the mistake
//! survived so long. `block_state_property` **parsed** — it was a recognised
//! condition with its own enum variant — and then evaluated as a constant
//! `false`, so [`LootTable::unsupported_features`] reported nothing and the
//! curated bundle's own "zero unsupported features" guarantee held while 154 of
//! its 1,241 tables took the wrong branch of an `alternatives` on every roll.
//! Fully-grown wheat dropped one seed and no wheat; a slab dropped one instead of
//! two; a candle dropped one regardless of how many were stacked.
//!
//! The lesson generalises past this condition: **the bundle's guarantee is about
//! parsing, and a variant that discards its own JSON data is invisible to it.**
//! After this landing the remaining constant-`false` conditions are
//! `entity_properties`, `damage_source_properties`, `location_check` and
//! `killed_by_player`; the first three each discard a `predicate` object they
//! parsed, so they are exactly the same shape of hole.
//! [`LootTable::context_blind_features`] reports them, which is the instrument
//! `unsupported_features` structurally cannot be.
//!
//! A feature this module does not recognise is **parsed but marked
//! unsupported** (see [`LootTable::unsupported_features`]) rather than aborting
//! the load — the same tolerance `recipe_json` shows for future recipe types —
//! and contributes nothing to a roll: an unsupported condition fails, an
//! unsupported function/entry/provider is a no-op. Every table shipped under
//! `assets/loot_table/` is curated to have **zero** unsupported features, so
//! [`LootTableSet::load_bundled`] rolls exactly the vanilla loot those JSON
//! files define.
//!
//! # How to change it
//!
//! To support a new condition/function/number-provider: add the variant to the
//! enum, add its arm to the parse function, add its empty-context semantics to
//! `test`/`apply`/`int`/`float`, and add a test. To bundle another table, drop
//! the verbatim JSON under `assets/loot_table/` (ids are `minecraft:` +
//! path-minus-extension) — `build.rs` re-embeds it. Keep the "zero unsupported"
//! invariant for bundled tables: [`LootTableSet::load_bundled`] asserts it in
//! debug builds.
//!
//! ## Gotchas
//!
//! * **The RNG is `SpawnRng`, not a JVM-compatible one.** Rolling with a fixed
//!   seed is deterministic and the *distribution* of every draw matches Java
//!   (`nextFloat`, `nextDouble`, uniform `nextInt` ranges), but the exact
//!   stream differs from `RandomSource.create(seed)` (SplitMix64 here vs
//!   Xoroshiro, modulo vs rejection-sampled `nextInt(bound)`). The issue's
//!   JVM-roll oracle gate — byte-exact stream parity — is a follow-up built on
//!   top of this seam, not part of it.
//! * **`set_count` can produce a count-0 stack** and the roller keeps it, exactly
//!   as vanilla's `createStackSplitter` passes a `count < maxStackSize` stack
//!   through (a `uniform 0..N` count is common, e.g. zombie rotten-flesh).
//!   Container `fill` drops zero stacks; a `getRandomItems`-style Vec consumer
//!   sees them. [`roll_loot`] filters nothing.
//! * **Nested-table recursion is guarded by table id**, so a self-referential
//!   table produces nothing on the recursive branch rather than hanging
//!   (vanilla's `LootTable.getRandomItemsRaw` warns instead).
//! * `minecraft:tag` entries are unsupported here: expanding an item tag needs
//!   an item-tag census, which `lodestone-data` does not bundle (only the block
//!   tags `tool.rs` decodes at runtime). Only one table in the 26.2 corpus uses
//!   one.
//!
//! The data under `assets/loot_table/` is copied verbatim from
//! `.cache/mc/26.2/client-src/data/minecraft/loot_table/` (Mojang's own
//! generated data, CLAUDE.md data-source #1); the `tests/loot_corpus.rs` gate
//! re-reads the full corpus from that cache and cross-checks the bundle against
//! it.

use std::str::FromStr;

use lodestone_model::{ItemStack, ResourceKey};
use serde_json::Value;
use thiserror::Error;

use crate::mob_spawn::SpawnRng;

include!(concat!(env!("OUT_DIR"), "/embedded_loot.rs"));

/// The loot context a roll can read. Issue #337's starting point was the empty
/// context — no entity, no level, no tool, no block state, no explosion —
/// carrying only luck; issue #539 added [`tool`](Self::tool). Later landings
/// extend this struct (and the condition/function evaluators) with entity/level
/// state rather than threading new arguments through every call.
///
/// **Not `Copy`** since #539: [`LootTool`] owns its key and enchantment list.
/// Every consumer already took `&LootContext`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LootContext {
    /// `LootContextParams.luck` — 0 for the empty context. Feeds entry quality
    /// (`weight + quality·luck`) and `bonus_rolls`.
    pub luck: f32,
    /// `LootContextParams.TOOL` — the item the block was broken with (or the
    /// weapon, for a mob table). `None` is vanilla's absent parameter, which is
    /// what a bare hand and a mob death with no attacker both are.
    ///
    /// **A present tool changes the RNG stream even at enchantment level 0**,
    /// which is the single easiest thing to get wrong here: `ApplyBonusCount.run`
    /// guards on `tool != null`, *not* on `level > 0`, so with a tool in hand
    /// `uniform_bonus_count` draws `nextInt(1)` and `binomial_with_bonus_count`
    /// draws `extra` times, both to no effect. See [`BonusFormula`].
    pub tool: Option<LootTool>,
    /// `LootContextParams.EXPLOSION_RADIUS` — the blast radius, when this roll is
    /// a block destroyed by an explosion rather than mined. `None` is vanilla's
    /// absent parameter, which every mined block and every mob death is.
    ///
    /// **Its presence changes both the draw count and the outcome**, and in the
    /// unsafe direction if it is left out: `survives_explosion` returns `true`
    /// unconditionally with no draw when it is absent, so a blast rolling against
    /// an empty context drops **every** destroyed block at full rate instead of
    /// vanilla's `1/radius`. That is why `crate::explosion_blocks` dropped nothing
    /// at all until this parameter existed — nothing is a wrong answer, but
    /// everything is a much wronger one.
    ///
    /// A `f32`, not a bool: the probability is `1.0 / radius`, so a creeper's
    /// `3.0` keeps one block in three and a larger blast keeps proportionally
    /// fewer.
    pub explosion_radius: Option<f32>,
    /// `LootContextParams.BLOCK_STATE` — the state of the block being broken.
    /// `None` is vanilla's absent parameter, which is what a mob table and a
    /// chest fill are.
    ///
    /// **Its absence made every state-conditioned block table take the wrong
    /// branch**, silently: `block_state_property` was a hardcoded `false`, so
    /// fully-grown wheat dropped one seed and no wheat (the `alternatives` fell
    /// through to the seed child, and the bonus-seed pool's pool-level condition
    /// skipped the pool entirely). 154 of the 1,241 bundled tables carry the
    /// condition — crops, candles, slabs, doors, beds, tall flowers, snow layers,
    /// cave vines and sea pickles — so this is not one block's quirk.
    pub block_state: Option<LootBlockState>,
}

/// The block state a roll happens against — `LootContextParams.BLOCK_STATE`,
/// reduced to what `LootItemBlockStatePropertyCondition.test` reads off it: the
/// block's identity and its property values.
///
/// # Why the properties are a full, canonical list and not "whatever the state
/// string spelled"
///
/// Vanilla's `StatePropertiesPredicate.PropertyMatcher.match` looks the property
/// up in the block's `StateDefinition` and then reads the *state's* value for
/// it — so a property the caller did not mention still has a value, its default.
/// A matcher against a property the block does not have at all is `false`
/// (`property != null &&`), never a match.
///
/// So a consumer must hand over the **resolved** property set, not the substring
/// between the brackets: `"minecraft:wheat"` and `"minecraft:wheat[age=0]"` are
/// the same state and must both fail an `age=7` matcher for the same reason,
/// rather than one failing because the property is absent.
/// [`crate::block_drops::loot_block_state`] does that resolution through
/// `lodestone_data::block_states`, which is the 26.2 server's own state table.
#[derive(Debug, Clone, PartialEq)]
pub struct LootBlockState {
    /// The block's registry key, e.g. `minecraft:wheat`. Compared against the
    /// condition's own **required** `block` field, so a table applied to the
    /// wrong block cannot silently pass.
    pub block: ResourceKey,
    /// Every property of this state as `(name, serialized value)`, e.g.
    /// `[("age", "7")]`. A name absent from this list is a property the block
    /// does not have, which no matcher matches.
    pub properties: Vec<(String, String)>,
}

impl LootBlockState {
    /// A state with no properties — enough for a block whose table gates only on
    /// `block`.
    #[must_use]
    pub fn new(block: ResourceKey) -> Self {
        Self {
            block,
            properties: Vec::new(),
        }
    }

    /// A state carrying `properties`, as `(name, value)` pairs.
    #[must_use]
    pub fn with_properties<I, K, V>(block: ResourceKey, properties: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            block,
            properties: properties
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// This state's value for `name`, or `None` if the block has no such
    /// property — vanilla's `StateDefinition.getProperty(name)` returning null,
    /// which makes the matcher fail rather than pass.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The tool a roll happens with — `LootContextParams.TOOL`, reduced to the three
/// things vanilla's loot conditions and functions actually read off it.
///
/// # Why enchantments are keyed by name and not by id
///
/// `lodestone_model::ItemEnchantment` carries a **network registry id**, because
/// `minecraft:enchantment` is a datapack registry whose ids are assigned per
/// session at configuration time — it is not in Mojang's `registries.json` at
/// all, so no static name↔id table exists to generate. This crate is
/// version-free and cannot resolve one. So the seam is: whoever *has* the
/// session's registry resolves ids to keys and hands them here, and
/// [`LootTool::from_held_item`] (which has no registry) builds a tool with an
/// empty enchantment list — correct for every stack this server can currently
/// hold, and honest about it. See `docs/loot-tables.md`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LootTool {
    /// The item's registry key, e.g. `minecraft:diamond_pickaxe`. `None` is
    /// never a tool — that is [`LootContext::tool`] being `None`.
    pub item: Option<ResourceKey>,
    /// Stack size, for `ItemPredicate`'s `count` bounds.
    pub count: u32,
    /// Enchantment levels by **key**, e.g. `("minecraft:fortune", 3)`. An
    /// enchantment absent from this list is level 0, exactly as
    /// `EnchantmentHelper.getItemEnchantmentLevel` reports it.
    pub enchantments: Vec<(ResourceKey, u32)>,
}

impl LootTool {
    /// A tool holding `item`, one of it, unenchanted.
    #[must_use]
    pub fn new(item: ResourceKey) -> Self {
        Self {
            item: Some(item),
            count: 1,
            enchantments: Vec::new(),
        }
    }

    /// Builder: adds (or replaces) an enchantment level.
    #[must_use]
    pub fn with_enchantment(mut self, enchantment: ResourceKey, level: u32) -> Self {
        self.enchantments.retain(|(key, _)| key != &enchantment);
        self.enchantments.push((enchantment, level));
        self
    }

    /// The tool for a held [`ItemStack`], as the server sees it.
    ///
    /// Carries **no** enchantments, deliberately and visibly: see this type's
    /// own doc comment. A stack whose enchantments were resolvable should be
    /// built with [`with_enchantment`](Self::with_enchantment) instead.
    #[must_use]
    pub fn from_held_item(stack: &ItemStack) -> Self {
        Self {
            item: Some(stack.item.clone()),
            count: stack.count,
            enchantments: Vec::new(),
        }
    }

    /// `EnchantmentHelper.getItemEnchantmentLevel` — 0 for an enchantment this
    /// tool does not carry.
    #[must_use]
    pub fn enchantment_level(&self, enchantment: &ResourceKey) -> u32 {
        self.enchantments
            .iter()
            .find(|(key, _)| key == enchantment)
            .map_or(0, |(_, level)| *level)
    }
}

/// Resolves a nested `minecraft:loot_table` entry to its table during a roll.
///
/// [`LootTableSet`] implements this against its own loaded set. A table rolled
/// through [`LootTable::roll`] (no resolver) resolves every nested reference to
/// nothing.
pub trait LootTableResolver {
    /// The table registered under `id`, if any.
    fn loot_table(&self, id: &ResourceKey) -> Option<&LootTable>;
}

/// One parsed loot table.
///
/// Parsing never fails on an *unknown* feature — those are recorded in
/// [`unsupported_features`](Self::unsupported_features) and ignored when
/// rolling. A structurally malformed known shape is a [`LootError`].
#[derive(Debug, Clone)]
pub struct LootTable {
    /// The table's id, e.g. `minecraft:blocks/dirt`.
    pub id: ResourceKey,
    pools: Vec<LootPool>,
    functions: Vec<LootFunction>,
    unsupported: Vec<String>,
    context_blind: Vec<String>,
}

/// Marks an audit entry as **recognised but not evaluated** rather than
/// unrecognised. [`LootTable::from_value`] partitions the parse audit on this
/// prefix, so the two live in one `Vec<String>` through every parse helper and
/// separate once at the top.
const CONTEXT_BLIND_PREFIX: &str = "context-blind ";

impl LootTable {
    /// Parses one table document. `id` is the table's registry id (e.g.
    /// `minecraft:blocks/dirt`), used for self-references and resolution.
    ///
    /// # Errors
    ///
    /// Returns [`LootError::Json`] on malformed JSON and [`LootError`] on a
    /// structurally malformed known shape (a missing required field, a
    /// mistyped weight, a non-list `entries`...). Unknown feature *types* are
    /// never an error — see [`unsupported_features`](Self::unsupported_features).
    pub fn from_json(id: &ResourceKey, text: &str) -> Result<Self, LootError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| LootError::Json(e.to_string()))?;
        let mut audit = Vec::new();
        let table = Self::from_value(id.clone(), &value, &mut audit)?;
        Ok(table)
    }

    fn from_value(id: ResourceKey, value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        // `type` (the param set) and `random_sequence` do not affect a roll.
        let pools = match value.get("pools") {
            Some(p) => p
                .as_array()
                .ok_or_else(|| LootError::UnexpectedType("pools", "an array"))?
                .iter()
                .map(|p| LootPool::from_value(p, audit))
                .collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        let functions = parse_functions(value.get("functions"), audit)?;
        let (context_blind, unsupported): (Vec<String>, Vec<String>) = audit
            .iter()
            .cloned()
            .partition(|entry| entry.starts_with(CONTEXT_BLIND_PREFIX));
        let context_blind = context_blind
            .into_iter()
            .map(|entry| entry[CONTEXT_BLIND_PREFIX.len()..].to_string())
            .collect();
        Ok(Self { id, pools, functions, unsupported, context_blind })
    }

    /// Rolls the table in `context` with `rng`, resolving nested
    /// `minecraft:loot_table` entries through `resolver`.
    ///
    /// The order of application mirrors vanilla: entry functions, then pool
    /// functions, then table functions, per produced stack.
    #[must_use]
    pub fn roll_with(
        &self,
        context: &LootContext,
        rng: &mut SpawnRng,
        resolver: &dyn LootTableResolver,
    ) -> Vec<ItemStack> {
        let mut out = Vec::new();
        let mut visited = vec![self.id.clone()];
        self.roll_into(context, rng, resolver, &mut out, &mut visited);
        out
    }

    /// Rolls the table with no resolver — nested `minecraft:loot_table` entries
    /// produce nothing. Prefer [`LootTableSet::roll`] / [`roll_loot`] when a
    /// table may reference others.
    #[must_use]
    pub fn roll(&self, context: &LootContext, rng: &mut SpawnRng) -> Vec<ItemStack> {
        self.roll_with(context, rng, &NoTables)
    }

    fn roll_into(
        &self,
        context: &LootContext,
        rng: &mut SpawnRng,
        resolver: &dyn LootTableResolver,
        out: &mut Vec<ItemStack>,
        visited: &mut Vec<ResourceKey>,
    ) {
        for pool in &self.pools {
            pool.roll_into(context, rng, resolver, out, visited);
        }
        if !self.functions.is_empty() {
            for stack in out.iter_mut() {
                for function in &self.functions {
                    function.apply(stack, context, rng);
                }
            }
        }
    }

    /// Feature ids this table uses that the empty-context roller does not
    /// evaluate — e.g. `"function minecraft:enchant_randomly"`. Empty for every
    /// bundled table, so a non-empty list means "rolls here are best-effort,
    /// not vanilla-exact".
    #[must_use]
    pub fn unsupported_features(&self) -> &[String] {
        &self.unsupported
    }

    /// Feature ids this table uses that **parse into a recognised variant and
    /// then ignore their own JSON data** — e.g.
    /// `"condition minecraft:entity_properties"`, which parses an `entity` and a
    /// `predicate` and evaluates as a constant `false`.
    ///
    /// # Why this is a separate list from [`unsupported_features`](Self::unsupported_features)
    ///
    /// Because the bundle's curation invariant is stated in terms of *that* list,
    /// and it is silent about this one — which is how 154 state-conditioned tables
    /// shipped taking the wrong branch on every roll while every gate stayed
    /// green. A predicate that parses and returns a constant is "supported" by any
    /// test that only asks whether the parser recognised it.
    ///
    /// Reported rather than fatal, deliberately: making these `Unsupported` would
    /// eject 200-odd tables from the bundle and change nothing a player sees (a
    /// constant-`false` condition and an absent table both drop nothing), while
    /// destroying the byte-identical-to-Mojang property the corpus gate rests on.
    /// The value is in *counting* them, so the next condition that becomes
    /// evaluable is a number that moves rather than a discovery.
    ///
    /// **Ordinary and expected to be non-empty.** Every entry names a context
    /// parameter this crate does not yet carry, not a defect in the table.
    #[must_use]
    pub fn context_blind_features(&self) -> &[String] {
        &self.context_blind
    }
}

/// Accumulates a whole loot-table corpus from arbitrarily-sourced JSON
/// documents, the `CorpusBuilder` shape `recipe_json` established.
///
/// A malformed document is recorded in [`failures`](Self::failures) rather than
/// aborting the load; a document that only uses *unknown* features loads but is
/// reported by [`LootTable::unsupported_features`].
#[derive(Debug, Default)]
pub struct LootTableBuilder {
    tables: Vec<LootTable>,
    failures: Vec<(String, LootError)>,
}

impl LootTableBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses and stages one table document.
    pub fn push_table(&mut self, id: &str, json: &str) {
        match ResourceKey::from_str(id) {
            Ok(key) => match LootTable::from_json(&key, json) {
                Ok(table) => self.tables.push(table),
                Err(error) => self.failures.push((id.to_string(), error)),
            },
            Err(error) => self.failures.push((id.to_string(), LootError::BadIdentifier(error.to_string()))),
        }
    }

    /// Documents that failed to parse, as `(id, error)`.
    #[must_use]
    pub fn failures(&self) -> &[(String, LootError)] {
        &self.failures
    }

    /// Builds the [`LootTableSet`], consuming the builder.
    #[must_use]
    pub fn finish(self) -> LootTableSet {
        LootTableSet { tables: self.tables }
    }
}

/// Unsupported features a **bundled** table is allowed to use, because each one
/// only *decorates* an item the roll already produced correctly: an enchantment,
/// a custom name, an exploration map's target, a suspicious stew's effect. The
/// item id and the count are right; the decoration is absent.
///
/// This is deliberately a short allowlist rather than a blanket relaxation. An
/// unsupported **condition** *fails*, so a table using one drops items it should
/// have produced — a silently short chest, not a cosmetically plain one — and an
/// unsupported entry or number provider is the same class of loss. Those must
/// still keep a table out of the bundle.
///
/// The four structure-chest tables (`chests/shipwreck_{map,supply}`,
/// `chests/underwater_ruin_{small,big}`) are bundled under exactly this rule.
pub const DECORATION_ONLY_UNSUPPORTED: &[&str] = &[
    "function minecraft:enchant_randomly",
    "function minecraft:exploration_map",
    "function minecraft:set_name",
    "function minecraft:set_stew_effect",
];

/// A loaded set of loot tables, keyed by id. This is the object a server holds
/// — the analogue of `RecipeBook` — and the provider for [`roll_loot`].
#[derive(Debug, Default)]
pub struct LootTableSet {
    tables: Vec<LootTable>,
}

impl LootTableSet {
    /// Loads every table bundled under `assets/loot_table/` (issue #337's
    /// "bundled assets" seam; `build.rs` embeds them as `include_str!`s).
    ///
    /// # Panics
    ///
    /// Panics on a malformed embedded document, and — in debug builds — if any
    /// bundled table uses a feature the empty-context roller does not support:
    /// the bundle is curated to be fully supported, so a non-empty report here
    /// is a packaging error, not a runtime condition.
    #[must_use]
    pub fn load_bundled() -> Self {
        let mut builder = LootTableBuilder::new();
        for (id, raw) in EMBEDDED_LOOT {
            builder.push_table(&format!("minecraft:{id}"), raw);
        }
        let set = builder.finish();
        for table in &set.tables {
            for feature in &table.unsupported {
                debug_assert!(
                    DECORATION_ONLY_UNSUPPORTED.contains(&feature.as_str()),
                    "bundled table {} uses unsupported feature {feature}, which is not \
                     decoration-only: {table:?}",
                    table.id,
                );
            }
        }
        set
    }

    /// The table registered under `id`, if any.
    #[must_use]
    pub fn get(&self, id: &ResourceKey) -> Option<&LootTable> {
        self.tables.iter().find(|t| &t.id == id)
    }

    /// Number of loaded tables.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tables.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Every loaded table, in insertion order.
    #[must_use]
    pub fn iter(&self) -> impl Iterator<Item = &LootTable> {
        self.tables.iter()
    }

    /// Rolls the table `id` in `context` with `rng`, resolving nested tables
    /// within this set. An unknown id rolls to nothing (the same answer
    /// vanilla's `LootTable.EMPTY` gives for a missing table).
    #[must_use]
    pub fn roll(&self, id: &ResourceKey, context: &LootContext, rng: &mut SpawnRng) -> Vec<ItemStack> {
        match self.get(id) {
            Some(table) => table.roll_with(context, rng, self),
            None => Vec::new(),
        }
    }
}

impl LootTableResolver for LootTableSet {
    fn loot_table(&self, id: &ResourceKey) -> Option<&LootTable> {
        self.get(id)
    }
}

/// Issue #337's starting point: roll the table `table_id` from `set` in the
/// empty loot context (luck 0, no entity/level/tool) and return the item stacks
/// it produces. Nested `minecraft:loot_table` entries resolve within `set`.
///
/// This is the `roll_loot(table_id) -> Vec<ItemStack>` of the issue — the call
/// a future mob-death handler (`MobSim`'s `killed` branch, `mobs.rs`) or chest
/// filler makes after it has mapped its entity/block to a table id.
#[must_use]
pub fn roll_loot(set: &LootTableSet, table_id: &ResourceKey, rng: &mut SpawnRng) -> Vec<ItemStack> {
    set.roll(table_id, &LootContext::default(), rng)
}

/// One pool: a weighted entry list rolled `rolls` times.
#[derive(Debug, Clone)]
struct LootPool {
    entries: Vec<LootEntry>,
    conditions: Vec<LootCondition>,
    functions: Vec<LootFunction>,
    rolls: NumberProvider,
    bonus_rolls: NumberProvider,
}

impl LootPool {
    fn from_value(value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .ok_or(LootError::MissingField("entries"))?
            .iter()
            .map(|e| LootEntry::from_value(e, audit))
            .collect::<Result<Vec<_>, _>>()?;
        let conditions = parse_conditions(value.get("conditions"), audit)?;
        let functions = parse_functions(value.get("functions"), audit)?;
        let rolls = value
            .get("rolls")
            .ok_or(LootError::MissingField("rolls"))
            .and_then(|r| parse_number_provider(r, audit))?;
        let bonus_rolls = match value.get("bonus_rolls") {
            Some(b) => parse_number_provider(b, audit)?,
            None => NumberProvider::Constant(0.0),
        };
        Ok(Self { entries, conditions, functions, rolls, bonus_rolls })
    }

    fn roll_into(
        &self,
        context: &LootContext,
        rng: &mut SpawnRng,
        resolver: &dyn LootTableResolver,
        out: &mut Vec<ItemStack>,
        visited: &mut Vec<ResourceKey>,
    ) {
        if !self.conditions.iter().all(|c| c.test(context, rng)) {
            return;
        }
        // `LootPool.addRandomItems`: rolls + floor(bonus_rolls * luck).
        let rolls = self.rolls.int(context, rng)
            + (self.bonus_rolls.float(context, rng) * context.luck).floor() as i32;
        for _ in 0..rolls.max(0) {
            let mut produced = Vec::new();
            self.roll_one(context, rng, resolver, &mut produced, visited);
            // The pool's functions decorate each item the roll produced.
            for function in &self.functions {
                for stack in &mut produced {
                    function.apply(stack, context, rng);
                }
            }
            out.extend(produced);
        }
    }

    /// One `LootPool.addRandomItem`: expand the entry tree into weighted leaves,
    /// draw a leaf, emit it.
    fn roll_one(
        &self,
        context: &LootContext,
        rng: &mut SpawnRng,
        resolver: &dyn LootTableResolver,
        out: &mut Vec<ItemStack>,
        visited: &mut Vec<ResourceKey>,
    ) {
        let mut leaves: Vec<Leaf> = Vec::new();
        let mut total_weight: i32 = 0;
        for entry in &self.entries {
            entry.expand(context, rng, resolver, visited, &mut leaves, &mut total_weight);
        }
        if total_weight == 0 || leaves.is_empty() {
            return;
        }
        if leaves.len() == 1 {
            leaves[0].create(context, rng, resolver, visited, out);
            return;
        }
        // The weight stored on each leaf is already luck-adjusted, so this
        // walk subtracts the same values `entry.getWeight(luck)` would.
        let mut index = rng.next_int(total_weight);
        for leaf in &leaves {
            index -= leaf.weight;
            if index < 0 {
                leaf.create(context, rng, resolver, visited, out);
                return;
            }
        }
    }
}

/// A pool entry: a weighted leaf or a composite of entries.
#[derive(Debug, Clone)]
enum LootEntry {
    Item {
        name: ResourceKey,
        weight: i32,
        quality: i32,
        conditions: Vec<LootCondition>,
        functions: Vec<LootFunction>,
    },
    Empty {
        weight: i32,
        quality: i32,
        conditions: Vec<LootCondition>,
    },
    /// Nested `minecraft:loot_table` reference. Only the id form is supported;
    /// an *inline* table value is parsed as unsupported (rare, needs a
    /// sub-parser for the embedded document).
    Table {
        name: ResourceKey,
        weight: i32,
        quality: i32,
        conditions: Vec<LootCondition>,
        functions: Vec<LootFunction>,
    },
    Alternatives {
        conditions: Vec<LootCondition>,
        children: Vec<LootEntry>,
    },
    Group {
        conditions: Vec<LootCondition>,
        children: Vec<LootEntry>,
    },
    Sequence {
        conditions: Vec<LootCondition>,
        children: Vec<LootEntry>,
    },
    /// A feature this build does not support (`minecraft:tag`,
    /// `minecraft:dynamic`, ...). Never expands, so it can never win a roll;
    /// the feature id is reported by [`LootTable::unsupported_features`].
    Unsupported,
}

impl LootEntry {
    fn from_value(value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        let id = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(LootError::MissingField("type"))?;
        match id {
            "minecraft:item" => {
                let name = parse_id(value.get("name"), "name")?;
                let weight = parse_int_default(value.get("weight"), 1)?;
                let quality = parse_int_default(value.get("quality"), 0)?;
                let conditions = parse_conditions(value.get("conditions"), audit)?;
                let functions = parse_functions(value.get("functions"), audit)?;
                Ok(Self::Item { name, weight, quality, conditions, functions })
            }
            "minecraft:empty" => {
                let weight = parse_int_default(value.get("weight"), 1)?;
                let quality = parse_int_default(value.get("quality"), 0)?;
                let conditions = parse_conditions(value.get("conditions"), audit)?;
                Ok(Self::Empty { weight, quality, conditions })
            }
            "minecraft:loot_table" => {
                let weight = parse_int_default(value.get("weight"), 1)?;
                let quality = parse_int_default(value.get("quality"), 0)?;
                let conditions = parse_conditions(value.get("conditions"), audit)?;
                let functions = parse_functions(value.get("functions"), audit)?;
                let entry_value = value.get("value").ok_or(LootError::MissingField("value"))?;
                if let Some(name) = entry_value.as_str() {
                    let name = name
                        .parse()
                        .map_err(|_| LootError::BadIdentifier(name.to_string()))?;
                    Ok(Self::Table { name, weight, quality, conditions, functions })
                } else {
                    audit.push("entry minecraft:loot_table (inline table value)".to_string());
                    Ok(Self::Unsupported)
                }
            }
            "minecraft:alternatives" => {
                let conditions = parse_conditions(value.get("conditions"), audit)?;
                let children = parse_entries(value.get("children"), audit)?;
                Ok(Self::Alternatives { conditions, children })
            }
            "minecraft:group" => {
                let conditions = parse_conditions(value.get("conditions"), audit)?;
                let children = parse_entries(value.get("children"), audit)?;
                Ok(Self::Group { conditions, children })
            }
            "minecraft:sequence" => {
                let conditions = parse_conditions(value.get("conditions"), audit)?;
                let children = parse_entries(value.get("children"), audit)?;
                Ok(Self::Sequence { conditions, children })
            }
            other => {
                audit.push(format!("entry {other}"));
                Ok(Self::Unsupported)
            }
        }
    }

    /// Expands this entry into weighted [`Leaf`]s for one roll, mirroring
    /// `LootPoolEntryContainer.expand` / the `ComposableEntryContainer`
    /// compositions. Returns whether anything (even a zero-weight leaf) was
    /// produced — what `alternatives` short-circuits on.
    fn expand(
        &self,
        context: &LootContext,
        rng: &mut SpawnRng,
        resolver: &dyn LootTableResolver,
        visited: &mut Vec<ResourceKey>,
        leaves: &mut Vec<Leaf>,
        total_weight: &mut i32,
    ) -> bool {
        let mut pass = |conditions: &[LootCondition]| conditions.iter().all(|c| c.test(context, rng));
        match self {
            Self::Item { name, weight, quality, conditions, functions } => {
                if !pass(conditions) {
                    return false;
                }
                push_leaf(LeafKind::Item(name.clone()), *weight, *quality, functions, context, leaves, total_weight);
                true
            }
            Self::Empty { weight, quality, conditions } => {
                if !pass(conditions) {
                    return false;
                }
                push_leaf(LeafKind::Empty, *weight, *quality, &[], context, leaves, total_weight);
                true
            }
            Self::Table { name, weight, quality, conditions, functions } => {
                if !pass(conditions) {
                    return false;
                }
                push_leaf(LeafKind::Table(name.clone()), *weight, *quality, functions, context, leaves, total_weight);
                true
            }
            Self::Alternatives { conditions, children } => {
                if !pass(conditions) {
                    return false;
                }
                for child in children {
                    if child.expand(context, rng, resolver, visited, leaves, total_weight) {
                        return true;
                    }
                }
                false
            }
            Self::Group { conditions, children } => {
                if !pass(conditions) {
                    return false;
                }
                for child in children {
                    child.expand(context, rng, resolver, visited, leaves, total_weight);
                }
                true
            }
            Self::Sequence { conditions, children } => {
                if !pass(conditions) {
                    return false;
                }
                for child in children {
                    if !child.expand(context, rng, resolver, visited, leaves, total_weight) {
                        return false;
                    }
                }
                true
            }
            Self::Unsupported => false,
        }
    }
}

/// A single weighted leaf a pool roll can select.
#[derive(Debug, Clone)]
struct Leaf {
    kind: LeafKind,
    /// Luck-adjusted weight, already `max(floor(weight + quality·luck), 0)` and
    /// already filtered to `> 0`.
    weight: i32,
    functions: Vec<LootFunction>,
}

/// What a selected leaf emits.
#[derive(Debug, Clone)]
enum LeafKind {
    /// A stack of `item` (count 1 before functions).
    Item(ResourceKey),
    /// Nothing — a selected `minecraft:empty` entry still consumes the roll.
    Empty,
    /// Roll the referenced table, recursively.
    Table(ResourceKey),
}

impl Leaf {
    /// `LootPoolEntry.createItemStack`: emit the leaf's item(s), applying the
    /// entry's functions first.
    fn create(
        &self,
        context: &LootContext,
        rng: &mut SpawnRng,
        resolver: &dyn LootTableResolver,
        visited: &mut Vec<ResourceKey>,
        out: &mut Vec<ItemStack>,
    ) {
        match &self.kind {
            LeafKind::Item(name) => {
                let mut stack = ItemStack::new(name.clone(), 1);
                for function in &self.functions {
                    function.apply(&mut stack, context, rng);
                }
                out.push(stack);
            }
            LeafKind::Empty => {}
            LeafKind::Table(id) => {
                if visited.contains(id) {
                    return;
                }
                visited.push(id.clone());
                // `LootTableReference.createItemStack`: every stack the
                // referenced table produced is decorated by this entry's own
                // functions before being emitted.
                let start = out.len();
                if let Some(table) = resolver.loot_table(id) {
                    table.roll_into(context, rng, resolver, out, visited);
                }
                visited.pop();
                for stack in &mut out[start..] {
                    for function in &self.functions {
                        function.apply(stack, context, rng);
                    }
                }
            }
        }
    }
}

fn push_leaf(
    kind: LeafKind,
    weight: i32,
    quality: i32,
    functions: &[LootFunction],
    context: &LootContext,
    leaves: &mut Vec<Leaf>,
    total_weight: &mut i32,
) {
    // `LootPoolSingletonContainer.EntryBase.getWeight`: max(floor(w + q·luck), 0).
    let effective = (weight as f32 + quality as f32 * context.luck).floor().max(0.0) as i32;
    if effective > 0 {
        *total_weight += effective;
        leaves.push(Leaf { kind, weight: effective, functions: functions.to_vec() });
    }
}

/// A number provider (`NumberProviders.CODEC`): a bare float constant, a typed
/// object, or the bare-`{min,max}` uniform fallback.
#[derive(Debug, Clone)]
enum NumberProvider {
    Constant(f32),
    Uniform {
        min: Box<NumberProvider>,
        max: Box<NumberProvider>,
    },
    Binomial {
        n: Box<NumberProvider>,
        p: Box<NumberProvider>,
    },
    Sum(Vec<NumberProvider>),
    /// A number-provider type this build does not evaluate (`minecraft:linear`,
    /// `minecraft:score`, ...). Rolls as 0; the feature id is reported by
    /// [`LootTable::unsupported_features`].
    Unsupported,
}

impl NumberProvider {
    fn from_value(value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        if let Some(f) = value.as_f64() {
            return Ok(Self::Constant(f as f32));
        }
        let object = value
            .as_object()
            .ok_or_else(|| LootError::UnexpectedType("number provider", "a number or object"))?;
        // Vanilla's `Codec.withAlternative(TYPED_CODEC, UniformGenerator.MAP_CODEC)`:
        // a bare `{min, max}` (no `type`) is a uniform.
        if object.get("type").is_none() && object.contains_key("min") && object.contains_key("max") {
            return Ok(Self::Uniform {
                min: Box::new(parse_number_provider(value.get("min").unwrap(), audit)?),
                max: Box::new(parse_number_provider(value.get("max").unwrap(), audit)?),
            });
        }
        let id = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(LootError::MissingField("type"))?;
        match id {
            "minecraft:constant" => {
                let value = value.get("value").ok_or(LootError::MissingField("value"))?;
                Ok(Self::Constant(parse_f32(value, "value")?))
            }
            "minecraft:uniform" => {
                let min = parse_number_provider(value.get("min").ok_or(LootError::MissingField("min"))?, audit)?;
                let max = parse_number_provider(value.get("max").ok_or(LootError::MissingField("max"))?, audit)?;
                Ok(Self::Uniform { min: Box::new(min), max: Box::new(max) })
            }
            "minecraft:binomial" => {
                let n = parse_number_provider(value.get("n").ok_or(LootError::MissingField("n"))?, audit)?;
                let p = parse_number_provider(value.get("p").ok_or(LootError::MissingField("p"))?, audit)?;
                Ok(Self::Binomial { n: Box::new(n), p: Box::new(p) })
            }
            "minecraft:sum" => {
                let summands = parse_number_providers(value.get("summands"), audit)?;
                Ok(Self::Sum(summands))
            }
            other => {
                audit.push(format!("number provider {other}"));
                Ok(Self::Unsupported)
            }
        }
    }

    /// `NumberProvider.getInt` — `Mth.floor(getFloat)` unless overridden.
    fn int(&self, context: &LootContext, rng: &mut SpawnRng) -> i32 {
        match self {
            Self::Constant(v) => v.floor() as i32,
            // `Mth.nextInt`: min >= max ? min : min + nextInt(max - min + 1).
            Self::Uniform { min, max } => {
                let lo = min.int(context, rng);
                let hi = max.int(context, rng);
                if lo >= hi { lo } else { rng.next_int(hi - lo + 1) + lo }
            }
            Self::Binomial { n, p } => {
                let draws = n.int(context, rng);
                let probability = p.float(context, rng);
                let mut hits = 0;
                for _ in 0..draws.max(0) {
                    if rng.next_f32() < probability {
                        hits += 1;
                    }
                }
                hits
            }
            Self::Sum(summands) => summands.iter().map(|s| s.float(context, rng)).sum::<f32>().floor() as i32,
            Self::Unsupported => 0,
        }
    }

    fn float(&self, context: &LootContext, rng: &mut SpawnRng) -> f32 {
        match self {
            Self::Constant(v) => *v,
            // `Mth.nextFloat`: min >= max ? min : nextFloat * (max - min) + min.
            Self::Uniform { min, max } => {
                let lo = min.float(context, rng);
                let hi = max.float(context, rng);
                if lo >= hi { lo } else { rng.next_f32() * (hi - lo) + lo }
            }
            Self::Binomial { .. } => self.int(context, rng) as f32,
            Self::Sum(summands) => summands.iter().map(|s| s.float(context, rng)).sum(),
            Self::Unsupported => 0.0,
        }
    }
}

/// A loot-table function (`LootItemFunctions` dispatch on `function`).
///
/// Each variant here has a defined empty-context effect (see the module doc's
/// table); an unsupported function is a no-op and is reported by
/// [`LootTable::unsupported_features`].
#[derive(Debug, Clone)]
enum LootFunction {
    SetCount {
        count: NumberProvider,
        add: bool,
        conditions: Vec<LootCondition>,
    },
    SetItem {
        item: ResourceKey,
        conditions: Vec<LootCondition>,
    },
    /// No attacker in the empty context, so enchantment level is 0 and the
    /// function is a no-op — parsed so the table is recognised as supported.
    EnchantedCountIncrease {
        conditions: Vec<LootCondition>,
    },
    /// `ApplyBonusCount` — a count formula driven by an enchantment level on the
    /// context's tool. **A no-op with no tool, but not with an unenchanted one:**
    /// vanilla guards on `tool != null`, not on `level > 0`.
    ApplyBonus {
        enchantment: ResourceKey,
        formula: BonusFormula,
        conditions: Vec<LootCondition>,
    },
    /// No explosion in the empty context — a no-op.
    ExplosionDecay {
        conditions: Vec<LootCondition>,
    },
    FurnaceSmelt {
        use_input_count: bool,
        conditions: Vec<LootCondition>,
    },
    /// `minecraft:sequence` — applies each child in order.
    Sequence(Vec<LootFunction>),
    /// A function this build does not apply (`minecraft:enchant_randomly`,
    /// `minecraft:set_potion`, ...). A no-op in a roll; the feature id is
    /// reported by [`LootTable::unsupported_features`].
    Unsupported,
}

impl LootFunction {
    fn from_value(value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        let id = value
            .get("function")
            .and_then(Value::as_str)
            .ok_or(LootError::MissingField("function"))?;
        let conditions = parse_conditions(value.get("conditions"), audit)?;
        match id {
            "minecraft:set_count" => {
                let count = parse_number_provider(value.get("count").ok_or(LootError::MissingField("count"))?, audit)?;
                let add = value.get("add").and_then(Value::as_bool).unwrap_or(false);
                Ok(Self::SetCount { count, add, conditions })
            }
            "minecraft:set_item" => {
                let item = parse_id(value.get("item"), "item")?;
                Ok(Self::SetItem { item, conditions })
            }
            "minecraft:enchanted_count_increase" => Ok(Self::EnchantedCountIncrease { conditions }),
            "minecraft:apply_bonus" => {
                let enchantment = parse_id(value.get("enchantment"), "enchantment")?;
                let formula = BonusFormula::from_value(value, audit)?;
                Ok(Self::ApplyBonus {
                    enchantment,
                    formula,
                    conditions,
                })
            }
            "minecraft:explosion_decay" => Ok(Self::ExplosionDecay { conditions }),
            "minecraft:furnace_smelt" => {
                let use_input_count = value.get("use_input_count").and_then(Value::as_bool).unwrap_or(true);
                Ok(Self::FurnaceSmelt { use_input_count, conditions })
            }
            "minecraft:sequence" => {
                let functions = parse_functions(value.get("functions"), audit)?;
                Ok(Self::Sequence(functions))
            }
            other => {
                audit.push(format!("function {other}"));
                Ok(Self::Unsupported)
            }
        }
    }

    fn apply(&self, stack: &mut ItemStack, context: &LootContext, rng: &mut SpawnRng) {
        match self {
            Self::SetCount { count, add, conditions } => {
                if !conditions.iter().all(|c| c.test(context, rng)) {
                    return;
                }
                let base = if *add { stack.count as i32 } else { 0 };
                stack.count = (base + count.int(context, rng)).max(0) as u32;
            }
            Self::SetItem { item, conditions } => {
                if conditions.iter().all(|c| c.test(context, rng)) {
                    stack.item = item.clone();
                }
            }
            Self::EnchantedCountIncrease { conditions } => {
                let _ = conditions;
                // `EnchantedCountIncrease` reads `ATTACKING_ENTITY`'s weapon,
                // which is never set for a block break; it returns the stack
                // untouched and draws nothing when the parameter is absent.
                // Wiring it means giving the context that parameter, not
                // changing this.
            }
            Self::ExplosionDecay { conditions } => {
                if !conditions.iter().all(|c| c.test(context, rng)) {
                    return;
                }
                // `ApplyExplosionDecay.run`, transcribed:
                //
                // ```java
                // Float explosionRadius = context.getOptionalParameter(EXPLOSION_RADIUS);
                // if (explosionRadius != null) {
                //     float probability = 1.0F / explosionRadius;
                //     int currentCount = itemStack.getCount();
                //     int resultCount = 0;
                //     for (int i = 0; i < currentCount; i++) {
                //         if (random.nextFloat() <= probability) { resultCount++; }
                //     }
                //     itemStack.setCount(resultCount);
                // }
                // ```
                //
                // **One draw per item in the stack**, not one per stack — that is
                // the whole difference between this and `survives_explosion`, and
                // it is why the two exist side by side: a single-item drop is
                // gated by the condition, a multi-item drop (a fortune-boosted
                // ore, a clutch of seeds) is *thinned* item by item here.
                //
                // `setCount(0)` is reachable and left as `count = 0`, not turned
                // into an absent stack: vanilla's caller drops empty stacks when
                // it collects them, and doing it here would hide the difference
                // between "rolled nothing" and "never rolled".
                let Some(radius) = context.explosion_radius else {
                    return;
                };
                let probability = 1.0 / radius;
                let mut kept = 0u32;
                for _ in 0..stack.count {
                    if rng.next_f32() <= probability {
                        kept += 1;
                    }
                }
                stack.count = kept;
            }
            Self::ApplyBonus {
                enchantment,
                formula,
                conditions,
            } => {
                if !conditions.iter().all(|c| c.test(context, rng)) {
                    return;
                }
                // `ApplyBonusCount.run`: **the guard is `tool != null`**, so a
                // bare hand skips the formula entirely (and draws nothing) while
                // an unenchanted tool runs it at level 0 — which for two of the
                // three formulas still consumes draws. Getting this backwards
                // produces a statistically identical count and desyncs the
                // stream for every later stack in the same roll.
                let Some(tool) = context.tool.as_ref() else {
                    return;
                };
                let level = tool.enchantment_level(enchantment);
                let new_count = formula.calculate(rng, stack.count as i32, level as i32);
                stack.count = new_count.max(0) as u32;
            }
            Self::FurnaceSmelt { use_input_count, conditions } => {
                if !conditions.iter().all(|c| c.test(context, rng)) {
                    return;
                }
                let ingredient = stack.item.to_string();
                if let Some(recipe) = crate::furnace::recipe_for(crate::furnace::FurnaceKind::Furnace, &ingredient) {
                    // `SmeltItemFunction.run`: result × (use_input_count ? count : 1).
                    let count = if *use_input_count { stack.count } else { 1 } * recipe.count;
                    if let Ok(output) = recipe.result.parse::<ResourceKey>() {
                        stack.item = output;
                        stack.count = count;
                    }
                }
            }
            Self::Sequence(functions) => {
                for function in functions {
                    function.apply(stack, context, rng);
                }
            }
            Self::Unsupported => {}
        }
    }
}

/// `ApplyBonusCount.Formula` — one of three, dispatched on the `formula` field
/// with its own `parameters` object.
///
/// Every arm is transcribed from the record body in
/// `.cache/mc/26.2/src/net/minecraft/world/level/storage/loot/functions/ApplyBonusCount.java`,
/// **not** from a summary of a call site, because two of the three are wrong in a
/// way that reads plausibly:
///
/// * `ore_drops` is commonly restated as `count * max(1, nextInt(level + 2))`.
///   That is arithmetically right and **draw-count wrong**: the record is
///   `if (level > 0) { … } else { return count; }`, so at level 0 it draws
///   *nothing*. The restatement draws `nextInt(2)` and throws it away, shifting
///   every later draw in the roll.
/// * `uniform_bonus_count` is `count + nextInt(bonusMultiplier * level + 1)`,
///   which at level 0 is `nextInt(1)` — **one draw, always zero**. It is not
///   guarded on the level at all.
/// * `binomial_with_bonus_count` loops `level + extra` times, so at level 0 it
///   still makes `extra` draws.
///
/// The corpus uses exactly four instantiations, all on `minecraft:fortune`:
/// `ore_drops` (17), `uniform_bonus_count {bonusMultiplier: 1}` (9),
/// `binomial_with_bonus_count {extra: 3, probability: 0.5714286}` (4), and
/// `uniform_bonus_count {bonusMultiplier: 2}` (2).
#[derive(Debug, Clone, PartialEq)]
enum BonusFormula {
    /// `OreDrops`: `level > 0 ? count * (max(nextInt(level + 2) - 1, 0) + 1) : count`.
    OreDrops,
    /// `BinomialWithBonusCount`: `level + extra` Bernoulli trials at `probability`,
    /// each adding one.
    BinomialWithBonusCount { extra: i32, probability: f32 },
    /// `UniformBonusCount`: `count + nextInt(bonusMultiplier * level + 1)`.
    UniformBonusCount { bonus_multiplier: i32 },
    /// A formula id this build does not evaluate. Leaves the count alone and
    /// draws nothing; reported by [`LootTable::unsupported_features`].
    Unsupported,
}

impl BonusFormula {
    fn from_value(value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        let id = value
            .get("formula")
            .and_then(Value::as_str)
            .ok_or(LootError::MissingField("formula"))?;
        let parameters = value.get("parameters");
        match id {
            "minecraft:ore_drops" => Ok(Self::OreDrops),
            "minecraft:binomial_with_bonus_count" => {
                let parameters =
                    parameters.ok_or(LootError::MissingField("parameters"))?;
                let extra = parameters
                    .get("extra")
                    .and_then(Value::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or(LootError::MissingField("extra"))?;
                let probability = parse_f32(
                    parameters
                        .get("probability")
                        .ok_or(LootError::MissingField("probability"))?,
                    "probability",
                )?;
                Ok(Self::BinomialWithBonusCount { extra, probability })
            }
            "minecraft:uniform_bonus_count" => {
                let parameters =
                    parameters.ok_or(LootError::MissingField("parameters"))?;
                let bonus_multiplier = parameters
                    .get("bonusMultiplier")
                    .and_then(Value::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or(LootError::MissingField("bonusMultiplier"))?;
                Ok(Self::UniformBonusCount { bonus_multiplier })
            }
            other => {
                audit.push(format!("apply_bonus formula {other}"));
                Ok(Self::Unsupported)
            }
        }
    }

    /// `Formula.calculateNewCount`. Integer arithmetic throughout — a float
    /// transliteration of `count * (bonus + 1)` would be host-libm dependent at
    /// the rounding boundary.
    fn calculate(&self, rng: &mut SpawnRng, count: i32, level: i32) -> i32 {
        match self {
            Self::OreDrops => {
                if level > 0 {
                    let bonus = (rng.next_int(level + 2) - 1).max(0);
                    count * (bonus + 1)
                } else {
                    count
                }
            }
            Self::BinomialWithBonusCount { extra, probability } => {
                let mut count = count;
                let rounds = level + extra;
                for _ in 0..rounds.max(0) {
                    if rng.next_f32() < *probability {
                        count += 1;
                    }
                }
                count
            }
            Self::UniformBonusCount { bonus_multiplier } => {
                // `nextInt(bound)` with `bound <= 0` throws in vanilla; the
                // corpus never produces one (`bonusMultiplier` is 1 or 2 and
                // level is non-negative), so `max(1)` is a guard, not a policy.
                count + rng.next_int((bonus_multiplier * level + 1).max(1))
            }
            Self::Unsupported => count,
        }
    }
}

/// A loot condition (`LootItemConditions` dispatch on `condition`).
///
/// Each variant evaluates exactly as vanilla does when its referenced context
/// params are absent — which is what the empty context is. An unsupported
/// condition fails (so the entry/pool it gates produces nothing).
#[derive(Debug, Clone)]
enum LootCondition {
    RandomChance(NumberProvider),
    RandomChanceWithEnchantedBonus {
        unenchanted_chance: f32,
    },
    KilledByPlayer,
    SurvivesExplosion,
    /// `MatchTool(Optional<ItemPredicate>)` — `tool != null && (predicate is
    /// empty || predicate.test(tool))`. **The absent-predicate case is `true`,
    /// not `false`**: `match_tool` with no `predicate` at all asks only "is
    /// anything in hand".
    MatchTool {
        predicate: Option<ItemPredicate>,
    },
    EntityProperties,
    /// `LootItemBlockStatePropertyCondition(Holder<Block> block,
    /// Optional<StatePropertiesPredicate> properties)`, whose `test` is
    ///
    /// ```text
    /// state != null && state.is(this.block)
    ///     && (this.properties.isEmpty() || this.properties.get().matches(state))
    /// ```
    ///
    /// Three things the record settles that a paraphrase gets wrong. `block` is
    /// **not** optional in the codec, and it is ANDed — a table whose condition
    /// names a different block than the state being broken fails, so a
    /// misapplied table cannot silently pass. `properties` **is** optional, and
    /// absent means "any state of this block", i.e. `true`, not `false`. And a
    /// matcher naming a property the block does not have is `false`, because
    /// `PropertyMatcher.match` starts `property != null &&`.
    BlockStateProperty {
        block: ResourceKey,
        properties: Vec<StatePropertyMatcher>,
    },
    DamageSourceProperties,
    LocationCheck,
    /// `BonusLevelTableCondition` — `nextFloat() < chances[min(level, len-1)]`.
    /// **Always draws**, tool or not, so its draw count does not depend on the
    /// context; only which chance is compared does.
    TableBonus {
        enchantment: ResourceKey,
        chances: Vec<f32>,
    },
    Inverted(Box<LootCondition>),
    AllOf(Vec<LootCondition>),
    AnyOf(Vec<LootCondition>),
    /// A condition this build does not evaluate (`minecraft:weather_check`,
    /// ...). Always fails in a roll; the feature id is reported by
    /// [`LootTable::unsupported_features`].
    Unsupported,
}

impl LootCondition {
    fn from_value(value: &Value, audit: &mut Vec<String>) -> Result<Self, LootError> {
        // Inline `{"all_of": [...]}` form (`AllOfCondition.INLINE_CODEC`).
        if let Some(terms) = value.get("all_of").and_then(Value::as_array) {
            let terms = terms.iter().map(|t| Self::from_value(t, audit)).collect::<Result<Vec<_>, _>>()?;
            return Ok(Self::AllOf(terms));
        }
        let id = value
            .get("condition")
            .and_then(Value::as_str)
            .ok_or(LootError::MissingField("condition"))?;
        match id {
            "minecraft:random_chance" => {
                let chance = parse_number_provider(value.get("chance").ok_or(LootError::MissingField("chance"))?, audit)?;
                Ok(Self::RandomChance(chance))
            }
            "minecraft:random_chance_with_enchanted_bonus" => {
                let unenchanted_chance = parse_f32(value.get("unenchanted_chance").ok_or(LootError::MissingField("unenchanted_chance"))?, "unenchanted_chance")?;
                Ok(Self::RandomChanceWithEnchantedBonus { unenchanted_chance })
            }
            "minecraft:killed_by_player" => Ok(Self::KilledByPlayer),
            "minecraft:survives_explosion" => Ok(Self::SurvivesExplosion),
            "minecraft:match_tool" => match value.get("predicate") {
                None => Ok(Self::MatchTool { predicate: None }),
                Some(raw) => match ItemPredicate::from_value(raw)? {
                    // A predicate shape this build does not model must **fail
                    // closed**, not match everything: `Unsupported` tests
                    // `false`, which is what `match_tool` did for every
                    // predicate before #539. Reporting it as an unsupported
                    // feature is what keeps such a table out of the curated
                    // bundle (`LootTableSet::load_bundled`'s debug assertion).
                    None => {
                        audit.push("condition minecraft:match_tool (unmodelled predicate)".to_string());
                        Ok(Self::Unsupported)
                    }
                    Some(predicate) => Ok(Self::MatchTool {
                        predicate: Some(predicate),
                    }),
                },
            },
            // These three parse a `predicate`/`entity`/offsets object and then
            // discard it: each references a context parameter this crate does not
            // carry, so each is a constant `false`. Recorded as *context-blind*
            // rather than unsupported — see `LootTable::context_blind_features`
            // for why the distinction is the point.
            "minecraft:entity_properties" => {
                audit.push(format!("{CONTEXT_BLIND_PREFIX}condition {id}"));
                Ok(Self::EntityProperties)
            }
            "minecraft:block_state_property" => {
                let block = parse_id(value.get("block"), "block")?;
                let properties = match value.get("properties") {
                    None => Vec::new(),
                    Some(raw) => {
                        let map = raw.as_object().ok_or_else(|| {
                            LootError::UnexpectedType("block_state_property properties", "an object")
                        })?;
                        map.iter()
                            .map(|(name, matcher)| StatePropertyMatcher::from_value(name, matcher))
                            .collect::<Result<Vec<_>, _>>()?
                    }
                };
                Ok(Self::BlockStateProperty { block, properties })
            }
            "minecraft:damage_source_properties" => {
                audit.push(format!("{CONTEXT_BLIND_PREFIX}condition {id}"));
                Ok(Self::DamageSourceProperties)
            }
            "minecraft:location_check" => {
                audit.push(format!("{CONTEXT_BLIND_PREFIX}condition {id}"));
                Ok(Self::LocationCheck)
            }
            "minecraft:table_bonus" => {
                let chances = value
                    .get("chances")
                    .and_then(Value::as_array)
                    .ok_or(LootError::MissingField("chances"))?
                    .iter()
                    .map(|c| parse_f32(c, "chances entry"))
                    .collect::<Result<Vec<_>, _>>()?;
                if chances.is_empty() {
                    return Err(LootError::EmptyTableBonus);
                }
                let enchantment = parse_id(value.get("enchantment"), "enchantment")?;
                Ok(Self::TableBonus {
                    enchantment,
                    chances,
                })
            }
            "minecraft:inverted" => {
                let term = Self::from_value(value.get("term").ok_or(LootError::MissingField("term"))?, audit)?;
                Ok(Self::Inverted(Box::new(term)))
            }
            "minecraft:all_of" => {
                let terms = parse_conditions(value.get("terms"), audit)?;
                Ok(Self::AllOf(terms))
            }
            "minecraft:any_of" => {
                let terms = parse_conditions(value.get("terms"), audit)?;
                Ok(Self::AnyOf(terms))
            }
            other => {
                audit.push(format!("condition {other}"));
                Ok(Self::Unsupported)
            }
        }
    }

    fn test(&self, context: &LootContext, rng: &mut SpawnRng) -> bool {
        match self {
            Self::RandomChance(chance) => rng.next_f32() < chance.float(context, rng),
            Self::RandomChanceWithEnchantedBonus { unenchanted_chance } => rng.next_f32() < *unenchanted_chance,
            // No relevant context param, so vanilla's `hasParameter`/null check
            // reads absent: each is `false`.
            Self::KilledByPlayer
            | Self::EntityProperties
            | Self::DamageSourceProperties
            | Self::LocationCheck => false,
            // `LootItemBlockStatePropertyCondition.test`, transcribed. Consumes
            // no RNG either way, so filling the block state in cannot shift the
            // stream through this condition — only through which entry wins.
            Self::BlockStateProperty { block, properties } => match context.block_state.as_ref() {
                None => false,
                Some(state) => {
                    state.block == *block && properties.iter().all(|m| m.matches(state))
                }
            },
            // `ExplosionCondition.test`, transcribed:
            //
            // ```java
            // Float explosionRadius = context.getOptionalParameter(EXPLOSION_RADIUS);
            // if (explosionRadius != null) {
            //     float probability = 1.0F / explosionRadius;
            //     return random.nextFloat() <= probability;
            // } else { return true; }
            // ```
            //
            // Two things a paraphrase gets wrong. The comparison is `<=`, not
            // `<` — irrelevant for a continuous draw but not for a reader
            // checking the port. And the absent-parameter branch draws
            // **nothing**, so this condition's draw count is 1 for a blast and 0
            // for a mined block; a version that always drew would shift every
            // later roll in a mining stream.
            Self::SurvivesExplosion => match context.explosion_radius {
                None => true,
                Some(radius) => rng.next_f32() <= 1.0 / radius,
            },
            // `MatchTool.test`: `tool != null && (predicate.isEmpty() ||
            // predicate.get().test(tool))`. Consumes no RNG either way, so
            // adding a tool cannot shift the stream through this condition.
            Self::MatchTool { predicate } => match context.tool.as_ref() {
                None => false,
                Some(tool) => predicate.as_ref().is_none_or(|p| p.test(tool)),
            },
            // `values[min(level, len-1)]`, where level is 0 with no tool — which
            // is `values[0]`, the pre-#539 behaviour, reached now by the general
            // path rather than by assumption.
            Self::TableBonus {
                enchantment,
                chances,
            } => {
                let level = context
                    .tool
                    .as_ref()
                    .map_or(0, |tool| tool.enchantment_level(enchantment));
                let index = (level as usize).min(chances.len() - 1);
                rng.next_f32() < chances[index]
            }
            Self::Inverted(term) => !term.test(context, rng),
            Self::AllOf(terms) => terms.iter().all(|t| t.test(context, rng)),
            Self::AnyOf(terms) => terms.iter().any(|t| t.test(context, rng)),
            Self::Unsupported => false,
        }
    }
}

/// One entry of a `StatePropertiesPredicate` — `PropertyMatcher(String name,
/// ValueMatcher valueMatcher)`, i.e. one `"age": "7"` pair out of the condition's
/// `properties` object.
///
/// `match` is `definition.getProperty(name) != null && valueMatcher.match(…)`, so
/// an unknown property name fails the whole predicate rather than being ignored.
#[derive(Debug, Clone, PartialEq)]
struct StatePropertyMatcher {
    name: String,
    matcher: StateValueMatcher,
}

impl StatePropertyMatcher {
    fn from_value(name: &str, value: &Value) -> Result<Self, LootError> {
        Ok(Self {
            name: name.to_string(),
            matcher: StateValueMatcher::from_value(value)?,
        })
    }

    fn matches(&self, state: &LootBlockState) -> bool {
        match state.property(&self.name) {
            None => false,
            Some(value) => self.matcher.matches(value),
        }
    }
}

/// `StatePropertiesPredicate.ValueMatcher` — `Codec.either(ExactMatcher,
/// RangedMatcher)`, so a property's JSON value is either a bare string or a
/// `{"min": …, "max": …}` object.
///
/// # Both bounds are strings in the record, and the comparison is *typed*
///
/// `RangedMatcher(Optional<String> minValue, Optional<String> maxValue)` compares
/// with `property.getValue(bound)` then `value.compareTo(bound)` — the
/// **property's own** ordering, which for an `IntegerProperty` is numeric and for
/// a `BooleanProperty` is `false < true`. Comparing the serialized strings
/// lexicographically instead would put `"9" > "10"`, so [`Self::matches`] parses
/// numerically when both sides are integers and compares booleans as booleans.
///
/// An `EnumProperty`'s ordering is its **declaration order**, which is not
/// recoverable from a serialized name, so a ranged matcher over an enum property
/// fails closed here. That path is unreachable from the bundle: all 258
/// `block_state_property` conditions across the 1,241 bundled tables use the
/// exact-string form and **none** uses a range, measured by walking the JSON.
#[derive(Debug, Clone, PartialEq)]
enum StateValueMatcher {
    /// `ExactMatcher(String value)` — `property.getValue(value)` is present and
    /// `compareTo == 0`. Every typed value has exactly one serialized form and
    /// `getValue` is its inverse, so string equality is the same predicate: a
    /// bound outside the property's domain (`age: "99"`) matches no real state
    /// under either reading.
    Exact(String),
    /// `RangedMatcher(Optional<String>, Optional<String>)`. An absent bound is
    /// vacuously satisfied; a bound outside the property's domain fails
    /// (`typedMinValue.isEmpty() → return false`), which the numeric parse
    /// reproduces by failing to parse.
    Ranged {
        min: Option<String>,
        max: Option<String>,
    },
}

impl StateValueMatcher {
    fn from_value(value: &Value) -> Result<Self, LootError> {
        match value {
            Value::String(exact) => Ok(Self::Exact(exact.clone())),
            Value::Object(bounds) => {
                let bound = |key: &str| -> Result<Option<String>, LootError> {
                    match bounds.get(key) {
                        None => Ok(None),
                        Some(Value::String(raw)) => Ok(Some(raw.clone())),
                        Some(_) => Err(LootError::UnexpectedType(
                            "block state property bound",
                            "a string",
                        )),
                    }
                };
                Ok(Self::Ranged {
                    min: bound("min")?,
                    max: bound("max")?,
                })
            }
            _ => Err(LootError::UnexpectedType(
                "block state property value",
                "a string or a min/max object",
            )),
        }
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Exact(expected) => value == expected,
            Self::Ranged { min, max } => {
                if let Some(min) = min {
                    if compare_state_values(value, min).is_none_or(|o| o.is_lt()) {
                        return false;
                    }
                }
                if let Some(max) = max {
                    if compare_state_values(value, max).is_none_or(|o| o.is_gt()) {
                        return false;
                    }
                }
                true
            }
        }
    }
}

/// `value.compareTo(bound)` in the property's own ordering, for the two property
/// kinds whose ordering a serialized name determines: integers compare
/// numerically, booleans as `false < true`.
///
/// `None` means "not comparable" — an enum property, or a bound outside the
/// property's domain — and every caller treats that as a failed match, matching
/// `RangedMatcher.match`'s `typedMinValue.isEmpty() → false`.
fn compare_state_values(value: &str, bound: &str) -> Option<std::cmp::Ordering> {
    if let (Ok(value), Ok(bound)) = (value.parse::<i64>(), bound.parse::<i64>()) {
        return Some(value.cmp(&bound));
    }
    if let (Ok(value), Ok(bound)) = (value.parse::<bool>(), bound.parse::<bool>()) {
        return Some(value.cmp(&bound));
    }
    None
}

/// `advancements/predicates/ItemPredicate` — `(items, count, components)`, of
/// which this models `items` and `count` plus the one `components.predicates`
/// entry the 26.2 loot corpus uses.
///
/// # What the corpus actually contains
///
/// Surveyed across all 1,355 tables: 203 `match_tool` conditions, and the
/// `predicate` object has exactly **three** shapes — `{"predicates": {…}}` (156,
/// and the only key ever used inside is `minecraft:enchantments`),
/// `{"items": […]}` (47), and one of those 47 whose `items` is a `#tag` string.
/// `components` (the *exact* matcher) never appears, and neither does `count`.
/// So this models `items`-as-a-list and `enchantments`, and reports anything
/// else unsupported — which fails the condition closed, exactly as the whole of
/// `match_tool` did before.
///
/// Vanilla's `test` is `items.isEmpty() || stack.is(items)`, then
/// `count.matches(stack.count())`, then `components.test(stack)` — an AND of all
/// three, each vacuously true when absent.
#[derive(Debug, Clone, PartialEq)]
struct ItemPredicate {
    /// `Optional<HolderSet<Item>>` in its direct-list form. `None` is the absent
    /// field, which matches any item.
    items: Option<Vec<ResourceKey>>,
    /// `MinMaxBounds.Ints count` — `ANY` when absent.
    count: IntBounds,
    /// `components.predicates["minecraft:enchantments"]`, a list every element of
    /// which must match (vanilla ANDs the `partial` map's values, and the
    /// enchantments predicate itself ANDs its list — see
    /// `EnchantmentsPredicate.matches`).
    enchantments: Vec<EnchantmentPredicate>,
}

impl ItemPredicate {
    /// Parses one `predicate` object, or `Ok(None)` if it uses a shape this build
    /// does not model (the caller turns that into an unsupported, always-failing
    /// condition rather than a match-everything one).
    fn from_value(value: &Value) -> Result<Option<Self>, LootError> {
        let object = value
            .as_object()
            .ok_or_else(|| LootError::UnexpectedType("match_tool predicate", "an object"))?;
        // `components` is the *exact* data-component matcher. Nothing in the
        // 26.2 loot corpus uses it, and modelling it needs a component-value
        // vocabulary this crate does not have.
        if object.contains_key("components") {
            return Ok(None);
        }
        let items = match object.get("items") {
            None => None,
            Some(Value::String(one)) => {
                // A `#tag` needs an item-tag census `lodestone-data` does not
                // bundle (only block tags). A bare id string is the
                // single-element `HolderSet` form and is fine.
                if one.starts_with('#') {
                    return Ok(None);
                }
                Some(vec![one
                    .parse()
                    .map_err(|_| LootError::BadIdentifier(one.clone()))?])
            }
            Some(Value::Array(list)) => {
                let mut out = Vec::with_capacity(list.len());
                for entry in list {
                    let raw = entry
                        .as_str()
                        .ok_or(LootError::UnexpectedType("items entry", "a string"))?;
                    if raw.starts_with('#') {
                        return Ok(None);
                    }
                    out.push(
                        raw.parse()
                            .map_err(|_| LootError::BadIdentifier(raw.to_string()))?,
                    );
                }
                Some(out)
            }
            Some(_) => return Err(LootError::UnexpectedType("items", "a string or array")),
        };
        let count = match object.get("count") {
            None => IntBounds::ANY,
            Some(raw) => IntBounds::from_value(raw)?,
        };
        let mut enchantments = Vec::new();
        if let Some(predicates) = object.get("predicates") {
            let map = predicates
                .as_object()
                .ok_or_else(|| LootError::UnexpectedType("predicates", "an object"))?;
            for (key, raw) in map {
                if key != "minecraft:enchantments" {
                    return Ok(None);
                }
                let list = raw
                    .as_array()
                    .ok_or(LootError::UnexpectedType("enchantments", "an array"))?;
                for entry in list {
                    enchantments.push(EnchantmentPredicate::from_value(entry)?);
                }
            }
        }
        Ok(Some(Self {
            items,
            count,
            enchantments,
        }))
    }

    fn test(&self, tool: &LootTool) -> bool {
        if let Some(items) = &self.items {
            match &tool.item {
                None => return false,
                Some(held) => {
                    if !items.contains(held) {
                        return false;
                    }
                }
            }
        }
        if !self.count.matches(i32::try_from(tool.count).unwrap_or(i32::MAX)) {
            return false;
        }
        self.enchantments.iter().all(|p| p.matches(tool))
    }
}

/// `advancements/predicates/EnchantmentPredicate` — `(enchantments, level)`.
///
/// `containedIn` has three branches, and the two beyond the common one are easy
/// to miss: with `enchantments` present it is "any listed enchantment is on the
/// stack at a matching level"; with `enchantments` absent but `levels` set it is
/// "**any** enchantment on the stack has a matching level"; with both absent it
/// is "the stack is enchanted at all".
#[derive(Debug, Clone, PartialEq)]
struct EnchantmentPredicate {
    enchantments: Option<Vec<ResourceKey>>,
    levels: IntBounds,
}

impl EnchantmentPredicate {
    fn from_value(value: &Value) -> Result<Self, LootError> {
        let enchantments = match value.get("enchantments") {
            None => None,
            Some(Value::String(one)) => Some(vec![
                one.parse()
                    .map_err(|_| LootError::BadIdentifier(one.clone()))?,
            ]),
            Some(Value::Array(list)) => {
                let mut out = Vec::with_capacity(list.len());
                for entry in list {
                    let raw = entry
                        .as_str()
                        .ok_or(LootError::UnexpectedType("enchantments entry", "a string"))?;
                    out.push(
                        raw.parse()
                            .map_err(|_| LootError::BadIdentifier(raw.to_string()))?,
                    );
                }
                Some(out)
            }
            Some(_) => {
                return Err(LootError::UnexpectedType(
                    "enchantments",
                    "a string or array",
                ));
            }
        };
        let levels = match value.get("levels") {
            None => IntBounds::ANY,
            Some(raw) => IntBounds::from_value(raw)?,
        };
        Ok(Self {
            enchantments,
            levels,
        })
    }

    fn matches(&self, tool: &LootTool) -> bool {
        match &self.enchantments {
            Some(wanted) => wanted.iter().any(|key| {
                let level = tool.enchantment_level(key);
                // `matchesEnchantment`: level 0 means absent, never "matches a
                // `{max: 0}` bound".
                level != 0 && (self.levels.is_any() || self.levels.matches(level as i32))
            }),
            None if !self.levels.is_any() => tool
                .enchantments
                .iter()
                .any(|(_, level)| self.levels.matches(*level as i32)),
            None => !tool.enchantments.is_empty(),
        }
    }
}

/// `MinMaxBounds.Ints` — `{min, max}`, either bound optional, **or** a bare
/// number meaning `exactly(n)` (`Codec.either(rangeCodec, numberCodec)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct IntBounds {
    min: Option<i32>,
    max: Option<i32>,
}

impl IntBounds {
    const ANY: Self = Self {
        min: None,
        max: None,
    };

    fn from_value(value: &Value) -> Result<Self, LootError> {
        if let Some(exact) = value.as_i64() {
            let exact = i32::try_from(exact)
                .map_err(|_| LootError::UnexpectedType("bounds", "a 32-bit integer"))?;
            return Ok(Self {
                min: Some(exact),
                max: Some(exact),
            });
        }
        let object = value
            .as_object()
            .ok_or_else(|| LootError::UnexpectedType("bounds", "a number or object"))?;
        let bound = |key: &str| -> Result<Option<i32>, LootError> {
            match object.get(key) {
                None => Ok(None),
                Some(raw) => raw
                    .as_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .map(Some)
                    .ok_or(LootError::UnexpectedType("bounds entry", "an integer")),
            }
        };
        Ok(Self {
            min: bound("min")?,
            max: bound("max")?,
        })
    }

    fn is_any(self) -> bool {
        self.min.is_none() && self.max.is_none()
    }

    fn matches(self, value: i32) -> bool {
        self.min.is_none_or(|min| value >= min) && self.max.is_none_or(|max| value <= max)
    }
}

/// A resolver that resolves no nested tables — [`LootTable::roll`]'s default.
struct NoTables;

impl LootTableResolver for NoTables {
    fn loot_table(&self, _id: &ResourceKey) -> Option<&LootTable> {
        None
    }
}

/// Errors from parsing a loot-table document.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LootError {
    /// The document was not valid JSON.
    #[error("invalid json: {0}")]
    Json(String),
    /// A required field was absent.
    #[error("missing required field {0}")]
    MissingField(&'static str),
    /// A field had the wrong shape.
    #[error("expected {0}, found {1}")]
    UnexpectedType(&'static str, &'static str),
    /// A namespaced id could not be parsed.
    #[error("invalid identifier {0:?}")]
    BadIdentifier(String),
    /// A `table_bonus` condition carried an empty `chances` list.
    #[error("table_bonus condition has no chances")]
    EmptyTableBonus,
}

fn parse_id(value: Option<&Value>, field: &'static str) -> Result<ResourceKey, LootError> {
    let raw = value
        .and_then(Value::as_str)
        .ok_or(LootError::MissingField(field))?;
    raw.parse().map_err(|_| LootError::BadIdentifier(raw.to_string()))
}

fn parse_f32(value: &Value, field: &'static str) -> Result<f32, LootError> {
    value
        .as_f64()
        .map(|f| f as f32)
        .ok_or_else(|| LootError::UnexpectedType(field, "a number"))
}

fn parse_int_default(value: Option<&Value>, default: i32) -> Result<i32, LootError> {
    match value {
        Some(v) => v
            .as_i64()
            .and_then(|i| i32::try_from(i).ok())
            .ok_or_else(|| LootError::UnexpectedType("weight/quality", "an integer")),
        None => Ok(default),
    }
}

fn parse_conditions(value: Option<&Value>, audit: &mut Vec<String>) -> Result<Vec<LootCondition>, LootError> {
    match value {
        Some(v) => v
            .as_array()
            .ok_or(LootError::UnexpectedType("conditions", "an array"))?
            .iter()
            .map(|c| LootCondition::from_value(c, audit))
            .collect(),
        None => Ok(Vec::new()),
    }
}

fn parse_functions(value: Option<&Value>, audit: &mut Vec<String>) -> Result<Vec<LootFunction>, LootError> {
    match value {
        Some(v) => v
            .as_array()
            .ok_or(LootError::UnexpectedType("functions", "an array"))?
            .iter()
            .map(|f| LootFunction::from_value(f, audit))
            .collect(),
        None => Ok(Vec::new()),
    }
}

fn parse_number_providers(value: Option<&Value>, audit: &mut Vec<String>) -> Result<Vec<NumberProvider>, LootError> {
    match value {
        Some(v) => v
            .as_array()
            .ok_or(LootError::UnexpectedType("number providers", "an array"))?
            .iter()
            .map(|p| parse_number_provider(p, audit))
            .collect(),
        None => Ok(Vec::new()),
    }
}

fn parse_number_provider(value: &Value, audit: &mut Vec<String>) -> Result<NumberProvider, LootError> {
    NumberProvider::from_value(value, audit)
}

fn parse_entries(value: Option<&Value>, audit: &mut Vec<String>) -> Result<Vec<LootEntry>, LootError> {
    value
        .ok_or(LootError::MissingField("children"))?
        .as_array()
        .ok_or(LootError::UnexpectedType("children", "an array"))?
        .iter()
        .map(|e| LootEntry::from_value(e, audit))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled `minecraft:blocks/dirt` table, re-parsed from a raw string
    /// so the test does not depend on the embedded build table (which
    /// `load_bundled` exercises separately).
    const DIRT: &str = r#"{
      "type": "minecraft:block",
      "pools": [
        {
          "conditions": [ { "condition": "minecraft:survives_explosion" } ],
          "entries": [ { "type": "minecraft:item", "name": "minecraft:dirt" } ],
          "rolls": 1.0
        }
      ]
    }"#;

    fn table(id: &str, json: &str) -> LootTable {
        let key: ResourceKey = id.parse().unwrap();
        LootTable::from_json(&key, json).expect("table parses")
    }

    fn describe(stacks: &[ItemStack]) -> Vec<String> {
        stacks.iter().map(|s| format!("{}x{}", s.item, s.count)).collect()
    }

    #[test]
    fn dirt_rolls_exactly_one_dirt() {
        let t = table("minecraft:blocks/dirt", DIRT);
        assert!(t.unsupported_features().is_empty());
        let mut rng = SpawnRng::new(1);
        let out = t.roll(&LootContext::default(), &mut rng);
        assert_eq!(describe(&out), vec!["minecraft:dirtx1".to_string()]);
    }

    #[test]
    fn stone_drops_cobblestone_not_stone_in_empty_context() {
        // `alternatives`: the silk-touch arm is gated on `match_tool` (no tool
        // → false), so the roll lands on the `survives_explosion` cobblestone
        // arm.
        let json = r#"{
          "pools": [{
            "entries": [{
              "type": "minecraft:alternatives",
              "children": [
                { "type": "minecraft:item", "name": "minecraft:stone",
                  "conditions": [ { "condition": "minecraft:match_tool", "predicate": {} } ] },
                { "type": "minecraft:item", "name": "minecraft:cobblestone",
                  "conditions": [ { "condition": "minecraft:survives_explosion" } ] }
              ]
            }],
            "rolls": 1.0
          }]
        }"#;
        let t = table("minecraft:blocks/stone", json);
        assert!(t.unsupported_features().is_empty());
        let mut rng = SpawnRng::new(2);
        let out = t.roll(&LootContext::default(), &mut rng);
        assert_eq!(describe(&out), vec!["minecraft:cobblestonex1".to_string()]);
    }

    #[test]
    fn weighted_item_selection_follows_the_weights() {
        // Two entries, weights 3 and 1: the first must win ~75% of rolls.
        let json = r#"{
          "pools": [{
            "entries": [
              { "type": "minecraft:item", "name": "minecraft:common", "weight": 3 },
              { "type": "minecraft:item", "name": "minecraft:rare", "weight": 1 }
            ],
            "rolls": 1.0
          }]
        }"#;
        let t = table("minecraft:test/weighted", json);
        let mut common = 0usize;
        const SAMPLES: usize = 40_000;
        for seed in 0..SAMPLES as u64 {
            let mut rng = SpawnRng::new(seed);
            let out = t.roll(&LootContext::default(), &mut rng);
            assert_eq!(out.len(), 1, "one roll produces one stack");
            if out[0].item.to_string() == "minecraft:common" {
                common += 1;
            }
        }
        let p = common as f64 / SAMPLES as f64;
        // p = 0.75, σ ≈ sqrt(0.75·0.25/40000) ≈ 0.0022 → 3σ ≈ 0.0065.
        assert!((0.72..0.78).contains(&p), "weight-3 entry won {p:.4} of rolls");
    }

    #[test]
    fn empty_entry_can_absorb_a_roll() {
        let json = r#"{
          "pools": [{
            "entries": [
              { "type": "minecraft:item", "name": "minecraft:loot", "weight": 1 },
              { "type": "minecraft:empty", "weight": 1 }
            ],
            "rolls": 1.0
          }]
        }"#;
        let t = table("minecraft:test/empty", json);
        let mut looted = 0usize;
        const SAMPLES: usize = 40_000;
        for seed in 0..SAMPLES as u64 {
            let mut rng = SpawnRng::new(seed);
            let out = t.roll(&LootContext::default(), &mut rng);
            if !out.is_empty() {
                assert_eq!(describe(&out), vec!["minecraft:lootx1".to_string()]);
                looted += 1;
            }
        }
        let p = looted as f64 / SAMPLES as f64;
        assert!((0.48..0.52).contains(&p), "empty entry absorbed {:.4} of rolls", p);
    }

    #[test]
    fn uniform_set_count_produces_the_expected_distribution() {
        let json = r#"{
          "pools": [{
            "entries": [{
              "type": "minecraft:item",
              "name": "minecraft:item",
              "functions": [{
                "function": "minecraft:set_count",
                "count": { "type": "minecraft:uniform", "min": 1.0, "max": 3.0 }
              }]
            }],
            "rolls": 1.0
          }]
        }"#;
        let t = table("minecraft:test/count", json);
        let mut counts = [0usize; 3];
        for seed in 0..12_000u64 {
            let mut rng = SpawnRng::new(seed);
            let out = t.roll(&LootContext::default(), &mut rng);
            assert_eq!(out.len(), 1);
            counts[(out[0].count - 1) as usize] += 1;
        }
        for (i, n) in counts.iter().enumerate() {
            let p = *n as f64 / 12_000.0;
            assert!((0.31..0.36).contains(&p), "count {} appeared {p:.4}", i + 1);
        }
    }

    #[test]
    fn zombie_rolls_only_rotten_flesh_in_the_empty_context() {
        // The killed_by_player / entity_properties pools gate to false without
        // an entity; the rotten-flesh pool always rolls, count uniform 0..2.
        let set = LootTableSet::load_bundled();
        let id: ResourceKey = "minecraft:entities/zombie".parse().unwrap();
        let zombie = set.get(&id).expect("bundled zombie table");
        assert!(zombie.unsupported_features().is_empty());
        for seed in 0..2000u64 {
            let mut rng = SpawnRng::new(seed);
            let out = set.roll(&id, &LootContext::default(), &mut rng);
            assert!(!out.is_empty(), "rotten-flesh pool always rolls");
            for stack in &out {
                assert_eq!(stack.item.to_string(), "minecraft:rotten_flesh");
                assert!((0..=2).contains(&stack.count));
            }
        }
    }

    #[test]
    fn random_chance_gates_a_roll() {
        // chance 1.0 → always; chance 0.0 → never.
        let always = table(
            "minecraft:test/chance_always",
            r#"{
              "pools": [{
                "conditions": [ { "condition": "minecraft:random_chance", "chance": 1.0 } ],
                "entries": [ { "type": "minecraft:item", "name": "minecraft:yes" } ],
                "rolls": 1.0
              }]
            }"#,
        );
        let never = table(
            "minecraft:test/chance_never",
            r#"{
              "pools": [{
                "conditions": [ { "condition": "minecraft:random_chance", "chance": 0.0 } ],
                "entries": [ { "type": "minecraft:item", "name": "minecraft:yes" } ],
                "rolls": 1.0
              }]
            }"#,
        );
        for seed in 0..500u64 {
            let mut rng = SpawnRng::new(seed);
            assert_eq!(always.roll(&LootContext::default(), &mut rng).len(), 1);
            assert!(never.roll(&LootContext::default(), &mut rng).is_empty());
        }
    }

    #[test]
    fn nested_table_resolves_through_the_resolver() {
        let outer = table(
            "minecraft:test/outer",
            r#"{
              "pools": [{
                "entries": [
                  { "type": "minecraft:loot_table", "value": "minecraft:test/inner", "weight": 1 },
                  { "type": "minecraft:item", "name": "minecraft:outer_item", "weight": 1 }
                ],
                "rolls": 1.0
              }]
            }"#,
        );
        let inner = table(
            "minecraft:test/inner",
            r#"{
              "pools": [{
                "entries": [ { "type": "minecraft:item", "name": "minecraft:inner_item" } ],
                "rolls": 1.0
              }]
            }"#,
        );
        let set = LootTableBuilder::new()
            .push_table_owned(outer)
            .push_table_owned(inner)
            .finish();
        let id: ResourceKey = "minecraft:test/outer".parse().unwrap();
        let mut rng = SpawnRng::new(7);
        let out = set.roll(&id, &LootContext::default(), &mut rng);
        // Both leaves have weight 1; every roll emits exactly one of them.
        assert_eq!(out.len(), 1);
        let name = out[0].item.to_string();
        assert!(name == "minecraft:inner_item" || name == "minecraft:outer_item");
    }

    #[test]
    fn self_referential_table_terminates() {
        // The self branch shares the pool's weight with the anchor, so a roll
        // that selects it correctly produces nothing rather than recursing
        // forever (the visited-set guard). The anchor must still be reachable.
        let self_ref = table(
            "minecraft:test/self",
            r#"{
              "pools": [{
                "entries": [
                  { "type": "minecraft:loot_table", "value": "minecraft:test/self", "weight": 1 },
                  { "type": "minecraft:item", "name": "minecraft:anchor", "weight": 1 }
                ],
                "rolls": 1.0
              }]
            }"#,
        );
        let set = LootTableBuilder::new().push_table_owned(self_ref).finish();
        let id: ResourceKey = "minecraft:test/self".parse().unwrap();
        let mut saw_anchor = false;
        for seed in 0..200u64 {
            let mut rng = SpawnRng::new(seed);
            let out = set.roll(&id, &LootContext::default(), &mut rng);
            assert!(out.len() <= 1, "the self branch produces nothing");
            if let Some(stack) = out.first() {
                assert_eq!(stack.item.to_string(), "minecraft:anchor");
                saw_anchor = true;
            }
        }
        assert!(saw_anchor, "the anchor must be reachable");
    }

    #[test]
    fn unknown_function_is_reported_not_fatal() {
        let t = table(
            "minecraft:test/unknown",
            r#"{
              "pools": [{
                "entries": [{
                  "type": "minecraft:item",
                  "name": "minecraft:book",
                  "functions": [ { "function": "minecraft:enchant_randomly" } ]
                }],
                "rolls": 1.0
              }]
            }"#,
        );
        assert_eq!(
            t.unsupported_features(),
            &["function minecraft:enchant_randomly".to_string()],
        );
        // The unknown function is skipped; the item still drops, unenchanted.
        let mut rng = SpawnRng::new(9);
        let out = t.roll(&LootContext::default(), &mut rng);
        assert_eq!(describe(&out), vec!["minecraft:bookx1".to_string()]);
    }

    #[test]
    fn malformed_known_shape_is_a_failure() {
        let mut builder = LootTableBuilder::new();
        // `minecraft:item` with no `name`.
        builder.push_table(
            "minecraft:test/bad",
            r#"{ "pools": [ { "entries": [ { "type": "minecraft:item" } ], "rolls": 1.0 } ] }"#,
        );
        assert_eq!(builder.failures().len(), 1);
        assert!(builder.failures()[0].0.ends_with("minecraft:test/bad"));
        // A pool without `entries` is also malformed.
        let mut builder = LootTableBuilder::new();
        builder.push_table("minecraft:test/bad2", r#"{ "pools": [ { "rolls": 1.0 } ] }"#);
        assert_eq!(builder.failures().len(), 1);
    }

    #[test]
    fn inline_all_of_condition_parses() {
        let t = table(
            "minecraft:test/inline_all_of",
            r#"{
              "pools": [{
                "conditions": [
                  { "all_of": [ { "condition": "minecraft:survives_explosion" } ] }
                ],
                "entries": [ { "type": "minecraft:item", "name": "minecraft:yes" } ],
                "rolls": 1.0
              }]
            }"#,
        );
        let mut rng = SpawnRng::new(4);
        assert_eq!(t.roll(&LootContext::default(), &mut rng).len(), 1);
    }

    #[test]
    fn bare_min_max_uniform_parses_as_a_number_provider() {
        let t = table(
            "minecraft:test/bare_uniform",
            r#"{
              "pools": [{
                "entries": [{
                  "type": "minecraft:item",
                  "name": "minecraft:item",
                  "functions": [
                    { "function": "minecraft:set_count", "count": { "min": 2.0, "max": 4.0 } }
                  ]
                }],
                "rolls": 1.0
              }]
            }"#,
        );
        let mut rng = SpawnRng::new(5);
        let out = t.roll(&LootContext::default(), &mut rng);
        assert!((2..=4).contains(&out[0].count), "count was {}", out[0].count);
    }

    /// Since #538 the bundle is the **whole clean subset** of Mojang's 26.2
    /// corpus, not a hand-picked handful. Two invariants, and the count is
    /// deliberately exact rather than a floor.
    ///
    /// `1241` is not a preference: it is the number of tables in
    /// `.cache/mc/26.2/client-src/data/minecraft/loot_table/` (1,355) this roller
    /// either fully evaluates or only fails to *decorate*
    /// ([`DECORATION_ONLY_UNSUPPORTED`]), measured by `tests/loot_corpus.rs`'s own
    /// scan. A change to the roller that makes more or fewer tables clean must
    /// move this number *and* regenerate the bundle (`just regen-loot-corpus`),
    /// which is what stops the two drifting apart silently — the corpus gate
    /// asserts the same number from the cache side.
    ///
    /// **"Rolls something" is not asserted**, and that is a correction rather
    /// than a relaxation: plenty of real tables legitimately roll nothing at a
    /// given seed (an `empty` entry winning, a `random_chance` failing, a
    /// `killed_by_player` pool with no killer). The old six-table version could
    /// assert non-emptiness only because all six happened to be unconditional.
    /// What is still asserted is that every table rolls **without panicking** and
    /// produces only well-formed stacks.
    #[test]
    fn bundled_tables_are_all_fully_supported_and_roll() {
        let set = LootTableSet::load_bundled();
        assert_eq!(
            set.len(),
            1241,
            "the bundle is the clean subset of the 1,355-table vanilla corpus; \
             if this moved, regenerate with `just regen-loot-corpus` and say why"
        );
        for table in set.iter() {
            for feature in table.unsupported_features() {
                assert!(
                    DECORATION_ONLY_UNSUPPORTED.contains(&feature.as_str()),
                    "bundled {} uses {feature}, which is not decoration-only",
                    table.id,
                );
            }
        }
        // Every bundled table rolls without panicking, and every stack it
        // produces names a parseable item.
        let mut produced = 0usize;
        for table in set.iter() {
            let mut rng = SpawnRng::new(42);
            for stack in set.roll(&table.id, &LootContext::default(), &mut rng) {
                assert!(
                    !stack.item.to_string().is_empty(),
                    "{} produced a nameless stack",
                    table.id
                );
                produced += 1;
            }
        }
        assert!(
            produced > 1000,
            "one seed across 1,230 tables must produce a lot of stacks; {produced} \
             suggests the roller is short-circuiting"
        );
        // The five tables #538 replaced still behave exactly as they did, which
        // is the regression guard for the bulk import.
        for (id, expected) in [
            ("minecraft:blocks/dirt", "minecraft:dirt"),
            ("minecraft:blocks/stone", "minecraft:cobblestone"),
            ("minecraft:blocks/coal_ore", "minecraft:coal"),
            ("minecraft:blocks/iron_ore", "minecraft:raw_iron"),
        ] {
            let key: ResourceKey = id.parse().unwrap();
            let mut rng = SpawnRng::new(42);
            let out = set.roll(&key, &LootContext::default(), &mut rng);
            assert_eq!(describe(&out), vec![format!("{expected}x1")], "{id}");
        }
    }

    #[test]
    fn roll_loot_convenience_matches_set_roll() {
        let set = LootTableSet::load_bundled();
        let id: ResourceKey = "minecraft:blocks/dirt".parse().unwrap();
        let mut a = SpawnRng::new(11);
        let mut b = SpawnRng::new(11);
        assert_eq!(roll_loot(&set, &id, &mut a), set.roll(&id, &LootContext::default(), &mut b));
    }

    /// A synthetic one-item table whose single entry carries `functions`.
    fn with_functions(functions: &str) -> LootTable {
        table(
            "minecraft:test/bonus",
            &format!(
                r#"{{
                  "pools": [{{
                    "entries": [{{
                      "type": "minecraft:item",
                      "name": "minecraft:redstone",
                      "functions": [{functions}]
                    }}],
                    "rolls": 1.0
                  }}]
                }}"#
            ),
        )
    }

    fn pickaxe_with(enchantment: &str, level: u32) -> LootContext {
        LootContext {
            luck: 0.0,
            tool: Some(
                LootTool::new("minecraft:diamond_pickaxe".parse().unwrap())
                    .with_enchantment(enchantment.parse().unwrap(), level),
            ),
            explosion_radius: None,
            block_state: None,
        }
    }

    fn bare_pickaxe() -> LootContext {
        LootContext {
            luck: 0.0,
            tool: Some(LootTool::new("minecraft:diamond_pickaxe".parse().unwrap())),
            explosion_radius: None,
            block_state: None,
        }
    }

    /// `UniformBonusCount.calculateNewCount` is
    /// `count + random.nextInt(bonusMultiplier * level + 1)` — **unguarded on the
    /// level**, unlike `ore_drops`.
    ///
    /// So the exact predictions are: at level 0 the count is *always* `1` and
    /// **one draw happens anyway** (`nextInt(1)`); at level `L` with multiplier
    /// `M` the support is exactly `1..=1 + M*L`, uniformly. Both the ceiling and
    /// the *presence* of the top value are asserted, because a port that wrote
    /// `nextInt(M*L)` would silently lose the top value and one that wrote
    /// `nextInt(M*L + 1) + 1` would shift the whole support up by one.
    #[test]
    fn uniform_bonus_count_matches_the_records_unguarded_uniform_range() {
        let t = with_functions(
            r#"{ "function": "minecraft:apply_bonus",
                 "enchantment": "minecraft:fortune",
                 "formula": "minecraft:uniform_bonus_count",
                 "parameters": { "bonusMultiplier": 2 } }"#,
        );
        assert!(t.unsupported_features().is_empty());

        // Level 0: always 1, over many seeds. A degenerate exact prediction.
        for seed in 0..256u64 {
            let mut rng = SpawnRng::new(seed);
            let out = t.roll(&bare_pickaxe(), &mut rng);
            assert_eq!(out[0].count, 1, "nextInt(2*0 + 1) is always 0 (seed {seed})");
        }
        // Level 3, multiplier 2: support is exactly 1..=7.
        let mut seen = std::collections::BTreeSet::new();
        for seed in 0..8192u64 {
            let mut rng = SpawnRng::new(seed);
            let out = t.roll(&pickaxe_with("minecraft:fortune", 3), &mut rng);
            let count = out[0].count;
            assert!(
                (1..=7).contains(&count),
                "count {count} outside 1..=1 + 2*3 (seed {seed})"
            );
            seen.insert(count);
        }
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            (1..=7).collect::<Vec<u32>>(),
            "every value in the record's support must occur; a missing 7 means \
             `nextInt(M*L)` and a missing 1 means the range was shifted"
        );
    }

    /// **A tool present at level 0 still costs `uniform_bonus_count` one draw**,
    /// which is the draw-count claim the distribution above cannot make.
    ///
    /// The observable: roll the same table twice from the same seed, once with no
    /// tool and once with an unenchanted one, and then draw one more float from
    /// each stream. Equal counts (both 1) with **different** trailing draws is
    /// exactly "the formula ran and consumed a draw".
    #[test]
    fn a_present_tool_costs_uniform_bonus_count_a_draw_even_at_level_zero() {
        let t = with_functions(
            r#"{ "function": "minecraft:apply_bonus",
                 "enchantment": "minecraft:fortune",
                 "formula": "minecraft:uniform_bonus_count",
                 "parameters": { "bonusMultiplier": 1 } }"#,
        );

        let mut no_tool = SpawnRng::new(0xB0_1234);
        let a = t.roll(&LootContext::default(), &mut no_tool);
        let after_no_tool = no_tool.next_f64();

        let mut with_tool = SpawnRng::new(0xB0_1234);
        let b = t.roll(&bare_pickaxe(), &mut with_tool);
        let after_with_tool = with_tool.next_f64();

        assert_eq!(a[0].count, b[0].count, "level 0 adds nextInt(1) == 0 either way");
        assert_ne!(
            after_no_tool, after_with_tool,
            "`ApplyBonusCount.run` guards on `tool != null`, not on `level > 0`, \
             so an unenchanted tool must have consumed exactly one draw here"
        );
    }

    /// `BinomialWithBonusCount.calculateNewCount` loops `level + extra` times,
    /// adding one per `nextFloat() < probability`.
    ///
    /// The corpus's only instantiation is `{extra: 3, probability: 0.5714286}` on
    /// fortune (wheat, carrots, potatoes, beetroots seeds). So at level 0 the
    /// support is `1..=4` and the mean count is `1 + 3 × 0.5714286 = 2.714`; at
    /// level 3 it is `1..=7` with mean `1 + 6 × 0.5714286 = 4.429`. Both means are
    /// computed here from the record's own constants, and the assertion is on the
    /// **value**, not on "more with fortune".
    #[test]
    fn binomial_with_bonus_count_matches_the_records_predicted_mean_and_support() {
        let t = with_functions(
            r#"{ "function": "minecraft:apply_bonus",
                 "enchantment": "minecraft:fortune",
                 "formula": "minecraft:binomial_with_bonus_count",
                 "parameters": { "extra": 3, "probability": 0.5714286 } }"#,
        );
        assert!(t.unsupported_features().is_empty());

        const SAMPLES: u64 = 16_384;
        const P: f64 = 0.571_428_6;
        for level in [0u32, 3] {
            let rounds = f64::from(level) + 3.0;
            let predicted_mean = 1.0 + rounds * P;
            let max = 1 + level + 3;
            let mut total = 0u64;
            for seed in 0..SAMPLES {
                let mut rng = SpawnRng::new(seed);
                let out = t.roll(&pickaxe_with("minecraft:fortune", level), &mut rng);
                let count = out[0].count;
                assert!(
                    (1..=max).contains(&count),
                    "level {level}: count {count} outside 1..={max} — the loop runs \
                     level + extra = {rounds} times, each adding at most one"
                );
                total += u64::from(count);
            }
            let mean = total as f64 / SAMPLES as f64;
            // σ of the mean is sqrt(rounds·p·(1-p)/N) ≤ 0.0097, so 0.05 is 5σ.
            assert!(
                (mean - predicted_mean).abs() < 0.05,
                "level {level}: mean count {mean:.4}, the record predicts \
                 1 + {rounds} × {P} = {predicted_mean:.4}"
            );
        }
    }

    /// A `match_tool` predicate on `items` — the corpus's other shape (47 of 203),
    /// and the one that is **live today** because it needs no enchantment
    /// registry, only the held item's key.
    #[test]
    fn match_tool_items_predicate_tests_the_held_items_key() {
        let t = table(
            "minecraft:test/shears",
            r#"{
              "pools": [{
                "entries": [{
                  "type": "minecraft:alternatives",
                  "children": [
                    { "type": "minecraft:item", "name": "minecraft:oak_leaves",
                      "conditions": [ { "condition": "minecraft:match_tool",
                        "predicate": { "items": ["minecraft:shears", "minecraft:diamond_sword"] } } ] },
                    { "type": "minecraft:item", "name": "minecraft:stick" }
                  ]
                }],
                "rolls": 1.0
              }]
            }"#,
        );
        assert!(t.unsupported_features().is_empty());
        let with = |item: &str| {
            let ctx = LootContext {
                luck: 0.0,
                tool: Some(LootTool::new(item.parse().unwrap())),
                explosion_radius: None,
                block_state: None,
            };
            let mut rng = SpawnRng::new(3);
            t.roll(&ctx, &mut rng)[0].item.to_string()
        };
        assert_eq!(with("minecraft:shears"), "minecraft:oak_leaves");
        assert_eq!(with("minecraft:diamond_sword"), "minecraft:oak_leaves");
        assert_eq!(
            with("minecraft:diamond_axe"),
            "minecraft:stick",
            "an item outside the list must fail the predicate, not merely be present"
        );
        let mut rng = SpawnRng::new(3);
        assert_eq!(
            t.roll(&LootContext::default(), &mut rng)[0].item.to_string(),
            "minecraft:stick",
            "no tool at all fails `tool != null` first"
        );
    }

    /// `match_tool` with **no** `predicate` field is "is anything in hand", which
    /// is `true` for any tool — the one case where the absent-optional default is
    /// permissive rather than restrictive, and the opposite of what
    /// `Unsupported`'s fail-closed default would give.
    #[test]
    fn match_tool_with_no_predicate_is_tool_presence_alone() {
        let t = table(
            "minecraft:test/anything",
            r#"{
              "pools": [{
                "conditions": [ { "condition": "minecraft:match_tool" } ],
                "entries": [ { "type": "minecraft:item", "name": "minecraft:yes" } ],
                "rolls": 1.0
              }]
            }"#,
        );
        assert!(t.unsupported_features().is_empty());
        let mut rng = SpawnRng::new(1);
        assert_eq!(t.roll(&bare_pickaxe(), &mut rng).len(), 1);
        assert!(t.roll(&LootContext::default(), &mut rng).is_empty());
    }

    /// An unmodelled `match_tool` predicate shape must **fail closed** and be
    /// reported, not match everything. An item `#tag` is the corpus's one such
    /// case (`brush`'s sand table), and it is the reason that table cannot be
    /// bundled without an item-tag census.
    #[test]
    fn an_unmodelled_match_tool_predicate_fails_closed_and_is_reported() {
        for predicate in [
            r##"{ "items": "#minecraft:swords" }"##,
            r#"{ "components": { "minecraft:damage": 0 } }"#,
            r#"{ "predicates": { "minecraft:custom_data": {} } }"#,
        ] {
            let t = table(
                "minecraft:test/unmodelled",
                &format!(
                    r#"{{
                      "pools": [{{
                        "conditions": [ {{ "condition": "minecraft:match_tool", "predicate": {predicate} }} ],
                        "entries": [ {{ "type": "minecraft:item", "name": "minecraft:yes" }} ],
                        "rolls": 1.0
                      }}]
                    }}"#
                ),
            );
            assert_eq!(
                t.unsupported_features(),
                &["condition minecraft:match_tool (unmodelled predicate)".to_string()],
                "predicate {predicate} must be reported, so a table using it cannot \
                 slip into the curated bundle"
            );
            let mut rng = SpawnRng::new(1);
            assert!(
                t.roll(&bare_pickaxe(), &mut rng).is_empty(),
                "an unmodelled predicate must fail, never match everything"
            );
        }
    }

    /// `EnchantmentPredicate.containedIn`'s three branches, each of which is easy
    /// to collapse into the first one.
    #[test]
    fn the_enchantment_predicate_has_three_distinct_branches() {
        let gate = |predicate: &str| {
            let t = table(
                "minecraft:test/ench",
                &format!(
                    r#"{{
                      "pools": [{{
                        "conditions": [ {{ "condition": "minecraft:match_tool",
                          "predicate": {{ "predicates": {{ "minecraft:enchantments": [{predicate}] }} }} }} ],
                        "entries": [ {{ "type": "minecraft:item", "name": "minecraft:yes" }} ],
                        "rolls": 1.0
                      }}]
                    }}"#
                ),
            );
            assert!(t.unsupported_features().is_empty());
            move |ctx: &LootContext| {
                let mut rng = SpawnRng::new(1);
                !t.roll(ctx, &mut rng).is_empty()
            }
        };

        // Branch 1: a named enchantment at a level bound.
        let named = gate(r#"{ "enchantments": "minecraft:silk_touch", "levels": { "min": 1 } }"#);
        assert!(named(&pickaxe_with("minecraft:silk_touch", 1)));
        assert!(!named(&pickaxe_with("minecraft:fortune", 3)));
        assert!(
            !named(&bare_pickaxe()),
            "level 0 is `absent`, and `matchesEnchantment` returns false before \
             ever consulting the bound"
        );

        // Branch 2: no `enchantments`, but a level bound — *any* enchantment at
        // that level. A collapse into branch 1 makes this always false.
        let any_at_level = gate(r#"{ "levels": { "min": 3 } }"#);
        assert!(any_at_level(&pickaxe_with("minecraft:fortune", 3)));
        assert!(!any_at_level(&pickaxe_with("minecraft:fortune", 2)));
        assert!(!any_at_level(&bare_pickaxe()));

        // Branch 3: neither field — "is enchanted at all".
        let enchanted = gate("{}");
        assert!(enchanted(&pickaxe_with("minecraft:mending", 1)));
        assert!(!enchanted(&bare_pickaxe()));
    }

    /// `table_bonus` clamps its index to `chances.len() - 1`
    /// (`values.get(Math.min(level, values.size() - 1))`). A `values[level]`
    /// transliteration panics on a level above the list, which is reachable:
    /// Fortune's vanilla max is 3 but a command can set any level.
    #[test]
    fn table_bonus_clamps_a_level_above_its_chances_list() {
        let t = table(
            "minecraft:test/clamp",
            r#"{
              "pools": [{
                "conditions": [ { "condition": "minecraft:table_bonus",
                  "enchantment": "minecraft:fortune", "chances": [0.0, 1.0] } ],
                "entries": [ { "type": "minecraft:item", "name": "minecraft:yes" } ],
                "rolls": 1.0
              }]
            }"#,
        );
        let hits = |level: u32| {
            let ctx = pickaxe_with("minecraft:fortune", level);
            (0..64u64)
                .filter(|&seed| {
                    let mut rng = SpawnRng::new(seed);
                    !t.roll(&ctx, &mut rng).is_empty()
                })
                .count()
        };
        assert_eq!(hits(0), 0, "chances[0] = 0.0 never passes");
        assert_eq!(hits(1), 64, "chances[1] = 1.0 always passes");
        assert_eq!(hits(9), 64, "level 9 clamps to chances[1], it does not panic");
    }

    /// Test helper so a `LootTableBuilder` can be built from owned tables
    /// without threading a `&str` json round-trip.
    trait TapBuilder {
        fn push_table_owned(self, table: LootTable) -> Self;
    }

    impl TapBuilder for LootTableBuilder {
        fn push_table_owned(mut self, table: LootTable) -> Self {
            self.tables.push(table);
            self
        }
    }
    // ---------------------------------------------------------------------
    // `LootContextParams.EXPLOSION_RADIUS`. The parameter whose *absence* was
    // the defect: with it unset, `survives_explosion` passes unconditionally, so
    // a blast rolling an empty context drops every block it destroyed.
    // ---------------------------------------------------------------------

    /// One item, gated only by `survives_explosion` — the shape every ordinary
    /// block table has.
    const SURVIVES: &str = r#"{
      "type": "minecraft:block",
      "pools": [{
        "rolls": 1.0,
        "conditions": [{"condition": "minecraft:survives_explosion"}],
        "entries": [{"type": "minecraft:item", "name": "minecraft:cobblestone"}]
      }]
    }"#;

    /// **The magnitude gate**, and the whole point of the parameter. Two
    /// hypotheses, both computed from vanilla's own constants rather than from a
    /// prior run of this code:
    ///
    /// | hypothesis | expected survivors of 30,000 rolls |
    /// |---|---|
    /// | radius absent (the pre-fix behaviour) | **30,000** — the condition returns `true` with no draw |
    /// | radius `3.0` (a creeper's) | `1/3` of them, i.e. **10,000** |
    ///
    /// The two differ by 3×, so a direction-only assertion ("fewer blocks
    /// dropped") would be satisfied by any wrong probability. Both arms are
    /// asserted, the absent one **exactly** because it involves no randomness at
    /// all, and the present one inside a 1.5% band.
    #[test]
    fn survives_explosion_keeps_one_in_radius_and_all_of_them_with_no_radius() {
        const ROLLS: usize = 30_000;
        let t = table("minecraft:blocks/stone", SURVIVES);
        assert!(t.unsupported_features().is_empty());

        let survivors = |radius: Option<f32>, seed: u64| {
            let mut rng = SpawnRng::new(seed);
            let ctx = LootContext {
                luck: 0.0,
                tool: None,
                explosion_radius: radius,
                block_state: None,
            };
            (0..ROLLS).filter(|_| !t.roll(&ctx, &mut rng).is_empty()).count()
        };

        // The control, and it is exact rather than statistical: with the
        // parameter absent the condition never draws, so *every* roll survives.
        // This is the number a blast produced before the parameter existed.
        assert_eq!(
            survivors(None, 7),
            ROLLS,
            "with no EXPLOSION_RADIUS the condition must pass unconditionally"
        );

        // A creeper's radius. 1/3 of 30,000 is 10,000.
        let creeper = survivors(Some(3.0), 7);
        let expected = ROLLS / 3;
        let band = ROLLS / 66; // ~1.5% of the roll count
        assert!(
            creeper.abs_diff(expected) < band,
            "expected about {expected} survivors at radius 3.0, got {creeper} \
             (the no-radius hypothesis is {ROLLS})"
        );

        // A larger blast keeps proportionally fewer, which is what makes this a
        // function of the radius rather than a constant: 1/6 of 30,000 is 5,000.
        let big = survivors(Some(6.0), 7);
        let big_expected = ROLLS / 6;
        assert!(
            big.abs_diff(big_expected) < band,
            "expected about {big_expected} survivors at radius 6.0, got {big}"
        );
    }

    /// `ApplyExplosionDecay` thins a stack **item by item**, which is the whole
    /// difference between it and `survives_explosion` — one draw per item, not
    /// one per stack.
    ///
    /// A 9-item stack at radius `3.0` therefore averages `9/3 = 3` items, and the
    /// distribution must actually span (a per-stack implementation would yield
    /// only 0 or 9). Both properties are asserted: the mean lands on 3, and at
    /// least one roll produced a count that is neither.
    #[test]
    fn explosion_decay_thins_a_stack_item_by_item() {
        const ROLLS: usize = 4_000;
        let t = table(
            "minecraft:blocks/test",
            r#"{
              "type": "minecraft:block",
              "pools": [{
                "rolls": 1.0,
                "entries": [{
                  "type": "minecraft:item",
                  "name": "minecraft:wheat_seeds",
                  "functions": [
                    {"function": "minecraft:set_count", "count": 9.0},
                    {"function": "minecraft:explosion_decay"}
                  ]
                }]
              }]
            }"#,
        );
        assert!(t.unsupported_features().is_empty());

        let mut rng = SpawnRng::new(11);
        let ctx = LootContext {
            luck: 0.0,
            tool: None,
            explosion_radius: Some(3.0),
            block_state: None,
        };
        let mut total = 0u64;
        let mut intermediate = 0usize;
        for _ in 0..ROLLS {
            let out = t.roll(&ctx, &mut rng);
            let count = out.first().map_or(0, |s| s.count);
            total += u64::from(count);
            if count != 0 && count != 9 {
                intermediate += 1;
            }
        }
        let mean = total as f64 / ROLLS as f64;
        assert!(
            (mean - 3.0).abs() < 0.15,
            "9 items at 1/3 each averages 3.0, got {mean}"
        );
        assert!(
            intermediate > ROLLS / 2,
            "a per-item implementation must produce counts between 0 and 9; only \
             {intermediate} of {ROLLS} were, which is what a per-*stack* thinning \
             would look like"
        );

        // The control: with no radius the function is a no-op and every roll is
        // the full 9. Without this, a decay that always returned 0 or always
        // returned the input would be indistinguishable from a working one.
        let bare = LootContext::default();
        let mut rng = SpawnRng::new(11);
        for _ in 0..64 {
            assert_eq!(
                t.roll(&bare, &mut rng).first().map_or(0, |s| s.count),
                9,
                "with no EXPLOSION_RADIUS explosion_decay must not touch the stack"
            );
        }
    }

    /// A bare-handed break of `state`, i.e. the context `block_drops` builds.
    fn broken(state: &str) -> LootContext {
        LootContext {
            luck: 0.0,
            tool: None,
            explosion_radius: None,
            block_state: crate::block_drops::loot_block_state(state),
        }
    }

    /// `LootItemBlockStatePropertyCondition.test`'s three clauses, each isolated
    /// against a state that satisfies the other two.
    ///
    /// Every expectation comes from the record rather than from a roll:
    /// `state != null && state.is(block) && (properties.isEmpty() ||
    /// properties.matches(state))`, with `PropertyMatcher.match` opening
    /// `property != null &&`. So a wrong *block* fails even with matching
    /// properties, an absent `properties` object passes for any state of the right
    /// block, and a property the block does not carry fails rather than being
    /// skipped — the last being the clause a "just compare the pairs that are
    /// present" implementation gets backwards.
    #[test]
    fn block_state_property_tests_block_identity_then_each_named_property() {
        let condition = |properties: &str| {
            table(
                "minecraft:test/state",
                &format!(
                    r#"{{
                      "pools": [{{
                        "entries": [{{ "type": "minecraft:item", "name": "minecraft:wheat" }}],
                        "conditions": [{{
                          "condition": "minecraft:block_state_property",
                          "block": "minecraft:wheat"{properties}
                        }}],
                        "rolls": 1.0
                      }}]
                    }}"#
                ),
            )
        };
        let drops = |t: &LootTable, context: &LootContext| {
            let mut rng = SpawnRng::new(7);
            !t.roll(context, &mut rng).is_empty()
        };

        // `properties` absent: any state of the right block passes, no state of
        // the wrong block does, and an absent BLOCK_STATE parameter fails.
        let any_state = condition("");
        assert!(any_state.unsupported_features().is_empty());
        assert!(drops(&any_state, &broken("minecraft:wheat[age=0]")));
        assert!(drops(&any_state, &broken("minecraft:wheat[age=7]")));
        assert!(
            !drops(&any_state, &broken("minecraft:beetroots[age=3]")),
            "the record's `state.is(this.block)` clause is ANDed, so a table \
             applied to the wrong block must not pass"
        );
        assert!(
            !drops(&any_state, &LootContext::default()),
            "no BLOCK_STATE parameter is the `state != null` clause"
        );

        // Exact matcher.
        let age_seven = condition(r#", "properties": { "age": "7" }"#);
        assert!(drops(&age_seven, &broken("minecraft:wheat[age=7]")));
        assert!(!drops(&age_seven, &broken("minecraft:wheat[age=6]")));
        assert!(
            !drops(&age_seven, &broken("minecraft:wheat")),
            "a bare name is the default state (age=0), so it must fail because \
             zero is not seven — not because the string omitted the property"
        );

        // A property `minecraft:wheat` does not have. Vanilla's
        // `definition.getProperty(\"lit\")` is null, so the matcher is false.
        let no_such = condition(r#", "properties": { "lit": "true" }"#);
        assert!(
            !drops(&no_such, &broken("minecraft:wheat[age=7]")),
            "a matcher naming a property the block has not got must fail, not be \
             ignored"
        );

        // Every named property is ANDed (`matches` returns on the first miss).
        let two = condition(r#", "properties": { "age": "7", "lit": "true" }"#);
        assert!(!drops(&two, &broken("minecraft:wheat[age=7]")));
    }

    /// `RangedMatcher.match` compares in the **property's** ordering, not the
    /// serialized string's — `value.compareTo(typedMinValue)` after
    /// `property.getValue(min)`.
    ///
    /// The discriminating input is a bound that crosses a decimal-digit boundary:
    /// for `minecraft:candle`'s `candles` (1..=4) no such input exists, so this
    /// uses `minecraft:composter`'s `level` (0..=8) against a bound of `"10"`,
    /// where the numeric answer (`8 < 10`, no match) and the lexicographic answer
    /// (`"8" > "10"`, match) differ. A `min` of `"2"` is *not* a test: both
    /// readings agree there.
    #[test]
    fn a_ranged_state_matcher_compares_numerically_not_lexicographically() {
        let ranged = |bounds: &str| {
            table(
                "minecraft:test/ranged",
                &format!(
                    r#"{{
                      "pools": [{{
                        "entries": [{{ "type": "minecraft:item", "name": "minecraft:wheat" }}],
                        "conditions": [{{
                          "condition": "minecraft:block_state_property",
                          "block": "minecraft:composter",
                          "properties": {{ "level": {bounds} }}
                        }}],
                        "rolls": 1.0
                      }}]
                    }}"#
                ),
            )
        };
        let drops = |t: &LootTable, state: &str| {
            let mut rng = SpawnRng::new(7);
            !t.roll(&broken(state), &mut rng).is_empty()
        };

        let at_least_ten = ranged(r#"{ "min": "10" }"#);
        assert!(
            !drops(&at_least_ten, "minecraft:composter[level=8]"),
            "8 is not >= 10; a lexicographic compare would say \"8\" >= \"10\" and \
             pass, which is the whole point of this input"
        );
        let at_most_ten = ranged(r#"{ "max": "10" }"#);
        assert!(
            drops(&at_most_ten, "minecraft:composter[level=8]"),
            "8 is <= 10; a lexicographic compare would say \"8\" > \"10\" and fail"
        );
        // Inclusivity, and a bound below the value.
        assert!(drops(&ranged(r#"{ "min": "8" }"#), "minecraft:composter[level=8]"));
        assert!(drops(&ranged(r#"{ "max": "8" }"#), "minecraft:composter[level=8]"));
        assert!(!drops(&ranged(r#"{ "min": "9" }"#), "minecraft:composter[level=8]"));
        assert!(!drops(&ranged(r#"{ "max": "7" }"#), "minecraft:composter[level=8]"));
        // Both bounds, and an absent one being vacuous.
        assert!(drops(&ranged(r#"{ "min": "2", "max": "10" }"#), "minecraft:composter[level=8]"));
        assert!(!drops(&ranged(r#"{ "min": "2", "max": "7" }"#), "minecraft:composter[level=8]"));
        assert!(drops(&ranged("{}"), "minecraft:composter[level=8]"));
    }

    /// **The reported bug, and its control.** Breaking fully-grown wheat dropped
    /// one seed and no wheat, at every age.
    ///
    /// The expectations are read out of the bundle's own `blocks/wheat.json`
    /// (Mojang's generated data, copied verbatim — the corpus gate asserts it is
    /// byte-identical), not out of a roll:
    ///
    /// * pool 1 is an `alternatives` whose first child is `minecraft:wheat` gated
    ///   on `age: "7"` and whose second is an unconditional `wheat_seeds`, so the
    ///   pool yields **wheat at age 7 and a seed at every other age**;
    /// * pool 2 is a `wheat_seeds` entry gated at *pool* level on the same
    ///   condition, so it contributes **one more stack at age 7 and nothing
    ///   otherwise**. Its `apply_bonus` needs a tool (`ApplyBonusCount.run`
    ///   guards on `tool != null`), so bare-handed the second stack is exactly 1.
    ///
    /// `age=7` alone cannot distinguish a working condition from a hardcoded
    /// `true`, and `age=3` alone cannot distinguish one from the hardcoded `false`
    /// that shipped — so the pair is the test and neither half is dropped.
    #[test]
    fn fully_grown_wheat_drops_wheat_and_an_unripe_stalk_drops_one_seed() {
        let set = LootTableSet::load_bundled();
        let wheat: ResourceKey = "minecraft:blocks/wheat".parse().unwrap();
        let roll = |state: &str| {
            let mut rng = SpawnRng::new(42);
            describe(&set.roll(&wheat, &broken(state), &mut rng))
        };

        assert_eq!(
            roll("minecraft:wheat[age=7]"),
            vec![
                "minecraft:wheatx1".to_string(),
                "minecraft:wheat_seedsx1".to_string(),
            ],
            "at max age the alternatives takes its first child and pool 2 runs"
        );
        for age in 0..7 {
            assert_eq!(
                roll(&format!("minecraft:wheat[age={age}]")),
                vec!["minecraft:wheat_seedsx1".to_string()],
                "age {age} is not 7, so the alternatives falls through and pool 2 \
                 is skipped"
            );
        }

        // The control, and it is the shipped bug rather than a description of it:
        // an absent BLOCK_STATE makes `block_state_property` false for every
        // state, which is byte-for-byte what the hardcoded `false` did. If this
        // arm ever agreed with the one above, the gate would be proving nothing.
        let mut rng = SpawnRng::new(42);
        let blind = describe(&set.roll(&wheat, &LootContext::default(), &mut rng));
        assert_eq!(
            blind,
            vec!["minecraft:wheat_seedsx1".to_string()],
            "with no block state the roll must reproduce the reported bug exactly"
        );
        assert_ne!(
            blind,
            roll("minecraft:wheat[age=7]"),
            "the fixed arm and the blind arm must differ, or the state is not \
             reaching the condition"
        );
    }

    /// Fortune's bonus seeds: `apply_bonus` with
    /// `binomial_with_bonus_count(extra = 3, probability = 0.5714286)` on wheat's
    /// second pool.
    ///
    /// `BinomialWithBonusCount.calculateNewCount` is `count + Σ(nextFloat() <
    /// probability)` over `level + extra` rounds, so the **support** is computed
    /// from the JSON's own parameters rather than guessed: `1..=1 + (level + 3)`.
    /// At level 0 that is `1..=4` and at level 3 it is `1..=7`, and the presence
    /// of the top value is asserted as well as the ceiling — a port writing
    /// `rounds = extra` would cap at 4 for both levels and pass a range-only
    /// check at level 0.
    ///
    /// The wrong hypothesis is checked at the same input: bare-handed the function
    /// does not run at all (`tool != null`), so the count is *always* 1. That is
    /// what makes the tool arm a measurement rather than an observation.
    #[test]
    fn wheat_bonus_seeds_are_binomial_over_the_tables_own_extra() {
        let set = LootTableSet::load_bundled();
        let wheat: ResourceKey = "minecraft:blocks/wheat".parse().unwrap();
        let ripe = "minecraft:wheat[age=7]";

        let seed_counts = |context: &LootContext, seeds: std::ops::Range<u64>| {
            let mut seen = std::collections::BTreeSet::new();
            let mut out_of_range = Vec::new();
            for s in seeds {
                let mut rng = SpawnRng::new(s);
                let rolled = set.roll(&wheat, context, &mut rng);
                let count = rolled
                    .iter()
                    .find(|stack| stack.item.to_string() == "minecraft:wheat_seeds")
                    .map_or(0, |stack| stack.count);
                seen.insert(count);
                if count == 0 {
                    out_of_range.push((s, count));
                }
            }
            (seen, out_of_range)
        };

        // Bare hand: the function is skipped, so exactly 1 every time.
        let (bare, missing) = seed_counts(&broken(ripe), 0..512);
        assert!(missing.is_empty(), "every roll must produce seeds: {missing:?}");
        assert_eq!(
            bare.into_iter().collect::<Vec<_>>(),
            vec![1],
            "with no tool ApplyBonusCount.run returns early, so no bonus at all"
        );

        // Unenchanted tool, level 0: rounds = 0 + 3, support 1..=4.
        let with_tool = |level: u32| LootContext {
            luck: 0.0,
            tool: Some(if level == 0 {
                LootTool::new("minecraft:iron_hoe".parse().unwrap())
            } else {
                LootTool::new("minecraft:iron_hoe".parse().unwrap())
                    .with_enchantment("minecraft:fortune".parse().unwrap(), level)
            }),
            explosion_radius: None,
            block_state: crate::block_drops::loot_block_state(ripe),
        };
        let (level_zero, _) = seed_counts(&with_tool(0), 0..4096);
        assert_eq!(
            level_zero.into_iter().collect::<Vec<_>>(),
            vec![1, 2, 3, 4],
            "extra = 3 with a tool in hand gives exactly 1..=4, all reachable"
        );
        // Fortune 3: rounds = 3 + 3, support 1..=7. The ceiling moves with the
        // level, which is what separates `rounds = level + extra` from
        // `rounds = extra`.
        let (level_three, _) = seed_counts(&with_tool(3), 0..8192);
        assert_eq!(
            level_three.into_iter().collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7],
            "fortune 3 must reach 7; a port ignoring the level would stop at 4"
        );
    }

    /// The families beyond wheat, each with the exact bare-handed drop its own
    /// bundled JSON implies — and each a *different* shape of the same bug, which
    /// is why one crop is not a sufficient fixture.
    ///
    /// | family | what the condition gates | what a constant `false` produced |
    /// |---|---|---|
    /// | crop, `alternatives` (`wheat`, `beetroots`) | which child of pool 1 wins | the seed, never the crop |
    /// | crop, extra pool (`carrots`) | a *second* stack from pool 2 | one carrot instead of two |
    /// | `set_count` (`stone_slab`, `candle`) | the count of the one stack | always 1 |
    /// | whole pool (`white_bed`, `oak_door`, `cave_vines`) | whether anything drops | **nothing at all** |
    ///
    /// That last row is worth the table on its own: a `part=head` / `half=lower`
    /// gate means every bed and every door in the game dropped no item, which no
    /// amount of reasoning about "off by one branch" would have predicted.
    ///
    /// Mismatches are collected rather than asserted in the loop, so neutering the
    /// condition shows every family failing rather than only the alphabetically
    /// first.
    #[test]
    fn every_state_conditioned_family_drops_what_its_own_json_implies() {
        let set = LootTableSet::load_bundled();
        // `(state, expected)`. Every expectation is derived from that block's
        // `assets/loot_table/blocks/*.json`, read as a record.
        let cases: &[(&str, &[&str])] = &[
            // `alternatives`: crop at the final age, seed otherwise.
            ("minecraft:beetroots[age=3]", &["minecraft:beetrootx1", "minecraft:beetroot_seedsx1"]),
            ("minecraft:beetroots[age=2]", &["minecraft:beetroot_seedsx1"]),
            // Pool 1 is unconditional here, so the ripe case is *two* stacks of
            // one carrot and the unripe case is one — not crop-versus-seed.
            ("minecraft:carrots[age=7]", &["minecraft:carrotx1", "minecraft:carrotx1"]),
            ("minecraft:carrots[age=6]", &["minecraft:carrotx1"]),
            // `set_count` gated on the state: a double slab is two.
            ("minecraft:stone_slab[type=double]", &["minecraft:stone_slabx2"]),
            ("minecraft:stone_slab[type=bottom]", &["minecraft:stone_slabx1"]),
            ("minecraft:stone_slab[type=top]", &["minecraft:stone_slabx1"]),
            // Three chained `set_count`s, one per candle count above one.
            ("minecraft:candle[candles=4]", &["minecraft:candlex4"]),
            ("minecraft:candle[candles=3]", &["minecraft:candlex3"]),
            ("minecraft:candle[candles=2]", &["minecraft:candlex2"]),
            ("minecraft:candle[candles=1]", &["minecraft:candlex1"]),
            // Whole-drop gates. The `false` branch dropped nothing at all.
            ("minecraft:white_bed[part=head]", &["minecraft:white_bedx1"]),
            ("minecraft:white_bed[part=foot]", &[]),
            ("minecraft:oak_door[half=lower]", &["minecraft:oak_doorx1"]),
            ("minecraft:oak_door[half=upper]", &[]),
            ("minecraft:cave_vines[berries=true]", &["minecraft:glow_berriesx1"]),
            ("minecraft:cave_vines[berries=false]", &[]),
        ];

        let mut mismatches = Vec::new();
        for (state, expected) in cases {
            let id = crate::block_drops::block_loot_table_id(state).expect("state names a block");
            let mut rng = SpawnRng::new(42);
            let got = describe(&set.roll(&id, &broken(state), &mut rng));
            let want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
            if got != want {
                mismatches.push(format!("{state}: expected {want:?}, got {got:?}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} of {} state-conditioned drops are wrong:\n  {}",
            mismatches.len(),
            cases.len(),
            mismatches.join("\n  "),
        );
    }

    /// Sweet berries: two pools, `age=3` yielding `uniform 2..3` berries and
    /// `age=2` yielding `uniform 1..2`, both gated on the state — so an unripe
    /// bush drops nothing and the two ripe stages differ in *count*.
    ///
    /// The count is a range rather than a value, so the assertion is the range's
    /// endpoints computed from the JSON's own `min`/`max` **plus** the requirement
    /// that both endpoints occur: a provider that ignored `min` would still land
    /// inside `2..=3` about half the time.
    #[test]
    fn sweet_berries_count_comes_from_the_pool_its_age_selects() {
        let set = LootTableSet::load_bundled();
        let id: ResourceKey = "minecraft:blocks/sweet_berry_bush".parse().unwrap();
        let support = |state: &str| {
            let mut seen = std::collections::BTreeSet::new();
            for seed in 0..1024u64 {
                let mut rng = SpawnRng::new(seed);
                let rolled = set.roll(&id, &broken(state), &mut rng);
                seen.insert(rolled.iter().map(|s| s.count).sum::<u32>());
            }
            seen.into_iter().collect::<Vec<_>>()
        };
        assert_eq!(support("minecraft:sweet_berry_bush[age=3]"), vec![2, 3]);
        assert_eq!(support("minecraft:sweet_berry_bush[age=2]"), vec![1, 2]);
        assert_eq!(
            support("minecraft:sweet_berry_bush[age=1]"),
            vec![0],
            "neither pool's condition holds below age 2, so nothing drops"
        );
        assert_eq!(support("minecraft:sweet_berry_bush[age=0]"), vec![0]);
    }

    /// The instrument the curation gate could not be: how many bundled tables
    /// still carry a condition that parses and then ignores its own JSON.
    ///
    /// `unsupported_features` answers "did the parser recognise this", which is
    /// why a constant-`false` `block_state_property` was reported as fully
    /// supported across 154 tables. `context_blind_features` answers "is this
    /// *evaluated*", and the count is asserted exactly so that making one of them
    /// evaluable is a number that has to move rather than something a future
    /// reader discovers.
    ///
    /// **Non-zero is correct**, not a defect: each remaining entry names a loot
    /// context parameter this crate does not carry (the killing entity, the damage
    /// source, the level around the block).
    #[test]
    fn the_bundle_reports_which_conditions_are_recognised_but_not_evaluated() {
        let set = LootTableSet::load_bundled();
        let mut by_feature: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        let mut tables_with_any = 0usize;
        for table in set.iter() {
            let blind: std::collections::BTreeSet<&String> =
                table.context_blind_features().iter().collect();
            if !blind.is_empty() {
                tables_with_any += 1;
            }
            for feature in blind {
                *by_feature.entry(feature.clone()).or_default() += 1;
            }
        }
        assert_eq!(
            by_feature
                .iter()
                .map(|(f, n)| format!("{f} in {n} tables"))
                .collect::<Vec<_>>(),
            vec![
                "condition minecraft:damage_source_properties in 4 tables".to_string(),
                "condition minecraft:entity_properties in 25 tables".to_string(),
                "condition minecraft:location_check in 3 tables".to_string(),
            ],
            "if this moved, a condition became evaluable (or a new blind one \
             landed) — say which in the commit message"
        );
        assert_eq!(
            tables_with_any, 30,
            "30 of the 1,241 bundled tables carry at least one; the four counts \
             above come from walking assets/loot_table/**/*.json for the condition \
             ids, not from this accessor"
        );

        // And the thing that made this list necessary: `block_state_property` is
        // no longer on it, while still being in the bundle 154 times.
        assert!(
            !by_feature.contains_key("condition minecraft:block_state_property"),
            "block_state_property is evaluated now and must not be reported blind"
        );
        // And the connectedness half: for how many bundled block tables does
        // *some* state of that block roll differently from the blind context?
        //
        // The sweep is over every state of the block, not its default state,
        // because most of these gates are false in the default state — wheat's
        // default is `age=0`, a slab's is `type=bottom`, a bed's is `part=foot`.
        // A default-state-only sweep answers 31, which measures the defaults
        // rather than the wiring, and a threshold picked to look impressive next
        // to it would have been a guess.
        let mut states_by_block: std::collections::BTreeMap<&str, Vec<String>> =
            std::collections::BTreeMap::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            let Some(name) = lodestone_data::block_states::block_name(id) else {
                continue;
            };
            let properties = lodestone_data::block_states::properties(id).unwrap_or(&[]);
            let state = if properties.is_empty() {
                name.to_string()
            } else {
                let pairs: Vec<String> = properties
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                format!("{name}[{}]", pairs.join(","))
            };
            states_by_block.entry(name).or_default().push(state);
        }

        // Which tables carry the condition is read off the embedded JSON *text* —
        // Mojang's own bytes, the same thing a grep over `assets/loot_table` sees —
        // rather than off the parsed table, so the population under test is not
        // defined by the parser being tested.
        let carries: std::collections::BTreeSet<String> = EMBEDDED_LOOT
            .iter()
            .filter(|(_, raw)| raw.contains("minecraft:block_state_property"))
            .map(|(id, _)| format!("minecraft:{id}"))
            .collect();

        let mut reached = 0usize;
        let mut inert = Vec::new();
        for table in set.iter() {
            let id = table.id.to_string();
            if !carries.contains(&id) {
                continue;
            }
            let block = match id.strip_prefix("minecraft:blocks/") {
                Some(path) => format!("minecraft:{path}"),
                None => {
                    inert.push(id);
                    continue;
                }
            };
            let states = states_by_block
                .get(block.as_str())
                .unwrap_or_else(|| panic!("{block} is not in the block-state census"));
            let mut rng = SpawnRng::new(1);
            let blind = describe(&table.roll(&LootContext::default(), &mut rng));
            if states.iter().any(|state| {
                let mut rng = SpawnRng::new(1);
                describe(&table.roll(&broken(state), &mut rng)) != blind
            }) {
                reached += 1;
            } else {
                inert.push(id);
            }
        }
        assert_eq!(
            reached + inert.len(),
            154,
            "154 bundled tables carry a block_state_property condition, measured by \
             grepping assets/loot_table for the condition id"
        );
        // The six a bare-handed sweep cannot move, named rather than absorbed into
        // a threshold — three different reasons, and only two of them are
        // outstanding work:
        //
        // * `glow_lichen`, `sculk_vein` — the *entry* is gated on `match_tool`
        //   (shears, silk touch), so with no tool nothing is produced for the
        //   state-conditioned `set_count`s to add to. These are reached with the
        //   right tool in hand; the sweep is bare-handed, not the code blind.
        // * `snow` (`entity_properties`), `large_fern` and `tall_grass`
        //   (`location_check`) — the enclosing *pool* is gated on a condition that
        //   is still constant `false`, so no block state can rescue them. Real
        //   remaining wrongness, and `context_blind_features` above is what counts
        //   it.
        // * `harvest/sweet_berry_bush` — not keyed under `blocks/`, so
        //   `block_drops::block_loot_table_id` never resolves a broken block to it
        //   at all. It is the *bone-meal harvest* table, reached by a different
        //   action this crate does not model yet.
        assert_eq!(
            inert,
            vec![
                "minecraft:blocks/glow_lichen".to_string(),
                "minecraft:blocks/large_fern".to_string(),
                "minecraft:blocks/sculk_vein".to_string(),
                "minecraft:blocks/snow".to_string(),
                "minecraft:blocks/tall_grass".to_string(),
                "minecraft:harvest/sweet_berry_bush".to_string(),
            ],
            "a table entering or leaving this list is a real change in coverage"
        );
        assert_eq!(reached, 148);
    }
}
