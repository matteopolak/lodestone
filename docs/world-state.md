# World state: game rules, difficulty and the clock

## What it is

One shared, persistable store for the world's *scalars* —
`crates/lodestone-server/src/world_state.rs` — closing issues #327 (game rules),
#328 (difficulty) and #323 (time simulation). Those were the same defect three
times: **stored-and-broadcast is not enforced, and per-connection is not stored.**

| issue | what existed | what was wrong |
|---|---|---|
| #327 game rules | `game_rules.rs`: a typed registry with every 26.2 rule, its jar-checked default, `/gamerule`, and typed accessors | **the file was an orphan.** `game_rules.rs` and `commands.rs` were never declared as modules, so none of their ~1,300 lines was in the crate at all — including `game_rule_defaults_match_the_jar`. Meanwhile the live `SET_GAME_RULE` path wrote a separate, unvalidated, **per-connection** `HashMap<String, String>` |
| #328 difficulty | decode → store → confirm, with a real gate | stored on the same per-connection struct, and read by nothing |
| #323 world time | `SET_TIME` decoded, and a client's sky really moved | the **value** was `ticks_since(play_start)` — wall-clock elapsed since *this connection* joined. `tick.rs`'s real counter never reached the encoder |

#323 is the shape `cargo xtask connectedness` structurally cannot see: every link
green, wrong value on the wire. So the fix is not a new wire — it is making the
world own the clock and the broadcast read it.

## How it works

`WorldStateHandle` is `Arc<Mutex<WorldState>>`, shaped like `BlockEntityHandle`,
with **no `subscriber()`**: every clone is the same store, which is the point.

```text
IntegratedServer::open_in_memory_with_mobs_using
  -> WorldStateHandle::new()
       |-- clone -> serve_connection_with_mob_events_shared   (the connection)
       |-- move  -> run_tick_loop_with_weather                (the tick loop)
       '-- clone -> IntegratedServer::world_state()           (the host, persistence)
```

`serve_connection_inner` takes it as its last parameter; every pre-existing entry
point passes `&WorldStateHandle::default()` (the compatibility shape this file uses
for `block_ticks`/`explosions`/`weather`/`border`), so no off-limits call site
broke. The two `_shared` wrappers `IntegratedServer` uses carry the real one.

### The clock

`run_tick_loop` calls `tick_time()` once per world tick. **`game_time` always
advances; `day_time` advances only when `advance_time` is on** — vanilla's
`ServerLevel.tickTime`, where `gameTime` is unconditional and `setDayTime` is
gated. That asymmetry is the whole meaning of the rule: `/gamerule advance_time
false` freezes the sun without freezing anything measured in game ticks.

The periodic broadcast now sends `encode_set_time(game_time, Some(day_time))`. The
`Some` matters and is a deliberate change from the previous `None`: an empty
world-clock map means "keep the anchor you already have", and a client that keeps
its own anchor keeps advancing it — so a frozen server clock would still show a
moving sun.

### What is enforced, and where

Every accessor here has a named production reader. That is the invariant; a rule
with an accessor and no reader is the island this module exists to stop creating.

| rule / scalar | read at |
|---|---|
| `advance_time` | `WorldStateHandle::tick_time`, and the night-skip jump in `run_tick_loop` |
| `random_tick_speed` | `run_tick_loop`'s random-tick pass (issue #508 — the getter had waited for a reader) |
| `mob_griefing` | `run_tick_loop`'s graze drain |
| `block_drops` | `server.rs`'s block-break arm, vanilla's own gate site inside `Block.dropResources` |
| `mob_drops` | `MobSim::reap_dead`, via `set_mob_drops` (the sim is version-free and holds no world handle, so the loop hands it the flag) |
| **difficulty** | four readers now — see [what difficulty changes](#what-difficulty-changes) below |
| `spawn_mobs` | `run_tick_loop`'s natural-spawn cycle (issues #221/#222 — the getter had waited for a pass to gate; see [`natural-mob-spawning.md`](./natural-mob-spawning.md)) |

### What difficulty changes

The "stored and read by nothing" report is closed. Four production readers, in the
order a player notices them:

| behaviour | where | vanilla |
|---|---|---|
| **Peaceful evicts forbidden mobs** | `MobSim::remove_monsters`, from `run_tick_loop` | `Mob.checkDespawn`'s `difficulty == PEACEFUL && !getType().isAllowedInPeaceful()` |
| **Peaceful never proposes them** | `NaturalSpawner::set_difficulty`, checked in its cluster loop | `SpawnPlacements.checkSpawnRules`' first statement |
| **The starvation floor** | `food.rs`'s `starvation_allowed`, fed the real difficulty from `serve_play`'s hunger tick | `FoodData.tick`'s `health > 10 \|\| HARD \|\| (health > 1 && NORMAL)` |
| **Fire spread odds** | `FireEnv::overworld_in`, from `run_tick_loop`'s fire arm | `FireBlock.tick`'s `difficulty.getId() * 7` term |

Two things about the first two rows, because getting either wrong is silent:

* **The predicate is the per-type `notInPeaceful` flag, never `MobCategory ==
  MONSTER`.** `mob_spawn::allowed_in_peaceful` carries the 38 registrations from the
  pinned decompile. Seven `MONSTER` types are **not** in it — `piglin`, `shulker`,
  `ender_dragon`, `zombie_horse`, `zombie_nautilus`, `camel_husk`, `sulfur_cube` —
  and vanilla keeps all seven on Peaceful. The old code answered from a 22-name
  hostility list built for the *category* question, so a slime, magma cube,
  silverfish, phantom, vex, ravager, hoglin or warden survived Peaceful. Slimes
  really do spawn here, because the spawner models slime chunks.
* **Evicting is not a substitute for refusing.** The tick loop evicts *before* the
  spawn cycle, so a monster proposed on Peaceful lived one tick — long enough for
  the loop to publish its snapshot and the connection to send `ADD_ENTITY`, with
  `REMOVE_ENTITIES` the pass after. Monsters blinked. Vanilla refuses in both
  places and so do we.

Two parts of difficulty scaling are **not** modelled, and both are blocked rather
than forgotten:

* **Damage scaling.** Vanilla's mechanism is on the *receiving* side —
  `Player.hurtServer` scales the amount when `source.scalesWithDifficulty()`
  (Peaceful 0, Easy `min(d/2 + 1, d)`, Hard `d * 3/2`) — and `scalesWithDifficulty`
  is `false` for a `when_caused_by_living_non_player` type with no causing entity.
  Every player-damage path this crate has today (`fall`, `outside_border`, `drown`,
  `starve`, effect damage) is exactly that case, so implementing the scaling would
  change nothing until mob-on-player melee or blast damage to the player exists.
  The `scaling` column is already in `lodestone_data::damage_types`, so it is a
  table lookup once there is a hit to scale.
* **Regional difficulty** (`DifficultyInstance`). The formula is small and its
  inputs (world `game_time`, chunk inhabited time, moon phase) are all reachable
  from here, but its only vanilla consumers are `Mob.finalizeSpawn`'s geared/enchanted
  equipment, zombie reinforcements and mob potion effects — none of which are
  modelled — so building it now would be a computed number with no reader. Left
  deliberately; chunk inhabited time is not tracked either.

`keep_inventory` has an accessor and **no** reader, and that is recorded rather
than hidden: there is no death-drop path to keep an inventory through. Adding an
accessor without a decision point is not progress.

### Persistence

`level_data_fields()` / `load_level_data()` are an inverse pair using vanilla's own
`level.dat` names (`GameRules`, `Time`, `DayTime`, `difficulty_settings`), so a
world this server writes is readable by a real 26.2 server. `LevelDatHandle::write`
takes them as `extra` and merges them into the `Data` compound; the autosave and
the shutdown flush both pass them, and the open path loads them.

## How to change it

* **Enforcing another rule**: add the typed accessor in `game_rules.rs`, forward it
  on `WorldStateHandle`, and read it at the decision point. Finding the decision
  point is the work.
* **Persisting another scalar**: `level_data_fields` and `load_level_data` must stay
  inverse; `level_data_round_trips_rules_difficulty_and_the_clock` is the gate.

### Gotchas

* **`GameRules` in `level.dat` is a compound of *string* values**, even for an
  integer rule (`GameRules.java`'s `serialize`/`deserialize` go through `String`).
  An `Nbt::Int` there produces a file vanilla silently drops every rule from —
  pinned by `game_rules_persist_as_strings_even_for_an_integer_rule`.
* **A camelCase rule name is not a rule.** 26.2 renamed all of them and moved some
  concepts: `doDaylightCycle` → `advance_time`, `doTileDrops` → `block_drops`,
  `disableRaids` → `raids` with **inverted polarity**, and `doFireTick` is gone
  entirely (replaced by an *integer* `fire_spread_radius_around_player`). See
  `game_rules.rs`'s own table. `GameRules::set` **rejects** an unknown key rather
  than storing it, and `apply_game_rule_changed` confirms back only the entries it
  accepted — so a rejected key is visibly absent from the reply instead of being
  agreed with and then never read.
* **A locked difficulty refuses a change** and the confirmation still carries the
  *stored* value, so a refused request corrects the client's own UI.
* **The load races the join by one broadcast interval.** `load_level_data` runs
  after `open_in_memory_with_mobs_using` has already spawned the connection task, so
  a join may carry a zero clock for up to one second before the periodic broadcast
  corrects it. Moving the load earlier needs the store built outside that
  constructor, which is #300's own follow-up.

## What is still not wired

* **`commands.rs`'s `/gamerule` subtree.** The file now compiles and its tests run,
  but `ServerCommands::new()` still has no production constructor — that is #48's
  wiring, not this module's. The client's own game-rule UI (`SET_GAME_RULE`) is the
  live path and it works.
* **`advance_weather` and `players_sleeping_percentage`** are still
  constant-returning stubs in `tick.rs`. Both are real rules; neither has an
  accessor here yet.

## Dependencies

`game_rules` (the typed registry), `lodestone-core` (NBT), `lodestone-model`
(`Difficulty`). No protocol and no packet id — the encoders are a `ServerProtocol`
seam as always.
