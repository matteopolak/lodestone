//! One shared, persistable store for the world's *scalars*: game rules,
//! difficulty, and the clock (issues #327, #328, #323).
//!
//! # What it is
//!
//! [`WorldStateHandle`] is the world-scoped counterpart of
//! [`crate::BlockEntityHandle`]: a cheap clone, one store. It holds
//! [`crate::game_rules::GameRules`], the difficulty and its lock, and the two
//! halves of vanilla's clock (`gameTime` and `dayTime`).
//!
//! Three issues converge here because they were the same defect three times:
//! **stored-and-broadcast is not enforced, and per-connection is not stored.**
//!
//! | issue | what existed | what was wrong |
//! |---|---|---|
//! | #327 game rules | a typed `GameRules` registry with `/gamerule` and typed accessors | **zero production constructors** — `GameRulesHandle::new()` was called only under `#[cfg(test)]`, while the live `SET_GAME_RULE` path wrote a separate, unvalidated, **per-connection** `HashMap<String, String>` |
//! | #328 difficulty | decode → store → confirm, gated by a real test | stored on the same per-connection struct, and read by nothing |
//! | #323 world time | `SET_TIME` decoded, and a connected client's sky really moved | the **value** was `ticks_since(play_start)` — wall-clock elapsed since *this connection* joined. `tick.rs`'s real counter never reached the encoder |
//!
//! #323 is the shape `cargo xtask connectedness` structurally cannot see: every
//! link green, wrong value on the wire. So the fix is not a new wire — it is
//! making the world own the clock and the broadcast read it.
//!
//! # How it works
//!
//! `run_tick_loop` calls [`WorldStateHandle::tick_time`] once per world tick.
//! `game_time` always advances; `day_time` advances **only** when the
//! `advance_time` rule is on — vanilla's `ServerLevel.tickTime`, where
//! `gameTime` is unconditional and `setDayTime` is gated by the rule. That one
//! asymmetry is what makes `/gamerule advance_time false` freeze the sun without
//! freezing anything measured in game ticks.
//!
//! Every connection reads the same store, so a rule set by one LAN player is the
//! rule every player and the tick loop sees.
//!
//! # How to change it
//!
//! * **Enforcing another rule**: add the typed accessor in
//!   [`crate::game_rules`], forward it here, and read it at the decision point.
//!   The accessor is the cheap half — a rule with an accessor and no reader is
//!   the island this whole module exists to stop creating.
//! * **Persisting another scalar**: [`WorldStateHandle::level_data_fields`] and
//!   [`WorldStateHandle::load_level_data`] are the pair, and they must stay
//!   inverse. Both use vanilla's own `level.dat` field names
//!   (`GameRules`, `Time`, `DayTime`, `difficulty_settings`), so a world this
//!   server writes is readable by a real 26.2 server and vice versa.
//!
//! ## Gotchas
//!
//! * **`GameRules` in `level.dat` is a compound of *string* values**, even for an
//!   integer rule (`GameRules.java`'s `serialize`/`deserialize` go through
//!   `String`). Writing `Nbt::Int` there produces a file vanilla silently drops
//!   every rule from.
//! * **A locked difficulty refuses a change** ([`set_difficulty`](WorldStateHandle::set_difficulty)
//!   returns `false`), which vanilla enforces in `MinecraftServer.setDifficulty`'s
//!   `if (level.getLevelData().isDifficultyLocked())` guard. Applying it anyway
//!   and only *displaying* the lock is the failure that looks like it works.
//! * **The clock is `i64` and `day_time` is not reduced mod 24000.** Vanilla
//!   keeps a monotonically growing `dayTime` too and the client takes the
//!   remainder for rendering; truncating it here would break "how many days has
//!   this world existed".
//!
//! # Dependencies
//!
//! [`crate::game_rules`] for the typed registry, `lodestone-core` for the NBT
//! codec, `lodestone-model` for [`Difficulty`]. No protocol, no packet id.

use std::sync::{Arc, Mutex};

use lodestone_core::Nbt;
use lodestone_model::Difficulty;

use crate::game_rules::{GameRuleError, GameRuleValue, GameRules};

/// Ticks in one vanilla day (`Level.TICKS_PER_DAY`). The client takes
/// `day_time % 24000` to place the sun; nothing here needs to.
pub const TICKS_PER_DAY: i64 = 24_000;

/// The two halves of vanilla's clock, as [`WorldStateHandle::time`] reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorldTime {
    /// `gameTime` — total world ticks, **always** advancing. What a `SET_TIME`
    /// packet's monotonic first field carries.
    pub game_time: i64,
    /// `dayTime` — the day/night anchor, frozen while `advance_time` is off.
    pub day_time: i64,
}

/// The world's scalars, behind [`WorldStateHandle`].
///
/// **`PartialEq` but not `Eq`**, since the world spawn carries `f64`/`f32`
/// coordinates. Nothing here needs total equality; the derive existed only for
/// test convenience.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldState {
    rules: GameRules,
    difficulty: Difficulty,
    difficulty_locked: bool,
    time: WorldTime,
    /// The world spawn point, once something has resolved one. `None` means "not
    /// yet searched", which is what a brand-new world is — see
    /// [`WorldStateHandle::world_spawn`] for why the distinction matters.
    spawn: Option<crate::world_spawn::WorldSpawn>,
}

impl Default for WorldState {
    fn default() -> Self {
        Self {
            rules: GameRules::new(),
            // `LevelSettings.DEFAULT`'s difficulty.
            difficulty: Difficulty::Normal,
            difficulty_locked: false,
            time: WorldTime::default(),
            spawn: None,
        }
    }
}

/// A cheap, cloneable handle to **one** world's [`WorldState`].
///
/// Deliberately has no `subscriber()`: every clone shares the store, which is the
/// whole point (a per-connection copy is the bug #327 and #328 were reported for).
#[derive(Debug, Clone, Default)]
pub struct WorldStateHandle(Arc<Mutex<WorldState>>);

impl WorldStateHandle {
    /// A handle to a fresh world: every rule at its vanilla default, Normal
    /// difficulty unlocked, clock at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` against the shared state.
    ///
    /// **Synchronous by construction**, like [`crate::BlockEntityHandle::with`]: a
    /// closure cannot contain an `.await`, so the compiler guarantees the guard
    /// never crosses a suspension point.
    pub fn with<R>(&self, f: impl FnOnce(&mut WorldState) -> R) -> R {
        f(&mut self.0.lock().expect("world state lock poisoned"))
    }

    /// Whether this handle and `other` name the same store — for the sharing
    /// gate's negative control (two handles at the same default value look
    /// identical otherwise).
    #[must_use]
    pub fn is_same_store(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Advances the clock by one world tick and returns the new value.
    ///
    /// `game_time` always advances; `day_time` advances only when `advance_time`
    /// is on. See the module doc for why that asymmetry is the whole rule.
    pub fn tick_time(&self) -> WorldTime {
        self.with(|state| {
            state.time.game_time += 1;
            if state.rules.advance_time() {
                state.time.day_time += 1;
            }
            state.time
        })
    }

    /// The clock, without advancing it.
    #[must_use]
    pub fn time(&self) -> WorldTime {
        self.with(|state| state.time)
    }

    /// Overwrites `day_time` — a night skip (`crate::sleep`) or `/time set`.
    pub fn set_day_time(&self, day_time: i64) {
        self.with(|state| state.time.day_time = day_time);
    }

    /// The difficulty and whether it is locked.
    #[must_use]
    pub fn difficulty(&self) -> (Difficulty, bool) {
        self.with(|state| (state.difficulty, state.difficulty_locked))
    }

    /// Sets the difficulty, returning `false` if the world's difficulty is
    /// **locked** — vanilla's `MinecraftServer.setDifficulty` guard.
    pub fn set_difficulty(&self, difficulty: Difficulty) -> bool {
        self.with(|state| {
            if state.difficulty_locked {
                return false;
            }
            state.difficulty = difficulty;
            true
        })
    }

    /// Locks or unlocks the difficulty. Vanilla only ever *locks* (the button is
    /// one-way in the UI), but the packet carries a boolean, so both are honoured.
    pub fn set_difficulty_locked(&self, locked: bool) {
        self.with(|state| state.difficulty_locked = locked);
    }

    /// Sets one game rule from its wire/command string form, validating the
    /// identifier and value against [`crate::game_rules::GAME_RULES`].
    pub fn set_rule(&self, name: &str, raw: &str) -> Result<GameRuleValue, GameRuleError> {
        self.with(|state| state.rules.set(name, raw))
    }

    /// Every rule explicitly set on this world, as `(identifier, value)` strings —
    /// what `REQUEST_GAMERULE_VALUES` replies with.
    #[must_use]
    pub fn rule_entries(&self) -> Vec<(String, String)> {
        self.with(|state| state.rules.entries())
    }

    /// A snapshot of the whole rule set, for a caller that wants several reads
    /// without re-locking.
    #[must_use]
    pub fn rules(&self) -> GameRules {
        self.with(|state| state.rules.clone())
    }

    /// `advance_time` — whether the day/night clock moves.
    #[must_use]
    pub fn advance_time(&self) -> bool {
        self.with(|state| state.rules.advance_time())
    }

    /// `random_tick_speed` — random ticks per section per tick (issue #508).
    #[must_use]
    pub fn random_tick_speed(&self) -> u32 {
        self.with(|state| state.rules.random_tick_speed())
    }

    /// `spawn_mobs` — whether natural mob spawning runs.
    #[must_use]
    pub fn spawn_mobs(&self) -> bool {
        self.with(|state| state.rules.spawn_mobs())
    }

    /// `mob_griefing` — whether a mob may change the world.
    #[must_use]
    pub fn mob_griefing(&self) -> bool {
        self.with(|state| state.rules.mob_griefing())
    }

    /// `keep_inventory` — whether a player keeps their items through death.
    #[must_use]
    pub fn keep_inventory(&self) -> bool {
        self.with(|state| state.rules.keep_inventory())
    }

    /// `natural_health_regeneration` — whether a fed player heals over time. Read by
    /// `crate::server`'s hunger tick; see [`crate::game_rules::GameRules::natural_health_regeneration`]
    /// for what it does and does *not* gate.
    #[must_use]
    pub fn natural_health_regeneration(&self) -> bool {
        self.with(|state| state.rules.natural_health_regeneration())
    }

    /// `block_drops` — whether breaking a block drops anything.
    #[must_use]
    pub fn block_drops(&self) -> bool {
        self.with(|state| state.rules.block_drops())
    }

    /// `mob_drops` — whether a mob's death drops anything.
    #[must_use]
    pub fn mob_drops(&self) -> bool {
        self.with(|state| state.rules.mob_drops())
    }

    /// Whether natural mob spawning of *monsters* is allowed at this difficulty —
    /// `Peaceful` forbids it (`NaturalSpawner`'s `MobCategory.MONSTER` pass is
    /// skipped, and `ServerLevel.setDayTime`'s sibling `Mob.checkDespawn` removes
    /// the ones already alive).
    ///
    /// Difficulty's **first** real consumer: before this, nothing read the stored
    /// value at all, which is why #328 was "stored and broadcast, not enforced".
    #[must_use]
    pub fn monsters_may_spawn(&self) -> bool {
        self.with(|state| state.difficulty != Difficulty::Peaceful)
    }

    /// The world spawn point, or `None` if nothing has resolved one for this world
    /// yet.
    ///
    /// # Why this is stored on the world rather than derived per join
    ///
    /// `crate::world_spawn::find_initial_spawn` is a 121-iteration spiral over
    /// generated columns — vanilla's `MinecraftServer.setInitialSpawn`, which runs
    /// **once, at world creation**, and writes its answer to `level.dat`. This
    /// crate ran it in every connection's `ConfigurationFinished` arm instead, and
    /// read the persisted value back nowhere, so every join re-paid for the search
    /// and a `/setworldspawn` could never stick.
    ///
    /// So `None` is meaningful: it is "no search has happened", and the first join
    /// to a fresh world resolves it and calls
    /// [`set_world_spawn`](Self::set_world_spawn). Every later join and every later
    /// session reads it back through [`load_level_data`](Self::load_level_data).
    #[must_use]
    pub(crate) fn world_spawn(&self) -> Option<crate::world_spawn::WorldSpawn> {
        self.with(|state| state.spawn)
    }

    /// Records the world spawn — the first join's spiral result, or a
    /// `/setworldspawn`. Persisted on the next autosave through
    /// [`level_data_fields`](Self::level_data_fields).
    pub(crate) fn set_world_spawn(&self, spawn: crate::world_spawn::WorldSpawn) {
        self.with(|state| state.spawn = Some(spawn));
    }

    /// Forgets the world spawn, so the next join searches for one.
    ///
    /// The one caller is world *creation*: `LevelDat::for_new_world` writes a
    /// placeholder `spawn` compound (it has to write something, and it has no
    /// terrain to consult), and loading that placeholder back would suppress the
    /// spiral search forever. Clearing it after the load is what keeps a fresh
    /// world's first join the thing that actually finds the spawn.
    pub(crate) fn clear_world_spawn(&self) {
        self.with(|state| state.spawn = None);
    }

    /// The fields this world contributes to `level.dat`'s `Data` compound, using
    /// vanilla's own names so a real 26.2 server can read the world back.
    ///
    /// `difficulty_settings` is written whole (difficulty + lock) because that is
    /// how 26.2 nests them; the other three are flat.
    #[must_use]
    pub fn level_data_fields(&self) -> Vec<(String, Nbt)> {
        self.with(|state| {
            let mut fields = vec![
                (
                    "GameRules".to_owned(),
                    // Strings, even for an integer rule — see the module doc.
                    Nbt::Compound(
                        state
                            .rules
                            .entries()
                            .into_iter()
                            .map(|(name, value)| (name, Nbt::String(value)))
                            .collect(),
                    ),
                ),
                ("Time".to_owned(), Nbt::Long(state.time.game_time)),
                ("DayTime".to_owned(), Nbt::Long(state.time.day_time)),
                (
                    "difficulty_settings".to_owned(),
                    Nbt::Compound(vec![
                        (
                            "difficulty".to_owned(),
                            Nbt::String(difficulty_name(state.difficulty).to_owned()),
                        ),
                        (
                            "difficulty_locked".to_owned(),
                            Nbt::Byte(i8::from(state.difficulty_locked)),
                        ),
                    ]),
                ),
            ];
            // `spawn` is contributed only when there *is* one: a world whose spawn
            // has not been resolved yet must not overwrite the placeholder
            // `LevelDat::for_new_world` wrote with a zeroed compound, which is a
            // file that reads back as a real spawn at the origin.
            //
            // **The field is `spawn`, a nested compound — not the flat
            // `SpawnX`/`SpawnY`/`SpawnZ` ints.** Those are pre-1.21; 26.2 nests
            // them, and `lodestone_anvil::level_dat::Spawn` is the measured shape.
            // Writing the old names produces a world vanilla opens at the origin.
            if let Some(spawn) = state.spawn {
                fields.push((
                    "spawn".to_owned(),
                    Nbt::Compound(vec![
                        (
                            "pos".to_owned(),
                            Nbt::IntArray(vec![
                                spawn.pos.x.floor() as i32,
                                spawn.pos.y.floor() as i32,
                                spawn.pos.z.floor() as i32,
                            ]),
                        ),
                        ("pitch".to_owned(), Nbt::Float(spawn.pitch)),
                        (
                            "dimension".to_owned(),
                            Nbt::String("minecraft:overworld".to_owned()),
                        ),
                        ("yaw".to_owned(), Nbt::Float(spawn.yaw)),
                    ]),
                ));
            }
            fields
        })
    }

    /// Loads whatever of [`level_data_fields`](Self::level_data_fields) is present
    /// in a `level.dat` `Data` compound. **Total and non-failing**: a missing or
    /// malformed field leaves that scalar at its current value, so a world written
    /// by an older build (or by vanilla, with rules this server does not model)
    /// still loads. An unknown rule name is dropped by
    /// [`GameRules::set`](crate::game_rules::GameRules::set)'s own validation.
    pub fn load_level_data(&self, data: &Nbt) {
        let Nbt::Compound(fields) = data else { return };
        let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, v)| v);

        if let Some(Nbt::Compound(rules)) = field("GameRules") {
            self.with(|state| {
                for (name, value) in rules {
                    if let Nbt::String(raw) = value {
                        let _ = state.rules.set(name, raw);
                    }
                }
            });
        }
        self.with(|state| {
            if let Some(Nbt::Long(time)) = field("Time") {
                state.time.game_time = *time;
            }
            if let Some(Nbt::Long(day)) = field("DayTime") {
                state.time.day_time = *day;
            }
            // The nested 26.2 `spawn` compound. Read leniently: a world written by
            // a build before this field existed simply leaves `spawn` at `None` and
            // the next join searches, which is the correct degradation.
            if let Some(Nbt::Compound(spawn)) = field("spawn") {
                let sub = |name: &str| spawn.iter().find(|(key, _)| key == name).map(|(_, v)| v);
                if let Some(Nbt::IntArray(pos)) = sub("pos") {
                    if let [x, y, z] = pos[..] {
                        let angle = |name: &str| match sub(name) {
                            Some(Nbt::Float(value)) => *value,
                            _ => 0.0,
                        };
                        state.spawn = Some(crate::world_spawn::WorldSpawn {
                            pos: lodestone_model::Vec3::new(
                                f64::from(x),
                                f64::from(y),
                                f64::from(z),
                            ),
                            yaw: angle("yaw"),
                            pitch: angle("pitch"),
                        });
                    }
                }
            }
            if let Some(Nbt::Compound(settings)) = field("difficulty_settings") {
                for (key, value) in settings {
                    match (key.as_str(), value) {
                        ("difficulty", Nbt::String(name)) => {
                            if let Some(difficulty) = difficulty_from_name(name) {
                                state.difficulty = difficulty;
                            }
                        }
                        ("difficulty_locked", Nbt::Byte(locked)) => {
                            state.difficulty_locked = *locked != 0;
                        }
                        _ => {}
                    }
                }
            }
        });
    }
}

/// `Difficulty.getKey()` — the lowercase name `level.dat` stores.
fn difficulty_name(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Peaceful => "peaceful",
        Difficulty::Easy => "easy",
        Difficulty::Normal => "normal",
        Difficulty::Hard => "hard",
    }
}

fn difficulty_from_name(name: &str) -> Option<Difficulty> {
    match name {
        "peaceful" => Some(Difficulty::Peaceful),
        "easy" => Some(Difficulty::Easy),
        "normal" => Some(Difficulty::Normal),
        "hard" => Some(Difficulty::Hard),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock's one asymmetry, which is the whole of `advance_time`'s meaning:
    /// `game_time` counts every tick, `day_time` only counts while the rule is on.
    ///
    /// The expected values come from the record definition, not from our producer:
    /// `ServerLevel.tickTime` increments `gameTime` unconditionally and calls
    /// `setDayTime` only under `GameRules.RULE_DAYLIGHT`. So after `n` ticks with
    /// the rule off for `k` of them, `game_time == n` and `day_time == n - k`
    /// exactly — a gate asserting only "the sun stopped" is satisfied by a clock
    /// that froze both.
    #[test]
    fn advance_time_freezes_the_day_clock_and_not_the_game_clock() {
        let world = WorldStateHandle::new();
        for _ in 0..10 {
            world.tick_time();
        }
        assert_eq!(
            world.time(),
            WorldTime {
                game_time: 10,
                day_time: 10
            }
        );

        world.set_rule("advance_time", "false").expect("known rule");
        for _ in 0..7 {
            world.tick_time();
        }
        assert_eq!(
            world.time(),
            WorldTime {
                game_time: 17,
                day_time: 10
            },
            "game_time must keep counting while day_time freezes"
        );

        world.set_rule("advance_time", "true").expect("known rule");
        world.tick_time();
        assert_eq!(
            world.time(),
            WorldTime {
                game_time: 18,
                day_time: 11
            }
        );
    }

    /// A locked difficulty refuses a change. The control is the *unlocked* arm:
    /// without it, a `set_difficulty` that never worked at all would pass.
    #[test]
    fn a_locked_difficulty_refuses_a_change() {
        let world = WorldStateHandle::new();
        assert!(world.set_difficulty(Difficulty::Hard));
        assert_eq!(world.difficulty(), (Difficulty::Hard, false));

        world.set_difficulty_locked(true);
        assert!(!world.set_difficulty(Difficulty::Peaceful));
        assert_eq!(
            world.difficulty(),
            (Difficulty::Hard, true),
            "a locked world keeps its difficulty"
        );
    }

    /// Difficulty's first real reader: Peaceful forbids monster spawning.
    #[test]
    fn peaceful_forbids_monster_spawning() {
        let world = WorldStateHandle::new();
        assert!(world.monsters_may_spawn());
        assert!(world.set_difficulty(Difficulty::Peaceful));
        assert!(!world.monsters_may_spawn());
    }

    /// Every clone is the same store — the property both #327 and #328 were
    /// reported for, with `is_same_store` as the control (two fresh handles at the
    /// same default would otherwise look identical).
    #[test]
    fn every_clone_shares_one_store() {
        let a = WorldStateHandle::new();
        let b = a.clone();
        assert!(a.is_same_store(&b));
        a.set_rule("random_tick_speed", "7").expect("known rule");
        assert!(a.set_difficulty(Difficulty::Hard));
        assert_eq!(b.random_tick_speed(), 7);
        assert_eq!(b.difficulty().0, Difficulty::Hard);

        let separate = WorldStateHandle::new();
        assert!(!a.is_same_store(&separate));
        assert_eq!(separate.random_tick_speed(), 3, "an unrelated world is untouched");
    }

    /// Persistence round-trips through vanilla's own `level.dat` field names, and
    /// the pair is inverse.
    #[test]
    fn level_data_round_trips_rules_difficulty_and_the_clock() {
        let saved = WorldStateHandle::new();
        saved.set_rule("advance_time", "false").expect("known rule");
        saved.set_rule("random_tick_speed", "11").expect("known rule");
        assert!(saved.set_difficulty(Difficulty::Hard));
        saved.set_difficulty_locked(true);
        for _ in 0..500 {
            saved.tick_time();
        }

        let data = Nbt::Compound(saved.level_data_fields());
        let loaded = WorldStateHandle::new();
        loaded.load_level_data(&data);

        assert_eq!(loaded.random_tick_speed(), 11);
        assert!(!loaded.advance_time());
        assert_eq!(loaded.difficulty(), (Difficulty::Hard, true));
        assert_eq!(
            loaded.time(),
            WorldTime {
                game_time: 500,
                day_time: 0
            },
            "advance_time was off for all 500 ticks, so only game_time moved"
        );
        assert_eq!(loaded.rule_entries(), saved.rule_entries());
    }

    /// An integer rule must be stored as a **string** in `level.dat`; an `Nbt::Int`
    /// there is a file vanilla drops every rule from.
    #[test]
    fn game_rules_persist_as_strings_even_for_an_integer_rule() {
        let world = WorldStateHandle::new();
        world.set_rule("random_tick_speed", "9").expect("known rule");
        let fields = world.level_data_fields();
        let (_, rules) = fields
            .iter()
            .find(|(name, _)| name == "GameRules")
            .expect("GameRules field");
        let Nbt::Compound(entries) = rules else {
            panic!("GameRules must be a compound");
        };
        assert_eq!(
            entries,
            &vec![(
                "random_tick_speed".to_owned(),
                Nbt::String("9".to_owned())
            )]
        );
    }
    /// The world spawn round-trips through the **nested** 26.2 `spawn` compound,
    /// and its `None` state survives — which is the property the join path reads.
    ///
    /// Three separate claims, because getting any one wrong is silent:
    ///
    /// * A fresh world's spawn is `None`, and `level_data_fields` contributes **no**
    ///   `spawn` field at all. A zeroed compound would read back as a real spawn at
    ///   the origin, which is the worst possible answer for an ocean world.
    /// * A resolved spawn is written under `spawn` as `pos`/`yaw`/`pitch` — the
    ///   nested shape `lodestone_anvil::level_dat::Spawn` measured, not the
    ///   pre-1.21 flat `SpawnX`/`SpawnY`/`SpawnZ` ints.
    /// * Loading it back yields the same coordinates, so the spiral search does not
    ///   re-run on the next session.
    #[test]
    fn the_world_spawn_round_trips_and_absence_writes_no_field() {
        let saved = WorldStateHandle::new();
        assert_eq!(saved.world_spawn(), None, "a fresh world has not searched yet");
        assert!(
            !saved
                .level_data_fields()
                .iter()
                .any(|(name, _)| name == "spawn"),
            "an unresolved spawn must contribute no field, or the file reads back as \
             a real spawn at (0, 0, 0)"
        );

        saved.set_world_spawn(crate::world_spawn::WorldSpawn {
            pos: lodestone_model::Vec3::new(136.0, 71.0, -24.0),
            yaw: 0.0,
            pitch: 0.0,
        });
        let fields = saved.level_data_fields();
        let (_, spawn) = fields
            .iter()
            .find(|(name, _)| name == "spawn")
            .expect("a resolved spawn contributes the field");
        let Nbt::Compound(entries) = spawn else {
            panic!("spawn must be a compound, not {spawn:?}");
        };
        assert_eq!(
            entries.iter().find(|(k, _)| k == "pos").map(|(_, v)| v),
            Some(&Nbt::IntArray(vec![136, 71, -24])),
            "26.2 nests the position as an IntArray under `spawn`"
        );

        let loaded = WorldStateHandle::new();
        loaded.load_level_data(&Nbt::Compound(fields));
        let restored = loaded.world_spawn().expect("the spawn loads back");
        assert_eq!(restored.pos, lodestone_model::Vec3::new(136.0, 71.0, -24.0));

        // The control: a `Data` compound with no `spawn` field must leave the
        // loading world at `None` rather than at some default, or a world written by
        // an older build would silently spawn everyone at the origin.
        let older = WorldStateHandle::new();
        older.load_level_data(&Nbt::Compound(vec![(
            "Time".to_owned(),
            Nbt::Long(5),
        )]));
        assert_eq!(
            older.world_spawn(),
            None,
            "no spawn field must stay unresolved, so the next join searches"
        );
    }
}
