# Villager economy and the Brain substrate

## What it is

The implementation plan for the villager-economy arc — issues #231 (villager Brain behaviours), #243
(professions and POI claiming), #244 (gossip), #245 (trades), #246 (reputation), #247 (curing), #240
(wandering trader) and #241 (raids and patrols) — planned as one architecture rather than eight
features. Its central findings: the Brain substrate the issues assume is missing **already exists and
runs in production** (`lodestone_entity::brain`, driven through `BrainGoal` on the goal scheduler),
so the substrate decision is to *extend* it, not to build it; the missing substrate is **POI state
and the world-facts seam into `BrainMob`**; and the 26.2 jar ships trades, trade sets and the
villager schedule as **plain data files already on disk** under `.cache/mc/26.2/src/data/minecraft/`,
which collapses what the issues treat as the hardest extraction problem.

Everything in §1–§2 was re-verified against the tree on 2026-08-13 with `/usr/bin/grep`; symbols only,
no line numbers.

---

## 1. Census: what already exists

### 1.1 The Brain substrate — built, wired, and running villagers today

`crates/lodestone-entity/src/brain/` (~2,300 lines) is a faithful port of vanilla's Brain:

- `Brain` (`brain/mod.rs`) — memories, sensors, priority-scheduled behaviours grouped into
  activities, `Brain::tick` in vanilla's exact order (forget → sensors → start → tick).
  **`Brain::set_schedule` and `Brain::update_activity_from_schedule` already exist** — a
  `(start_tick, activity)` timeline with vanilla's 20-tick re-evaluation gate, tested.
- `Memories`/`MemoryModuleType` (`brain/memory.rs`) — expiring blackboard. Registered types today:
  `WALK_TARGET`, `LOOK_TARGET`, `ATTACK_TARGET`, `NEAREST_VISIBLE_PLAYER`,
  `NEAREST_VISIBLE_LIVING_ENTITIES`, `IS_PANICKING`, `HURT_BY`, `CANT_REACH_WALK_TARGET_SINCE`,
  `PATH`. **No `JOB_SITE`, `POTENTIAL_JOB_SITE`, `HOME`, `MEETING_POINT` yet.**
- `Activity` (`brain/activity.rs`) — **`CORE`, `IDLE`, `WORK`, `PLAY`, `REST`, `MEET`, `PANIC`,
  `FIGHT`, `AVOID`, `SWIM` are already declared.** The activity vocabulary for #231 exists.
- `GateBehavior` with `OrderPolicy::Shuffled` + weighted run-one — the weighted pick-one-of-many
  shape every villager activity package leans on.
- `BrainGoal` (`brain/driver.rs`) — the production driver: a `Goal` at priority 0 holding
  `Flag::Move + Flag::Look`, never interruptible, ticking the brain through
  `MobController::brain_mob`. `roster::goals_for` consults `brain::brain_for(species)`, and
  `MobSim::spawn_species` installs whatever `goals_for` returns — zero host-side knowledge of brains.
- `BRAIN_SPECIES` (`brain/roster.rs`) — 20 species incl. **villager** and **piglin**, gated against
  the decompiled jar by `tests/brain_census.rs`. `zombie_villager` is correctly *not* a brain species
  (it runs the same goal-based AI a regular zombie does, not a Brain). Every brain species today
  gets the same generic CORE+IDLE `scaffold` — wander
  and watch players, on real paths, in the running game.
- `BrainMob` (`brain/mob.rs`) — the body seam, **10 methods**: rng, `game_time`, `position`,
  `in_water`, `move_to`/`navigation_done`/`navigation_stuck`/`stop_navigation`, `look_at`,
  `nearest_visible_player`, `random_land_pos`. `NavigatingMob` implements it (and `MobController`
  beside it — note the documented same-name-method ambiguity tax, and that the two `move_to`s differ
  in float width). **No day-time, no POI query, no hurt signal, no nearby-villager census, no
  bed/sleep interaction.** That gap, not the scheduler, is what blocks #231.

### 1.2 The server side

- `MobSim` / `SimMob` (`lodestone_server::mobs`) — the sim. `spawn_species` resolves attributes,
  shape, category, and installs `roster::goals_for` output. `SimMob::last_hurt_by` and
  `hurt_by_player_until` exist (fed by the damage path). Breeding, taming/ownership
  (`Owner`-style state landed with the interact-packet dispatch), vocalisation effects
  (`take_vocalisations` → `crate::effects::WorldEffect`), item entities, projectiles, vehicles all
  live here. **`MobSim` holds its `ChunkWorld` immutably** — terrain reaches it via the
  `tick_with_terrain` block-state closure; it cannot see block edits except through what the host
  passes in. **No `day_time` reaches `MobSim`** (the `NaturalSpawner` gets it via `set_day_time`;
  the sim does not). **Mobs cannot hold status effects** — `ActiveEffects`
  (`lodestone_server::mob_effects`) is wired for players only, and
  `commands/effect_command.rs` documents that its `Effect` is per-*player* by construction.
- The tick loop (`lodestone_server::tick::run_tick_loop`) — unified 20 Hz: ticks `MobSim`, publishes
  `EntitySnapshot`s (which carry `metadata: Vec<MetadataField>`, diffed and encoded by
  `server.rs`'s sync via `encode_set_entity_data`), runs the **`NaturalSpawner` in production** with
  `set_day_time`/`set_difficulty`/`begin_cycle` — the natural-spawn island named in
  `docs/plans/mob-ai-roster.md` is closed. The spawner is the template for the patrol and
  wandering-trader spawners.
- Spawn eggs work (`spawn_egg.rs` → `spawn_species`), including `villager_spawn_egg`,
  `zombie_villager_spawn_egg`, `wandering_trader_spawn_egg` — a villager can reach the screen today.
- `BlockEntityRegistry` / `BlockEntityHandle` (`block_entities.rs`) — the `Arc<Mutex<…>>.with(…)`
  handle pattern to copy for the POI index.
- `WorldState` (`world_state.rs`) — day time, difficulty, game rules (**`spawn_patrols` and
  `spawn_wandering_traders` are already modelled as the separate 26.2 rules**), and
  `level_data_fields`/`load_level_data` for level persistence.
- `EntityStorage`/`SavedEntity` (`entity_storage.rs`) — entity persistence with the
  consumed-field pattern done right: `extra: Vec<(String, Nbt)>` carries **verbatim** every field the
  decode did not consume. A villager's `VillagerData`/`Gossips`/`Offers` NBT already survives a
  save/load round-trip untouched; the units below must *consume* those fields, never name-list them.
- Sleep (`sleep.rs`), effects for players (`server.rs` + `mob_effects.rs`), brewing knows
  weakness potions (`brewing.rs`), commands incl. `/effect` (players only), container screens and
  `container_click.rs` for menu plumbing.

### 1.3 The protocol and client side

- **Clientbound `MERCHANT_OFFERS` decode exists** (`v770::adapter::decode_merchant_offers`), with a
  documented trap: five of `MerchantOffer`'s fields are **big-endian `i32`s, not VarInts**. The
  decoded offers flow to `ClientEvent::MerchantOffersReceived` → `SessionTrades`
  (`lodestone_game::trades`) — the client *data* half of trading is done.
- **Serverbound `SELECT_TRADE` is decoded and discarded**: `v770::server_protocol` does
  `let _ = decode_full::<SelectTrade>(payload)` — a stranded packet, the serverbound island shape.
- **The merchant *screen* is deliberately unimplemented**: `lodestone_shell::container::frame`
  documents that `beacon` and `merchant` are excluded because the merchant screen needs to compose
  trade-level text rather than merely moving an anchor, unlike every other container screen.
- **`VILLAGER_DATA` metadata decodes client-side** (`v770::packets::metadata`, serializer 18 → a
  villager type/profession/level variant) and the shell resolves variant textures
  (`EntityTexture::resolve` has a production caller since the mob-variant-texture fix). **But the
  server cannot send it**: `lodestone_server::protocol::MetadataField` has no villager-data variant.
  Assigning a profession server-side is invisible until that variant plus its
  `server_protocol.rs` encode arm exist.
- `BOSS_EVENT` decodes client-side and the HUD draws boss bars with styled spans; **the server never
  encodes it** — raids need a new encode arm.
- `encode_set_entity_data(&self, entity_id, &[MetadataField])` is the generic metadata encoder;
  use it, never a new single-purpose one.

### 1.4 POI-shaped state: none

No POI registry, no gossip, no reputation, no trade state, no raid or patrol logic anywhere in the
tree (`/usr/bin/grep -rn "Poi\|gossip\|Gossip"` across `lodestone-server`, `lodestone-entity`,
`lodestone-world`: only false positives). This is genuinely greenfield.

### 1.5 The data and oracles on disk — better than any issue knows

- **Trades are data files**: `.cache/mc/26.2/src/data/minecraft/villager_trade/<profession>/<1–5>/
  <name>.json` — plain `{gives, wants{id,count}, max_uses, xp, reputation_discount}` records — and
  `trade_set/<profession>/<level>.json` plus **`trade_set/wandering_trader/{buying,common,
  uncommon}.json`** define the per-level pick sets. In 26.2 the trade tables are produced by a
  data-gen bootstrap class rather than hand-authored; the JSONs are the shipped truth. No container
  boot needed.
- **The villager schedule is a data file**: `.cache/mc/26.2/src/data/minecraft/timeline/
  villager_schedule.json` — keyframes `idle@10, work@2000, meet@9000, idle@11000, rest@12000`
  (and the baby timeline) on a 24000-tick clock. 26.2 replaced the old schedule-registry mechanism
  with environment-attribute timelines set directly on the brain at spawn time;
  **do not transcribe pre-26.2 schedule constants from memory or wiki** — our
  `Brain::set_schedule(Vec<(i32, Activity)>)` matches the keyframe shape exactly.
- **Vanilla-authored POI region files**: `.cache/mc/survival/world/dimensions/minecraft/overworld/
  poi/*.mca` (and nether) — vanilla's own answer to "which blocks in this chunk are POIs, with what
  type and how many free tickets", sitting beside the `region/*.mca` blocks that produced them.
- **Vanilla-authored persistence fixtures**: `.cache/mc/survival/world/data/minecraft/
  wandering_trader.dat` (world-level spawner state — note 26.2 splits saved data into per-name
  `.dat` files, *not* `level.dat` fields) and `.cache/mc/survival/world/dimensions/minecraft/
  overworld/data/minecraft/raids.dat`. Exact key names and NBT types for V8 and V10, written by the
  other side.
- 19 entity region files under `…/overworld/entities/` — whether they contain villagers with
  `Gossips`/`Offers` is unverified (§7).
- Behaviour truth for goal packages, gossip weighting and decay, the block→POI-type table with
  capacities, villager and zombie-villager mechanics, the wandering-trader and patrol spawn
  cadences, and full raid mechanics all lives in the decompiled source under `.cache/mc/26.2/src/`
  — real files, not doc guesses. For scale: the wandering-trader spawner is under 150 lines, the
  patrol spawner under 100, and the raid logic is the largest single piece here, at over 800.

---

## 2. Issue-body verdicts

| issue | verdict |
|---|---|
| #231 | **Half stale, and the other half has since landed.** Written as if the Brain behaviour system needs standing up; `Brain`, `BrainGoal`, activities, schedule support and the villager's production scaffold all landed with the Brain-driver commit ("the Brain AI system reaches a real mob on a real path"). What was actually missing — the villager *package* and the world-facts seam (§3.2) — is now built: `docs/villager-work-rest-schedule.md` is V2, landed close to this plan's own shape (schedule mode in `BrainGoal::tick`, `WalkToPoi` behaviours, `day_time`/POI-position feed into `NavigatingMob`, `BellClaims` as the third POI ledger). Golem-summon-on-hurt (§6's own "not built here") also landed separately, in `MobSim::tick_golem_summon`. Still open: piglin's own Brain package (bundled into #231, deliberately split out below), and V2's own disclosed cuts (no work-at-poi restocking/sleep pose/trade-UI-at-work, no baby schedule). |
| #243 | Accurate. POI registry genuinely absent. The "shared POI-count query with the iron-golem issue" note stands — design `PoiIndex`'s query for both callers, but do not build the golem half (§6). |
| #244 | Accurate — "no gossip propagation exists" re-verified true. |
| #245 | **Right direction, stale mechanism.** "Pull from the 26.2 jar's registry data, not minecraft-data" is correct, but there is no extraction problem left: trades are plain JSONs on disk (§1.5). The body also doesn't know the client data half (`MERCHANT_OFFERS` decode → `SessionTrades`) already exists and that the merchant *screen* was deliberately excluded — the UI is the missing client piece, not the decode. |
| #246 | Accurate. Its advice to keep Hero of the Village separately-triggered is honoured: HotV lands inside the raid unit (§6). |
| #247 | Accurate on mechanics but **silent on a real dependency**: mobs cannot hold status effects, so "weakness + golden apple" has no way to apply the weakness. §5 V7 makes the mob-effects substrate part of the unit. |
| #240 | Accurate. Two things it doesn't know: `spawn_wandering_traders` is already a modelled game rule, and vanilla persists the spawner in `data/minecraft/wandering_trader.dat` (we have a vanilla-written fixture). |
| #241 | **Stale on raid mechanics.** "Bad-omen-effect-triggers-on-village-entry" is the pre-1.21 flow. In 26.2: Bad Omen (from an ominous bottle) is converted on village entry into the Raid Omen effect, and the raid starts when Raid Omen expires at the player's position. Also stale on patrols: "not covered by anything in `ai/goals.rs` today" predates the pillager landing in `roster/ranged.rs` — the *mob* exists; only the leader-follow shape and the spawner are missing. The split it suggests (patrol vs raid) is right and adopted. |

---

## 3. The substrate decision

### 3.1 Behaviour substrate: ratify Brain-in-a-Goal; extend, do not rebuild

**Decision: villager (and later piglin) behaviour is written as Brain activity packages on the
existing `Brain`, driven by the existing `BrainGoal`, installed through the existing
`brain_for`/`goals_for` route. No new driver, no goal-scheduler expression of villager behaviour.**

The question the dispatch brief poses — port Brain vs express on the goal scheduler — was already
answered in the tree, and the answer is the correct one:

- **A second host-side driver was considered and rejected in `brain/driver.rs`'s own module doc**,
  for the right reason: a subsystem whose only route to production is a call site somebody must
  remember to add is how this repo's islands get built. `BrainGoal` rides the already-wired
  `goals_for → spawn_species → MobSim::tick` path and cannot be silently dropped.
- **Expressing villager behaviour as `Goal`s is rejected** on three grounds. (a) Vanilla's villager
  logic is memory-shaped: several small behaviours coordinate purely by writing and reading
  `JOB_SITE`/`POTENTIAL_JOB_SITE` memories with expiry — scanning nearby POIs, resolving competing
  claims, taking a job site, and assigning the resulting profession — and
  flags-and-priorities has no equivalent of memory erasure on activity switch, and a
  translation layer would be a permanent divergence to maintain against every future vanilla port.
  (b) The villager is already routed to a brain in production; a goal-based villager means
  un-routing one of 20 species and forking the roster convention. (c) Schedule-driven activity
  gating already exists on `Brain` and is tested; it has no goal-system analogue.
- **What is genuinely missing is not the scheduler but its inputs.** This is the same shape as the
  perception-starvation finding in `docs/plans/mob-ai-roster.md` §1.1: the machinery is fine, the
  seam is starved. `BrainMob` needs day time, POI queries, a hurt signal, nearby-villager and
  wanted-item perception, and bed/job-site interaction; `MobSim` must feed them. Every new
  `BrainMob` method with a permissive default (`None`/`false`/`0`) is a behaviour that silently
  never fires — each unit's negative control below exists for exactly that.

One driver change is required: `BrainGoal` re-evaluates a static candidate list each tick;
vanilla's villager instead switches activity **by schedule**, re-checking it once per AI tick from
its own top-level tick method, plus a fixed low-priority behaviour in every package whose only job
is to keep the current activity in sync with the schedule. `BrainGoal` grows an optional schedule
mode: when enabled, `tick` calls
`Brain::update_activity_from_schedule(mob.day_time(), time)` before the candidate scan. `day_time`
becomes a `BrainMob` method fed from `WorldState` through `MobSim` (the same route
`NaturalSpawner::set_day_time` already takes).

### 3.2 POI substrate: a derived index with villager-held claims — not a port of vanilla's own POI manager

**Decision: a new `PoiIndex` in `lodestone-server` (`poi.rs`), maintained from block state, with
claim *ownership* living on the claiming villager and persisted through the villager's NBT. The
vanilla `poi/*.mca` files are a validation oracle, not our storage format.**

Requirements, from the consumers: workstation claiming (#243), bed and meeting-point claiming
(#231), raid-center location (#241), and the POI-count query #243 says to share with iron-golem
construction. All need: per-position type + capacity + occupancy, queries by type/predicate within a
radius ("nearest unclaimed job site"), and invalidation when the block is broken or replaced.

- **Why not port vanilla's own POI manager?** It is a persisted-section store with its own region
  persistence, ticket lifecycles, and a distance-ordered stream API — ~800 lines of machinery whose
  persistence half duplicates information that is a pure function of chunk blocks. POI *existence*
  can always be rebuilt by scanning loaded chunks against the vanilla POI-type table (14 workstations +
  `home` = beds + `meeting` = bell, capacities 1/1/32, per that table's own registration). Only
  *claims* are real state, and vanilla itself stores the claim twice (a ticket in the POI region
  file, a job-site memory slot on the villager). We store it once, on the villager, and rebuild
  occupancy from mobs at load. No new region format, no double bookkeeping, same observable
  semantics.
- **Where it lives:** `lodestone-server`, behind a `PoiHandle` clone of the
  `BlockEntityHandle` pattern. Not `lodestone-entity` — that crate is world-free by design; brains
  reach the index only through the `BrainMob` seam (a query closure/trait object handed to
  `NavigatingMob` the way `tick_with_terrain` hands in `block_state`).
- **Maintenance:** populate on chunk activation by scanning sections for POI blocks; update from the
  same block-mutation choke the block-entity registry uses. The block→POI-type table is generated
  from vanilla's own POI-type registration under the `LODESTONE_REGEN=1` generate-or-assert pattern
  (it is a jar claim; hand lists have been wrong five times in this repo).
- **The validation oracle:** parse a vanilla `poi/*.mca` region with an independent gzip+NBT script
  (the `players/data` XP-table precedent), scan the corresponding `region/*.mca` chunks with our
  table, and require the derived POI set to match vanilla's records — chunks chosen for POI
  *density*, not convenience, and committed as a fixture table with provenance so the gate does not
  depend on `.cache`.

### 3.3 Economy data: generated tables in `lodestone-data`, trade state on the mob

Trades, trade sets, and the schedule keyframes are generated from the jar data files (§1.5) into
`lodestone-data` with generate-or-assert gates, committed. Runtime trade state (offer uses, demand,
special-price deltas, villager XP/level) lives on the villager server-side in a new
`lodestone_server::villager` module, serialised through the `SavedEntity` consumed-field pattern.
Gossip is its own container (`gossip.rs`) because the wandering trader must *not* have one and the
raid unit reads reputation without being a villager.

---

## 4. Units, file clusters, and ordering

The binding constraint is file contention: `mobs.rs` and `server.rs` serialise agents. The
decomposition gives each unit a disjoint primary cluster; every touch of a choke file is listed as a
small **brokered** patch (orchestrator applies or sequences it — ownership per the dispatch
conventions). Every unit ends on screen.

### Wave 1 — three units, fully parallel

**V1 — POI index and professions (#243).**
*Primary cluster:* new `crates/lodestone-server/src/poi.rs`; new
`crates/lodestone-server/src/villager.rs` (profession + villager-data state); a generated
POI-type table in `crates/lodestone-data`.
*Brokered:* `lib.rs` mod lines; a `MetadataField::VillagerData` variant in
`lodestone-server/src/protocol.rs` + its encode arm in `v770/src/server_protocol.rs`; a short
`mobs.rs` window (villager state on `SimMob`, snapshot metadata); the block-mutation hook.
*Screen:* spawn-egg an unemployed villager next to a placed lectern → it acquires the librarian
profession and **its robe changes** (the client's variant-texture path is already live); break the
lectern → robe reverts. The `AcquirePoi` walk itself is V2's; V1 may assign on proximity.
*Unblocks:* V2 (claim memories), V10 (raid center), the golem issue's count query (not built here).

**V3 — merchant screen, client side (#245's UI half).**
*Primary cluster:* new `crates/lodestone-shell/src/container/merchant.rs`; wiring in
`container/frame.rs` (its own doc names why merchant was excluded — it needs to compose trade-level
text rather than simply relocate a fixed layout anchor); the `SELECT_TRADE` send on the shell's
serverbound path.
*Screen:* against any server that sends `MERCHANT_OFFERS` (a fixture-fed `SessionTrades` works for
development; the real join once V4 lands), the trade list renders, arrows and prices draw, clicking
a row sends `SELECT_TRADE`. Zero server files — fully parallel with everything.

**V9 — pillager patrols (#241a).**
*Primary cluster:* new `crates/lodestone-server/src/patrol.rs` (the `PatrolSpawner` port — 92 lines
of vanilla, spawn-interval/chance/group-size); the leader-follow goal in
`crates/lodestone-entity/src/ai/goals.rs` + pillager roster registration in `roster/ranged.rs`;
banner equipment on the leader.
*Brokered:* the `tick.rs` insertion beside `NaturalSpawner` (one call, same shape); `mobs.rs` only
if equipment needs a new seam.
*Note:* `spawn_patrols` game rule already exists; `timeline/early_game.json` gates patrol spawns
before tick 120000 — read the gate from the data file, not from the old wiki.
*Screen:* a patrol of pillagers marches past a player in the wild, leader wearing the ominous
banner.
*Unblocks:* V10 (wave spawning reuses the group-spawn machinery; captains reuse the banner).

### Wave 2 — two units, parallel with each other

**V2 — schedule, commute, and panic: the villager Brain package (#231's villager half). Landed** —
see `docs/villager-work-rest-schedule.md`; panic itself landed earlier (`docs/brain-target-acquisition.md`).
*Primary cluster:* new `crates/lodestone-entity/src/brain/villager.rs` (the package builder:
CORE/IDLE/WORK/MEET/REST/PANIC, matching vanilla's own villager package split); new behaviours beside
`brain/behaviors.rs`; new memory consts in `brain/memory.rs`; the schedule mode in
`brain/driver.rs`; `BrainMob` widening in `brain/mob.rs` + the `NavigatingMob` fields
(`ai/navigating_mob.rs`, exclusive for the window).
*Brokered:* a short `mobs.rs` window — `day_time` feed into the sim, the POI-query closure handed to
`NavigatingMob`, `brain_for("villager")` switched from `scaffold` to the villager package.
*Depends on:* V1's `PoiHandle` (can start immediately against a stub query trait; the seam is in the
entity crate, the impl in the server crate).
*Scope:* schedule-driven activity switching (`work@2000 / meet@9000 / idle@11000 / rest@12000` from
`timeline/villager_schedule.json`), `AcquirePoi` walks for job site / bed / bell, commute
(`SetWalkTargetFromBlockMemory`), bed sleep with the sleeping pose at REST, PANIC on hurt
(`HURT_BY` already exists; `VillagerPanicTrigger`). **Not** golem summoning (§6), **not**
`HarvestFarmland`/`UseBonemeal` (§6).
*Screen:* a village of spawned villagers visibly commutes — to workstations at dawn, to the bell at
9000, to beds at nightfall, lying down; hit one and it flees.

**V4 — trade generation and the merchant session, server side (#245's core).**
*Primary cluster:* generated trade + trade-set tables in `crates/lodestone-data` (from
`villager_trade/**.json` and `trade_set/**.json`, generate-or-assert, committed);
`crates/lodestone-server/src/villager.rs` (offer generation per profession/level, uses/demand
state); merchant menu slots in `container_click.rs`.
*Brokered:* the `InteractEntity` arm in `server.rs` (right-click villager → open merchant menu +
send offers — the taming dispatch is the template); `encode_merchant_offers` + the merchant
`OPEN_SCREEN` in `v770/src/server_protocol.rs`; routing the stranded `SelectTrade` decode into a
real `ServerBound` variant (it currently decodes and discards — a two-file join, grep the packet id).
*Depends on:* V1 (professions decide the table); V3 for pixels — V4 and V3 are the two halves of one
on-screen outcome and should be co-scheduled, V3 first.
*Screen:* right-click an employed villager → the trade screen opens with that profession's real
level-1 offers; executing a trade moves items and increments uses.

### Wave 3 — three units; V5 → V6 sequential (same file), V8 parallel

**V5 — gossip and reputation (#244 + #246's base).**
*Primary cluster:* new `crates/lodestone-server/src/gossip.rs` (the `GossipContainer` port: typed
entries with per-type weight/max/decay from vanilla's own gossip-type table, daily decay, `transferFrom` with
per-transfer decay and the discard threshold of 2, `getReputation` as the weighted sum); its
integration in `villager.rs` (price adjustment via each trade's `reputation_discount` field —
note the JSONs carry the discount factor per trade, not a global formula).
*Brokered:* gossip exchange at MEET — either a sim-side proximity census in the `mobs.rs` window or
a V2 behaviour hook; prefer the sim-side census (no new `BrainMob` surface).
*Depends on:* V4 (prices are the screen); V2's MEET activity makes exchange observable but a
proximity census works without it.
*Screen:* punch a villager, its prices for you rise; trade repeatedly, they fall.

**V6 — restock, XP, and levelling (#245's remainder).** Same owner as V5 or strictly after —
both edit `villager.rs`.
*Scope:* per-trade XP from the trade JSONs, level thresholds and the badge via the
`VillagerData` level field (V1's metadata variant carries it), restock when working at the claimed
POI (vanilla's own restock semantics: uses reset, demand recomputed), locked-out offers.
*Screen:* a sold-out trade shows the red X, the villager works at its site, the trade reopens; the
badge upgrades on level-up.

**V8 — wandering trader (#240).**
*Primary cluster:* new `crates/lodestone-server/src/wandering_trader.rs` (the
`WanderingTraderSpawner` port: 1200-tick check, decaying `spawnDelay`, spawn-chance ramp 25→75,
spawn near a random player, `DespawnDelay` 48000); llama-escort spawn; ware generation from the
`trade_set/wandering_trader` tables (V4's generated data).
*Brokered:* the `tick.rs` spawner insertion; new persisted fields in `world_state.rs` — match the
key names and types in the vanilla-written `.cache/mc/survival/world/data/minecraft/
wandering_trader.dat`, and note 26.2 stores this as its own `.dat`, not in `level.dat`.
*Depends on:* V4 (the merchant session path is shared — vanilla's `AbstractVillager` split; make the
trading arm take "a merchant", not "a villager"); V3 for pixels.
*Scope cut:* llamas spawn as escorts but are not leashed (leashing is its own issue) and the
drink-invisibility defence waits for V7's mob-effects substrate (§6).
*Screen:* a trader and two llamas appear near a player on the announced cycle; right-click trades
from the wandering-trader tables; both despawn later.

### Wave 4 — two units, parallel

**V7 — mob status effects and curing (#247).**
*Primary cluster:* `crates/lodestone-server/src/villager.rs` (conversion state machine) and a
dedicated **exclusive `mobs.rs` window** for the substrate: `ActiveEffects` on `SimMob` (the type in
`mob_effects.rs` is already mob-agnostic — a string-keyed `BTreeMap`), ticked in `MobSim::tick`;
extend `commands/effect_command.rs` to target mobs (its doc already names the gap).
*Scope:* weakness + golden apple on a zombie villager (the interact arm exists from taming) starts
the randomised 3600–6000-tick timer with the shaking cue (metadata converting flag — oracle first)
and the sound; conversion spawns a villager **consuming the zombie's `VillagerData` NBT** so the
profession survives; grants the cured-discount gossip (major-positive, feeding V5's prices). The
trigger is `/effect give <zv> minecraft:weakness` until splash potions exist (§6).
*Depends on:* V1 (profession round-trip), V5 (the discount is the visible payoff).
*Screen:* a shaking, red-swirling zombie villager becomes a villager with the same robes and
markedly cheaper trades.

**V10 — raids and Hero of the Village (#241b + the HotV slice of #246).**
*Primary cluster:* new `crates/lodestone-server/src/raid.rs` (`Raid` + `Raids`: omen absorption,
wave counts by difficulty from vanilla's own wave-group formula and spawn tables, wave spawn placement, victory/
defeat, the bell); Bad Omen / Raid Omen as *player* effects (the existing player-effect path
suffices — vanilla's own omen-absorption rule converts on village entry, raid starts on Raid-Omen
expiry, **not** the pre-1.21 flow the issue describes); vindicator added to `roster/hostile_melee.rs` (a melee
raider is cheap; see §6 for who is excluded).
*Brokered:* `BOSS_EVENT` encode in `v770/src/server_protocol.rs` (client HUD already draws it);
`tick.rs` raid-manager tick; persisted raid state matching the vanilla `raids.dat` fixture; the HotV
price hook in `villager.rs` (small; sequence behind V6).
*Depends on:* V9 (group spawn + captain banner), V1 (raid center = POI cluster), V5/V6 (HotV
discounts visible).
*Screen:* drink an ominous bottle, enter the village: the boss bar fills, waves of pillagers and
vindicators attack, the bell rings; win → fireworks, the HotV icon, and discounted trades.

### Dependency graph

```
V1 ──→ V2 ──────────────┐
 │                      ├─→ V7
 ├──→ V4 ──→ V5 ──→ V6 ─┤
V3 ──┘        │         └─→ V10 ←── V9
              └──→ V8
```

Concurrency summary: **V1 ∥ V3 ∥ V9**, then **V2 ∥ V4**, then **V5→V6 ∥ V8**, then **V7 ∥ V10**.
`mobs.rs` windows, in order: V1 (short), V2 (short), V7 (the substrate window). `server.rs`
windows: V4 only. `villager.rs` is the new contended file — V4, V5, V6, V7, V10 all touch it, which
is why V5/V6 share an owner and V7/V10's patches there are small and late.

---

## 5. Per-unit traps

Shared, from `CLAUDE.md` — restated against these units:

- **Metadata indices come from `EntityDataIndexOracle.java`**
  (`crates/protocol/v770/oracle-java/`, *not* `scripts/`), every time: the villager-data index is
  now settled at **19** (§7 item 4 — not the 17 a v770 test fixture used to guess; use 19 for the
  `MetadataField::VillagerData` encode arm), the
  zombie-villager converting flag (V7), raider celebrating / pillager charging (V9, V10), the
  trader's drinking flag if V8 ever grows it. Index 18 alone has 37 claimants, four of them `BYTE`,
  with **no census column separating them** — expect to add a `MetadataClass` rather than reuse a
  guard, as the experience-orb precedent required.
- **Never decide NBT carry-through from a name list.** `Offers`, `Gossips`, `VillagerData`, `Xp`,
  `ConversionTime`, `DespawnDelay` must be *consumed-or-passed-verbatim* through
  `SavedEntity::from_nbt`'s existing pattern. `Xp` on a villager is an `Int` that has nothing to do
  with a player's `XpTotal`; the type is part of the key.
- **Port wire formats from `write`/`read`, never from the constructor or field order.** V4's
  `encode_merchant_offers` is the live instance: our own decode's doc records that five
  `MerchantOffer` fields are big-endian `i32`s, not VarInts — and a round trip through our own
  encode+decode is satisfied by two symmetric misunderstandings. Gate against a byte string derived
  from vanilla's own merchant-offers packet writer, with pairwise-distinct field values so a
  transposition cannot survive. Same rule for `BOSS_EVENT` (V10).
- **Permissive `BrainMob` defaults are silent behaviour-killers** (the perception-starvation shape).
  Every new seam method (V2: `day_time`, POI queries, hurt signal) needs a negative control: the
  behaviour asserted *not* to fire when the input is left at its default, against a real
  `NavigatingMob`, not a test double that overrides everything.
- **Discriminating inputs.** The schedule gate must sample day times on *both* sides of each
  keyframe (1999/2001, 8999/9001…), not the middle of a phase. Gossip decay gates need a type whose
  `decayPerDay` differs from its `decayPerTransfer`. Price gates must pick a trade whose
  `reputation_discount` ≠ 0.05 *and* one at 0.05, so a hardcoded factor cannot pass. And the fixture
  corpus must not share one spawn point: POI queries tested only from chunk (0,0) inherit the
  offset-vs-absolute blindness the join-ring bug proved.
- **Two mutually exclusive requirements are two gates.** V4's "uses increment on trade" and "a
  maxed-out trade rejects" need different offer states; V7's "conversion preserves profession" and
  "conversion completes at all" need different timer positions. Don't fold them.
- **Ends-on-screen is per-unit, not per-arc.** V1 without the metadata encode arm is a green-tested
  island — the robe change *is* the deliverable. V4 without V3 is bytes into a client that draws
  nothing — co-schedule them. The `SELECT_TRADE` handler must be greped for by **packet id** in
  `server_protocol.rs` and `server.rs` both — a variant decoding into an ignored arm is stranded
  exactly like a clientbound packet nothing routes.
- **Live-oracle hazards** for any `#[ignore]`d gate: `unique_username`, `NoAI:1b` halts gravity (a
  patrol subject must not be `NoAI`), a summoned entity is not selector-visible until the next tick,
  `tick step` does not advance physics, `minecraft:generic` bypasses armour.

Unit-specific:

- **V1:** the POI table is a jar claim — generate-or-assert against vanilla's own POI-type registration, and remember
  `leatherworker` matches *cauldrons in any fill state* and `meeting` has capacity 32, not 1. The
  villager profession must **not** be assigned to babies or nitwits once those exist; encode the
  precondition now (profession assignment guarded on `profession == none && !baby`).
- **V2:** 26.2's schedule is a **data-driven timeline**, not the old `Schedule` registry —
  transcribing remembered constants is the exact wrong-source trap; `work` really is 2000 but
  `meet` is 9000 (not the wiki's 10; verify each keyframe from the JSON). `Brain`'s schedule tick
  has a 20-tick evaluation gate — a gate asserting the switch on the exact boundary tick will flake;
  assert within the window.
- **V4:** trade JSONs whose `gives` carries loot functions (enchanted books, maps, dyed items)
  cannot be built by our item model yet — **filter them out explicitly and count them in the
  generator output** ("N of M trades modelled") rather than silently skipping; a librarian with no
  book trades is a visible, named gap, not a bug report waiting to happen.
- **V5:** `transferFrom` decays per transfer and discards below 2 — an equal-value fixture cannot
  distinguish merge-max from merge-sum; pick values where the two policies differ (a transfer merge
  keeps the max of the two values, an addition merge sums them with a cap).
- **V7:** classify the *call site*: conversion must survive a chunk save/load mid-timer
  (`ConversionTime` consumed, not name-listed), and the timer is randomised — the gate pins the rng.
- **V8:** the spawner persists across restarts — assert against the vanilla
  `wandering_trader.dat` key names, and remember the biome exclusion tag
  (`WITHOUT_WANDERING_TRADER_SPAWNS`).
- **V9/V10:** vanilla's own raid-join check requires an entity's idle time to stay at or under 2400
  ticks — a raider parked by a test for two minutes silently stops counting toward the wave; keep
  gates short or tick the mob.

---

## 6. Deliberately not built

- **Piglin Brain packages** (bundled into #231): nether bartering shares only the scheduler with the
  villager economy — different sensors (wanted-item), different data (barter loot table), different
  biome. It gets its own plan once this arc proves the package pattern. Splitting halves #231's
  surface without orphaning anything.
- **Iron-golem summoning and construction**: the golem has no goal set (it falls to `FALLBACK`) and
  no combat roster; summoning one today produces a decorative wanderer. `PoiIndex`'s count query is
  *designed* for the golem issue's consumer but the consumer is not built here. Golem-summon-on-hurt
  drops out of #231's scope with it.
- **Splash/lingering potions**: the vanilla weakness-delivery mechanism is a thrown-projectile
  system with its own arc. V7 uses `/effect` on mobs as the trigger; the potion projectile joins the
  projectile track, not this one.
- **Trader drink-invisibility and llama leashing** (#240 slices): the first needs V7's mob-effects
  substrate (revisit after V7 as a small follow-up), the second is the leashing issue's
  entity-attach packet. Neither blocks the trader being visibly alive and tradeable.
- **Ravager, evoker, and witch raid waves**: raids spawn only implemented raiders (pillager,
  vindicator). The wave tables are generated in full from vanilla's own raid-wave data with
  unimplemented types **filtered and counted** — the same named-gap pattern as V4's loot-function
  trades — so later mob work slots in without touching raid logic.
- **`HarvestFarmland`/`UseBonemeal` work behaviours**: WORK is commute-and-restock here. Farmer
  agriculture needs crop-interaction seams the brain does not have and the economy does not require.
- **Hero of the Village as a standalone unit**: it is raid-triggered; building it before raids means
  building a trigger simulator nothing else uses. It lands inside V10 as the victory payoff, priced
  through V5/V6's existing hooks — honouring #246's "separately-triggered, don't fold into the base
  formula" in the opposite direction.
- **A persistence layer faithful to vanilla's own POI manager** — §3.2.

---

## 7. Could not be settled without running code

1. **Whether the survival oracle's `entities/*.mca` contain villagers with `Gossips`/`Offers`** —
   the village census (1 village) makes it plausible. First task of V5: parse the entity regions
   with an independent script; if villagers are there, their gossip NBT is a vanilla-written fixture
   and should be committed with provenance like the XP table was.
2. **The exact merchant `OPEN_SCREEN` interplay** — whether our client opens the (not-yet-built)
   merchant screen cleanly on the menu-type id alone, and what it does *today* (V3's first
   experiment; the container-screen plumbing suggests a generic fallback but nothing proves it).
3. **Pillager combat fidelity for patrols/raids** — the ranged roster registers pillager, but the
   projectile *damage* path's history ("hit detection not implemented" in the roster plan) needs
   re-verification at V9 dispatch; a patrol that cannot hurt anyone still satisfies V9's screen
   deliverable, a raid that cannot is not a raid.
4. **The villager-data metadata index** — **settled: 19, not 17.** The existing v770 test fixture's
   `17` was an unverified guess; the committed `EntityDataIndexOracle` dump
   (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`) records
   `19 Villager.DATA_VILLAGER_DATA 18 VILLAGER_DATA` — index **19**, serializer 18
   (`VILLAGER_DATA`). `ZombieVillager.DATA_VILLAGER_DATA` sits at index **20** (its own
   `DATA_CONVERTING_ID`/`DATA_VILLAGER_DATA_FINALIZED` occupy 19/21 instead — the two
   species never share a concrete type, so this is the same mutual-exclusion shape as
   every other index-18-class collision in this file, not a real ambiguity).
   `crates/protocol/v770/src/packets/metadata.rs`'s decode is unaffected by the wrong
   guess — its `read_entity_metadata` matches `Value::Villager` by **serializer alone**,
   not by index (see that match arm's own comment), so the fixture's `17` decoded
   correctly by coincidence. The **encode** side (this plan's `MetadataField::VillagerData`
   variant, not yet built) must use **19**, or a real vanilla client receiving our
   `SET_ENTITY_DATA` would apply the payload to whatever `AgeableMob.AGE_LOCKED`-adjacent
   accessor it has registered at 17 instead, which is a type mismatch on a real client.
   The fixture in `metadata.rs`'s `villager_data_raises_villager_variant` test now pushes
   `19` for this reason, with a comment explaining why the index was decorative for that
   particular test but not for the encoder V1 will write.
5. **How many trade JSONs carry loot functions** (V4's named gap) — a one-line `grep -rl` over
   `villager_trade/` at dispatch time; the count goes in the generator's output either way.
