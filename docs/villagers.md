# Villagers

## What it is

The villager subsystem: professions and job-site, bed and bell claiming, per-profession trade generation and the demand/restock economy, gossip reputation and its price effect, the WORK/MEET/REST schedule, zombie-villager curing, the wandering trader and golem construction. It also covers the six workstation-container screens (anvil, grindstone, smithing table, enchanting table, loom, stonecutter), which involve no villagers.

## How it works

Villager code lives under `crates/lodestone-server/src/mobs/villager/` plus hooks on `MobSim` in `mobs/mod.rs`.

### Claims

`villager/mod.rs` holds the `Profession` enum, the workstation block, POI type and profession tables, and leveling. Claims are ticket accounting on `crate::poi_record::PoiRecord`:

| ledger | POI | tickets | driver |
|---|---|---|---|
| `WorkstationClaims` | job-site block | 1 | `tick_villager_professions` |
| `BedClaims` | beds tag | 1 | `tick_villager_beds` |
| `BellClaims` | bell | 32 | `tick_villager_bells` |

Each pass runs every sim tick: an unclaimed villager past its search cooldown scans a bounded cube and claims the nearest free one; a claimed villager re-checks its block each tick and loses the claim when it stops matching (no block-change hook exists, so this is the only detector). A bed whose state reads `occupied=true` is skipped even with a free ticket. A claimed bed is the occupancy signal for raids (`MobSim::occupied_homes_in_range`). None of the ledgers persist, so a restart loses every claim.

The profession texture reaches clients through `MetadataField::VillagerData`. Interacting with a professioned villager that has trades returns `InteractOutcome::OpenTrade`, and `open_merchant_screen` sends `open_screen` plus `merchant_offers`.

### Trade generation

`lodestone_data::villager_trades` holds every `TradeRecord` for all thirteen workstation professions and five levels, transcribed from the jar's `villager_trade`, `trade_set` and `tags/villager_trade` data. `pool_for(profession_path, level)` resolves a pool (including nested tags such as the shared smith levels), keyed by bare registry path because the crate sits below `lodestone-server`. `VillagerLevel` accepts only `1..=5`. Records computing part of their result at runtime (enchanted books, cartographer maps, enchanted weapons, tipped arrows) are excluded rather than given invented numbers.

Selection is not the reference RNG: `offers_for`/`offers_up_to` take a pool's first `amount` entries in tag order. The numbers are exact; the subset differs from a real server with the same seed.

### Economy

`lodestone_server::villager_trade` is the dynamic half. `OfferState` holds `uses`, `demand`, `special_price_diff` and the price formula; `RestockState` the restock cadence; `VillagerTrades` bundles offers with restock behind `try_trade`/`maybe_restock`. A record's `reputation_discount` (0.05 or 0.2) is demand elasticity, not gossip. `OfferState::update_demand` must run before `reset_uses`.

`ServerBound::SelectTrade` dispatches to `crate::server::open_containers::attempt_villager_trade`, tracked by `OpenMerchant`. Each employed villager owns a persistent `VillagerTrades` (`SimMob::ensure_trades`), restocked from the mob tick. Screen and purchase both go through `MobSim::villager_offers` / `try_villager_trade`, which fold reputation and Hero of the Village into `special_price_diff` first, so displayed and charged prices agree. Offer state does not persist.

### Gossip

`villager/gossip.rs` has five gossip kinds (major/minor negative, minor/major positive, trading) with weight, max and decay constants; major positive never decays. Reputation is the signed weighted sum, not a raw count. Each `SimMob` carries a container with a 24000-tick decay cadence. `villager/reputation.rs` has `apply_reputation_event` and `update_special_prices` (reputation discount plus Hero of the Village, additive, floored at a discount of 1).

Live wiring: `spread_villager_gossip` runs every 100 ticks over an 8-block all-pairs scan (an approximation of the brain-sensor spread); `attack_from_player` writes hurt gossip, or killed gossip to every witness on death; a trade calls `record_reputation_event(..., Trade, player_uuid)`; curing seeds cured gossip. Iron golems do not react to reputation, so there is nothing to port. Gossip does not persist.

### Zombie villager curing

`villager/conversion.rs`: `ConversionState` (curer UUID, remaining ticks), `roll_conversion_ticks` (3600 to 6000) and `conversion_progress` (1 per tick, more via a 1% roll scanning nearby iron bars and beds, capped at 14, lazy so the scan skips ~99% of ticks). In `MobSim::interact`, a golden apple without Weakness passes; with Weakness it starts the state, swaps Weakness for Strength and queues the sound. `tick_with_terrain` subtracts progress; at zero the type flips to `minecraft:villager`, stats recompute, cured gossip seeds (+125 predicted reputation), Nausea applies for 200 ticks and the sound plays. Not built: natural zombie-villager spawning and its random profession roll.

### Schedule

The day schedule comes from the jar's `timeline/villager_schedule.json`: `WORK` at a claimed workstation from tick 2000, `MEET` at a bell from 9000, `REST` at a bed from 12000, `IDLE` otherwise; `PANIC` (see [mob AI](mob-ai.md)) pre-exists. Claim positions flow `SimMob::{workstation,bed,meeting_point}` to `MobSim::feed_perception` to `NavigatingMob` setters to the `BrainMob` seam to `VillagerPoiSensor` to the `JOB_SITE`/`HOME`/`MEETING_POINT` memories to `Brain::update_activity_from_schedule` plus `WalkToPoi` and `MoveToTargetSink`.

`WalkToPoi::new(source_memory, speed, close_enough)` (radii 9 job, 6 bell, 1 bed) writes `WALK_TARGET` beyond that radius. Cuts: no intermediate walk for distant targets, and an unreachable claim is retried forever. `Brain::has_schedule()` keeps non-schedule species unaffected; the check is skipped while `PANIC` is active, and `villager_brain()`'s candidate list is `[PANIC]` alone so `IDLE` cannot fight the schedule.

The activities are commute-only: no harvest or restock animation, villager-initiated trading, sleeping pose or bell socialising, and no baby (`PLAY`) track.

### Wandering trader

`MobSim::spawn_wandering_trader` spawns the trader plus 1 to 2 trader llamas at fixed offsets of 2, leashed via `LeashHolder::Mob` (see [entity physics](entity-physics.md)). `run_wandering_trader_spawn_cycle` (in `mobs/mod.rs`, since it needs `ChunkWorld` and a player position) follows the 1200-tick poll, 24000-tick base delay, 25% to 75% climbing chance and 48-block/10-attempt search, gated on the `spawn_wandering_traders` game rule and called once per tick from `run_tick_loop`.

Gaps: no meeting-POI search (always near a random online player), no biome exclusion or space check, no despawn timer, wander target, home or persistence, and no wares (no trade model for the trader). The invisibility-at-night, milk-by-day behaviour needs a use-item-under-a-predicate goal and a time-of-day read, neither of which exists.

### Golem construction

`MobSim::try_construct_golem` (internals in `mobs/golem.rs`) runs a pattern matcher: a grid of predicates in local axes, brute-forced over a bounded cube against all 24 axis orientations (a golem can be built lying down).

| golem | shape | consumed |
|---|---|---|
| snow | pumpkin over two snow blocks | 3 |
| iron | pumpkin on a T of three iron blocks plus one centred below | 5 |

Snow is tried first. `MobSim` has no block-write authority, so this is a pure query over a caller-supplied block oracle; `GolemConstruction::consumed` (including the pumpkin cell) is a report that `apply_use_item_on` in `server/use_item_on.rs` writes to air and folds into the update list. The golem spawns through `spawn_species`. The player-created flag is not modelled, and the village-POI-count gate belongs to the unbuilt natural golem spawn.

### Workstation economy

One pure module per station reads a shared `enchantment_data.rs` registry (43 enchantments with weight, max level, cost curve, anvil fee, exclusive sets, curse and treasure membership; 77 enchantable items):

- `anvil.rs`: repair with material, combine, rename, prior-work penalty, too-expensive cap, plus grindstone strip, combine and XP refund.
- `smithing.rs`: netherite upgrade (12 recipes) and armour trim (18 patterns by 11 materials).
- `enchanting.rs`: the 32-position bookshelf ring, per-slot cost, weighted offer selection.
- `loom.rs`: one banner layer from a pattern item (10 items, 2 of which do not map to the identically named pattern) or the 32-pattern base grid in tag order.
- `stonecutting.rs`: `Recipe::Stonecutting` entries from `crate::crafting::recipe_book()` by ingredient, sorted by recipe id.

None is a block entity: input slots are scratch (`PlayerInventory::workstation`) returned on `ContainerClosed`. `container_click.rs` has `MenuKind::ItemCombiner { inputs, station }` for anvil, grindstone, smithing, loom and stonecutter (`Station` picks placement, quick-move ranges and take consumption) and `MenuKind::Enchanting` (two cells, enchants in place). XP is charged in the handlers in `server/container_clicks.rs` from pre-click cells, keeping `MenuKind` economy-free.

Gaps: no synced enchantment registry, so a client cannot show enchantment names (the glint renders); the anvil 12% degrade is not modelled; offer-button order is unproven against the reference.

## How to change it

- **Trades**: re-run extraction against `.cache/mc/<mc-version>/src/data/minecraft/{villager_trade,trade_set,tags/villager_trade}/<profession>/`. The codec default for `xp` is `1`, not `0`.
- **Mappings**: a profession-block mapping extends `poi_type_for_block`/`profession_for_poi_type`; a reputation event touches `reputation.rs`; a golem shape is a `GolemCell` pattern constant plus an arm in `try_construct_golem`.
- **Schedule behaviours**: a new priority slot in `villager_brain`. A new scheduled species calls `Brain::set_schedule` and feeds `day_time()`. Better `WalkToPoi` fidelity benefits every activity.
- **New station**: a `Station` variant (or `MenuKind` if no result slot), a block entry in `apply_use_item_on`, a `workstation_menu_type`/`container_title` entry and a compute module. New stonecutting recipes load from `assets/recipe/` automatically.
- **Metadata**: `encode_set_entity_data` has no `_ =>` arm, so a new `MetadataField` must be encoded to compile.
- **Gotchas**: absent `free_tickets` means zero, not unclaimed; `count_nearby_special_blocks` must stay behind the 1% gate; all state (claims, offers, gossip, trader cycle) is session-only.

## Configuration

Only the `spawn_wandering_traders` game rule. Constants live in `crates/lodestone-server/src/mobs/mod.rs` unless noted:

| constant | value | origin |
|---|---|---|
| job/bed/bell search interval | 100 ticks | scope choice |
| `villager::SEARCH_RADIUS` | 16 blocks (reference is ~48, indexed) | scope choice |
| gossip spread interval / radius, killed-witness radius | 100 ticks / 8 blocks | scope choice |
| `RESTOCK_COOLDOWN_TICKS` / `HALF_DAY_TICKS` / `MAX_RESTOCKS_PER_DAY` | 2400 / 12000 / 2 | reference |
| `CONVERSION_WAIT_MIN`/`MAX`, special-block count and radius | 3600 to 6000 | reference |
| `VILLAGER_SPEED_MODIFIER`, `VILLAGER_SCHEDULE` (`brain/roster.rs`) | 0.5, keyframes | reference |
| WORK/MEET/REST radii (inline) | 9 / 6 / 1 | reference |
| trader cycle (inline) | 1200 poll, 24000 delay, 25% to 75%, 48 blocks, 10 attempts | reference |

## Dependencies

`crate::poi_record` (shared with the native `poi_storage` region files; the claim ledgers run on wasm too), `crate::mobs::world::ChunkWorld`, `lodestone_entity::brain`, `lodestone_entity::attribute::default_attributes`, `crate::effects`, `crate::world_state::WorldStateHandle` (day time), `MobSim::try_leash`, `lodestone_data::{villager_trades, item_prototypes}`, and `crate::protocol::{MetadataField, MerchantOfferOut, ServerProtocol::encode_merchant_offers}` (implemented by the 26.2 family's `V770ServerProtocol`). The workstation economy adds `crate::container_click`, `crate::inventory`, `crate::experience::PlayerExperience` and `crate::mob_spawn::SpawnRng`.
