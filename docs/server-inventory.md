# Server-authoritative inventory (issue #408)

## What it is

`lodestone-server`'s model for a player's own inventory, and the decode +
consumer that lets `SET_CARRIED_ITEM` and `CONTAINER_CLICK` actually change
server state. Before this, `lodestone-server` had **no inventory/container
model at all** — three separate doc comments (`server.rs`'s
`apply_use_item_on`, `protocol.rs`'s `UseItemOn`, `vitals.rs`) already said
so, and issue #266's own investigation comment concluded that decoding any of
its 16 packets would produce "real bytes parsed with genuinely nothing to
write into and nothing that would read it." This closes that gap for two of
the sixteen — the two the investigation named as the shortest path from "a
model exists" to "a real client moves an item on our server and it sticks."

## How it works

### The model: `PlayerInventory`

`crates/lodestone-server/src/inventory.rs`. 41 native slots — hotbar
`0..=8`, main storage `9..=35`, armour `36..=39`, off-hand `40` — plus a
selected-hotbar-slot index. This is a direct restatement of vanilla's
`Inventory` class
(`.cache/mc/26.2/src/net/minecraft/world/entity/player/Inventory.java:31-56`,
`items` sized 36 + `EQUIPMENT_SLOT_MAPPING`'s feet/legs/chest/head/off-hand
entries), and, deliberately, the **same** native indexing
`lodestone-game`'s client-side `Menu` already established and documents at
`crates/lodestone-game/src/menu.rs:5-27` (`PLAYER_NATIVE_SIZE = 41`,
`OFFHAND_NATIVE = 40`). Restated rather than shared: this crate is
version- and client-free and does not depend on `lodestone-game`, so keeping
the numbering identical (rather than importing it) is what lets a wire
packet's menu-slot indices land in the same native slot the client's own
`Menu` already uses for the identical concept.

`PlayerInventory::apply_menu_slot_change(menu_slot, item)` is the
menu-index → native-index projection for window `0` (the player's own
inventory screen), taken straight from `Menu::player`'s own doc table:

| menu slot | native index |
|---|---|
| `5..=8` (armour head/chest/legs/feet) | `39`/`38`/`37`/`36` |
| `9..=35` (main storage) | `9..=35` (identity) |
| `36..=44` (hotbar) | `0..=8` |
| `45` (off-hand) | `40` |
| `0..=4` (2×2 crafting result/grid) | none — dropped |

The crafting grid/result have no native slot at all in vanilla either (they
live in `InventoryMenu`'s own scratch `CraftSlots` container); a
`CONTAINER_CLICK` reporting a change there is dropped, not misapplied — this
server has no recipe model to resolve a result for, the same "genuinely
different, no data to model it" cut `docs/container-cost-screens.md` already
made for the anvil/enchanting-table costs.

### The wire decode: `V770ServerProtocol`

`crates/protocol/v770/src/server_protocol.rs`.

- `SET_CARRIED_ITEM` decodes into `ServerBound::CarriedItemChanged { slot }`,
  reusing the existing `SetCarriedItem` wire struct
  (`crates/protocol/v770/src/packets/game.rs`). The decoder validates
  `0..HOTBAR_SIZE` before producing the variant, mirroring vanilla's
  `Inventory.isHotbarSlot` guard (`Inventory.java:70-76`); out of range
  decodes to `ServerBound::Ignored`.
- `CONTAINER_CLICK` decodes into `ServerBound::ContainerClicked { window_id,
  state_id, changed_slots, carried_item }` via a hand-written decoder
  (`decode_container_click`), mirroring the wire layout the client-side
  encoder (`crate::adapter::encode_container_click`) already produces: VarInt
  window id, VarInt state id, `i16` slot, `i8` button, VarInt click-type
  ordinal, a changed-slots map (VarInt count, then `i16` slot + `HashedStack`
  per entry), then the carried cursor stack as a trailing `HashedStack`.
  `read_hashed_stack` is the item decoder: a bool presence flag, then, if
  present, item id (VarInt), count (VarInt), and two VarInt component-patch
  counts (added, removed) — our own client's encoder always writes `0`/`0`
  for those (see `write_hashed_stack`'s own doc comment: "Creative slot-set
  with custom components is out of Phase 1's scope"), so a **nonzero** count
  fails the whole decode rather than guessing a skip length for bytes this
  decoder has no byte-accurate layout for.

### The consumer: `crate::server::dispatch_play_packet`

`crates/lodestone-server/src/server.rs`. `apply_carried_item_changed` writes
straight into `PlayerInventory::set_selected_hotbar_slot` — no confirmation
packet, matching vanilla's `handleSetCarriedItem`. `apply_container_clicked`
applies `changed_slots` directly into the model, for `window_id == 0` only;
any other window is decoded but dropped (no open-container model exists
yet).

**Scope, stated plainly: this does not re-run vanilla's `doClick` state
machine server-side.** `CONTAINER_CLICK`'s `changed_slots` is the client's
own post-click prediction — `lodestone-game`'s `click.rs` (issue #27,
`docs/container-clicks.md`) already computed the full result before encoding
the packet — and this landing applies that diff verbatim rather than
re-deriving the seven click modes / the quick-craft drag machine
server-side. This is deliberate, and it is consistent with today's actual
desync risk rather than a shortcut around it: the client already predicts
locally with **no server confirmation needed to look correct**
(`docs/container-clicks.md`), so nothing before this landing validated any
of it server-side at all. Applying the client's own diff verbatim cannot
introduce a *new* desync relative to that baseline — the server model
becomes a mirror of what the client already believes, by construction —
where a from-scratch, subtly-wrong reimplementation of `doClick` would. A
server-authoritative `doClick` (rejecting an impossible client diff, the
actual point of running it server-side at all) is real future work.

## How to change it

- **A new native slot / equipment kind** — extend `PlayerInventory` and
  `player_menu_native_index`'s match in `inventory.rs`. Keep the constant
  restated (not imported) from `lodestone-game`'s `menu.rs`; a change to one
  without the other is exactly the desync this doc comment exists to
  prevent — restate deliberately, don't share the dependency.
- **A new window (a real chest, furnace, etc.)** — `apply_container_clicked`
  currently drops anything but `window_id == 0`. This needs an
  open-container model (issue #266's other packets, #250/#251/#252) before
  a non-zero window has anywhere to land; do not special-case a window id
  without one.
- **Server-authoritative `doClick`** — would replace `apply_container_clicked`
  applying the client's diff with actually running `click.rs`'s verb table
  (or a server-side port of it) against `PlayerInventory`, and diffing its
  own result against the client's claimed `changed_slots` to detect
  disagreement. Not done here; see the scope note above for why applying the
  diff directly was the right first landing rather than a half-correct
  reimplementation.
- **`SET_CREATIVE_MODE_SLOT`** — the next packet in #266's list this model
  unblocks; needs `read_optional_item_stack`'s decode counterpart (see
  `crate::adapter::write_optional_item_stack`'s doc comment for the existing
  client-side encoder and its own scope note about the empty component
  patch).

## Configuration

None — no flags or env vars gate this.

## Dependencies

- `lodestone_model::ItemStack` — the wire-shaped stack `PlayerInventory`
  stores.
- `lodestone_data::items::item_name` — registry id → canonical item key
  resolution, the decode-side inverse of `lodestone_data::items::item_id`
  (already used by the client-side encoder).
- [`container-clicks.md`](container-clicks.md) — the client-side model and
  predictor this mirrors the native-slot numbering from, and the spec this
  landing's `changed_slots` semantics come from.
- [`container-cost-screens.md`](container-cost-screens.md) — the precedent
  for "genuinely different, no data to model it, drop rather than guess"
  scope cuts (there: furnace/brewing-stand shift-click routing; here: the
  crafting-grid menu slots).

## Verification

```bash
cargo test -p lodestone-server --lib --no-fail-fast -- inventory::
cargo test -p lodestone-v770 --lib --no-fail-fast -- inventory_decode_tests::
cargo test -p lodestone-v770 --test server_inventory_live
```

The last one is a real `lodestone-client` (real `V770Adapter`) sending
`SET_CARRIED_ITEM` + `CONTAINER_CLICK` over an in-memory transport against
the real `V770ServerProtocol`/`serve_connection`, with the server's own
`PlayerInventory` (surfaced via `ServeSummary::inventory`, added for exactly
this reason) asserted afterward — not just a decode round trip. Watched
failing with the consumer temporarily neutered (`left: None, right:
Some(diamond_pickaxe)`), then restored via `cp` from a scratchpad backup
with an md5 check, per this repo's shared-checkout convention.
