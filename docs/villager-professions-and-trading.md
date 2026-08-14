# Villager professions and trading (issues #243, #245)

## What it is

Workstation claiming — an unemployed villager finds a nearby job-site block,
claims it, and takes the matching profession — plus trade generation for the
farmer profession, ported from the real 26.2 registry data. Losing the
workstation loses the job. Interacting with a professioned villager opens a
real `minecraft:merchant` screen carrying generated offers.

## How it works

All new code lives under `crates/lodestone-server/src/mobs/villager/`:

- `villager/mod.rs` — the `Profession` enum, the workstation-block ↔ POI-type
  ↔ profession tables (`poi_type_for_block`, `profession_for_poi_type`,
  transcribed from `PoiTypes.bootstrap`/`VillagerProfession.bootstrap`), the
  live claim ledger (`WorkstationClaims`), the bounded nearest-first job
  search (`find_and_claim_workstation`), and leveling
  (`can_level_up`/`max_xp_for_level`/`level_up`, transcribed from
  `VillagerData.java`).
- `villager/trades.rs` — the ported `VillagerTrade` records (farmer only,
  all five levels) and the pool-selection function (`offers_for`/
  `offers_up_to`).

`crate::poi_storage` (`docs/point-of-interest-storage.md`) already carries
every profession POI type's ticket cap (`max_tickets`) and the claim
mechanics (`PoiRecord::acquire_ticket`/`release_ticket`) — that module's own
doc names villager professions as its natural second consumer after portal
lookup, and `WorkstationClaims` is exactly that: a
`HashMap<BlockPos, PoiRecord>` wrapper that claims/releases through the real
record type instead of a parallel claimed-by-uuid table.

`MobSim::tick_villager_professions` (new, in `mobs/mod.rs`) runs once per
sim tick: an unemployed villager whose job-search cooldown has expired scans
a bounded cube around itself for a recognised workstation block and claims
the nearest one with a free ticket; an employed villager has its workstation
re-checked every tick, and loses its profession the moment the block there
no longer matches (destroyed, or replaced with a different workstation
type).

`SimMob::snapshot` pushes a `MetadataField::VillagerData` (index 19,
serializer `VILLAGER_DATA`) for every `minecraft:villager`, unconditionally —
the field a client's `VillagerProfessionLayer` actually reads to pick a
texture. `MobSim::interact` short-circuits ahead of the taming dispatch for
a villager: a professioned villager with a real (non-empty) trade pool
returns `InteractOutcome::OpenTrade { profession, level }`; `crate::server`
turns that into an `open_screen` + `merchant_offers` send
(`open_merchant_screen`, new in `server.rs`).

## How to change it, and the gotchas

- **`free_tickets` absent means zero, not unclaimed** — see
  `docs/point-of-interest-storage.md`. `WorkstationClaims` never re-derives
  this; it only ever constructs fresh `PoiRecord`s via `PoiRecord::new`
  (full tickets) or claims/releases through `acquire_ticket`/`release_ticket`.
- **Adding a profession's trades** means adding a new `const FOO_LEVEL_N`
  table to `villager/trades.rs`, transcribed from
  `.cache/mc/26.2/src/data/minecraft/villager_trade/<profession>/<n>/*.json`
  (order from the matching `tags/villager_trade/<profession>/level_<n>.json`),
  and a `(Profession::Foo, n) => ...` arm in `pool_for`. **The `xp` field's
  codec default is `1`, not `0`, when the JSON omits it** — see
  `TradeRecord::xp`'s own doc comment; a hand-guessed `0` is the trap this
  repo's evidence standard warns about.
- **Selection is not vanilla's RNG.** `offers_for` takes a trade pool's first
  `amount` entries in the tag's declared order rather than reproducing
  vanilla's `RandomSequence`-seeded sampling — see `villager/trades.rs`'s
  module doc for why. The generated *numbers* are exact; the generated
  *subset* is not what a real vanilla server with the same seed would offer.
- **No on-disk persistence.** `WorkstationClaims` is a session-only ledger.
  A restart loses every claim; every villager re-scans from scratch. Wiring
  this into `crate::poi_storage::PoiStorage`'s save/restore path (the way
  `crate::portal::PortalIndex` already is) would touch `crate::integrated`.
- **No block-place/break event hook.** Losing a workstation is detected by
  `tick_villager_professions`'s own re-verification, not a push from
  wherever a block actually breaks — at most one tick of lag in practice.
- **Trade purchase is not wired.** `open_merchant_screen` sends real offers;
  nothing produces a `select_trade` response, so a player can see a trade
  and cannot yet buy one. Restocking and reputation-based pricing are
  likewise unbuilt. This is issue #245's third piece.
- **`VillagerType` (biome flavour) is not derived** — every villager reports
  `minecraft:plains`. Cosmetic only.
- **Adding a `MetadataField` variant is exhaustively enforced.**
  `V770ServerProtocol::encode_set_entity_data`'s match has no `_ =>` arm on
  purpose (see `crates/lodestone-server/CLAUDE.md`'s island-factory warning);
  a new field must be encoded or the crate fails to compile.

## Configuration

`MobSim::JOB_SEARCH_INTERVAL_TICKS` (100) throttles the search; not a
transcribed vanilla constant. `villager::SEARCH_RADIUS` (16 blocks) bounds
the scan — smaller than vanilla's real 48-block `PoiManager` search, since
nothing here backs the scan with a spatial index (see that constant's own
doc comment for the cost argument).

## Dependencies

`crate::poi_storage` (ticket accounting), `crate::mobs::world::ChunkWorld`
(the terrain scan), `crate::protocol::{MetadataField, MerchantOfferOut,
ServerProtocol::encode_merchant_offers}` (the wire seam),
`crates/protocol/v770`'s `V770ServerProtocol` (the one real encoder).

## Evidence

| claim | where |
|---|---|
| a second villager cannot claim an already-claimed workstation, with a control showing a release makes it claimable again | `mobs/villager/mod.rs`, `a_second_villager_cannot_claim_an_already_claimed_workstation` |
| losing the workstation clears the profession, not merely a ticket | `mobs/villager/mod.rs`, `losing_the_workstation_loses_the_job` |
| leveling uses the inclusive `>=` threshold reading, at an input where `>=` and `>` disagree (xp == 10 at level 1) | `mobs/villager/mod.rs`, `leveling_up_at_exactly_the_threshold_uses_the_inclusive_reading` |
| a generated farmer level-1 offer matches the real jar record exactly (wheat/potato-for-emerald) | `mobs/villager/trades.rs`, `farmer_level_1_generates_the_real_wheat_for_emerald_trade` |
| a trade record with no `xp` key generates `xp: 1` (the codec default), not the plausible-but-wrong `0` | `mobs/villager/trades.rs`, `a_trade_missing_its_xp_field_uses_the_codecs_default_of_one_not_zero` |
| an unported profession returns no offers rather than invented ones | `mobs/villager/trades.rs`, `an_unported_profession_returns_no_offers_rather_than_invented_ones` |
| trades accumulate across levels (a level-3 farmer still offers level-1/2 trades) | `mobs/villager/trades.rs`, `trades_accumulate_across_levels` |
