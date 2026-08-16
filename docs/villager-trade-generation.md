# Villager trade generation and refresh (issue #245)

## What it is

The data and economics half of villager trading: a complete, per-profession,
per-level trade table transcribed from the real 26.2 registry data (all
thirteen workstation professions, not just farmer), and the purchase/restock
mechanics — offer uses, demand-driven price fluctuation, and restock cadence
— that turn that static table into a live, buyable state per villager.

This is the trade-generation slice of the villager economy arc
(`docs/plans/villager-economy.md`); professions and workstation/POI claiming
(issue #243) landed separately in `crates/lodestone-server/src/mobs/villager/`
and are unchanged by this work.

## How it works

Two new pieces, in two different crates:

- **`lodestone_data::villager_trades`** (`crates/lodestone-data/src/villager_trades.rs`)
  — every `TradeRecord` for armorer through weaponsmith, all five levels,
  transcribed from `data/minecraft/{villager_trade,trade_set,tags/villager_trade}`
  under `.cache/mc/26.2/src`. `pool_for(profession_path, level)` returns the
  resolved pool plus the tag's own `amount`, keyed by the bare profession path
  (`"farmer"`, `"librarian"`, ...) rather than an enum, since this crate sits
  below `lodestone-server` and cannot name that crate's `Profession` type.
  Eighteen records that compute part of their result at runtime via
  `given_item_modifiers` (enchanted books, cartographer treasure/explorer
  maps, two enchanted-weapon/tipped-arrow records) are excluded and named in
  each level's own doc comment rather than given invented numbers; four
  profession/level combinations (`armorer` 4 and 5, `toolsmith` 5,
  `weaponsmith` 5) resolve to nothing portable at all.

  **This now supersedes `crate::mobs::villager::trades` in code too, not just
  in scope.** That module (under `mobs/**`) is a thin delegation onto this
  one: `Profession::path()` gives the bare registry path
  `lodestone_data::villager_trades::pool_for` is keyed on, and
  `crate::mobs::villager::trades::{offers_for, offers_up_to}` forward to it
  directly. It carries no trade data of its own any more.

- **`lodestone_server::villager_trade`** (`crates/lodestone-server/src/villager_trade.rs`)
  — the dynamic half: [`OfferState`] ports `MerchantOffer`'s mutable fields
  (`uses`, `demand`, `special_price_diff`) and its price formula
  (`modified_cost_a_count`, vanilla's `getModifiedCostCount`); [`RestockState`]
  ports `Villager`'s restock cadence (`shouldRestock`/`allowedToRestock`/
  `needsToRestock`); [`VillagerTrades`] bundles a profession/level's offer
  list with its restock state and exposes `try_trade`/`maybe_restock` as the
  two operations a caller needs. Every field and method is named after, and
  tested against, the real vanilla method it ports — see the module's own doc
  for the exact `MerchantOffer.java`/`Villager.java` methods and why the wire
  order (`write`/`read`), not the constructor, decided the field layout.

Both are pure logic: neither reaches the network, `SimMob`, or a player's
inventory. `crate::mobs::villager`'s `OpenTrade` interact outcome and
`crate::server::open_merchant_screen` (unchanged by this work) already
produce the *static* offer list a client sees when the screen opens, sourced
from `crate::mobs::villager::trades::offers_up_to` — which, now that it
delegates, reflects the real per-profession table for all thirteen
professions, not just farmer.

## How to change it, and the gotchas

- **Adding a profession's data** means re-running the extraction against
  `.cache/mc/26.2/src/data/minecraft/{villager_trade,trade_set,tags/villager_trade}/<profession>/`
  — every `villager_trade.rs` record cites its own source path in a comment
  immediately above it, so a diff against the jar is mechanical. Tag
  resolution must handle nesting (`armorer`/`toolsmith`/`weaponsmith` each
  pull in `#minecraft:common_smith/level_N`, which resolves to
  `villager_trade/smith/...` records) — see the module's own doc for the
  exact tag graph.
- **`price_multiplier` is not gossip or reputation.** It is each record's own
  `reputation_discount` field, which despite the name is `MerchantOffer`'s
  `priceMultiplier` — the coefficient plain demand (repeated buying, no
  gossip involved) uses to raise a price. Every kept record in the table sets
  it explicitly (`0.05` or `0.2`, verified for all 258 records); omitting it
  would silently make every trade demand-inelastic, not merely reputation-
  unaware.
- **`OfferState::update_demand` must run before `reset_uses`, not after** —
  the formula reads the pre-reset `uses` count. `VillagerTrades::maybe_restock`
  already gets this order right; a caller who separately resets uses before
  calling `update_demand` will always compute `demand - max_uses`.
- **The gossip (#244) and reputation (#246) hook is `OfferState::special_price_diff`**
  plus `add_special_price_diff`/`reset_special_price_diff` — vanilla's own
  `addToSpecialPriceDiff`/`resetSpecialPriceDiff`, the mechanism both Hero of
  the Village and ordinary reputation-driven pricing use. The field and its
  two mutators are tested here
  (`tests::a_special_price_diff_reduces_the_next_purchases_cost`); #244/#246
  themselves are now built in `crate::mobs::villager::{gossip, reputation}`
  and `crate::mobs::MobSim` (see `docs/villager-reputation.md`) — a real
  gossip ledger, reputation score and the `update_special_prices` caller of
  this module's hook all exist and reach a live `SimMob`. What still does
  **not** exist is a live per-villager `OfferState` list for
  `update_special_prices` to call against — see this doc's own "What
  remains" section below, unchanged by that work.

## What remains (issue #245's third piece, and the #243 broker)

- **`SELECT_TRADE` is now decoded and connected, with a disclosed
  simplification** (issue #616's remainder). `ServerBound::SelectTrade {
  index }` (`crate::protocol`) is constructed by `V770ServerProtocol::decode`
  and dispatched in `crate::server::dispatch_play_packet`, which tracks which
  villager a connection's open merchant screen belongs to
  (`crate::server::OpenMerchant`, set by the `InteractOutcome::OpenTrade`
  arm and cleared on `ContainerClosed`) and executes the trade directly
  through `crate::server::attempt_villager_trade` against the player's
  36-slot hotbar+main inventory (`PlayerInventory::count_of`/`consume`, new).
  **This does not go through `VillagerTrades::try_trade`, and does not use
  live demand pricing** — see `attempt_villager_trade`'s own doc comment for
  why: a villager is a `SimMob`, not a `BlockEntity`, so it has none of the
  `BlockPos`-keyed storage `OpenContainer`'s slot-sync machinery needs, and a
  second parallel storage-and-sync mechanism for one menu's two payment
  slots plus a result slot was judged out of scope for this pass. Every
  purchase is priced at the record's own base cost, and the cost items are
  found and consumed from wherever they sit in the inventory rather than
  from two manually-filled payment slots — a real, disclosed UX deviation
  from vanilla's `MerchantMenu`, not a silent one.
- **Still not built: tying a live `VillagerTrades` (with real demand/uses
  state) to a specific `SimMob`.** `VillagerTrades::try_trade` and
  `maybe_restock` remain unused in production; the SELECT_TRADE wiring above
  reads the static per-level offer table fresh on every purchase rather than
  a persisted, demand-adjusted one. Whoever builds real payment-slot
  mechanics (a `MenuKind::Merchant` in `crate::container_click`, and
  non-block-entity per-connection scratch storage for it, the same shape
  `PlayerInventory::workstation` already is for the anvil/grindstone/
  smithing table) gets this for free as a side effect, since that is also
  where a live `VillagerTrades` instance would naturally live.
- **Nothing calls `VillagerTrades::maybe_restock`.** Vanilla's own trigger is
  the `WorkAtPoi` Brain behavior (`body.shouldRestock(level)` /
  `body.restock()`, both inside that one behavior) — Brain package work is
  issue #231/#243's remainder, in `lodestone-entity`, off limits for this
  change.
- **On-disk persistence of offer state (uses/demand/special-price-diff) is
  not built** — matches `crate::mobs::villager`'s own disclosed gap for
  workstation claims; a restart would need to re-derive or reset every
  villager's trade state either way, once something actually holds it
  per-villager.

## Configuration

No env vars, flags, or config files. `lodestone_data::villager_trades`'s
table and `lodestone_server::villager_trade`'s tick constants
(`RESTOCK_COOLDOWN_TICKS` = 2400, `HALF_DAY_TICKS` = 12000,
`MAX_RESTOCKS_PER_DAY` = 2) are all transcribed vanilla constants, not tuned
values.

## Dependencies

`lodestone_server::villager_trade` depends on `lodestone_data::villager_trades`
(the static table) and `lodestone_data::item_prototypes` (cost-item max stack
size, for the price clamp), and reads `crate::mobs::villager::Profession`
read-only (no edits to that module). `crate::mobs::villager::trades` now
depends on `lodestone_data::villager_trades` the same way, for the same
reason. `lodestone_data::villager_trades` depends on nothing beyond
`lodestone-data`'s own conventions.

## Evidence

| claim | where |
|---|---|
| a generated record matches the real jar file exactly, at pairwise-distinct values | `lodestone-data/src/villager_trades.rs`, `farmer_level_1_wheat_for_emerald_matches_the_jar_record_exactly` |
| the codec's `xp` default (`1`, not `0`) is resolved correctly for a record with no `xp` key | `lodestone-data/src/villager_trades.rs`, `a_record_missing_its_xp_field_resolves_the_codecs_default_of_one` |
| a two-cost trade carries its real second cost | `lodestone-data/src/villager_trades.rs`, `a_two_cost_trade_carries_its_second_cost_item` |
| the `common_smith` tag composition is resolved, not just each profession's own trades | `lodestone-data/src/villager_trades.rs`, `armorer_pool_includes_the_shared_smith_trade` |
| a fully-unportable profession/level returns `None`, not invented numbers | `lodestone-data/src/villager_trades.rs`, `a_fully_unportable_level_returns_none` |
| a purchase takes and gives exactly the jar's amounts, with an insufficient-funds control that mutates nothing | `lodestone-server/src/villager_trade.rs`, `a_satisfied_trade_takes_and_gives_exactly_the_jar_amounts` |
| a two-cost trade requires both costs independently, checked with two single-short controls | `lodestone-server/src/villager_trade.rs`, `a_two_cost_trade_requires_both_costs` |
| demand is predicted exactly from the outside formula, both the under-use (price cannot drop below base) and heavy-use (price rises by the predicted amount) directions, at two different `price_multiplier` values so one test cannot pass by copying the other's arithmetic | `lodestone-server/src/villager_trade.rs`, `demand_after_underuse_does_not_lower_the_price_below_base`, `heavy_use_raises_the_price_by_the_predicted_amount` |
| the gossip/reputation hook discounts and surcharges correctly, with a floor at 1 | `lodestone-server/src/villager_trade.rs`, `a_special_price_diff_reduces_the_next_purchases_cost` |
| restock cadence matches vanilla's exact tick thresholds (2400-tick cooldown, 2-per-day cap, day rollover), not a round-number guess | `lodestone-server/src/villager_trade.rs`, `restock_cadence_matches_the_exact_tick_thresholds` |
| `crate::mobs::villager::trades` (what `open_merchant_screen` actually reads) now has a non-zero production-reader count on `lodestone_data::villager_trades` — a librarian, which the old farmer-only table always answered empty for, comes back non-empty | `lodestone-server/src/mobs/villager/trades.rs`, `a_librarian_now_offers_real_trades_through_this_seam` |
| every one of the thirteen ported professions is reachable through this same seam, not just the one discriminating case | `lodestone-server/src/mobs/villager/trades.rs`, `every_ported_profession_is_reachable_through_this_seam` |
| a neuter (wheat cost 20 → 21) turned the jar-match test red; restored via `cp` from an md5-checked backup | this session's own record, not a committed test |
