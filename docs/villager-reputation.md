# Villager gossip and reputation (issues #244, #246)

## What it is

The villager economy's opinion system: what a villager remembers about a UUID (gossip,
issue #244) and the single reputation score that memory reduces to, including its two
real consequences — a trade-price discount/surcharge and Hero of the Village's
additional discount (issue #246). This is the gossip/reputation hook
`docs/villager-trade-generation.md` already named as built-but-uncalled
(`OfferState::special_price_diff`) — this work is the caller.

## How it works

Three new modules, all under `crates/lodestone-server/src/mobs/villager/`:

- **`gossip.rs`** — `GossipType` (the five kinds: `MajorNegative`/`MinorNegative`/
  `MinorPositive`/`MajorPositive`/`Trading`, each with vanilla's own weight/max/decay
  constants) and `GossipContainer`, a port of vanilla's `GossipContainer`/`GossipType`
  (`.cache/mc/26.2/src/net/minecraft/world/entity/ai/gossip/`). `add`/`decay`/
  `transfer_from`/`reputation` are the four operations everything else is built from.
  Pure logic — no dependency on `SimMob`/`MobSim`.
- **`reputation.rs`** — `ReputationEventType` (the five vanilla event kinds) and two
  functions: `apply_reputation_event` (vanilla `Villager.onReputationEventFrom`'s
  four-branch gossip write) and `update_special_prices` (vanilla
  `Villager.updateSpecialPrices`'s reputation-discount-plus-Hero-of-the-Village
  formula, calling `OfferState::add_special_price_diff`). Also pure logic.
- **`conversion.rs`** — issue #247's zombie-villager curing; see
  `docs/zombie-villager-curing.md`. Its own `ConversionState` is what carries the
  curing player's UUID through to `apply_reputation_event`'s `ZombieVillagerCured` arm.

`SimMob` (in `crates/lodestone-server/src/mobs/mod.rs`) carries the live state: a
`gossip: villager::gossip::GossipContainer` field (one ledger per mob, empty for every
non-villager) and `last_gossip_decay_tick: Option<u64>` for the 24000-tick decay cadence
(`Villager.maybeDecayGossip`, ported inline in `MobSim::tick_with_terrain`'s per-mob
loop). `MobSim` adds:

- **`spread_villager_gossip`** — nearby-villager gossip spread (issue #244's "spread
  entries to nearby villagers on meeting"), called every tick from
  `tick_with_terrain`. Approximated as a periodic (`GOSSIP_SPREAD_INTERVAL_TICKS`,
  100 ticks), radius-bounded (`GOSSIP_SPREAD_RADIUS_SQR`, 8 blocks) all-pairs scan
  rather than vanilla's `Sensor`-driven "meet in village" Brain behaviour — that is
  issue #231/#243's remainder (`lodestone-entity`'s Brain package), off limits for
  this change. Both directions of a pair exchange from a pre-transfer snapshot of
  each side, so the second transfer never reads the first transfer's already-updated
  state.
- **`record_reputation_event`**/**`villager_reputation`** — the general entry points a
  caller with a villager id, an event, and a source UUID (or just a player UUID) uses.
- **`attack_from_player`** — `MobSim::attack` plus the villager-reputation half of
  `Villager.setLastHurtByMob`/`die`/`tellWitnessesThatIWasMurdered`: a
  player-identified attacker hurting a villager writes `VillagerHurt` gossip onto
  **that villager's own** ledger; killing one writes `VillagerKilled` gossip onto
  **every nearby witnessing villager's own** ledger instead (the victim is gone).
  A new method rather than a changed signature on `attack`, so `attack`'s other
  callers (every hermetic test that does not care about gossip) stay unchanged.
  `crate::server::apply_attack` (the only production caller of the old `attack`)
  now calls this one, passing `PlayerIdentity { uuid: player_uuid, entity_id:
  LOCAL_PLAYER_ENTITY_ID }` — a real player hit or kill on a villager writes gossip.
- **Trade gossip's producer** — `crate::server`'s `ServerBound::SelectTrade` handler
  (`attempt_villager_trade`'s call site) calls
  `MobSim::record_reputation_event(entity_id, ReputationEventType::Trade, player_uuid)`
  immediately after a purchase succeeds, matching `Villager.notifyTrade`'s own
  `onReputationEventFrom(Trade, buyer, this)` call. `record_reputation_event` existed
  and was tested before this; this is its first production caller.

## How to change it, and the gotchas

- **A raw stored gossip count is not reputation** — `GossipType::weight` is signed, so
  `GossipContainer::reputation` (the `weightedValue` sum) is the only correct read.
  See `gossip.rs`'s own module doc for the full list ("How to change it") — several of
  these are the same pitfalls vanilla's own `GossipContainer` has if read carelessly.
- **The reputation discount and the Hero of the Village discount are independent and
  additive**, and both scale/floor differently — see `reputation.rs`'s own doc.
- **`GossipContainer::decay`/`GossipContainer::transfer_from` are the daily/per-meeting
  steps alone; the cadence (24000 ticks / `GOSSIP_SPREAD_INTERVAL_TICKS`) is the
  caller's job** — this mirrors `lodestone_server::villager_trade`'s own
  `OfferState`/`VillagerTrades` split (mechanics vs. cadence).
- **Adding a new `ReputationEventType` arm** (e.g. `GolemKilled`, currently a no-op —
  vanilla's own `onReputationEventFrom` has no arm for it either) touches
  `reputation.rs`'s match, not `MobSim`.

## What remains (named rather than silent)

- **`update_special_prices` has no live per-villager `OfferState` list to call against
  yet.** `crate::villager_trade`'s own doc already discloses `SELECT_TRADE` is decoded
  and discarded and nothing calls `VillagerTrades::maybe_restock` — this module is
  ready the instant that lands (`crate::server`, off limits for this change). This is
  the one remaining reason the reputation score, though now written by every real
  producer below, still moves no visible price for a player.
- **Iron-golem aggression toward a low-reputation player** (named in issue #246's own
  body) has no evidenced vanilla mechanism — nothing in
  `.cache/mc/26.2/src/net/minecraft/world/entity/animal/golem/IronGolem.java` reads
  gossip or reputation at all. Not built; inventing one would have no jar citation.
- **No on-disk persistence** — matches every other villager-state gap this crate
  already discloses (`villager::WorkstationClaims`, `villager_trade::OfferState`).

## Configuration

No env vars or flags. `GossipType`'s weight/max/decay constants and
`GOSSIP_SPREAD_INTERVAL_TICKS`/`GOSSIP_SPREAD_RADIUS_SQR`/
`VILLAGER_KILLED_WITNESS_RADIUS_SQR` (the latter three this crate's own scope choices,
not transcribed vanilla constants — see their own doc comments) are the only tunables,
all `const`s in `crates/lodestone-server/src/mobs/mod.rs`/`gossip.rs`.

## Dependencies

`villager::gossip` depends on nothing beyond `uuid`. `villager::reputation` depends on
`villager::gossip` and `crate::villager_trade::OfferState`. `crate::mobs::MobSim`
depends on both, plus `crate::effects` for nothing in this feature specifically (that
is `villager::conversion`'s dependency — see `docs/zombie-villager-curing.md`).

## Evidence

| claim | where |
|---|---|
| reputation sums count × signed weight, correctly for both positive and negative types | `mobs/villager/gossip.rs`, `a_single_entry_contributes_its_count_times_weight`, `a_negative_gossip_type_lowers_reputation` |
| repeated additions clamp at the type's own max, not overflow it | `mobs/villager/gossip.rs`, `repeated_additions_clamp_at_the_types_own_max` |
| a value decayed below the discard threshold is removed entirely, entity included | `mobs/villager/gossip.rs`, `a_value_decayed_below_the_discard_threshold_is_removed_entirely` |
| `major_positive` never decays (decay_per_day = 0) — a neuter substituting a nonzero decay would fail only this test | `mobs/villager/gossip.rs`, `major_positive_gossip_never_decays` |
| transfer decays before the discard gate, and merges by max not sum | `mobs/villager/gossip.rs`, `a_transferred_entry_that_decays_below_threshold_is_dropped`, `transfer_merges_by_max_not_by_sum` |
| curing grants both gossip entries at their real predicted sum (125) | `mobs/villager/reputation.rs`, `curing_a_zombie_villager_grants_both_the_major_and_minor_positive_entries` |
| hurting/killing lower reputation by the predicted negative magnitudes | `mobs/villager/reputation.rs`, `hurting_and_killing_a_villager_lower_reputation_by_the_predicted_amounts` |
| the reputation discount scales by each offer's own `price_multiplier`, at two different values so one cannot be copied onto the other | `mobs/villager/reputation.rs`, `reputation_discount_scales_by_each_offers_own_price_multiplier` |
| a negative reputation raises the price (the mirror direction a positive-only test cannot see) | `mobs/villager/reputation.rs`, `negative_reputation_raises_the_price` |
| Hero of the Village floors at 1, and a higher amplifier yields a strictly larger discount at a discriminating `wants_count` | `mobs/villager/reputation.rs`, `hero_of_the_village_never_discounts_by_less_than_one`, `a_higher_hero_amplifier_yields_a_larger_discount` |
| the two discounts are additive | `mobs/villager/reputation.rs`, `reputation_and_hero_of_the_village_discounts_are_additive` |
| a golden apple, `record_reputation_event`, `attack_from_player` and gossip spread all reach a **real spawned `SimMob`** through `MobSim::interact`/`tick`/`attack_from_player`, not just the pure modules in isolation | `mobs/mod.rs`, `villager_gossip_reputation_and_curing_tests` module |
