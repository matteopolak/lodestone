# Villager economy and the Brain substrate

## What it is

The architecture plan for villager behaviour, professions and POI claims, gossip, trades, curing, wandering traders, patrols and raids, treated as one system. It records the substrate decisions and the work that is still open.

## How it works

Decisions already taken:

- **Behaviour runs on the existing `Brain`** (`crates/lodestone-entity/src/brain/`), driven by `BrainGoal` and installed through `brain_for`/`goals_for`. Villager behaviour is not expressed as `Goal`s: the logic is memory-shaped (several small behaviours coordinating through expiring `JOB_SITE`-style memories), and a goal translation layer would be a permanent divergence.
- **Schedule-driven activity switching.** `BrainGoal` calls `Brain::update_activity_from_schedule` with the day time before scanning candidates; `day_time` reaches the brain through `BrainMob`, fed from `WorldState` through `MobSim`.
- **Starved seams are the failure mode.** Every new `BrainMob` method with a permissive default (`None`/`false`/`0`) is a behaviour that silently never fires, so each unit needs a negative control proving its input arrives.
- **POI state is a derived index in `lodestone-server`**, not a port of the persisted POI store. Existence is a pure function of chunk blocks (block-to-POI-type table, generated from the jar under `LODESTONE_REGEN=1`); only claims are real state, and they live once, on the claiming villager, persisted through its NBT. The jar's `poi/*.mca` files are a validation oracle: a derived set over dense chunks must match them, committed as a fixture with provenance.
- **Economy data is generated** into `lodestone-data` from the jar's `villager_trade`, `trade_set` and `timeline/villager_schedule.json` data files (generate-or-assert, committed). Runtime trade state (uses, demand, price deltas, XP/level) lives on the villager in `lodestone_server::villager_trade`, serialised through the `SavedEntity` `extra` pass-through. Gossip is its own container because the wandering trader has none.
- Do not transcribe pre-26.2 schedule constants from memory; the schedule is the jar's keyframe timeline (`idle`, `work`, `meet`, `idle`, `rest` on a 24000-tick clock), matching `Brain::set_schedule`.

## Open work

| area | remaining |
|---|---|
| Brain and schedule | work-at-site restocking, sleeping pose, trade UI at work, unreachable-claim handling, baby schedule, piglin package (own plan) |
| POI claims | durable claim persistence, not a second ownership model |
| Gossip and reputation | persistence; golem response |
| Trading | per-villager mutable offer state made durable (uses, demand, restock) |
| Curing | needs mob status effects (`ActiveEffects` is player-only) and deterministic timers |
| Wandering trader | wares, placement fidelity, despawn state, persistence |
| Patrols and raids | patrol formation; raid waves, boss bar encode (HUD already decodes it), village-hero reward as the raid victory payoff |

Deliberately not built here: iron-golem summoning, splash/lingering potion delivery, trader invisibility and llama leashing, ravager/evoker/witch waves (raid wave tables are generated in full with unimplemented types filtered and counted), farmland work behaviours.

## How to change it

- `VILLAGER_DATA` metadata is index 19, serializer 18 (zombie-villager's accessor is 20), per the committed entity-data-index dump. Never hand-count an index.
- Five `MerchantOffer` fields are big-endian `i32`, not VarInts.
- The server cannot yet send villager-data metadata unless `lodestone_server::protocol::MetadataField` has the villager variant; use the generic `encode_set_entity_data`, not a single-purpose encoder.
- Save data for wandering traders and raids is per-name `.dat` files under the world's `data/`, not `level.dat` fields; the oracle world at `.cache/mc/survival/world` has `wandering_trader.dat` and `raids.dat` as reference fixtures.
- Whether the oracle world's entity regions contain villagers with `Gossips`/`Offers` is unverified; check with an independent NBT parser before relying on it.

## Dependencies

`lodestone-entity` (`brain`), `lodestone-server` (`mobs`, `world_state`, `entity_storage`, `villager_trade`), `lodestone-data` (generated tables), [mob-ai-roster](./mob-ai-roster.md).
