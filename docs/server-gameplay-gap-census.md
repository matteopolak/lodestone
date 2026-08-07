# Server-side gameplay gap census against vanilla 26.2

## What it is

An evidenced inventory of what the integrated server actually simulates, measured against
vanilla 26.2 — block drops, crafting, mob spawning, lighting, drowning, hunger, fluids,
fire, explosions, redstone, item entities, sound and particles. Every row carries a
verdict (**reached** / **orphaned** / **partial** / **absent**), what consumes it in
production, a `file:line` citation and an issue number, so a later reader can re-verify
rather than inherit. It is the server-side sibling of
[`worldgen-gap-census.md`](./worldgen-gap-census.md) and exists for the same reason: the
written record here goes stale silently, and this repo's dominant defect is the *island* —
code that is built, unit-tested green, and reached by nothing.

**Method.** Measured at `f54042bd` (2026-08-07) by grep and by reading the tree. For every
row the question asked was never "does the code exist" but "**what calls it in a running
server?**" — so each candidate was grepped for its *producer* tree-wide and every hit
classified as production, `#[cfg(test)]`, or under `tests/`. A subsystem counts as
**reached** only when a non-test caller traces up to one of these five entry points:

| entry point | file |
|---|---|
| `IntegratedServer::{open_in_memory_with_mobs, open_persistent_with_mobs, bind}` | `crates/lodestone-server/src/integrated.rs` |
| `serve_connection` / `serve_connection_inner` | `crates/lodestone-server/src/server.rs:1448` |
| `serve_play` (both variants) | `crates/lodestone-server/src/server.rs:3925`, `:4415` |
| `dispatch_play_packet` | `crates/lodestone-server/src/server.rs:3480` |
| `tick::run_tick_loop` / `run_tick_loop_with_weather` | `crates/lodestone-server/src/tick.rs:711`, `:775` |

Only `v770` implements `ServerProtocol`, so this is a `v770`-only census. No number here is
quoted from a doc or an issue body. Where a claim rests on absence, the grep that found
nothing is named so the detector can be re-run.

### Verdict key

| verdict | meaning |
|---|---|
| **reached** | implemented, and a non-test production caller drives it |
| **orphaned** | implemented, but every caller outside the crate's own tests is a test — the *island* |
| **partial** | some of it exists; the row says which parts |
| **absent** | no implementation |

---

## The four headline findings

1. **The server simulates a great deal and gives the player almost none of it.** Weather,
   sleep and the night-skip vote, brewing, furnaces, hoppers, composters, crop and sapling
   growth, leaf decay, redstone dust/torches/repeaters/comparators/observers, sheep grazing,
   creeper detonation, drowning, fall damage, the world border and player-to-player entity
   visibility are all **reached** and correct. What is missing is the ordinary survival
   loop: break a block and nothing drops, craft and the server never checks, and nothing
   ever makes a sound.

2. **The two most valuable findings are parity defects — fully-connected wires carrying the
   wrong value.** These are worse than absences because the feature looks done and
   `cargo xtask connectedness` returns green:
   - **Time is per-connection wall-clock since join, not the tick counter** (§7, [#323](https://github.com/matteopolak/lodestone/issues/323)). `SET_TIME` really does darken the sky. `tick.rs`'s real `game_tick` never reaches the encoder.
   - **A fall that ends in water banks its distance instead of cancelling it** (§6, [#534](https://github.com/matteopolak/lodestone/issues/534)), so the player takes the damage later, on dry land.
   - Two more: every served chunk carries all-`Missing` light (§8) and an empty heightmap NBT while working implementations sit unreachable.

3. **Islands are where the leverage is, and the pattern also applies to entry points.**
   Twelve subsystems are built, gated and reached by nothing. Four of them are stranded by a
   single fact: **`IntegratedServer::bind` has zero production callers**, so RCON, the query
   protocol and player-to-player visibility are unreachable in the shipping product
   ([#535](https://github.com/matteopolak/lodestone/issues/535)). Three more are stranded by
   the same shape one level down — every real constructor passes `CommandDispatch::none()`,
   `PluginChannelRegistry::default()` and `ResourcePackPushFeed::default()`, so **a real
   player's typed command is refused** ([#48](https://github.com/matteopolak/lodestone/issues/48)).
   "Wired into `IntegratedServer::bind`" reads as production wiring in a commit message and
   is not. Ranked in §12.

4. **Two independent, mutually unaware spawn engines exist**, each with its own
   incompatible `MobCategory` and its own separate `check_despawn` (§9). One has a caller
   shape waiting for it; the other has never had a consumer in any crate and should
   probably be deleted rather than finished.

---

## 1. What the owner asked about, in one table

| owner's words | verdict | one-line reason | issue |
|---|---|---|---|
| blocks dropping | **orphaned → in flight** | the loot roller, the item entity and the wire encoder all existed and none called the others; being joined now (§2) | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| crafting | **absent** | `PLACE_RECIPE` decodes to `Ignored`; the server trusts the client's claimed craft result (§3) | [#529](https://github.com/matteopolak/lodestone/issues/529) |
| bubbles when I go in the water | **reached** | fully wired end to end, with a pixel gate. Not a gap — see §5 | [#267](https://github.com/matteopolak/lodestone/issues/267) closed |
| mob spawning | **orphaned** | `mob_spawn.rs`'s cap engine has no driver; the only production spawn is a demo seeding (§9) | [#222](https://github.com/matteopolak/lodestone/issues/222), [#221](https://github.com/matteopolak/lodestone/issues/221), [#518](https://github.com/matteopolak/lodestone/issues/518) |
| lighting | **parity defect** | all-`Missing` light on the wire; a 1,105-line `LightEngine` is reachable only from the shell's own worldgen (§8) | [#517](https://github.com/matteopolak/lodestone/issues/517) |

**"Bubbles" was the surprise.** It was suspected to be an untracked island. It is neither
untracked ([#267](https://github.com/matteopolak/lodestone/issues/267),
[#60](https://github.com/matteopolak/lodestone/issues/60) and
[#390](https://github.com/matteopolak/lodestone/issues/390) are all closed, and
[#30](https://github.com/matteopolak/lodestone/issues/30) is titled "HUD animations and
air-supply bubbles") nor an island. All three legs are live; see §5.

---

## 2. Block drops, item entities and pickup

| feature | vanilla source of truth | verdict | consumed by | evidence | issue |
|---|---|---|---|---|---|
| loot-table parse + roll | datapack `loot_table/**.json` | **orphaned** | nothing | `crates/lodestone-server/src/loot.rs` (1,551 lines), `roll_loot` at `:370`. `grep -rn 'roll_loot\|LootTableResolver\|LootTableSet' --include='*.rs' crates/` → 3 hits: the `pub use` in `lib.rs:232` and two lines in `tests/loot_corpus.rs`. **Zero production consumers.** | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| bundled loot corpus | 1,113 block + 94 entity tables under `.cache/mc/26.2/src/data/minecraft/loot_table/` | **partial** | the drift gate | `crates/lodestone-server/assets/loot_table/` holds **5** block tables (`coal_ore`, `dirt`, `gravel`, `iron_ore`, `stone`) and **1** entity table (`zombie`). All 1,355 vanilla tables *parse* (`tests/loot_corpus.rs`); 6 are bundled. | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| block break → drop | `Block.popResource` (`Block.java:412-419`) | **in flight** | — | `apply_block_action`'s `StopDestroy` arm (`server.rs:2225-2239`) sets the cell to `AIR`, removes the block entity, and sends `encode_block_update`. No roll, no spawn. **A concurrent agent is closing this now** — `crates/lodestone-server/src/block_drops.rs` (559 lines) was uncommitted in the working tree at census time, and covers `block_loot_table_id`, `drop_block_loot` and `is_within_pickup_range`. No issue filed, to avoid duplicating it. | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| mob death → drop | `LivingEntity.dropFromLootTable` | **absent** | — | `MobSim::attack`'s kill path (`mobs.rs:2494-2538`) does `self.mobs.retain(...)` at `:2530` and nothing else. `grep -n 'crate::loot\|loot::' crates/lodestone-server/src/mobs.rs` → **0 hits**. `apply_attack` (`server.rs:3399-3420`) discards the `AttackOutcome`, so `killed` is never inspected. | [#272](https://github.com/matteopolak/lodestone/issues/272) |
| item entity lifecycle (age, despawn, pickup delay) | `ItemEntity.tick` | **reached** | `MobSim::tick` → `run_tick_loop` | `self.items.tick()` at `mobs.rs:2077-2082` | [#215](https://github.com/matteopolak/lodestone/issues/215) closed |
| item entity gravity / drag | `ItemEntity` | **reached** | same | `ItemMotion::tick`, `crates/lodestone-entity/src/item_entity.rs:306-321` | — |
| item entity **ground collision** | `ItemEntity` | **absent** | — | `ItemMotion.on_ground` is never assigned `true` anywhere in `mobs.rs`, so the bounce branch is unreachable and every dropped item free-falls forever. `MobSim` already owns a `ChunkWorld` the pathfinder queries every tick. | [#533](https://github.com/matteopolak/lodestone/issues/533) |
| item stack **merging** | `ItemEntity.mergeWithNeighbours` | **orphaned** | nothing | `ItemEntityRegistry::merge` (`item_entity.rs:237`) and `try_merge` (`:133`) have no call site outside their own unit tests | [#533](https://github.com/matteopolak/lodestone/issues/533) |
| item **pickup** | `Player.aiStep`'s inflated AABB (`Player.java:462`) | **in flight** | — | `can_be_picked_up` (`item_entity.rs:94`) had callers only in its own tests and `tests/projectile_and_item_registries.rs:183,190`, never against a player position; no `encode_take_item_entity` existed in the trait or in v770. The in-flight `block_drops.rs` adds the geometry (`is_within_pickup_range` at `:291`). The client half has been finished for a while: `net.rs:793` decodes `take_item_entity`, `entities.rs:1111` runs the fly-to-collector animation. | — |
| block-break validation (hardness, tool, range) | `ServerPlayerGameMode.incrementDestroyProgress` | **absent** | — | `apply_block_action`'s own doc (`server.rs:2171-2177`): "no hardness/timing validation … and no interaction-range or spawn-protection checks". `lodestone_data::hardness::hardness()` (`crates/lodestone-data/src/hardness.rs:59`) is read by `crates/lodestone-data/src/tool.rs:200` — the *client's* break-time predictor — and by nothing in `lodestone-server`. | [#531](https://github.com/matteopolak/lodestone/issues/531) |

**The shape worth noticing.** Three finished pieces sat one function call apart from each
other for an entire session: a 1,551-line loot roller with a 1,355-table oracle gate, a
server-side item entity already streamed to every client by `snapshots()`, and a client that
already animates pickups. Nothing consumed anything. This is the island pattern at its most
expensive — every part individually gated green, zero player-visible result.

---

## 3. Crafting and the container-click trust boundary

| feature | verdict | evidence | issue |
|---|---|---|---|
| server resolves a recipe | **absent** | `grep -n 'recipe' crates/lodestone-server/src/protocol.rs` → **0 hits**. No `ServerBound` recipe variant, no `encode_update_recipes`. | [#529](https://github.com/matteopolak/lodestone/issues/529) |
| `PLACE_RECIPE` | **absent** | decodes and is discarded: `crates/protocol/v770/src/server_protocol.rs:2231-2234`, `let _ = decode_full::<…>(payload); ServerBound::Ignored` | [#529](https://github.com/matteopolak/lodestone/issues/529), [#266](https://github.com/matteopolak/lodestone/issues/266) |
| container click | **partial, by design** | `apply_container_clicked` (`server.rs:3738-3749`) applies the client's own predicted per-slot diff rather than re-deriving `doClick`. Documented at `protocol.rs:402-436`. Fine for window 0; for a crafting grid it means the client decides what it crafted. | [#529](https://github.com/matteopolak/lodestone/issues/529) |
| client-side matcher | **orphaned** | `crates/lodestone-game/src/{recipe,recipe_json,menus}.rs` hold a real matcher, the `load_data_root` datapack loader and an indexed lookup. `RecipeBook::predicted_craft_result` (`menus.rs:673`) has **no callers anywhere in the repo**. | — |
| crafting stations (anvil, loom, smithing, grindstone, enchanting) | **absent** server-side | `find crates/lodestone-server/src -iname '*anvil*' -o -iname '*loom*' -o -iname '*smithing*' -o -iname '*enchant*' -o -iname '*grindstone*'` → nothing. Only client menu *screens* exist (`ab2a3b06`). | [#150](https://github.com/matteopolak/lodestone/issues/150), [#253](https://github.com/matteopolak/lodestone/issues/253), [#254](https://github.com/matteopolak/lodestone/issues/254), [#255](https://github.com/matteopolak/lodestone/issues/255) |

The matcher is version-free game logic, not shell code, so the server can call the same one.
Do not write a second one.

### Serverbound packets that decode and are then discarded

Every variant in the `ServerBound` enum (`protocol.rs:180-639`) **does** have a real arm in
`dispatch_play_packet`. The stranding happens one file upstream, in v770's `decode()`, which
maps these wire packets straight to `ServerBound::Ignored` so they never become a distinct
variant at all — a two-file join, not a one-file scan. Line numbers in
`crates/protocol/v770/src/server_protocol.rs`:

`INTERACT` (right-click entity) `:2113`, `SWING` `:2125`, `USE_ITEM` (eat/drink) `:2129`,
`SPECTATOR_ACTION` `:2156`, `TELEPORT_TO_ENTITY` `:2166`, `CONTAINER_BUTTON_CLICK` `:2191`,
`CONTAINER_SLOT_STATE_CHANGED` `:2195`, `PLACE_RECIPE` `:2231`,
`RECIPE_BOOK_CHANGE_SETTINGS` `:2235`, `RECIPE_BOOK_SEEN_RECIPE` `:2239`, `SELECT_TRADE`
`:2243`, `SET_BEACON` `:2251`, `EDIT_BOOK` `:2263`, `SIGN_UPDATE` `:2272`, `RENAME_ITEM`
`:2276`, `PICK_ITEM_FROM_BLOCK` `:2280`, `PICK_ITEM_FROM_ENTITY` `:2284`.

`SWING` and `USE_ITEM` are the two most consequential: without them the server sees no arm
swing to relay to other players and no eat/drink/bow-draw at all.

---

## 4. Blocks the server owns as interactive state

| feature | verdict | consumed by | evidence | issue |
|---|---|---|---|---|
| redstone dust, torches, repeaters, comparators, observers | **reached** | `tick.rs:1101-1129` from `run_tick_loop` | live-verified over RCON against a real 26.2 server: `redstone_diode_oracle_gate.rs` (692 lines), `redstone_placement_gate.rs` | [#315](https://github.com/matteopolak/lodestone/issues/315) closed here |
| doors/trapdoors/fence gates via **redstone** | **reached** | `random_tick::react_to_notification` (`random_tick.rs:1160-1192`) ← `propagate_and_react` ← `tick.rs:1142` | `redstone_openable.rs` (362 lines) | [#319](https://github.com/matteopolak/lodestone/issues/319) closed |
| doors/trapdoors/levers/buttons **by hand** | **absent** | — | `redstone_openable.rs:54-60`: "**Hand interaction** (`useWithoutItem`) is not modelled". `apply_use_item_on` (`server.rs:2748-3145`) has no door/trapdoor/lever/button keyword; its only family guard is `is_bed_block` at `:2907`. | [#532](https://github.com/matteopolak/lodestone/issues/532) |
| pistons / rails / dispensers / droppers / note blocks / tripwire / target | **absent** | — | `find` for `*piston*`, `*rail*`, `*dispenser*`, `*dropper*`, `*noteblock*`, `*tripwire*`, `*target_block*` → nothing anywhere in the repo | [#316](https://github.com/matteopolak/lodestone/issues/316), [#318](https://github.com/matteopolak/lodestone/issues/318), [#320](https://github.com/matteopolak/lodestone/issues/320), [#322](https://github.com/matteopolak/lodestone/issues/322) |
| comparator output on the wire | **parity defect** | — | `redstone.rs:311-323` encodes it as a **synthetic** `output=N` block-state property, which is not a real vanilla property; vanilla stores it in a `ComparatorBlockEntity`. Still true at HEAD. | [#476](https://github.com/matteopolak/lodestone/issues/476) |
| placement facing | **partial** | `apply_use_item_on` | `9aa5c9f6` gave real yaw-derived facing to **three** families (`REPEATER`, `COMPARATOR`, `OBSERVER`, `server.rs:2289-2297`). Every other directional block still falls through to the bare name at `server.rs:2959`. | [#475](https://github.com/matteopolak/lodestone/issues/475) |
| block entities simulated | **reached** | `block_entities.rs:209` in `tick_all_with_hopper_lock` ← `tick.rs:964` | exactly **four**: composter, furnace, hopper, brewing stand. Chests, spawners, vaults and the rest are `Opaque { id, nbt }`, round-tripped and never ticked. | [#477](https://github.com/matteopolak/lodestone/issues/477) |
| crop / sapling growth, leaf decay | **reached** | `random_tick.rs:203-618` ← `tick.rs:1166` | `growth_tick.rs` (697 lines). Landed as [#310](https://github.com/matteopolak/lodestone/issues/310), not #248. | — |
| bone meal | **absent** | — | `grep -in 'bone_meal\|BoneMeal\|Fertiliz'` across `crates/lodestone-server/src/` finds only the composter's *output item* (`server.rs:2663`, `composter.rs:50`). No bone-meal branch in `apply_use_item_on`. | [#248](https://github.com/matteopolak/lodestone/issues/248) |
| fluid flow (water/lava spread) | **absent** | — | `scheduled_tick.rs`'s fluid lane **is** drained every tick (`tick.rs:1149-1151`) into a literal no-op body: `for _due in fluid_ticks.drain_due(…) { }`. `neighbor_update.rs` has zero matches for fluid/water/lava. The plumbing exists; there is no flow logic to run. | [#309](https://github.com/matteopolak/lodestone/issues/309) |
| explosion **entity** damage + knockback | **reached** | `MobSim::tick` `mobs.rs:2057` → `explode` `:2395` → `take_detonations` `:2337` → `ExplosionFeed` `tick.rs:383` → `server.rs:4306-4313` `encode_explode` | a creeper really does detonate and hurt things | [#425](https://github.com/matteopolak/lodestone/issues/425) closed |
| explosion **block** destruction | **absent** | — | `crates/lodestone-entity/src/explosion.rs` (290 lines) implements only `seen_percent` / `entity_damage` / `knockback_power` / `knockback_direction`. `Detonation`'s doc (`mobs.rs:1560-1566`): "This crate tracks no block-destruction model". | [#313](https://github.com/matteopolak/lodestone/issues/313) |
| blast resistance data | **absent** | — | `grep` for `blast_resistance` / `explosion_resistance` across `lodestone-data` → **0 files**. Needs a jar dump like `hardness`/`collision_shapes`. | [#313](https://github.com/matteopolak/lodestone/issues/313) |
| fire spread, burning, lightning | **absent** | — | no strike, no ignition, no burn tick. `weather.rs` has no `strike`. `game_rules.rs:263-264` registers `fire_damage` and `fire_spread_radius_around_player`; grep for either string outside `game_rules.rs` → **0 hits** — registered and never read. The client's flame billboard (`5c9c3f45`) renders an `on_fire` metadata bit nothing sets server-side. | [#312](https://github.com/matteopolak/lodestone/issues/312), [#269](https://github.com/matteopolak/lodestone/issues/269) |

---

## 5. Drowning, air supply and bubbles — the row that is *not* a gap

Recorded in full because it was suspected to be an untracked island and is instead the
cleanest end-to-end chain in the server. All three legs verified independently:

| leg | verdict | evidence |
|---|---|---|
| server ticks it | **reached** | `PlayerVitals::tick(is_water(&eye_state))` on a dedicated `vitals_tick` interval inside `serve_play` — `server.rs:4231,4261-4263` |
| v770 encodes it | **reached** | the trait default emits nothing (`protocol.rs:1091-1094`) but v770 **overrides** it with a real `SET_ENTITY_DATA` metadata write — `crates/protocol/v770/src/server_protocol.rs:2963-2974` |
| client routes and draws it | **reached** | `lodestone_ecs::ingest::apply_local_player_air_supply` (`ingest.rs:845`), registered in the production system list (`ingest.rs:1059`) → `Vitals::air` (`session.rs:226-231,773-795`) → `Sim::air()` (`sim/session.rs:462`) → `HudFrame::air` (`app/redraw.rs:617`) → `hud.rs:1789` → `lodestone_render::bubble_row`. Pixel-gated through the **real** HUD path by `crates/lodestone-shell/tests/air_bubble_pixels.rs`, with a negative control. |

A methodological note worth keeping: the first grep for `encode_air_supply_update` was
piped through `head -20` and truncated **exactly before** the v770 override, which read as
"v770 does not implement it" and would have produced a confident, wrong island report. The
rule in `CLAUDE.md` — a shell pipeline will destroy the evidence you are about to reason
from — earned itself again here.

### A correction to `CLAUDE.md`'s router model

`CLAUDE.md` describes **three** event routers each ending in a terminal `_ =>` arm that is
an "island factory". That is now stale in the client's favour: `ingest::handles_event`
(`crates/lodestone-ecs/src/ingest.rs:130-132`) and `session::handles_event`
(`session.rs:536-538`) both delegate to one central table,
`lodestone_model::event::route` (`crates/lodestone-model/src/event.rs:2242`), whose match is
**exhaustive by gate**: a test at `event.rs:2590-2607` fails if a catch-all arm appears,
with a control proving the detector fires. The `ingest`-vs-`session` fork still exists and
still matters, but a new `ClientEvent` variant can no longer be silently dropped.

---

## 6. Player vitals, damage and physics

| feature | verdict | consumed by | evidence | issue |
|---|---|---|---|---|
| drowning damage, air supply | **reached** | `serve_play`'s `vitals_tick` | §5 | [#267](https://github.com/matteopolak/lodestone/issues/267) closed |
| fall damage, base case | **reached** | `ServerBound::PlayerMoved` → `FallTracker::on_player_moved`, `server.rs:3471,3632` | applied through `lodestone_entity::apply_reductions` | [#265](https://github.com/matteopolak/lodestone/issues/265) closed |
| fall damage cancellation cases | **absent** | — | no hay/slime/honey/dripstone `fallOn` override, no `FALL_DAMAGE_IMMUNE`, no elytra grace. **`FallTracker::reset` exists at `fall.rs:196` and nothing calls it.** Water landings are the parity defect: `on_ground` is only set by a grounded move sample, and a client falling into water reports ungrounded, so the distance stays banked until the player next touches solid ground and then hurts them. | [#534](https://github.com/matteopolak/lodestone/issues/534) |
| armour damage reduction formula | **reached** | `SimMob::apply_damage` `mobs.rs:1292` | `damage_after_armor` (`crates/lodestone-entity/src/damage.rs:155-164`), live-verified against a running 26.2 server | [#261](https://github.com/matteopolak/lodestone/issues/261) |
| the armour **values** the formula consumes | **absent** | — | `damage.rs:34-76`: `Defenses` is never fed from real equipped items; "There is no equipment/inventory model anywhere … that carries per-item armour/toughness/enchantment-level stats". No melee knockback impulse is computed anywhere in the workspace, so `knockback_resistance` has nothing to plug into. | [#261](https://github.com/matteopolak/lodestone/issues/261) |
| food, hunger, saturation, exhaustion, starvation | **absent** | — | `grep -c 'food\|hunger\|saturation' crates/lodestone-server/src/vitals.rs` → **0**. v770's `encode_set_health` always resends the fresh-spawn constants `food: 20, saturation: 5.0` (`server_protocol.rs:3210-3220`), documented as honest rather than a claim. The client HUD is finished and waiting (`3e1a0af0`). | [#258](https://github.com/matteopolak/lodestone/issues/258) |
| passive health **regeneration** | **absent** | — | no regen/heal tick anywhere in `crates/lodestone-server/src/*.rs`. Health only ever decreases (drowning, fall, border, attack) and is restored solely by `PlayerVitals::respawn` (`vitals.rs:490`). | [#258](https://github.com/matteopolak/lodestone/issues/258) |
| potion / status effects | **absent** | — | no `MobEffect` / `StatusEffect` / `ActiveEffect` type exists in `lodestone-server`; the only hits are `vitals.rs:12,21,78` disclaiming it. No `encode_update_mob_effect` / `encode_remove_mob_effect`. The client's screen overlays (`a2e13c6f`) are visual-only. | [#259](https://github.com/matteopolak/lodestone/issues/259) |
| XP orbs and levels | **orphaned** | nothing | `furnace::experience_for` (`furnace.rs:972`, re-exported `lib.rs:224`) is called only by `furnace.rs`'s own tests at `:1251-1257`; the re-export has zero consumers. No orb entity is ever spawned by `MobSim`; no player XP field exists. | [#256](https://github.com/matteopolak/lodestone/issues/256) |
| ladders and climbing | **absent** | — | the only `ladder` occurrence in `crates/lodestone-server/src/` is the string `"minecraft:ladder"` in `furnace.rs:477`, a fuel-table entry | [#534](https://github.com/matteopolak/lodestone/issues/534) |
| fluid push / current | **absent** | — | no fluid-velocity term in `SimMob` or `ItemMotion`. Deliberate for the *player* (`fall.rs:65-67`: movement is client-authoritative here); a real absence for mobs and dropped items. | [#534](https://github.com/matteopolak/lodestone/issues/534) |
| sleeping, and the night-skip vote | **reached** | `UseItemOn` on a bed → `sleep_vote.lay_down` (`server.rs:2907-2921`); `PlayerCommand` action 0 → `get_up` (`:3859`); `run_tick_loop` publishes `SleepEvent::SkippedNight` (`tick.rs:1000-1006`) which `serve_play` turns into `encode_set_time(game_time, Some(morning))` (`server.rs:4349-4356`) | the same bed arm also sets `RespawnPoint` — **which nothing reads**, see §11 | [#325](https://github.com/matteopolak/lodestone/issues/325) closed |
| respawn repositions the player | **absent** | — | `PERFORM_RESPAWN` (`server.rs:3248-3251`) calls only `vitals.respawn()`; a player respawns at the place they died. Note `sleep.rs` is **not** where the respawn point is set, despite being the obvious place to look. | [#329](https://github.com/matteopolak/lodestone/issues/329) |
| weather transitions | **reached** | `WeatherState::tick` in `run_tick_loop` → `WeatherFeed` → `encode_game_event` (`server.rs:4315-4319`), a real `GAME_EVENT` in v770 (`server_protocol.rs:3194-3199`) | production test at `tick.rs:1810-1827` names the real loop | [#324](https://github.com/matteopolak/lodestone/issues/324) closed |

---

## 7. Time — a parity defect, confirmed still live

`CLAUDE.md` names this as the canonical example of a wire that is green at every link and
carries the wrong value. **Verified still true at HEAD.** The producer is
`crates/lodestone-server/src/server.rs:4226-4228`:

```rust
_ = time_sync_tick.tick() => {
    let game_time = ticks_since(play_start);
    apply(conn, &mut state, proto.encode_set_time(game_time, None)).await?;
}
```

and `ticks_since` (`server.rs:3877-3883`) is wall clock:

```rust
fn ticks_since(start: tokio::time::Instant) -> i64 {
    (start.elapsed().as_millis() / MILLIS_PER_TICK) as i64
}
```

with `play_start = tokio::time::Instant::now()` set once **per connection** at
`server.rs:4082`. Meanwhile `run_tick_loop` maintains the real shared `game_tick` and the
night skip mutates a real `day_time` at `tick.rs:1001-1010`. The two are never connected, so
two players on one LAN world each see their own private clock starting at dawn from the
moment they joined. Only a gate whose expected value originates outside our own producer can
see this — [#323](https://github.com/matteopolak/lodestone/issues/323).

A second instance in the same family: `gamerule random_tick_speed` is fully implemented and
tested in `game_rules.rs`, and `tick.rs:1166` passes the hardcoded
`DEFAULT_RANDOM_TICK_SPEED` constant instead of reading it. Every non-`game_rules.rs` caller
of `.random_tick_speed()` is inside `#[cfg(test)]`
([#508](https://github.com/matteopolak/lodestone/issues/508)).

---

## 8. Lighting — a parity defect

Independently re-verified; [#517](https://github.com/matteopolak/lodestone/issues/517)'s body
is accurate.

- `crates/lodestone-world/src/lighting.rs` is a 1,105-line `LightEngine` port exporting
  `compute_column_light`, `compute_column_light_with_neighbours` and `diff_column_light`,
  unit-tested and benched.
- Its **only** non-test caller anywhere is
  `crates/lodestone-shell/src/worldgen.rs:261` — the shell's own local world, with
  `DemoLightProps`. Every other hit of `compute_column_light` in the tree is under
  `crates/lodestone-world/{tests,benches}/`.
- The integrated server sends `ColumnLight::new(shape.section_count)`, i.e.
  `vec![LightData::Missing; …]` for both sky and block. `encode_column_body`'s own doc
  comment (`crates/protocol/v770/src/server_protocol.rs:1455-1459`) says so: "Heightmaps are
  sent empty and light is sent as all-`Missing` … a documented gap, not a hidden one."
- There is no incremental path either: `LIGHT_UPDATE` exists only as a client *decode* arm.

Same shape, same file: every served chunk carries an **empty heightmap NBT**
(`Heightmaps::new().encode(&mut w)`) while a JVM-proven `MOTION_BLOCKING` implementation sits
unreachable — [#516](https://github.com/matteopolak/lodestone/issues/516).

---

## 9. Mob spawning — two engines, neither driving

| feature | verdict | evidence | issue |
|---|---|---|---|
| natural spawn cycle | **orphaned** | `MobSim::run_spawn_cycle` and `census` (`mobs.rs:2640-2676`) are written against `mob_spawn.rs`'s `SpawnState` and have **zero callers** outside `tests/mob_spawn.rs:82,91,94`. `mobs.rs:1824`: "`despawn_pass` … has no production caller." | [#222](https://github.com/matteopolak/lodestone/issues/222) |
| the only production spawn | **reached, but not vanilla** | `mobs::seed_demo_mobs` (`mobs.rs:3097`) ← `MobHandle::reseed` (`:3035`) ← `integrated.rs:611`. Its own doc (`:3089-3096`): "**not** vanilla natural spawning: there is no light-level, biome, or pack-size logic here." One-shot at world load. | [#222](https://github.com/matteopolak/lodestone/issues/222) |
| despawn state machine | **partial** | `check_despawn` is reached for the idle-timer-reset half only (`mobs.rs:1947`, inside `MobSim::tick` ← `tick.rs:903`); the discard/cap half is not | [#222](https://github.com/matteopolak/lodestone/issues/222) |
| per-species spawn rules | **absent** | `SpawnRule` **is not a trait and never has been** — two doc-comment mentions only (`crates/lodestone-entity/src/spawn.rs:24,218`); the real type is `SpawnConditions`. `SpawnEnvironment` exists (`spawn.rs:265`) with **zero** implementers. No per-species light/biome table exists. | [#221](https://github.com/matteopolak/lodestone/issues/221) |
| biome spawn lists | **absent** | all 66 bundled biome documents carry `spawners` and `spawn_costs`. `grep -rn '"spawners"\|"spawn_costs"'` across every `.rs` under `crates/` → **0 hits**. `EmbeddedResolver::biome_document` loads the whole document; consumers read only `carvers` and two `features` steps. | [#518](https://github.com/matteopolak/lodestone/issues/518) |
| spawn eggs, spawner blocks | **absent** | `apply_use_item_on`'s body has no "egg" or "spawner" hit. `BlockEntity` has four simulated variants; spawner is `Opaque`. `4d6c306b`'s body: "not spawn eggs (#224), not a spawner block, still not natural spawning." | [#224](https://github.com/matteopolak/lodestone/issues/224) |
| candidate source | **orphaned** | `SpawnCandidateSource`'s only implementer tree-wide is a test mock (`tests/mob_spawn.rs:32`). `mobs.rs:3091` says it: "every current impl is a test mock." | [#222](https://github.com/matteopolak/lodestone/issues/222) |
| Brain AI driver | **partial** | the seam is real and generic — `BrainGoal: impl Goal` (`brain/driver.rs:48,100`) is installed by `roster::goals_for` (`ai/roster/mod.rs:443`) and ticked by `MobSim::tick`. But `BRAIN_SPECIES` (`brain/roster.rs:78-98`) has **zero overlap** with `DEMO_SPECIES` (`mobs.rs:3177-3191`), the only species anything spawns. No live entity exercises it. | [#209](https://github.com/matteopolak/lodestone/issues/209) |
| aging → hitbox scale | **partial** | breeding and age-up ticking are real (`mobs.rs:2308,2318`, `navigating_mob.rs:1235,1945-1957`); nothing shrinks a baby's collision box. `spawn_species` always uses the adult shape, and the one function that folds `minecraft:scale` into a `MobShape` — `resolve_mob_shape` (`mob_spawn.rs:68`) — is test-only. | [#237](https://github.com/matteopolak/lodestone/issues/237) |
| sheep grazing | **reached** | `EatBlockGoal` → `pending_grazes` → `MobSim::take_grazes` → drained at `tick.rs:935-950`, which mutates the world and publishes the change | [#238](https://github.com/matteopolak/lodestone/issues/238) closed here |

### The two engines

| | `crates/lodestone-server/src/mob_spawn.rs` (660) | `crates/lodestone-entity/src/spawn.rs` (442) |
|---|---|---|
| `MobCategory` | 7 variants, `:105` | 8 variants, `:34` |
| despawn | its own `check_despawn`, `:303` | its own `check_despawn` |
| vocabulary | `SpawnState`, `SpawnCandidate`, `SpawnCandidateSource` | `DespawnDecision`, `DespawnCtx`, `SpawnConditions`, `SpawnSample` |
| cross-reference | imports only `AttributeMap` and `MobShape` from `lodestone-entity`, never its `spawn` module | never imported by anything, in any crate |
| history | actively developed | **one commit ever** — `32fb577d`, 2026-07-27, never touched again |
| caller shape waiting for it | yes: `MobSim::run_spawn_cycle`/`census` | none, ever |

**Wire `mob_spawn.rs`.** `lodestone-entity/src/spawn.rs` is a duplicate sketch of the same
cap and despawn arithmetic with no consumer; deleting it removes a second, incompatible
`MobCategory` from the tree, which is itself a latent trap.

---

## 10. Sound and particles — the server has no voice

| feature | verdict | evidence | issue |
|---|---|---|---|
| any sound encoder | **absent** | `grep -n 'encode_sound\|encode_particle\|encode_level_event'` against `crates/lodestone-server/src/protocol.rs` → **0 hits**; same grep against `crates/protocol/v770/src/server_protocol.rs` → **0 hits** | [#530](https://github.com/matteopolak/lodestone/issues/530) |
| `LEVEL_EVENT` / `LEVEL_PARTICLES` | **decode-only** | client decode arms at `crates/protocol/v770/src/adapter.rs:4342,4351`; ids in the generated table. Nothing encodes either. | [#530](https://github.com/matteopolak/lodestone/issues/530) |
| the one exception | — | `encode_explode` bakes `minecraft:entity.generic.explode` into the explosion packet's own body as a registry-referenced `Holder<SoundEvent>` (`server_protocol.rs:3176-3192`, `explosion_sound_registry_id()` at `:216-230`) — a field of that packet, not a general path | — |

Why this hides: the shell **predicts its own** break and place sounds and particles locally
(`docs/block-sound-types.md`, `docs/break-particles.md`), so single-player mining sounds
right. Silent is everything the client cannot predict — another player's actions over LAN,
mob hurt and death, and every `LEVEL_EVENT` the server owns (door open, chest open,
dispenser, fizz, brew, XP pickup, item pickup).

---

## 11. Server plumbing and world state

**The finding that dominates this section: `IntegratedServer::bind` has zero production
callers, and `IntegratedServer::start_rcon` has exactly one caller, its own test.** So the
island pattern also applies one level up, to entry points. Measured — every hit of
`grep -rn 'IntegratedServer::bind' --include='*.rs' crates/` is `tests/serve_play.rs:1864`,
`tests/lan_world_tick.rs:283,527`, the test gate at `src/ecs/gate.rs:141`, or a doc comment.
The shell constructs only in-memory servers (`crates/lodestone-shell/src/net.rs:1794,1829,1840`),
and `crates/lodestone-shell/src/menu/nav.rs:911-913` says "There is no LAN discovery here."

"Wired into `IntegratedServer::bind`" reads as production wiring in a commit message and is
not. Three landed subsystems are stranded behind it —
[#535](https://github.com/matteopolak/lodestone/issues/535).

A second, closely related pattern: **every real constructor passes the optional collaborators
as `::default()` or `::none()`**, so features that are correctly threaded through `serve_play`
can never be supplied a payload. `server.rs:1012,1060,1066,1129,1181,1250,1310,1430` — the
`ResourcePackPushFeed`, the `PluginChannelRegistry` and the `CommandDispatch` are all
defaulted in the paths the shell uses.

| feature | verdict | why | issue |
|---|---|---|---|
| RCON listener | **orphaned** | `rcon.rs` is real; `start_rcon`'s only caller is `tests/rcon.rs:91` | [#331](https://github.com/matteopolak/lodestone/issues/331), [#535](https://github.com/matteopolak/lodestone/issues/535) |
| GameSpy4/UT3 query | **orphaned** | started automatically by `bind` (`integrated.rs:1125-1147`), and only by `bind` | [#332](https://github.com/matteopolak/lodestone/issues/332), [#535](https://github.com/matteopolak/lodestone/issues/535) |
| plugin channel dispatch | **orphaned** | `.dispatch()` really is called on real `CustomPayload` packets (`server.rs:1893,3846`), but every production constructor hands it `&PluginChannelRegistry::default()` — an empty registry, so no handler is ever invoked. No bridge exists from `lodestone-ecs`'s plugin API. | [#335](https://github.com/matteopolak/lodestone/issues/335) |
| client-side plugin channels | **reached** | the other direction: `crates/lodestone-ecs/src/plugin_channel.rs` plus a real consuming plugin (`crates/plugins/lodestone-server-brand/`), gated against `V770Adapter::handle_packet`. **Not a duplicate of #335** — the serverbound half is the part still missing. | [#301](https://github.com/matteopolak/lodestone/issues/301) |
| resource pack push (server side) | **orphaned** | `ResourcePackPushFeed` is drained in `serve_play`'s `container_sync_tick`; every real constructor passes `::default()`, so nothing can publish | [#334](https://github.com/matteopolak/lodestone/issues/334) |
| resource pack push/pop in Configuration | **reached** | client decode arms at `crates/protocol/v770/src/adapter.rs:2936,2964-2975,2999`, reached via the shell's `run()` loop | [#294](https://github.com/matteopolak/lodestone/issues/294) closed here |
| advancements and statistics | **partial** | the framework is reached — join packet, per-connection state, `flush_dirty` every tick, `REQUEST_STATS` reply. But `grep -rn '\.grant(\|\.increment('` outside `advancements.rs` → **0 hits**: no gameplay event ever grants or increments anything. `server.rs:3199-3204` admits it. | [#338](https://github.com/matteopolak/lodestone/issues/338) |
| Brigadier command dispatch | **orphaned in practice** | the decode is real (`ServerBound::ChatCommand` → `commands.dispatch.run`, `server.rs:3817-3822`), but `open_in_memory_with_mobs`/`open_persistent_with_mobs` pass `&CommandDispatch::none()` (`server.rs:1060,1310`), which always refuses. `serve_connection_with_mob_events_and_commands_shared` is `#[allow(dead_code)]` with no caller (`server.rs:1092`). No `impl CommandSink` exists outside tests. **A real player's typed command is refused.** | [#48](https://github.com/matteopolak/lodestone/issues/48) |
| game rule storage | **orphaned, and bypassed** | `GameRules`/`GameRulesHandle` (`game_rules.rs:350-540`) is constructed only in `#[cfg(test)]`. The live `GameRuleChanged` handler (`server.rs:3160-3182`) uses a separate, unvalidated, **per-connection** `HashMap<String,String>` on `WorldAdminState` and says so: "This crate has no `GameRules` registry." | [#327](https://github.com/matteopolak/lodestone/issues/327), [#508](https://github.com/matteopolak/lodestone/issues/508) |
| world border | **reached** | `border.rs`, damage applied on `vitals_tick` | [#326](https://github.com/matteopolak/lodestone/issues/326) closed |
| world spawn search | **reached** | `find_initial_spawn` called at `server.rs:1632` on first join | [#329](https://github.com/matteopolak/lodestone/issues/329) |
| bed respawn point | **partial — written, never read** | the bed arm sets `RespawnPoint` (`server.rs:2907-2921`); `server.rs:3538-3541` says "Read back by no caller yet". `PERFORM_RESPAWN` (`server.rs:3248-3251`) calls only `vitals.respawn()` — **a player respawns at the place they died.** Respawn anchors are not modelled (`world_spawn.rs:611` asserts an anchor is *not* a bed block). `sleep.rs` does **not** set a respawn point. | [#329](https://github.com/matteopolak/lodestone/issues/329) |
| chunk unload + save-on-unload | **reached** | `chunk_store.rs:418-443` → `region_source.rs:886-892`; autosave task at `integrated.rs:866-895` | [#292](https://github.com/matteopolak/lodestone/issues/292) closed here |
| autosave scheduling | **reached** | real `tokio::time::interval` + `spawn_blocking` save, `integrated.rs:860-895` | [#305](https://github.com/matteopolak/lodestone/issues/305) |
| world upgrade / DataVersion migration | **absent** | `DATA_VERSION` (`chunk_nbt.rs:122,426`) is only ever *stamped*; no code path reads a loaded chunk's own `DataVersion` and compares it. Opening an older world has no upgrade path. | [#305](https://github.com/matteopolak/lodestone/issues/305) |
| block entities dropped on save | **reached** | `BlockEntity::Opaque` preserves unmodelled entries; the vanilla-fixture gate went from 1 of 6 surviving to 6 of 6 | [#477](https://github.com/matteopolak/lodestone/issues/477) closed here |
| player data (`.dat`) | **absent** | `grep -rln 'playerdata\|PlayerData'` under `crates/` → nothing. Only `level.dat` is read/written. A player's inventory, position and XP live only in memory for the connection's lifetime and are lost on disconnect. | [#302](https://github.com/matteopolak/lodestone/issues/302) |
| per-chunk entity and POI storage | **absent** | `WorldSaveHandle::save` (`region_source.rs:969-1030`) persists dirty blocks, block entities and scheduled ticks only. **A saved-and-reopened world loses every mob and every dropped item.** | [#303](https://github.com/matteopolak/lodestone/issues/303) |
| spawn-chunk keep-loaded ticket | **absent** | a fixed radius constant stands in; disclosed in two module doc comments (`integrated.rs:95-97,1084-1086`, `world_spawn.rs:24-25`) | [#297](https://github.com/matteopolak/lodestone/issues/297), [#289](https://github.com/matteopolak/lodestone/issues/289) |
| loot tables | **orphaned** — see §2 | | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| ops, whitelist, bans, permission levels | **absent** | nothing in `lodestone-server`; no `ops.json`/`whitelist.json`/`banned-players.json` handling anywhere. `lodestone-ecs`'s `PermissionLevel`/`PermissionStore` is unrelated plugin plumbing with no server wiring. | [#336](https://github.com/matteopolak/lodestone/issues/336) |
| difficulty | **partial** | decoded and echoed per connection (`apply_difficulty_change`, `server.rs:3141-3157`), but **per-connection, not persisted to `level.dat`, not shared across connections**, and `grep -n difficulty` across `mobs.rs`/`mob_spawn.rs`/`vitals.rs` → **0 hits**: it affects nothing | [#328](https://github.com/matteopolak/lodestone/issues/328) |
| regional difficulty and scaling | **absent** | `grep -rn 'regional_difficulty\|RegionalDifficulty\|difficulty_scal'` → nothing | [#223](https://github.com/matteopolak/lodestone/issues/223) |
| login encryption and compression | **absent** — offline mode only | | [#273](https://github.com/matteopolak/lodestone/issues/273) |
| server-side chat, signature verification | **absent** | | [#271](https://github.com/matteopolak/lodestone/issues/271) |
| registries / known-packs / tags in Configuration | **partial** | note `registries.json` is the wrong source for *which* registries are sent; the real list is `RegistryDataLoader.SYNCHRONIZED_REGISTRIES` (29 entries) | [#275](https://github.com/matteopolak/lodestone/issues/275) |

---

## 12. Where to start, ranked

Ordered by player-visible impact per unit of work. **Islands are listed first within each
tier**, because wiring an existing, already-gated implementation is far cheaper than writing
one.

### Islands — wire what exists

| # | work | why it is cheap | issue |
|---|---|---|---|
| 1 | ~~block break → `roll_loot` → `spawn_item`~~ | **in flight** — `block_drops.rs` was uncommitted in the tree at census time | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| 2 | mob death → `roll_loot` → `spawn_item` | rides the same join as row 1; `AttackOutcome::killed` is already computed and discarded at `server.rs:3399-3420` | [#272](https://github.com/matteopolak/lodestone/issues/272) |
| 3 | bundle the remaining 1,107 block loot tables | `tests/loot_corpus.rs` already proves all 1,355 parse; this is a copy plus a drift-gate refresh | [#337](https://github.com/matteopolak/lodestone/issues/337) |
| 4 | server light: call `compute_column_light` in the chunk path | 1,105 lines of ported, benched engine already exist and the shell already calls it | [#517](https://github.com/matteopolak/lodestone/issues/517) |
| 5 | encode a real `MOTION_BLOCKING` heightmap | JVM-proven implementation, one call site | [#516](https://github.com/matteopolak/lodestone/issues/516) |
| 6 | `run_tick_loop`'s `game_tick` → `encode_set_time` | delete `ticks_since`, thread the counter the loop already keeps | [#323](https://github.com/matteopolak/lodestone/issues/323) |
| 7 | `tick.rs:1166` reads `GameRules::random_tick_speed()` | the getter is implemented and tested; one argument | [#508](https://github.com/matteopolak/lodestone/issues/508) |
| 8 | drive `MobSim::run_spawn_cycle` from `run_tick_loop` | the cycle and census are already written against `mob_spawn.rs` — but it needs row 12 first to spawn anything but zombies | [#222](https://github.com/matteopolak/lodestone/issues/222) |
| 9 | item ground collision + merge pass | `merge`/`try_merge` exist; `MobSim` already owns the `ChunkWorld` to ask | [#533](https://github.com/matteopolak/lodestone/issues/533) |
| 10 | `FallTracker::reset` gets a caller; water cancels a fall | `reset()` is written and unused | [#534](https://github.com/matteopolak/lodestone/issues/534) |
| 11 | `PERFORM_RESPAWN` reads `RespawnPoint`, else world spawn | both are already written and both are already computed; nothing reads either | [#329](https://github.com/matteopolak/lodestone/issues/329) |
| 12 | thread a real `CommandDispatch` into the two constructors the shell uses | one argument each at `server.rs:1060,1310`; delete the `#[allow(dead_code)]` at `:1092`. Turns `/gamerule` and every plugin command from "always refused" into working | [#48](https://github.com/matteopolak/lodestone/issues/48) |
| 13 | call `.increment()` / `.grant()` from the five gameplay sites that already run | the whole advancement and statistics framework is reached and flushing every tick against nothing | [#338](https://github.com/matteopolak/lodestone/issues/338) |
| 14 | point `apply_game_rule_changed` at the tested `GameRulesHandle` instead of its own per-connection `HashMap` | until there is one store, every enforcement change has to pick which of two to read, and today's live one is the untested one | [#327](https://github.com/matteopolak/lodestone/issues/327) |
| 15 | an Open-to-LAN path that calls `IntegratedServer::bind` | unstrands RCON, query and player-to-player visibility in one change | [#535](https://github.com/matteopolak/lodestone/issues/535) |

### New implementation, highest impact first

| # | work | issue |
|---|---|---|
| 16 | `useWithoutItem`: doors, trapdoors, fence gates, levers, buttons — without it a hand-built redstone contraption cannot be triggered at all, despite the propagation being live and oracle-verified | [#532](https://github.com/matteopolak/lodestone/issues/532) |
| 17 | per-species spawn rules + biome spawn lists from the jar; the data is already bundled and read by nothing | [#221](https://github.com/matteopolak/lodestone/issues/221), [#518](https://github.com/matteopolak/lodestone/issues/518) |
| 18 | sound and particle encoders, then the obvious call sites | [#530](https://github.com/matteopolak/lodestone/issues/530) |
| 19 | server-side crafting: send the recipe book, handle `PLACE_RECIPE`, re-derive the result slot instead of trusting the client | [#529](https://github.com/matteopolak/lodestone/issues/529) |
| 20 | food and hunger, plus the regeneration it drives — the client HUD is finished and waiting | [#258](https://github.com/matteopolak/lodestone/issues/258) |
| 21 | `SWING` and `USE_ITEM`: stop discarding them, so arm swings relay and eat/drink exists | [#264](https://github.com/matteopolak/lodestone/issues/264), [#266](https://github.com/matteopolak/lodestone/issues/266) |
| 22 | XP orbs and levels; `furnace::experience_for` is already an orphan waiting for a consumer | [#256](https://github.com/matteopolak/lodestone/issues/256) |
| 23 | block-break validation — hardness, tool, range. Smaller than it looks: the table exists and the client already computes the number | [#531](https://github.com/matteopolak/lodestone/issues/531) |
| 24 | fluid flow — the tick lane is already drained into an empty body | [#309](https://github.com/matteopolak/lodestone/issues/309) |
| 25 | explosion block destruction, and a blast-resistance jar dump | [#313](https://github.com/matteopolak/lodestone/issues/313) |

---

## How to change this doc

Re-measure rather than edit. Every row is a `file:line` claim at one sha and will rot exactly
as the claims this census was written to replace did. When a row moves from **orphaned** to
**reached**, say which commit did it and which production caller now exists — not that it
"was wired". When adding a row, the verdict must rest on a tree-wide grep for the *producer*
with every hit classified prod/test, not on the presence of an implementation.

Two traps specific to this census:

- **`cargo xtask connectedness` cannot answer most of these questions.** It answers "is this
  clientbound packet reaching anything" and nothing else — it is silent on Rust call graphs,
  and blind to §7's and §6's fully-connected-but-wrong wires. Do not cite it for a row here.
- **Serverbound stranding is a two-file join.** A packet can decode cleanly in
  `crates/protocol/v770/src/server_protocol.rs` and be mapped to `ServerBound::Ignored`
  before `dispatch_play_packet` ever sees it. Checking only the server crate's match arms
  finds nothing wrong; see the list in §3.

## Dependencies

`crates/lodestone-server` (the version-free host), `crates/protocol/v770` (the only
`ServerProtocol` implementer), `crates/lodestone-entity` (mob AI, damage, item entities),
`crates/lodestone-world` (chunk storage, the unreached `LightEngine`), `crates/lodestone-data`
(the hardness and collision jar dumps), and the decompiled 26.2 source under
`.cache/mc/26.2/{src,client-src}` for every vanilla citation.
