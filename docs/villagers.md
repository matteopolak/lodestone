# Villagers

## What it is

The villager subsystem: professions and job-site/bed/bell claiming, per-profession trade
generation and the demand/restock economy behind it, gossip-driven reputation and its effect
on prices, the WORK/MEET/REST daily schedule, zombie-villager curing, the wandering trader,
and iron/snow golem construction. It also documents the unrelated workstation-container
economy (anvil, grindstone, smithing table, enchanting table, loom, stonecutter) for lack of
a better home — that machinery has no villager involvement at all; see its own section below.

## How it works

All villager-specific code lives under `crates/lodestone-server/src/mobs/villager/` plus
hooks on `MobSim` in `mobs/mod.rs`.

### Professions and workstation/bed/bell claiming

`villager/mod.rs` holds the `Profession` enum, the workstation-block ↔ POI-type ↔ profession
tables (transcribed from vanilla's own POI-type and profession bootstrap tables), and leveling
(transcribed from vanilla's own villager-data leveling table). Claims are ticket accounting on top of
`crate::poi_storage::PoiRecord` , not a parallel
claimed-by-uuid table:

| ledger | POI type | tickets | tick driver |
|---|---|---|---|
| `WorkstationClaims` | job-site block | 1 | `tick_villager_professions` |
| `BedClaims` | home POI kind (`#minecraft:beds`) | 1 | `tick_villager_beds` |
| `BellClaims` | meeting POI kind (`minecraft:bell`) | 32 (a crowd, not a queue of one) | `tick_villager_bells` |

Each pass runs independently every sim tick (job site, bed and bell are three separate vanilla
memories): an unclaimed villager whose search cooldown expired scans a bounded cube and claims
the nearest free one; a claimed villager has its block re-checked every tick and loses the claim
the instant it no longer matches (destroyed or replaced) — there is no block-place/break event
hook, so this re-verification is the only detector, at most one tick of lag. A bed with a free
ticket is still skipped while its block state reads `occupied=true`. A claimed bed **is**
vanilla's occupancy signal for raids (occupancy flips true the moment a ticket is
taken, independent of anyone lying down) — `MobSim::occupied_homes_in_range` is the live query a
raid trigger reads; bells feed `MEET` only (below) and are not needed for the raid trigger.

None of the three ledgers persist to disk — a restart loses every claim and every villager
re-scans from scratch, and a disk-backed occupancy read alone can never see a claim made only
through this module.

A professioned villager's texture reaches the client via `MetadataField::VillagerData` on every
snapshot. Interacting with one short-circuits ahead of taming: a professioned villager with a
non-empty trade pool returns `InteractOutcome::OpenTrade`, and `open_merchant_screen` sends a
real `open_screen` + `merchant_offers`.

### Trade generation

`lodestone_data::villager_trades` (`crates/lodestone-data/src/villager_trades.rs`) holds every
`TradeRecord` for all thirteen workstation professions, all five levels, transcribed from the
26.2 jar's `data/minecraft/{villager_trade,trade_set,tags/villager_trade}`. `pool_for(profession_path,
level)` resolves a pool (including nested tags — `armorer`/`toolsmith`/`weaponsmith` pull in
`#minecraft:common_smith/level_N`) keyed by the bare registry path string, since this crate sits
below `lodestone-server` and cannot name its `Profession` type. Records that compute part of
their result at runtime (enchanted books, cartographer maps, enchanted-weapon/tipped-arrow
trades) are excluded rather than given invented numbers; a handful of profession/level
combinations resolve to nothing portable at all. `crate::mobs::villager::trades` (what
`open_merchant_screen` actually reads) is a thin delegation onto this table, so all thirteen
professions are reachable, not just farmer.

**Selection is not vanilla's RNG.** `offers_for`/`offers_up_to` take a pool's first `amount`
entries in the tag's declared order rather than reproducing vanilla's seeded `RandomSequence`
sampling. The generated *numbers* are exact; the generated *subset* is not what a real vanilla
server with the same seed would offer.

### Trade economy: demand, restock, purchase

`lodestone_server::villager_trade` (`crates/lodestone-server/src/villager_trade.rs`) is the
dynamic half: `OfferState` ports `MerchantOffer`'s mutable fields (`uses`, `demand`,
`special_price_diff`) and its price formula (`modified_cost_a_count`); `RestockState` ports
`Villager`'s restock cadence; `VillagerTrades` bundles an offer list with restock state behind
`try_trade`/`maybe_restock`. `price_multiplier` (each record's `reputation_discount` field,
`0.05` or `0.2`) is plain demand elasticity, not gossip — repeated buying raises price on its
own, with no reputation involved. `OfferState::update_demand` must run before `reset_uses`; it
reads the pre-reset count.

`ServerBound::SelectTrade` is decoded and dispatched (`crate::server::attempt_villager_trade`),
tracked per-connection via `OpenMerchant`, and consumes/gives items directly from the player's
hotbar+main inventory. **This is a disclosed simplification**: it does not go through
`VillagerTrades::try_trade` and does not use live demand pricing — a villager is a `SimMob`, not
a block entity, so it has none of the slot-sync machinery a real payment-slot UI would need, and
nothing yet ties a persistent, demand-adjusted `VillagerTrades` to a specific `SimMob` (every
purchase reads the static per-level table fresh, `uses`/`demand` reset each call). Restock
(`maybe_restock`) has no caller — vanilla's own trigger lives in the `WorkAtPoi` Brain behavior,
off limits here. Both call sites (`open_merchant_screen` and the purchase dispatch) build their
offer list through `crate::server::priced_villager_offers`, which folds reputation and Hero of
the Village into `special_price_diff` before either side reads a price, so the displayed and
charged prices always agree. No offer state persists to disk.

### Gossip and reputation

`villager/gossip.rs` ports vanilla's `GossipContainer`/`GossipType`: five kinds
(`MajorNegative`/`MinorNegative`/`MinorPositive`/`MajorPositive`/`Trading`), each with its own
weight/max/decay constants — `major_positive` never decays. `add`/`decay`/`transfer_from`/
`reputation` are the primitives everything else builds on; **reputation is the signed-weighted
sum**, not a raw stored count, since weight can be negative. `SimMob` carries one
`GossipContainer` per mob plus a 24000-tick decay cadence (matching vanilla's own gossip-decay
cadence, inline in
the per-mob tick loop). `villager/reputation.rs` has `apply_reputation_event` (the four-branch
gossip write vanilla's own reputation-event handler does) and `update_special_prices` (the
reputation-discount-plus-Hero-of-the-Village formula feeding `OfferState::add_special_price_diff`).
The two discounts are independent and additive; Hero of the Village floors at a discount of 1.

`MobSim` wires it live: `spread_villager_gossip` runs every 100 ticks over an 8-block-radius
all-pairs scan (an approximation of vanilla's Brain-sensor "meet in village" spread — real Brain
work is out of scope here); `attack_from_player` (called from `crate::server::apply_attack`)
writes `VillagerHurt` gossip onto a hurt villager, or `VillagerKilled` onto every nearby
witnessing villager when one dies; a successful trade calls
`record_reputation_event(..., Trade, player_uuid)`, matching vanilla's own trade-notification hook;
curing a
zombie villager seeds `ZombieVillagerCured` gossip (below). Iron-golem aggression toward a
low-reputation player is **not built** — nothing in vanilla's own iron golem reads gossip or
reputation at all, so there is no mechanism to port. Nothing here persists to disk.

### Zombie villager curing

`villager/conversion.rs` is a pure state machine: `ConversionState` (curing player's UUID plus
remaining ticks), `roll_conversion_ticks` (`random.nextInt(2401) + 3600`, vanilla's 3600–6000
range), and `conversion_progress` (normally 1 tick of progress per tick, occasionally more via a
1% roll that scans nearby iron bars/beds, capped at 14 rolls — kept lazy behind that 1% gate so
the world scan does not run on ~99% of ticks).

`MobSim::interact`'s golden-apple-on-zombie-villager arm: without Weakness, falls through to
`Pass` (matches vanilla — the item is not consumed either way); with Weakness, starts a real
`ConversionState`, swaps Weakness for Strength, and queues the cure sound.
`MobSim::tick_with_terrain` subtracts `conversion_progress` from the remaining ticks each tick
for every mob with a live state; on completion the entity type flips to `minecraft:villager`,
stats recompute, gossip seeds via `apply_reputation_event(..., ZombieVillagerCured, starter)`
(a predicted +125 reputation), Nausea applies for 200 ticks, and the conversion sound fires.
Profession/level/xp need no carry-over — they are already generic `SimMob` fields. Not built:
natural zombie-villager spawning with conversion behaviour, and the initial random-profession
roll a naturally-spawned zombie villager would get.

### Daily schedule: WORK / MEET / REST

A real day/night activity schedule, read from 26.2's own
`data/minecraft/timeline/villager_schedule.json`: `IDLE` (morning/late night), `WORK` at a
claimed workstation from tick 2000, `MEET` at a claimed bell from tick 9000, `REST` at a claimed
bed from tick 12000. `PANIC` (hurt/hostile-nearby flee, see `docs/mob-ai.md`) already existed;
this adds the other four. Claim positions flow `SimMob::{workstation,bed,meeting_point}` →
`MobSim::feed_perception` (BlockPos → block-centre `Vec3`) → `NavigatingMob` setters → the
`BrainMob` trait seam → `VillagerPoiSensor` → the `JOB_SITE`/`HOME`/`MEETING_POINT` memories →
`Brain::update_activity_from_schedule` + `WalkToPoi` + `MoveToTargetSink` → a real position
change on a spawned villager.

`WalkToPoi::new(source_memory, speed, close_enough)` (vanilla's own close-enough radii: 9 for
job site, 6 for bell, 1 for bed) writes `WALK_TARGET` when farther than that from the memory's
position; two disclosed cuts against the jar: no intermediate-point walk for a very distant
target, and no abandonment of an unreachable claim (it retries forever). `Brain::has_schedule()`
guards the schedule check so non-schedule species (goat, warden, …) are unaffected, and the
check is skipped while `PANIC` is active so a hurt villager's schedule cannot override its flee
— `villager_brain()`'s candidate list is `[PANIC]` alone, since leaving `IDLE` in would fight the
schedule every non-check tick.

**The activities are commute-only.** A villager walks to its claim and stops; it does not run
the harvest/restock animation, villager-initiated trade UI, the sleeping pose, or bell
socialising — only the day/night switch and the walk are real. No baby-villager schedule track
(`PLAY` instead of `WORK`/`MEET`) is read; every villager uses the adult track.

### Wandering trader

`MobSim::spawn_wandering_trader` spawns a real `minecraft:wandering_trader` plus 1–2
`minecraft:trader_llama` escorts at fixed ±2 block offsets, leashed via `LeashHolder::Mob` (see
`docs/entity-physics.md`) — reused rather than duplicated. `MobSim::run_wandering_trader_spawn_cycle`
(in `mobs/mod.rs`, not the version-free `mob_spawn.rs`, since it needs `ChunkWorld`/player
position) ports vanilla's 1200-tick poll, 24000-tick base delay, 25%→75% climbing spawn chance,
and 48-block/10-attempt position search exactly, gated on the `spawn_wandering_traders` game
rule and called once per tick from `run_tick_loop`.

Disclosed gaps: no meeting-POI search (always around a random online player, never a
village), no biome exclusion, no space/collision check, no despawn timer/wander target/home
position, no persistence across a restart. **No wares at all** — no merchant-offer/trade-table
model for the trader, so a spawned one has nothing to trade; a separate feature. The "drinks
invisibility at night, milk by day" behaviour is a day/night-cycle goal in vanilla (a plain
light-level/time check, not the entity's own state), not damage-triggered as sometimes assumed, and is
unbuilt — it needs a generic use-item-under-a-predicate goal and a time-of-day read, neither of
which exist in `lodestone_entity::ai` yet.

### Golem construction

`MobSim::try_construct_golem` (matching internals in `mobs/golem.rs`) ports vanilla's own
carved-pumpkin golem-summon check and its underlying block-pattern-matching
engine directly: a pattern is a grid of predicates in local (right, down, forward) axes, and the
matcher brute-forces every position in a bounded cube against all 24 valid axis orientations —
not "upright only", since a golem really can be built lying on its side against a wall in
vanilla. Two patterns, read verbatim from vanilla's own snow- and iron-golem shape definitions:

| golem | shape | blocks consumed |
|---|---|---|
| snow | pumpkin over two snow blocks | 3 |
| iron | pumpkin on a T of three iron blocks + one centred below | 5 |

Snow is tried first and returns on a match. `MobSim` has no block-write authority (its world is a
read-only reference), so `try_construct_golem` is a pure detection query over a caller-supplied
block-lookup oracle; `GolemConstruction::consumed` (which **includes the pumpkin cell**) is a
report for the caller to clear, not an action — `server.rs`'s `apply_use_item_on` writes those
cells to air and folds them into the normal block-update notify list. The spawned golem goes
through the normal `spawn_species` path, so it gets the same goal set as any other spawn.
Vanilla's own player-created flag (suppresses the golem attacking the player who angered it) is not
modelled — no per-golem flag exists. The vanilla village-POI-count gate does **not** apply here
at all; that gate belongs to a separate, unbuilt natural village-golem spawn system.

### Workstation economy: anvil, grindstone, smithing table, enchanting table, loom, stonecutter

Unrelated to villagers, but documented here for lack of a better home: the server-side maths and
click wiring for these six container screens (client-side menu shape/layout is a separate, undocumented concern here). One pure-logic module per station reads a shared
`enchantment_data.rs` registry (43 enchantments — weight, max level, cost curve, anvil fee,
exclusive sets, curse/treasure membership — plus a 77-item enchantable census) rather than each
carrying its own copy: `anvil.rs` (repair-with-material, repair-by-combining, rename, prior-work
penalty, too-expensive cap, plus the grindstone's strip/combine-repair/XP-refund, since it reuses
the anvil's durability formula), `smithing.rs` (netherite upgrade, 12 recipes; armour trim, 18
patterns × 11 materials), `enchanting.rs` (real 32-position bookshelf-ring geometry, per-slot
level cost, weighted-draw offer selection), `loom.rs` (one banner pattern layer from a specific
pattern item — 10 items, 2 of which do **not** map to the identically-named pattern — or the
32-pattern base grid in tag-file order), and `stonecutting.rs` (filters
`crate::crafting::recipe_book()`'s `Recipe::Stonecutting` entries by ingredient, sorted by
recipe id).

None of the six is a real block entity — input slots are scratch space
(`PlayerInventory::workstation`) cleared back to the player on `ContainerClosed`.
`container_click.rs`'s `MenuKind::ItemCombiner { inputs, station }` covers anvil/grindstone/
smithing/loom/stonecutter (`Station` picks per-station placement rules, quick-move ranges, and
how a take consumes input cells); `MenuKind::Enchanting` has two cells and no result slot — the
item enchants in place. XP is charged outside `container_click` entirely, in the
`ContainerClicked`/`ContainerButtonClick` handlers in `server.rs`, from the pre-click cells,
keeping both `MenuKind`s themselves economy-free.

Known gaps: enchantment identity has no synced client registry, so a real client cannot show an
enchantment's *name* yet (the glint still renders); the anvil's own 12% degrade chance is not
modelled (cosmetic only); neither station's offer-button order is proven to match vanilla's real
registration order (harmless for the common single-option case).

## How to change it

- **A new profession's trades**: re-run extraction against
  `.cache/mc/26.2/src/data/minecraft/{villager_trade,trade_set,tags/villager_trade}/<profession>/`
  into `villager_trades.rs` (each record cites its own source path); the `xp` field's codec
  default is `1`, not `0`, when the JSON omits it — a hand-guessed `0` is the trap. A new
  profession-block mapping extends `poi_type_for_block`/`profession_for_poi_type` in
  `villager/mod.rs`; a new `ReputationEventType` arm touches `reputation.rs`'s match, not
  `MobSim`; a new golem shape is one `GolemCell` pattern constant plus a match arm in
  `try_construct_golem`.
- **A new WORK/MEET/REST behaviour** (harvest animation, sleep pose, bell socialising) is a new
  priority slot inside `villager_brain`'s relevant `add_activity` call. A new schedule-driven
  species calls `Brain::set_schedule` in its own roster constructor and feeds `day_time()`;
  `has_schedule()`'s guard means nothing else changes. Widening `WalkToPoi`'s fidelity (far-
  target intermediate walk, unreachable-claim abandonment) is one change every WORK/MEET/REST
  activity picks up for free.
- **A sixth workstation-economy station**: a `Station` variant (or new `MenuKind` if it has no
  result slot), a block-name entry in `apply_use_item_on`'s dispatch table, a
  `workstation_menu_type`/`container_title` entry, and a pure compute module alongside the
  existing ones. A new stonecutting recipe needs no code change — it loads automatically from
  `assets/recipe/` once bundled.
- **`MetadataField` additions are exhaustively enforced** — `V770ServerProtocol::encode_set_entity_data`
  has no `_ =>` arm on purpose, so a new field must be encoded or the crate fails to compile.
- **Gotchas**: `free_tickets` absent on a `PoiRecord` means zero, not unclaimed;
  `OfferState::update_demand` must run before `reset_uses`; `count_nearby_special_blocks` must
  stay behind `conversion_progress`'s 1% gate; a golem's consumed-blocks report includes the
  pumpkin cell; nothing here persists across a restart — claims, offer state (uses/demand/
  special-price-diff), gossip/reputation, and the wandering-trader spawn cycle are all
  session-only until wired into on-disk storage.

## Configuration

No env vars or feature flags anywhere in this subsystem except the `spawn_wandering_traders`
game rule. All constants below live in `crates/lodestone-server/src/mobs/mod.rs` unless noted.

| constant | value | transcribed? |
|---|---|---|
| `JOB_SEARCH_INTERVAL_TICKS`/`BED_SEARCH_INTERVAL_TICKS`/`BELL_SEARCH_INTERVAL_TICKS` | 100 ticks each | scope choice |
| `villager::SEARCH_RADIUS` | 16 blocks | scope choice — vanilla's real `PoiManager` search is ~48 blocks and spatially indexed |
| `GOSSIP_SPREAD_INTERVAL_TICKS` / `GOSSIP_SPREAD_RADIUS_SQR` / `VILLAGER_KILLED_WITNESS_RADIUS_SQR` | 100 ticks / 8 blocks | scope choice |
| `RESTOCK_COOLDOWN_TICKS` / `HALF_DAY_TICKS` / `MAX_RESTOCKS_PER_DAY` | 2400 / 12000 / 2 | vanilla |
| `CONVERSION_WAIT_MIN`/`MAX`, `MAX_SPECIAL_BLOCKS_COUNT`, `SPECIAL_BLOCK_RADIUS` | 3600–6000 ticks | vanilla |
| `VILLAGER_SPEED_MODIFIER` (`brain/roster.rs`) | 0.5 | vanilla |
| `VILLAGER_SCHEDULE` (`brain/roster.rs`) | the keyframe table | vanilla |
| WORK/MEET/REST close-enough radii (inline per `WalkToPoi::new` call) | 9 / 6 / 1 | vanilla |
| wandering-trader cycle (inline in `run_wandering_trader_spawn_cycle`) | 1200-tick poll, 24000-tick base delay, 25%→75% climbing chance, 48-block/10-attempt search | vanilla |

## Dependencies

`crate::poi_storage` (ticket accounting),
`crate::mobs::world::ChunkWorld` (terrain scans), `lodestone_entity::brain` (schedule/activity
machinery), `lodestone_entity::attribute::default_attributes` (`"wandering_trader"`/`"iron_golem"`
TypeSpecs), `crate::effects` (cure/conversion sounds), `crate::world_state::WorldStateHandle`
(the day-time feed), `MobSim::try_leash`/`LeashHolder::Mob` (the wandering trader's llama escort), `lodestone_data::villager_trades`/`item_prototypes` (the static trade
table and cost-item stack sizes), and `crate::protocol::{MetadataField, MerchantOfferOut,
ServerProtocol::encode_merchant_offers}` (implemented only by `crates/protocol/v770`'s
`V770ServerProtocol`). The workstation economy additionally depends on `crate::container_click`,
`crate::inventory`, `crate::experience::PlayerExperience`, and `crate::mob_spawn::SpawnRng`
(matches vanilla's draw order and count, not its bit stream).
