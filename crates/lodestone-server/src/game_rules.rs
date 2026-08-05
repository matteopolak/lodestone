//! The world's game rules: one shared, typed, defaulted store (issue #327).
//!
//! # What it is
//!
//! [`GameRules`] is a typed registry of vanilla 26.2's game rules — every rule
//! with its real identifier, type and default, transcribed from the jar — plus
//! [`GameRulesHandle`], a cheap cloneable handle so **one** store is shared by
//! the world tick loop and every connection.
//!
//! # Two islands this closes, and they are different islands
//!
//! **Storage existed and was per-connection.** `crate::server`'s
//! `WorldAdminState` held a bare `HashMap<String, String>` constructed as a
//! stack local inside `serve_play`, which runs once per accepted socket. Its own
//! doc comment was honest that two LAN players would each hold a private,
//! divergent view. That is fixed by the *sharing* half of this module: the
//! handle is cloned, never split, so a rule set by one connection is the rule
//! every connection and the tick loop reads.
//!
//! **Enforcement did not exist at all.** A rule nothing consults is worse than
//! an absent rule, because it reads as connected: the round trip confirms back
//! to the client, so the wire looks green while behaviour never changes. The
//! call sites this module actually reaches are listed in
//! `docs/game-rules.md`; the two in the world tick loop are the load-bearing
//! ones, because they run with no connection attached.
//!
//! # The identifiers are snake_case in 26.2, and this is the trap
//!
//! Every rule was renamed. `.cache/mc/26.2/src/net/minecraft/world/level/`
//! `gamerules/GameRules.java:24-92` is the authoritative list and it reads
//! `random_tick_speed`, not `randomTickSpeed`; `spawn_mobs`, not
//! `doMobSpawning`; `keep_inventory`, not `keepInventory`; `immediate_respawn`,
//! not `doImmediateRespawn`. Three renames are worse than a rename, because the
//! *concept* moved too:
//!
//! | pre-26.2 name | 26.2 identifier | note |
//! |---|---|---|
//! | `doDaylightCycle` | `advance_time` | |
//! | `doWeatherCycle` | `advance_weather` | |
//! | `doMobSpawning` | `spawn_mobs` | `spawn_monsters`/`spawn_phantoms`/`spawn_patrols`/`spawn_wardens`/`spawn_wandering_traders` are now **separate** rules, not one |
//! | `naturalRegeneration` | `natural_health_regeneration` | |
//! | `spawnRadius` | `respawn_radius` | |
//! | `doTileDrops` | `block_drops` | |
//! | `doMobLoot` | `mob_drops` | |
//! | `doEntityDrops` | `entity_drops` | |
//! | `maxCommandChainLength` | `max_command_sequence_length` | |
//! | `disableRaids` | `raids` | **polarity inverted** — `disableRaids=true` is `raids=false` |
//! | `disableElytraMovementCheck` | `elytra_movement_check` | **polarity inverted** |
//! | **`doFireTick`** | **gone** | replaced by `fire_spread_radius_around_player`, an **integer** (default 128, min -1), not a boolean |
//!
//! A camelCase key is therefore not a rule this server knows, and [`GameRules::set`]
//! rejects it rather than storing it — see that method for why storing it would
//! be the worse failure.
//!
//! # How to change it
//!
//! * **Adding a rule vanilla has and this table lacks:** add a [`GameRuleSpec`]
//!   to [`GAME_RULES`], copying the default (and, for an integer, the `min`/`max`)
//!   from `GameRules.java`'s own `registerBoolean`/`registerInteger` call. Do not
//!   guess a default: `game_rule_defaults_match_the_jar` pins every one of them
//!   against a transcription of that file, so a guessed value fails there rather
//!   than silently shipping.
//! * **Enforcing a rule:** add a typed accessor here, then read it at the
//!   decision point. The accessor is the cheap half; finding the decision point
//!   is the work, and a rule with an accessor and no reader is exactly the island
//!   this module exists to stop creating. Every accessor below has a named
//!   production reader, and `docs/game-rules.md` tables them.
//! * **Do not split the handle per connection.** `crate::BlockTickFeed` has a
//!   `subscriber()` that deliberately splits, because its outbound queue is
//!   drain-all. This type is the opposite: sharing *is* the fix, and a
//!   per-connection clone of the inner `Arc` is what #327 was reported for.
//!
//! # Configuration
//!
//! No environment variable and no config file. Defaults come from [`GAME_RULES`];
//! a running world's overrides come from `SET_GAME_RULE` frames and from
//! `/gamerule` (`crate::commands`). Persistence to `level.dat` is **not** wired —
//! see `docs/game-rules.md`'s own gap note.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A rule's value, in the two shapes 26.2 actually has.
///
/// `GameRules.java` registers exactly two types (`registerBoolean`,
/// `registerInteger`); there is no float, string or enum rule, so this is a
/// closed set rather than a lossy simplification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameRuleValue {
    Bool(bool),
    Int(i32),
}

impl GameRuleValue {
    /// The value as the wire and `/gamerule` both spell it — `"true"`/`"false"`
    /// for a boolean, decimal for an integer, matching `GameRule::serialize`.
    #[must_use]
    pub fn serialize(&self) -> String {
        match self {
            Self::Bool(b) => b.to_string(),
            Self::Int(i) => i.to_string(),
        }
    }

    /// The boolean this holds, or `None` if it is an integer rule.
    ///
    /// Deliberately not "truthy": an integer rule asked for as a boolean is a
    /// programming error at the call site, not a value to coerce.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Int(_) => None,
        }
    }

    /// The integer this holds, or `None` if it is a boolean rule.
    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Bool(_) => None,
        }
    }
}

/// One rule's identity: its 26.2 identifier, its default, and (for an integer)
/// the inclusive range vanilla's own `IntegerArgumentType`/`Codec.intRange`
/// enforces.
///
/// `min`/`max` are `None` for a boolean rule. They matter: `random_tick_speed`
/// is `integer(0, MAX)`, so a negative value is rejected by vanilla at parse
/// time rather than clamped later, and `crate::random_tick`'s `tick_speed: u32`
/// depends on that having already happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameRuleSpec {
    /// The registry identifier, without the `minecraft:` namespace — the form
    /// `/gamerule` and the `SET_GAME_RULE` frame both carry.
    pub name: &'static str,
    /// The value a world that has never set this rule reads.
    pub default: GameRuleValue,
    /// Inclusive lower bound for an integer rule.
    pub min: Option<i32>,
    /// Inclusive upper bound for an integer rule.
    pub max: Option<i32>,
}

impl GameRuleSpec {
    const fn boolean(name: &'static str, default: bool) -> Self {
        Self { name, default: GameRuleValue::Bool(default), min: None, max: None }
    }

    const fn integer(name: &'static str, default: i32, min: i32, max: i32) -> Self {
        Self { name, default: GameRuleValue::Int(default), min: Some(min), max: Some(max) }
    }

    /// Parses `raw` as this rule's own type, applying the integer range.
    ///
    /// Mirrors `GameRule<T>::deserialize` plus the `ArgumentType` bound that
    /// guards it in vanilla — both, because vanilla applies the bound at two
    /// layers (`IntegerArgumentType.integer(min, max)` for `/gamerule`,
    /// `Codec.intRange(min, max)` for `level.dat`) and this crate has one entry
    /// point for both.
    pub fn parse(&self, raw: &str) -> Result<GameRuleValue, GameRuleError> {
        match self.default {
            GameRuleValue::Bool(_) => match raw {
                // Exactly `BoolArgumentType`'s two literals. Not
                // `str::parse::<bool>()` by accident — that happens to agree
                // today, and stating the accepted set here is what keeps a
                // future "1"/"yes" leniency from creeping in unnoticed.
                "true" => Ok(GameRuleValue::Bool(true)),
                "false" => Ok(GameRuleValue::Bool(false)),
                _ => Err(GameRuleError::BadValue {
                    rule: self.name,
                    expected: "true or false",
                }),
            },
            GameRuleValue::Int(_) => {
                let parsed: i32 = raw.parse().map_err(|_| GameRuleError::BadValue {
                    rule: self.name,
                    expected: "an integer",
                })?;
                let min = self.min.unwrap_or(i32::MIN);
                let max = self.max.unwrap_or(i32::MAX);
                if parsed < min || parsed > max {
                    return Err(GameRuleError::OutOfRange { rule: self.name, min, max });
                }
                Ok(GameRuleValue::Int(parsed))
            }
        }
    }
}

/// Why a game-rule write was refused.
///
/// A refusal is an ordinary outcome, not a connection error — the same
/// reasoning `crate::command::CommandResponse` gives for not being a `Result`
/// at the wire layer. Vanilla logs a warning and drops the write; this crate
/// returns the reason so `/gamerule` can *tell the player*, which vanilla's own
/// command layer also does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameRuleError {
    /// No rule with this identifier exists in 26.2.
    ///
    /// The overwhelmingly likely cause is a pre-26.2 camelCase name — see this
    /// module's rename table.
    Unknown { rule: String },
    /// The value did not parse as the rule's own type.
    BadValue { rule: &'static str, expected: &'static str },
    /// The value parsed but fell outside vanilla's own declared range.
    OutOfRange { rule: &'static str, min: i32, max: i32 },
}

impl std::fmt::Display for GameRuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { rule } => write!(f, "Unknown game rule '{rule}'"),
            Self::BadValue { rule, expected } => {
                write!(f, "Game rule '{rule}' expects {expected}")
            }
            Self::OutOfRange { rule, min, max } => {
                write!(f, "Game rule '{rule}' must be between {min} and {max}")
            }
        }
    }
}

impl std::error::Error for GameRuleError {}

/// Every game rule 26.2 has, with its real identifier and default.
///
/// Transcribed from `.cache/mc/26.2/src/net/minecraft/world/level/gamerules/`
/// `GameRules.java:24-92` — the `registerBoolean`/`registerInteger` calls, in
/// that file's own order. The defaults are pinned by
/// `game_rule_defaults_match_the_jar`, so this is a table with an external
/// oracle rather than a list someone remembered.
///
/// # Two transcription notes
///
/// * `advance_time` and `advance_weather` are registered as
///   `!SharedConstants.DEBUG_WORLD_RECREATE`, which is `false` in any non-debug
///   build, so both default `true`. The expression is recorded here rather than
///   the folded constant because the fold is the thing that could be wrong.
/// * `max_minecart_speed` is registered behind
///   `FeatureFlagSet.of(FeatureFlags.MINECART_IMPROVEMENTS)`, so vanilla omits
///   it from a world whose feature set lacks that flag. This crate has no
///   feature-flag model, so it is unconditionally present — a disclosed
///   simplification whose only effect is that `/gamerule` lists one rule a
///   vanilla server might not.
pub const GAME_RULES: &[GameRuleSpec] = &[
    GameRuleSpec::boolean("advance_time", true),
    GameRuleSpec::boolean("advance_weather", true),
    GameRuleSpec::boolean("allow_entering_nether_using_portals", true),
    GameRuleSpec::boolean("block_drops", true),
    GameRuleSpec::boolean("block_explosion_drop_decay", true),
    GameRuleSpec::boolean("command_blocks_work", true),
    GameRuleSpec::boolean("command_block_output", true),
    GameRuleSpec::boolean("drowning_damage", true),
    GameRuleSpec::boolean("elytra_movement_check", true),
    GameRuleSpec::boolean("ender_pearls_vanish_on_death", true),
    GameRuleSpec::boolean("entity_drops", true),
    GameRuleSpec::boolean("fall_damage", true),
    GameRuleSpec::boolean("fire_damage", true),
    GameRuleSpec::integer("fire_spread_radius_around_player", 128, -1, i32::MAX),
    GameRuleSpec::boolean("forgive_dead_players", true),
    GameRuleSpec::boolean("freeze_damage", true),
    GameRuleSpec::boolean("global_sound_events", true),
    GameRuleSpec::boolean("immediate_respawn", false),
    GameRuleSpec::boolean("keep_inventory", false),
    GameRuleSpec::boolean("lava_source_conversion", false),
    GameRuleSpec::boolean("limited_crafting", false),
    GameRuleSpec::boolean("locator_bar", true),
    GameRuleSpec::boolean("log_admin_commands", true),
    GameRuleSpec::integer("max_block_modifications", 32768, 1, i32::MAX),
    GameRuleSpec::integer("max_command_forks", 65536, 0, i32::MAX),
    GameRuleSpec::integer("max_command_sequence_length", 65536, 0, i32::MAX),
    GameRuleSpec::integer("max_entity_cramming", 24, 0, i32::MAX),
    GameRuleSpec::integer("max_minecart_speed", 8, 1, 1000),
    GameRuleSpec::integer("max_snow_accumulation_height", 1, 0, 8),
    GameRuleSpec::boolean("mob_drops", true),
    GameRuleSpec::boolean("mob_explosion_drop_decay", true),
    GameRuleSpec::boolean("mob_griefing", true),
    GameRuleSpec::boolean("natural_health_regeneration", true),
    GameRuleSpec::boolean("player_movement_check", true),
    GameRuleSpec::integer("players_nether_portal_creative_delay", 0, 0, i32::MAX),
    GameRuleSpec::integer("players_nether_portal_default_delay", 80, 0, i32::MAX),
    GameRuleSpec::integer("players_sleeping_percentage", 100, 0, i32::MAX),
    GameRuleSpec::boolean("projectiles_can_break_blocks", true),
    GameRuleSpec::boolean("pvp", true),
    GameRuleSpec::boolean("raids", true),
    GameRuleSpec::integer("random_tick_speed", 3, 0, i32::MAX),
    GameRuleSpec::boolean("reduced_debug_info", false),
    GameRuleSpec::integer("respawn_radius", 10, 0, i32::MAX),
    GameRuleSpec::boolean("send_command_feedback", true),
    GameRuleSpec::boolean("show_advancement_messages", true),
    GameRuleSpec::boolean("show_death_messages", true),
    GameRuleSpec::boolean("spawner_blocks_work", true),
    GameRuleSpec::boolean("spawn_mobs", true),
    GameRuleSpec::boolean("spawn_monsters", true),
    GameRuleSpec::boolean("spawn_patrols", true),
    GameRuleSpec::boolean("spawn_phantoms", true),
    GameRuleSpec::boolean("spawn_wandering_traders", true),
    GameRuleSpec::boolean("spawn_wardens", true),
    GameRuleSpec::boolean("spectators_generate_chunks", true),
    GameRuleSpec::boolean("spread_vines", true),
    GameRuleSpec::boolean("tnt_explodes", true),
    GameRuleSpec::boolean("tnt_explosion_drop_decay", false),
    GameRuleSpec::boolean("universal_anger", false),
    GameRuleSpec::boolean("water_source_conversion", true),
];

/// The identifier of the rule that gates world-time advance
/// (`GameRules.ADVANCE_TIME`) — pre-26.2's `doDaylightCycle`.
pub const ADVANCE_TIME: &str = "advance_time";
/// `GameRules.MOB_GRIEFING`.
pub const MOB_GRIEFING: &str = "mob_griefing";
/// `GameRules.RANDOM_TICK_SPEED`.
pub const RANDOM_TICK_SPEED: &str = "random_tick_speed";
/// `GameRules.SPAWN_MOBS` — pre-26.2's `doMobSpawning`.
pub const SPAWN_MOBS: &str = "spawn_mobs";
/// `GameRules.KEEP_INVENTORY`.
pub const KEEP_INVENTORY: &str = "keep_inventory";

/// Looks a rule's spec up by identifier, tolerating a `minecraft:` namespace.
///
/// The namespace is accepted because the rule *registry* is namespaced
/// (`Registry.register(BuiltInRegistries.GAME_RULE, id, ...)`) even though every
/// wire and command form carries the bare id, so both spellings name the same
/// rule and rejecting one would be an arbitrary difference from vanilla.
#[must_use]
pub fn spec(name: &str) -> Option<&'static GameRuleSpec> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    GAME_RULES.iter().find(|r| r.name == bare)
}

/// One world's game rules: the rules that have been explicitly set, over the
/// [`GAME_RULES`] defaults.
///
/// # Why overrides rather than a fully-populated map
///
/// Two reasons, and the second is the one that would have caused a regression.
/// A rule that has never been set reads its default either way, so the resolved
/// value is identical. But `REQUEST_GAMERULE_VALUES` replies with **exactly the
/// rules that were set** (`crate::server`'s `apply_client_command`), which
/// `request_game_rule_values_replies_even_with_no_rules_set` pins at zero
/// entries for a fresh world. Populating every rule up front would have turned
/// that reply into 59 entries and broken a gate that is testing something else
/// entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameRules {
    overrides: BTreeMap<&'static str, GameRuleValue>,
}

impl GameRules {
    /// A world at every rule's vanilla default, with nothing explicitly set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The resolved value of `name` — its override if one was set, otherwise its
    /// vanilla default. `None` only for an identifier no rule has.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<GameRuleValue> {
        let spec = spec(name)?;
        Some(self.overrides.get(&spec.name).copied().unwrap_or(spec.default))
    }

    /// Sets `name` from its wire/command string form, validating the identifier
    /// and the value against [`GAME_RULES`].
    ///
    /// Returns the parsed value on success.
    ///
    /// # Why an unknown key is rejected rather than stored
    ///
    /// The previous storage kept every `(String, String)` verbatim and
    /// unvalidated, which is worse than it looks: `randomTickSpeed` (the
    /// pre-26.2 spelling) would be *accepted*, *echoed back to the client*, and
    /// then never read by anything, because the reader asks for
    /// `random_tick_speed`. The player sees their rule confirmed and no
    /// behaviour change, with nothing anywhere reporting a problem — the exact
    /// failure mode of a rule that reads as connected and is not. Rejecting is
    /// what makes that visible.
    pub fn set(&mut self, name: &str, raw: &str) -> Result<GameRuleValue, GameRuleError> {
        let spec = spec(name).ok_or_else(|| GameRuleError::Unknown { rule: name.to_owned() })?;
        let value = spec.parse(raw)?;
        self.overrides.insert(spec.name, value);
        Ok(value)
    }

    /// Every rule explicitly set on this world, as the `(identifier, value)`
    /// string pairs [`crate::ServerProtocol::encode_game_rule_values`] takes.
    ///
    /// Sorted by identifier — `BTreeMap`, not `HashMap`, so the reply is
    /// deterministic and a test can assert a whole vector rather than a set.
    #[must_use]
    pub fn entries(&self) -> Vec<(String, String)> {
        self.overrides
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.serialize()))
            .collect()
    }

    /// Whether `name` has been explicitly set (as opposed to reading its
    /// default).
    ///
    /// Exists for the negative controls: a gate asserting a rule had no effect
    /// must be able to show the rule was actually stored, rather than silently
    /// dropped by [`set`](Self::set)'s validation.
    #[must_use]
    pub fn is_set(&self, name: &str) -> bool {
        spec(name).is_some_and(|s| self.overrides.contains_key(&s.name))
    }

    fn boolean(&self, name: &str) -> bool {
        self.get(name)
            .and_then(GameRuleValue::as_bool)
            .expect("a boolean rule constant always names a boolean rule in GAME_RULES")
    }

    /// `advance_time` — whether world time advances (pre-26.2
    /// `doDaylightCycle`). Read by `crate::server`'s periodic time broadcast.
    #[must_use]
    pub fn advance_time(&self) -> bool {
        self.boolean(ADVANCE_TIME)
    }

    /// `mob_griefing` — whether a mob may change the world. Read by
    /// `crate::tick::run_tick_loop`'s graze drain.
    #[must_use]
    pub fn mob_griefing(&self) -> bool {
        self.boolean(MOB_GRIEFING)
    }

    /// `spawn_mobs` — whether natural mob spawning runs. Read by
    /// `crate::tick::run_tick_loop`'s spawn pass.
    #[must_use]
    pub fn spawn_mobs(&self) -> bool {
        self.boolean(SPAWN_MOBS)
    }

    /// `keep_inventory` — whether a player keeps their items through death.
    #[must_use]
    pub fn keep_inventory(&self) -> bool {
        self.boolean(KEEP_INVENTORY)
    }

    /// `random_tick_speed` — random ticks per randomly-ticking section per tick.
    /// Read by `crate::tick::run_tick_loop`'s random-tick pass and handed
    /// straight to [`crate::RandomTickScheduler::tick_chunk`].
    ///
    /// `u32`, not `i32`, because that is what `tick_chunk` takes. The cast is
    /// lossless in one direction only, and it is safe here for a reason worth
    /// stating: the rule's own `min` is `0` (`GameRules.java:74`,
    /// `integer("random_tick_speed", ..., 3, 0)`), so [`GameRules::set`] has
    /// already rejected every negative value. The `max(0)` is a second layer,
    /// not the primary one.
    #[must_use]
    pub fn random_tick_speed(&self) -> u32 {
        self.get(RANDOM_TICK_SPEED)
            .and_then(GameRuleValue::as_int)
            .unwrap_or(0)
            .max(0) as u32
    }
}

/// A cheap, cloneable handle to **one** world's [`GameRules`].
///
/// Shaped like [`crate::BlockTickFeed`] — `Clone + Default`, an inner
/// `Arc<Mutex<_>>`, so every existing `serve_connection*` entry point can pass
/// one — with one deliberate difference: **there is no `subscriber()`**. Every
/// clone of this handle shares the same store, and that is the whole point of
/// issue #327's scope half. A per-connection store is the bug, not the
/// behaviour.
///
/// `Mutex`, not `RwLock`: every access here is a single map lookup or insert
/// under microseconds of contention from at most a handful of connection tasks
/// plus one tick task, and `Mutex` is what the rest of this crate's shared
/// handles already use (`BlockTickFeed`, `BlockEntityHandle`), so matching them
/// costs nothing and keeps one idiom.
#[derive(Debug, Clone, Default)]
pub struct GameRulesHandle(Arc<Mutex<GameRules>>);

impl GameRulesHandle {
    /// A handle to a fresh world at every rule's vanilla default.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` against the shared rules.
    ///
    /// **Synchronous by construction**, and that is a safety property rather
    /// than a limitation — the same one `crate::region_source::
    /// ScheduledTickHandle::with` relies on: a closure cannot contain an
    /// `.await`, so the compiler guarantees the `MutexGuard` never crosses a
    /// suspension point and the holding task stays `Send`.
    pub fn with<R>(&self, f: impl FnOnce(&mut GameRules) -> R) -> R {
        f(&mut self.0.lock().expect("game rules lock poisoned"))
    }

    /// [`GameRules::advance_time`] through the shared store.
    #[must_use]
    pub fn advance_time(&self) -> bool {
        self.with(|r| r.advance_time())
    }

    /// [`GameRules::mob_griefing`] through the shared store.
    #[must_use]
    pub fn mob_griefing(&self) -> bool {
        self.with(|r| r.mob_griefing())
    }

    /// [`GameRules::spawn_mobs`] through the shared store.
    #[must_use]
    pub fn spawn_mobs(&self) -> bool {
        self.with(|r| r.spawn_mobs())
    }

    /// [`GameRules::keep_inventory`] through the shared store.
    #[must_use]
    pub fn keep_inventory(&self) -> bool {
        self.with(|r| r.keep_inventory())
    }

    /// [`GameRules::random_tick_speed`] through the shared store.
    #[must_use]
    pub fn random_tick_speed(&self) -> u32 {
        self.with(|r| r.random_tick_speed())
    }

    /// Whether this handle and `other` name the same store.
    ///
    /// Exists for the sharing gate: "a rule set on one connection is visible on
    /// another" is the property #327 reported broken, and its negative control
    /// needs to be able to state that two handles really are (or really are
    /// not) the same store, rather than inferring it from a value that could
    /// have agreed by both being at the default.
    #[must_use]
    pub fn is_same_store(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The external oracle for [`GAME_RULES`]: every identifier, type and
    /// default, transcribed a second time from
    /// `.cache/mc/26.2/src/net/minecraft/world/level/gamerules/GameRules.java:24-92`
    /// as `(identifier, serialized default)`.
    ///
    /// This is a *second* transcription on purpose. It is not
    /// `decode(encode(x)) == x` — the expected values originate in the jar, not
    /// in the table under test — but two transcriptions by one author is the
    /// weak form of that evidence, so the gate below asserts **count** as well
    /// as content: a rule dropped from both lists in the same way is the one
    /// failure this shape cannot see, and the count makes a *silent* drop from
    /// one of them impossible.
    const JAR_DEFAULTS: &[(&str, &str)] = &[
        ("advance_time", "true"),
        ("advance_weather", "true"),
        ("allow_entering_nether_using_portals", "true"),
        ("block_drops", "true"),
        ("block_explosion_drop_decay", "true"),
        ("command_blocks_work", "true"),
        ("command_block_output", "true"),
        ("drowning_damage", "true"),
        ("elytra_movement_check", "true"),
        ("ender_pearls_vanish_on_death", "true"),
        ("entity_drops", "true"),
        ("fall_damage", "true"),
        ("fire_damage", "true"),
        ("fire_spread_radius_around_player", "128"),
        ("forgive_dead_players", "true"),
        ("freeze_damage", "true"),
        ("global_sound_events", "true"),
        ("immediate_respawn", "false"),
        ("keep_inventory", "false"),
        ("lava_source_conversion", "false"),
        ("limited_crafting", "false"),
        ("locator_bar", "true"),
        ("log_admin_commands", "true"),
        ("max_block_modifications", "32768"),
        ("max_command_forks", "65536"),
        ("max_command_sequence_length", "65536"),
        ("max_entity_cramming", "24"),
        ("max_minecart_speed", "8"),
        ("max_snow_accumulation_height", "1"),
        ("mob_drops", "true"),
        ("mob_explosion_drop_decay", "true"),
        ("mob_griefing", "true"),
        ("natural_health_regeneration", "true"),
        ("player_movement_check", "true"),
        ("players_nether_portal_creative_delay", "0"),
        ("players_nether_portal_default_delay", "80"),
        ("players_sleeping_percentage", "100"),
        ("projectiles_can_break_blocks", "true"),
        ("pvp", "true"),
        ("raids", "true"),
        ("random_tick_speed", "3"),
        ("reduced_debug_info", "false"),
        ("respawn_radius", "10"),
        ("send_command_feedback", "true"),
        ("show_advancement_messages", "true"),
        ("show_death_messages", "true"),
        ("spawner_blocks_work", "true"),
        ("spawn_mobs", "true"),
        ("spawn_monsters", "true"),
        ("spawn_patrols", "true"),
        ("spawn_phantoms", "true"),
        ("spawn_wandering_traders", "true"),
        ("spawn_wardens", "true"),
        ("spectators_generate_chunks", "true"),
        ("spread_vines", "true"),
        ("tnt_explodes", "true"),
        ("tnt_explosion_drop_decay", "false"),
        ("universal_anger", "false"),
        ("water_source_conversion", "true"),
    ];

    #[test]
    fn game_rule_defaults_match_the_jar() {
        assert_eq!(
            GAME_RULES.len(),
            JAR_DEFAULTS.len(),
            "the table and the jar transcription must cover the same rule count"
        );
        let fresh = GameRules::new();
        for (name, expected) in JAR_DEFAULTS {
            let value = fresh
                .get(name)
                .unwrap_or_else(|| panic!("GAME_RULES is missing the rule {name:?}"));
            assert_eq!(
                &value.serialize(),
                expected,
                "default for {name:?} disagrees with the jar"
            );
        }
    }

    /// The renames are the trap this module exists to make loud. A pre-26.2
    /// camelCase name must be **rejected**, not stored — see
    /// [`GameRules::set`]'s own doc comment for why storing it is the worse
    /// failure.
    #[test]
    fn pre_26_2_camel_case_names_are_rejected_rather_than_silently_stored() {
        let mut rules = GameRules::new();
        for stale in [
            "randomTickSpeed",
            "doMobSpawning",
            "keepInventory",
            "mobGriefing",
            "doDaylightCycle",
            "doFireTick",
            "doImmediateRespawn",
        ] {
            assert_eq!(
                rules.set(stale, "true"),
                Err(GameRuleError::Unknown { rule: stale.to_owned() }),
                "{stale:?} is not a 26.2 rule and must not be accepted"
            );
            assert!(!rules.is_set(stale), "{stale:?} must not be stored");
        }
        // The control: the 26.2 spelling of the same rules is accepted, so the
        // rejections above are about the *name* and not about `set` refusing
        // everything.
        assert!(rules.set("random_tick_speed", "6").is_ok());
        assert!(rules.set("spawn_mobs", "false").is_ok());
        assert!(rules.set("keep_inventory", "true").is_ok());
        assert!(rules.set("mob_griefing", "false").is_ok());
        assert!(rules.set("advance_time", "false").is_ok());
        assert!(rules.set("immediate_respawn", "true").is_ok());
        assert_eq!(rules.random_tick_speed(), 6);
        assert!(!rules.spawn_mobs());
        assert!(rules.keep_inventory());
        assert!(!rules.mob_griefing());
        assert!(!rules.advance_time());
    }

    /// `random_tick_speed`'s declared range is `integer(0, MAX)`, so a negative
    /// value is refused at the boundary rather than clamped downstream — which
    /// is what lets [`GameRules::random_tick_speed`] hand a `u32` to
    /// `tick_chunk` without the cast being able to wrap.
    #[test]
    fn an_integer_rule_enforces_its_declared_range() {
        let mut rules = GameRules::new();
        assert_eq!(
            rules.set("random_tick_speed", "-1"),
            Err(GameRuleError::OutOfRange {
                rule: "random_tick_speed",
                min: 0,
                max: i32::MAX
            })
        );
        // Refused means *not stored*, so the reader still sees the default.
        assert!(!rules.is_set("random_tick_speed"));
        assert_eq!(rules.random_tick_speed(), 3);

        // `max_snow_accumulation_height` is the one rule with a real upper
        // bound (`integer(..., 1, 0, 8)`), so it separates "range is enforced"
        // from "only the lower bound is enforced".
        assert!(rules.set("max_snow_accumulation_height", "8").is_ok());
        assert_eq!(
            rules.set("max_snow_accumulation_height", "9"),
            Err(GameRuleError::OutOfRange {
                rule: "max_snow_accumulation_height",
                min: 0,
                max: 8
            })
        );
    }

    #[test]
    fn a_boolean_rule_accepts_exactly_true_and_false() {
        let mut rules = GameRules::new();
        assert!(rules.set("keep_inventory", "true").is_ok());
        assert!(rules.set("keep_inventory", "false").is_ok());
        for bad in ["1", "yes", "TRUE", "", "3"] {
            assert_eq!(
                rules.set("keep_inventory", bad),
                Err(GameRuleError::BadValue {
                    rule: "keep_inventory",
                    expected: "true or false"
                })
            );
        }
    }

    /// The `minecraft:` namespace names the same rule, because the registry is
    /// namespaced even though every wire form is not.
    #[test]
    fn the_minecraft_namespace_names_the_same_rule() {
        let mut rules = GameRules::new();
        assert!(rules.set("minecraft:random_tick_speed", "0").is_ok());
        assert_eq!(rules.random_tick_speed(), 0);
        // Stored under the bare id, so the wire echo carries the bare form.
        assert_eq!(rules.entries(), vec![("random_tick_speed".to_owned(), "0".to_owned())]);
    }

    /// A fresh world reports **zero** set rules, which is what
    /// `request_game_rule_values_replies_even_with_no_rules_set` depends on —
    /// see [`GameRules`]'s own doc comment for why the store holds overrides
    /// rather than a fully-populated table.
    #[test]
    fn a_fresh_world_reports_no_set_rules_but_reads_every_default() {
        let rules = GameRules::new();
        assert_eq!(rules.entries(), Vec::<(String, String)>::new());
        assert_eq!(rules.random_tick_speed(), 3);
        assert!(rules.advance_time());
        assert!(rules.mob_griefing());
        assert!(rules.spawn_mobs());
        assert!(!rules.keep_inventory());
    }

    /// The scope half of #327: every clone of a handle is the **same** store.
    /// A per-connection store was the reported bug.
    #[test]
    fn every_clone_of_a_handle_shares_one_store() {
        let a = GameRulesHandle::new();
        let b = a.clone();
        assert!(a.is_same_store(&b));

        a.with(|r| r.set("random_tick_speed", "6")).expect("set");
        assert_eq!(b.random_tick_speed(), 6, "a clone must observe the write");

        // The control: two *independently constructed* handles are different
        // stores and do not see each other's writes. Without this, the
        // assertion above would also pass if `random_tick_speed` happened to
        // agree for an unrelated reason.
        let separate = GameRulesHandle::new();
        assert!(!a.is_same_store(&separate));
        assert_eq!(
            separate.random_tick_speed(),
            3,
            "an independent handle must still be at the default"
        );
    }
}
