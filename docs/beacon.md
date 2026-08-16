# Beacon: pyramid detection, power selection and periodic effects

## What it is

Server-side simulation for `minecraft:beacon` (issue #616's `SET_BEACON` remainder):
pyramid-tier detection, the primary/secondary power menu, and the periodic status-effect
grant to nearby players. Before this, `SET_BEACON` decoded and was discarded, and there was
no beacon block entity, menu, or effect-selection state anywhere in `lodestone-server`.

## How it works

Two halves, split the way `lodestone-server` splits every block entity: pure derivations in
`crates/lodestone-server/src/beacon.rs`, stateful wiring in `crates/lodestone-server/src/
{block_entities,chunk_nbt,container_click,server}.rs`.

**Pure derivations** (`crate::beacon`), each a direct port of one `BeaconBlockEntity`/
`BeaconMenu` method, independently unit-tested against a bare `ChunkSource`:

- `beacon_levels` — the pyramid tier (`0..=4`) beneath a position: for step `1..=4`, the
  whole `(2*step+1)²` square at `y - step` must be entirely one of the five
  `minecraft:beacon_base_blocks` (iron/gold/emerald/diamond/netherite block), and the
  result is the highest step whose layer *and every layer above it* passed — a broken
  layer 2 caps the result at `1` even if layers 3–4 would otherwise qualify.
- `beam_unobstructed` — an approximation of vanilla's `!beamSections.isEmpty()`. Vanilla
  tracks the beam as coloured segments (for the render, which this crate does not do
  server-side); only *emptiness* gates effect application, so this checks "every block from
  directly above the beacon to a fixed scan height is beam-transparent" (air, beacon, glass,
  tinted glass, or stained glass/pane) instead of tracking segments or colour. **Known gap**:
  vanilla's real gate is `getLightDampening() >= 15`, a general block-opacity value; this
  checks membership in the beam-transparent family instead, which agrees for every block a
  player is likely to build a shaft from and can disagree for an unusual low-opacity block
  (a carpet, say) that is not in that family.
- `required_levels_for` / `validate_beacon_effects` — the tier a power requires, and
  `BeaconBlockEntity.validateEffects`'s full gate: a secondary needs the level-4 pyramid; each
  pick's own tier must fit the pyramid actually built; the primary can never be the
  level-4-only power (regeneration); a secondary, if present, must be either that level-4
  power or identical to the primary (the same-effect amplifier boost) — never a *different*
  tier-1..3 power.
- `beacon_effects` — the horizontal reach and the primary/secondary application (effect,
  amplifier, duration) at a given pyramid tier, `BeaconBlockEntity.applyEffects`'s exact
  arithmetic (`range = levels*10+10`, `duration = (9+levels*2)*20` ticks, amplifier `1` only
  when levels `>= 4` and the secondary equals the primary).
- `encode_beacon_effect` / `decode_beacon_effect` — `BeaconMenu`'s own `container_set_data`
  wire form for an optional power (`0` = none, else the mob-effect registry id `+ 1`).
- `is_beacon_payment_item` — reads `minecraft:beacon_payment_items` straight out of the
  bundled tag JSON `crate::crafting::EMBEDDED_ITEM_TAGS` already carries for the crafting
  corpus, rather than a second hardcoded copy.

**Stateful wiring**:

- `BlockEntity::Beacon(BeaconData)` (`block_entities.rs`) carries `levels`, `primary_effect`,
  `secondary_effect` and the one-slot `payment`. `levels` is **not** ticked continuously —
  unlike vanilla's 80-tick background recompute, it is refreshed from `beacon_levels` only
  when the menu opens or a `SET_BEACON` is handled. A menu left open while the pyramid is
  being dismantled will not show the new number until the next such refresh; effect
  *application* (below) always recomputes live regardless, so this staleness cannot outlive
  a broken pyramid.
- NBT round trip (`chunk_nbt.rs`): `Levels`, `primary_effect`/`secondary_effect` (bare
  strings, written only when set) — `BeaconBlockEntity.saveAdditional`. The payment slot is
  menu-only scratch space in vanilla too and is not persisted.
- The menu (`container_click.rs`'s `MenuKind::Beacon`): one payment slot restricted to
  `is_beacon_payment_item`, capped to a stack of one, then the standard 27+9 player tail —
  opened through the same generic `open_container_screen` every other block-entity menu
  uses. **Known gap**: its shift-click routing reuses the generic two-range shape rather than
  vanilla's own upfront `!paymentSlot.hasItem() && mayPlace && count == 1` gate, so
  shift-clicking a stack of more than one eligible item can split one off where vanilla would
  skip straight to the storage/hotbar shuffle — `may_place`/`max_stack_size` still refuse the
  wrong item or a second one outright.
- `SET_BEACON`'s consumer (`server.rs`'s `apply_set_beacon`): validates against the block
  entity's own (last-refreshed) `levels`, and on success consumes one payment item and
  resends the payment slot plus all three `container_set_data` values. A refused submission
  (no payment, or an invalid pair) is a no-op — vanilla disconnects the client instead, which
  this crate does not reproduce, matching the "malformed packet drops the effect, not the
  connection" convention `PlayerInventory::set_selected_hotbar_slot` already uses.
- Closing the menu (`ContainerClosed`) drops the payment item to the floor —
  `BeaconMenu.removed`'s `player.drop(itemStack, false)` — rather than merging it into the
  inventory the way the crafting-grid/workstation return does.
- Periodic effect application (`server.rs`'s per-connection tick section, right before the
  `/effect`-shared `effects.tick()` block): every 80 game ticks (vanilla's own
  `level.getGameTime() % 80L == 0L`), scan every tracked `Beacon` with a primary power set,
  recompute its pyramid and beam live, and — if within `beacon_effects`' own range — call
  `ActiveEffects::apply` and send `ServerProtocol::encode_update_mob_effect`. Run
  per-connection rather than from the world tick loop because the wire notification and
  `ActiveEffects` are per-connection state, the same architecture `/effect`'s own tick
  already uses.

## How to change it

- The five base blocks and four effect tiers are hardcoded constants
  (`beacon.rs`'s `BASE_BLOCKS`/`BEACON_EFFECT_TIERS`) rather than read from a bundled tag —
  `minecraft:beacon_base_blocks` has no corresponding JSON under this crate's
  `assets/tags/block/` the way `beacon_payment_items` does under `assets/tags/item/`. If that
  changes, prefer reading the bundled tag over hand-editing the constant.
- The vertical reach of a beacon's effect approximates vanilla's `AABB.inflate(range)
  .expandTowards(0, height, 0)` as "no lower than `range` below, no upper bound" — `crate::
  chunk::ChunkSource` has no height accessor to derive the real upper bound from generically.
- The native tick loop is the only wired driver of periodic application; the `wasm32` loop's
  own effects section (`server.rs`, the second `if !effects.is_empty()` occurrence) does not
  yet carry the same beacon sweep — a real, known gap, not an oversight.
- `apply_use_item_on`'s right-click dispatch refreshes `BeaconData::levels` for *any* beacon
  before falling into the generic `open_container_screen` path — if you add a second way to
  open a beacon's menu, refresh `levels` there too or the displayed tier can be stale.

## Configuration

None — every rule here is a vanilla constant, not a server setting.

## Dependencies

`crate::chunk::ChunkSource` (pyramid/beam reads), `crate::mob_effects::ActiveEffects`
(the actual status-effect state a beacon grants), `crate::crafting::EMBEDDED_ITEM_TAGS`
(the payment-item tag), `lodestone_data::mob_effects` (the effect registry id ↔ name table),
and the new `ServerProtocol::encode_update_mob_effect`/`encode_remove_mob_effect` (see
[`mob-effect-wire-sync.md`](./mob-effect-wire-sync.md)) for the wire delivery.
