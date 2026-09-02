//! World-level admin state the server reports to the client: the default spawn
//! point and the game-rule values.
//!
//! # What it is
//!
//! Two small folds that shared one property before this module existed — both
//! `ClientEvent::SpawnPositionChanged` and `ClientEvent::GameRulesChanged`
//! decoded correctly, were covered by protocol tests, and reached **nothing**.
//! `docs/plans/world-state.md` §R2 calls this cluster "world-level admin state",
//! which is where the module name comes from.
//!
//! They live together because they are the same *kind* of thing — a scalar the
//! server owns and the client only reads — and neither is per-entity. They are
//! nonetheless two independent types with two independent components, because
//! `lodestone_ecs::session` holds the invariant that exactly one system writes
//! each session component and a single merged component would put two unrelated
//! writers on one.
//!
//! # How it works
//!
//! ## Spawn point
//!
//! [`SpawnPoint`] is a straight record of the last
//! `ClientboundSetDefaultSpawnPositionPacket`. Every legacy family's packet
//! struct documents this as "setting the client's **compass target**", and that
//! is the consumer: `lodestone_render::item_render` lists `minecraft:compass`
//! among the item-model range properties that are *deliberately unsourced*
//! because the datum is not decoded. It is decoded — it just never arrived
//! anywhere. This is the fold that lets it be sourced.
//!
//! Note the `Option`: pre-report state is represented honestly rather than
//! guessed as the origin, the same convention
//! `lodestone_ecs::session::ServerDifficulty` documents. A compass that points at
//! `(0, 0)` because nothing has been reported is indistinguishable from one
//! correctly pointing at a spawn that really is at the origin, and the difference
//! matters for whether the needle should spin.
//!
//! ## Game rules
//!
//! [`GameRuleValues`] stores the raw wire strings in an ordered map, with typed
//! accessors over the top. It is **not** the typed server-side registry: that
//! is a *server-side* 59-rule table specified in `docs/plans/world-state.md`
//! and is not built. Storing raw strings and parsing at the accessor is the
//! honest client-side shape until that registry exists, and it means an unknown
//! or unparseable rule degrades to "not present" rather than to a wrong default.
//!
//! Two things about this packet family that are easy to get wrong, both recorded
//! in the plan (`docs/plans/world-state.md:127-154`):
//!
//! * **`GAME_RULE_VALUES` is request/response, not broadcast.** Its only vanilla
//!   send site is its own send-game-rule-values step, reachable solely via
//!   the client's own request-game-rule-values command. Nothing pushes
//!   rule *changes* to clients. So this fold's contents are whatever was last
//!   asked for, and a client that never asks has an empty table — which is why
//!   every accessor returns `Option` and no caller may treat absence as a
//!   default.
//! * **26.2 renamed most rules to snake_case**, and the renames are silent: the
//!   1.21 name simply is not present. `doDaylightCycle` is `advance_time`,
//!   `doImmediateRespawn` is `immediate_respawn`, `randomTickSpeed` is
//!   `random_tick_speed`. The [`rules`] module holds the ones the plan cites,
//!   so a caller never spells one by hand.
//!
//! # How to change it
//!
//! Adding a typed accessor: put the key constant in [`rules`] with its
//! vanilla default *in the doc comment only* — do
//! not bake the default into the accessor. Returning a default from an absent key
//! would erase the request/response distinction above and make "the server never
//! told us" look like "the server told us `false`".
//!
//! Parsing mirrors vanilla: booleans through the equivalent of
//! `Boolean.parseBoolean` (case-insensitive `"true"`, everything else `false`),
//! integers through `Integer.parseInt` with a parse failure treated as absent
//! (vanilla logs and keeps its default).
//!
//! # Dependencies
//!
//! `lodestone_model` only.

use lodestone_model::event::ClientEvent;
use lodestone_model::ids::{DimensionId, Identifier};
use lodestone_model::math::BlockPos;
use std::collections::BTreeMap;

/// The 26.2 game-rule keys this crate names.
///
/// 26.2 moved game rules to a registry
/// entries under vanilla's own game-rule registry, renamed to snake_case, 59 of
/// them, only `BOOL` and `INT` types. The renames are the trap: a 1.21 name is
/// not deprecated, it is simply absent, so a lookup with the old spelling returns
/// `None` forever and looks like a server that did not report the rule.
pub mod rules {
    /// `advance_time` — was `doDaylightCycle`. Default `true` in release builds.
    pub const ADVANCE_TIME: &str = "advance_time";
    /// `advance_weather` — was `doWeatherCycle`. Default `true` in release
    /// builds.
    pub const ADVANCE_WEATHER: &str = "advance_weather";
    /// `immediate_respawn` — was `doImmediateRespawn`. Default `false`.
    pub const IMMEDIATE_RESPAWN: &str = "immediate_respawn";
    /// `keep_inventory` — was `keepInventory`. Default `false`.
    pub const KEEP_INVENTORY: &str = "keep_inventory";
    /// `mob_griefing` — was `mobGriefing`. Default `true`.
    pub const MOB_GRIEFING: &str = "mob_griefing";
    /// `natural_health_regeneration` — was `naturalRegeneration`. Default
    /// `true`.
    pub const NATURAL_HEALTH_REGENERATION: &str = "natural_health_regeneration";
    /// `players_sleeping_percentage` — was `playersSleepingPercentage`. Default
    /// `100`.
    pub const PLAYERS_SLEEPING_PERCENTAGE: &str = "players_sleeping_percentage";
    /// `random_tick_speed` — was `randomTickSpeed`. Default `3`.
    pub const RANDOM_TICK_SPEED: &str = "random_tick_speed";
    /// `respawn_radius` — was `spawnRadius`. Default `10`.
    pub const RESPAWN_RADIUS: &str = "respawn_radius";
    /// `spawn_mobs` — was `doMobSpawning`. Default `true`.
    pub const SPAWN_MOBS: &str = "spawn_mobs";
}

/// The world's default spawn point, as the server last reported it.
///
/// `None` until `ClientboundSetDefaultSpawnPositionPacket` arrives. See the
/// module docs for why that is not defaulted to the origin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpawnPoint(pub Option<SpawnPointRecord>);

/// One reported spawn point.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnPointRecord {
    /// Dimension the spawn point is in.
    pub dimension: DimensionId,
    /// The spawn block.
    pub pos: BlockPos,
    /// Spawn yaw, degrees.
    pub angle: f32,
    /// Spawn pitch, degrees.
    pub pitch: f32,
}

impl SpawnPoint {
    /// Fold one event, returning whether it belonged to this aggregate.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::SpawnPositionChanged {
                dimension,
                pos,
                angle,
                pitch,
            } => {
                // Assigned as a whole record for the same reason
                // `ServerDifficulty` is: one packet reports all four fields
                // together, so there is no way to hold a stale angle next to a
                // fresh position.
                self.0 = Some(SpawnPointRecord {
                    dimension: dimension.clone(),
                    pos: *pos,
                    angle: *angle,
                    pitch: *pitch,
                });
                true
            }
            _ => false,
        }
    }

    /// The reported spawn block, if any.
    #[must_use]
    pub fn pos(&self) -> Option<BlockPos> {
        self.0.as_ref().map(|r| r.pos)
    }

    /// Whether a spawn point has been reported at all.
    ///
    /// A compass needs this: with no reported spawn, vanilla's needle has no
    /// target and spinning is correct behaviour rather than pointing north.
    #[must_use]
    pub fn is_reported(&self) -> bool {
        self.0.is_some()
    }
}

/// The server's game-rule values, keyed by rule identifier, holding raw wire
/// strings.
///
/// Ordered (`BTreeMap`) so that a debug dump and any UI listing are stable
/// between runs; the wire order carries no meaning.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GameRuleValues(pub BTreeMap<Identifier, String>);

impl GameRuleValues {
    /// Fold one event, returning whether it belonged to this aggregate.
    ///
    /// Reported rules are **merged**, not replaced. Vanilla's packet is a
    /// response to a request and may legitimately carry a subset, so dropping
    /// previously-known rules on each response would lose information the server
    /// never retracted.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::GameRulesChanged { values } => {
                for (key, value) in values {
                    self.0.insert(key.clone(), value.clone());
                }
                true
            }
            _ => false,
        }
    }

    /// The raw wire string for `key`, if the server reported it.
    ///
    /// `key` goes through [`Identifier`]'s `FromStr`, so a bare `"advance_time"`
    /// resolves as `minecraft:advance_time` — the [`rules`] constants are written
    /// bare for that reason. A `key` that is not a well-formed identifier at all
    /// returns `None` rather than panicking.
    #[must_use]
    pub fn raw(&self, key: &str) -> Option<&str> {
        let id: Identifier = key.parse().ok()?;
        self.0.get(&id).map(String::as_str)
    }

    /// `key` parsed as a boolean, mirroring `Boolean.parseBoolean`:
    /// case-insensitive `"true"` is `true` and **everything else is `false`**,
    /// including nonsense.
    ///
    /// `None` means the server did not report the rule — never "it is false".
    #[must_use]
    pub fn bool_rule(&self, key: &str) -> Option<bool> {
        self.raw(key).map(|v| v.eq_ignore_ascii_case("true"))
    }

    /// `key` parsed as an integer. `None` covers both "not reported" and "did
    /// not parse", matching vanilla's behaviour of logging a parse failure and
    /// keeping its own default rather than storing a sentinel.
    #[must_use]
    pub fn int_rule(&self, key: &str) -> Option<i32> {
        self.raw(key).and_then(|v| v.trim().parse::<i32>().ok())
    }

    /// Number of rules the server has reported.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no rule has been reported yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `immediate_respawn` — skip the death screen. `None` if unreported.
    #[must_use]
    pub fn immediate_respawn(&self) -> Option<bool> {
        self.bool_rule(rules::IMMEDIATE_RESPAWN)
    }

    /// `natural_health_regeneration`. `None` if unreported.
    #[must_use]
    pub fn natural_health_regeneration(&self) -> Option<bool> {
        self.bool_rule(rules::NATURAL_HEALTH_REGENERATION)
    }

    /// `random_tick_speed`. `None` if unreported.
    #[must_use]
    pub fn random_tick_speed(&self) -> Option<i32> {
        self.int_rule(rules::RANDOM_TICK_SPEED)
    }

    /// `players_sleeping_percentage`. `None` if unreported.
    #[must_use]
    pub fn players_sleeping_percentage(&self) -> Option<i32> {
        self.int_rule(rules::PLAYERS_SLEEPING_PERCENTAGE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(s: &str) -> Identifier {
        s.parse().expect("test identifier parses")
    }

    #[test]
    fn spawn_point_starts_unreported_and_records_the_whole_packet() {
        let mut sp = SpawnPoint::default();
        assert!(!sp.is_reported(), "pre-report state must be honest");
        assert_eq!(sp.pos(), None);

        assert!(sp.apply(&ClientEvent::SpawnPositionChanged {
            dimension: ident("minecraft:overworld"),
            pos: BlockPos::new(120, 64, -8),
            angle: 90.0,
            pitch: 0.0,
        }));
        assert!(sp.is_reported());
        assert_eq!(sp.pos(), Some(BlockPos::new(120, 64, -8)));
        let rec = sp.0.as_ref().expect("reported");
        assert!((rec.angle - 90.0).abs() < f32::EPSILON);
    }

    /// Negative control for `SpawnPoint::apply`'s `_ => false`.
    #[test]
    fn spawn_point_rejects_unrelated_events_and_changes_nothing() {
        let mut sp = SpawnPoint::default();
        assert!(!sp.apply(&ClientEvent::KeepAlive { id: 1 }));
        assert!(!sp.apply(&ClientEvent::WorldBorderSizeChanged { size: 10.0 }));
        assert!(!sp.is_reported(), "an unrelated event must not report a spawn");
    }

    #[test]
    fn game_rules_merge_rather_than_replace() {
        let mut g = GameRuleValues::default();
        assert!(g.is_empty());
        assert!(g.apply(&ClientEvent::GameRulesChanged {
            values: vec![
                (ident("minecraft:immediate_respawn"), "true".to_owned()),
                (ident("minecraft:random_tick_speed"), "7".to_owned()),
            ],
        }));
        assert_eq!(g.len(), 2);

        // A second, *partial* response must not drop `random_tick_speed`.
        assert!(g.apply(&ClientEvent::GameRulesChanged {
            values: vec![(ident("minecraft:immediate_respawn"), "false".to_owned())],
        }));
        assert_eq!(g.len(), 2, "a subset response must merge, not replace");
        assert_eq!(g.immediate_respawn(), Some(false), "and must overwrite");
        assert_eq!(g.random_tick_speed(), Some(7), "and must retain");
    }

    /// Vanilla's `Boolean.parseBoolean` semantics, including the part people get
    /// wrong: a value that is neither `"true"` nor `"false"` is `false`, not an
    /// error and not absent. The distinction that actually matters is between
    /// `Some(false)` (reported as something non-true) and `None` (never
    /// reported) — asserted at both ends so they cannot be conflated.
    #[test]
    fn bool_parsing_matches_java_and_absence_is_distinct_from_false() {
        let mut g = GameRuleValues::default();
        g.apply(&ClientEvent::GameRulesChanged {
            values: vec![
                (ident("minecraft:a"), "true".to_owned()),
                (ident("minecraft:b"), "TRUE".to_owned()),
                (ident("minecraft:c"), "false".to_owned()),
                (ident("minecraft:d"), "banana".to_owned()),
            ],
        });
        assert_eq!(g.bool_rule("minecraft:a"), Some(true));
        assert_eq!(g.bool_rule("minecraft:b"), Some(true), "case-insensitive");
        assert_eq!(g.bool_rule("minecraft:c"), Some(false));
        assert_eq!(
            g.bool_rule("minecraft:d"),
            Some(false),
            "Boolean.parseBoolean returns false for nonsense, it does not throw"
        );
        assert_eq!(
            g.bool_rule("minecraft:never_sent"),
            None,
            "absence must be distinguishable from false -- GAME_RULE_VALUES is \
             request/response, so an unreported rule is the normal case"
        );
    }

    #[test]
    fn int_parse_failure_reads_as_absent() {
        let mut g = GameRuleValues::default();
        g.apply(&ClientEvent::GameRulesChanged {
            values: vec![
                (ident("minecraft:random_tick_speed"), "not_a_number".to_owned()),
                (ident("minecraft:players_sleeping_percentage"), "50".to_owned()),
            ],
        });
        assert_eq!(g.random_tick_speed(), None, "parse failure is not a sentinel");
        assert_eq!(g.players_sleeping_percentage(), Some(50));
    }

    /// The 26.2 renames, controlled against a *plausible wrong* transliteration
    /// rather than against the literal 1.21 spellings.
    ///
    /// This distinction is the whole value of the test. `doDaylightCycle` contains
    /// capitals, which `Identifier`'s own path validation rejects, so asserting
    /// `raw("minecraft:doDaylightCycle") == None` would pass because the string is
    /// not a legal identifier — a control whose premise is false, and which would
    /// keep passing if the rename table were wrong in every entry. The mistake
    /// actually available to a maintainer is to snake_case the *old* names
    /// (`do_daylight_cycle`, `random_tick_speed` — note the second one happens to
    /// be right, which is exactly why guessing is unsafe). Those are well-formed
    /// identifiers, so they exercise a real lookup, and they must miss.
    #[test]
    fn renamed_rules_resolve_and_snake_cased_old_names_do_not() {
        let mut g = GameRuleValues::default();
        g.apply(&ClientEvent::GameRulesChanged {
            values: vec![
                (ident("minecraft:immediate_respawn"), "true".to_owned()),
                (ident("minecraft:random_tick_speed"), "3".to_owned()),
                (ident("minecraft:advance_time"), "true".to_owned()),
            ],
        });
        // The 26.2 spellings resolve.
        assert_eq!(g.bool_rule(rules::IMMEDIATE_RESPAWN), Some(true));
        assert_eq!(g.int_rule(rules::RANDOM_TICK_SPEED), Some(3));
        assert_eq!(g.bool_rule(rules::ADVANCE_TIME), Some(true));
        // Bare keys resolve through the `minecraft:` default namespace, which is
        // what makes the `rules` constants safe to write bare.
        assert_eq!(g.raw("advance_time"), Some("true"));

        // A snake_cased *old* name is a legal identifier and must still miss.
        // These are the real renames, so a lookup by the transliterated 1.21
        // name finds nothing.
        for wrong in [
            "minecraft:do_daylight_cycle", // real key: advance_time
            "minecraft:do_immediate_respawn", // real key: immediate_respawn
            "minecraft:do_weather_cycle",  // real key: advance_weather
            "minecraft:spawn_radius",      // real key: respawn_radius
            "minecraft:natural_regeneration", // real key: natural_health_regeneration
        ] {
            // Premise check: the control string must itself be a *valid*
            // identifier, or this assertion proves nothing about renaming.
            assert!(
                wrong.parse::<Identifier>().is_ok(),
                "{wrong} must be a well-formed identifier for this control to mean anything"
            );
            assert_eq!(
                g.raw(wrong),
                None,
                "{wrong} is a transliterated 1.21 name; 26.2 renamed it and does not alias it"
            );
        }
    }

    /// Negative control for `GameRuleValues::apply`'s `_ => false`.
    #[test]
    fn game_rules_reject_unrelated_events_and_change_nothing() {
        let mut g = GameRuleValues::default();
        assert!(!g.apply(&ClientEvent::KeepAlive { id: 1 }));
        assert!(!g.apply(&ClientEvent::SimulationDistanceChanged { distance: 4 }));
        assert!(g.is_empty());
    }
}
