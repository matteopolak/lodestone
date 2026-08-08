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
packet, matching vanilla's `handleSetCarriedItem`. `apply_container_clicked` **derives** the
click's result rather than applying the claim — see below.

## The click is derived, not trusted (was a scope cut, now closed)

~~"this does not re-run vanilla's `doClick` state machine server-side"~~ — it
does, in `crates/lodestone-server/src/container_click.rs`. The struck-through
paragraph that used to be here argued that applying the client's own
`changed_slots` prediction verbatim could not introduce a *new* desync, which
was true and beside the point: **it let any client mint any item in any slot by
naming it in a diff.** Issue #529 closed the crafting *result* alone; this closes
the general case.

What the consumer now does with each `CONTAINER_CLICK`:

1. builds the [`MenuLayout`] for the tracked window — the player screen, a
   block-entity container, or a crafting table,
2. reads the menu's slots out of their real backing stores,
3. runs `container_click::do_click` (the port of
   `AbstractContainerMenu.doClick`) over them, from `(slot, button, click_type)`
   alone,
4. writes the result back, with grid cells routed through
   `CraftingState::set_input` so the result slot is re-derived,
5. compares the client's `changed_slots`/`carried_item` prediction against what
   it derived, and sends a full corrective `container_set_content` **only** on a
   disagreement.

So the packet's item payloads are never stored, and an honest client pays no
extra traffic. Step 5's comparison is what makes the correction a comparison
rather than an unconditional resend — the property
`a_claimed_item_is_never_stored_and_the_client_is_corrected` asserts in both
directions.

The cursor and the in-progress drag live on `PlayerInventory`
(`click_state()`), for the same reason the crafting grid does: they are
per-connection menu state and `PlayerInventory` is the per-connection value every
container call site already holds, so neither costs a new parameter on
`dispatch_play_packet` (which is at 28).

## Throwing items out (`Q` / `Ctrl+Q`)

`ServerBound::ItemDropped { whole_stack }` → `server.rs`'s `apply_item_dropped`,
which is vanilla's `ServerPlayer.drop(boolean)` in three steps: take from the
selected hotbar slot (one item, or all of it), spawn the item entity, and reply
with a `container_set_slot` for that slot.

**This did nothing at all until recently, and the failure was not in a router.**
The client half was complete — a keybind produced
`ClientAction::DropSelectedItem`/`DropSelectedItemStack`, and four adapters
encoded the right ordinals — but `V770ServerProtocol::decode`'s `PLAYER_ACTION`
arm handled ordinals 0-2 and sent everything else to `ServerBound::Ignored`. So
there was no `_ =>` arm to find in `ingest`/`session`/`forward`: the packet was
discarded one layer earlier, before any `ServerBound` variant existed to route.
When a keypress reaches no pixels, check the *decode* before the routers.

Three specifics worth not rediscovering:

- **Ordinal 3 is `DROP_ALL_ITEMS` and 4 is `DROP_ITEM`.** That reads backwards
  from the key bindings (`Q` is one item, `Ctrl+Q` is the stack), so the natural
  transposition makes a bare `Q` throw the whole stack — and both directions
  decode to a well-formed variant, so only an assertion catches it. Gated in
  `v770/tests/interaction_actions.rs`.
- **A thrown stack is not a popped block.** `block_drops::thrown_item_velocity` is
  a `0.3`-long impulse along the player's look vector plus spread;
  `dropped_item_velocity` is a `+0.2` vertical hop with no notion of facing.
  Reusing the latter drops the item at the player's feet, which looks like the
  throw not working.
- **Pickup delay is 40 ticks, not 10** (`THROWN_PICKUP_DELAY_TICKS` against
  `setDefaultPickUpDelay`'s 10). At 10 a player who throws while walking forwards
  picks their own throw straight back up — the entity really spawned, and the
  symptom is still "throwing does not work".

The other drop path — clicking outside an open window with a held stack, slot
`-999` in `container_click.rs`'s `do_click_with` — was already wired and now
routes through the same velocity and delay, which is what vanilla does
(`doClick`'s outside case calls the same `Player.drop`).

## How to change it

- **A new native slot / equipment kind** — extend `PlayerInventory` and
  `player_menu_native_index`'s match in `inventory.rs`. Keep the constant
  restated (not imported) from `lodestone-game`'s `menu.rs`; a change to one
  without the other is exactly the desync this doc comment exists to
  prevent — restate deliberately, don't share the dependency.
- **A new window (a real chest, furnace, etc.)** — **partially done**: a
  furnace or hopper (see `docs/block-entities.md`'s "gap 3 is closed too"
  update) now has somewhere for a non-zero window to land, via
  `crate::server::OpenContainer`/`sync_open_container` and
  `crate::inventory::container_menu_slot`. There is still no real chest
  block entity at all in this crate, and the brewing stand/composter remain
  unopenable for the reasons that doc gives — extending this to a new
  container kind means giving it a [`BlockEntity`](crate::block_entities::BlockEntity)
  variant with a real `menu_name`/`container_slots`/`data_properties`, not
  touching this file.
- **A new click behaviour** — `container_click.rs`'s `do_click`. What is
  deliberately *not* modelled there, and would be noticed only by a player:
  `tryItemClickBehaviourOverride` (bundles), `canDropItems`, and a menu-specific
  stack cap smaller than the item's own (no menu this crate opens has one). What
  *is* modelled and easy to get wrong: the per-item stack cap and the armour-slot
  `mayPlace`, both from `lodestone_data::item_prototypes`' jar dump — a constant
  64 there would let the server itself derive a 64-stack of swords.
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
