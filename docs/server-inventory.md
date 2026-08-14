# Server-authoritative inventory

## What it is

`lodestone-server`'s model for a player's own inventory, and the decode +
consumer that lets `SET_CARRIED_ITEM` and `CONTAINER_CLICK` actually change
server state. Before this, `lodestone-server` had **no inventory/container
model at all** — three separate doc comments (`server.rs`'s
`apply_use_item_on`, `protocol.rs`'s `UseItemOn`, `vitals.rs`) already said
so, and an earlier investigation comment concluded that decoding any of
its 16 packets would produce "real bytes parsed with genuinely nothing to
write into and nothing that would read it." This closes that gap for two of
the sixteen — the two the investigation named as the shortest path from "a
model exists" to "a real client moves an item on our server and it sticks."

## How it works

### The model: `PlayerInventory`

`crates/lodestone-server/src/inventory.rs`. 41 native slots — hotbar
`0..=8`, main storage `9..=35`, armour `36..=39`, off-hand `40` — plus a
selected-hotbar-slot index. This is a direct restatement of vanilla's
`Inventory` class (`Inventory.items` sized 36 +
`Inventory.EQUIPMENT_SLOT_MAPPING`'s feet/legs/chest/head/off-hand
entries), and, deliberately, the **same** native indexing
`lodestone-game`'s client-side `Menu` already established and documents in
`crates/lodestone-game/src/menu.rs` (`PLAYER_NATIVE_SIZE`,
`OFFHAND_NATIVE`). Restated rather than shared: this crate is
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
  `Inventory.isHotbarSlot` guard; out of range
  decodes to `ServerBound::Ignored`.
- `CONTAINER_CLICK` decodes into `ServerBound::ContainerClicked { window_id,
  state_id, changed_slots, carried_item }` via a hand-written decoder
  (`decode_container_click`), mirroring the wire layout the client-side
  encoder (`crate::adapter::serverbound::encode_container_click`) already produces: VarInt
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

### The join snapshot: `crate::server::join_inventory_snapshot`

**The one send on this path that no client packet asks for.** Everything else in
this document is a reply — a menu was opened, a slot was clicked, a recipe was
placed. A joining player has sent nothing, and until this existed they were told
nothing: the client kept its fresh-`Menu` default (an empty grid) while the server
held the fully restored inventory, and the first click produced a disagreement whose
corrective `container_set_content` flushed all 46 slots at once. The reported
symptom was exactly that — *"my inventory is empty, but if I shift-click something
then all the items pop in"*. Nothing was lost at any point;
`PlayerData::to_inventory` had already restored it.

Sent from the top of both `serve_play` variants (native and `wasm32`), which is
vanilla's own position: `PlayerList.placeNewPlayer` calls
`ServerPlayer::initInventoryMenu` **last** — after the abilities/held-slot/recipe
packets, after `sendPlayerPermissionLevel`, after the placement teleport, after the
player-info adds and after `sendLevelInfo`. By the time `serve_play` is entered
`serve_connection_inner` has done all of those, and the deferred chunk stream this
loop drains corresponds to vanilla's `PlayerChunkSender` feeding columns over
*subsequent* ticks.

Two things worth knowing before changing it:

- **It is `container_set_content`, not `set_player_inventory`.** Both exist in 26.2
  and our client decodes both, so the choice is not arbitrary.
  `AbstractContainerMenu::sendAllDataToRemote` hands the slot list to
  `ContainerSynchronizer::sendInitialData`, whose `ServerPlayer` implementation
  constructs `ClientboundContainerSetContentPacket`; the client's
  `handleContainerContent` routes `containerId == 0` to `player.inventoryMenu`, which
  is why the window id is `0`. `ClientboundSetPlayerInventoryPacket` is a
  **single-slot** record, `(int slot, ItemStack contents)`, whose only vanilla
  producer is `Inventory.createInventoryUpdatePacket` acknowledging one pickup — it
  carries neither a slot list nor the cursor, so it cannot express a snapshot.
- **The state id is `1`, and the constant `0` used by the other window-`0` sends in
  `server.rs` is inert.** `sendInitialData` passes `container.incrementStateId()`,
  which is `(stateId + 1) & 32767` from a `0` start, so a real client's first content
  packet carries `1`. The obvious worry — that a wrong state id makes the client
  reject the *next* update — is **backwards, and was checked rather than assumed**:
  no client validates the field. `handleContainerContent` calls
  `menu.initializeContents(packet.stateId(), …)` unconditionally and
  `initializeContents` simply assigns it, and our own client's
  `lodestone_game::reconcile` adopts whatever arrives. The only consumer anywhere is
  a **server** checking a click's *echoed* id
  (`ServerGamePacketListenerImpl.handleContainerClick` taking its
  `broadcastFullState()` branch), which is the other direction of travel and
  something this crate does not do at all — the `ServerBound::ContainerClicked` arm
  binds `state_id: _`. So window-`0` ids that move backwards cost nothing observable,
  which is why the snapshot is faithful to the record without threading a real
  counter through `dispatch_play_packet`'s parameter list for a field nothing reads.

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
naming it in a diff.** A previous fix closed the crafting *result* alone; this closes
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

## Pick-block / pick-entity (middle-click)

`ServerBound::PickItemFromBlock { pos, include_data }` and
`ServerBound::PickItemFromEntity { entity_id, include_data }` →
`dispatch_play_packet`'s two arms next to `PingRequest`, mirroring vanilla's
`ServerGamePacketListenerImpl::handlePickItemFromBlock`/
`handlePickItemFromEntity` → `tryPickItem`.

**Vanilla's client does no local prediction here.** `Minecraft
::pickBlockOrEntity` unconditionally forwards to
`MultiPlayerGameMode::handlePickItemFromBlock`/`handlePickItemFromEntity`,
which do nothing but send the packet — the three-way split below is entirely
server-authoritative, and the client's `SET_HELD_SLOT` round trip is exactly
vanilla's own latency, not a missing optimisation. (An earlier note here
assumed the opposite; re-reading `Minecraft.java`/`MultiPlayerGameMode
.java` in `.cache/mc/26.2/client-src` settled it.)

### The three-way split: `crate::item_use::try_pick_item`

The owner-reported behaviour, and vanilla's own decision order in
`ServerGamePacketListenerImpl::tryPickItem`:

1. **Already in the hotbar** (`Inventory.isHotbarSlot`) — just move the
   selection there. No slot's contents change.
2. **Elsewhere in the inventory** — swap it into a suitable (first empty,
   wrapping from the current selection) hotbar slot (`Inventory.pickSlot`).
3. **Not held anywhere, creative only** — mint it into a suitable hotbar
   slot, banking whatever was displaced into the first free slot
   (`Inventory.addAndPickItem`). Survival falls straight through and changes
   nothing.

All three answer through one `PickOutcome { selected, changed }`: `selected`
is always sent back (`ClientboundSetHeldSlotPacket`/`encode_set_held_slot`,
new — the client already decoded `SET_HELD_SLOT` into
`ClientEvent::HeldSlotChanged`, it just had no server-side encoder), and
`changed` is the native slots the caller must echo with
`encode_container_slot(0, 0, window_zero_menu_slot(native), ...)`, the same
window-`0`/state-`0` convention `crate::commands::Effect::GiveItems` already
uses for a server-initiated write.

### What to pick: block vs. entity

`dispatch_play_packet`'s two arms resolve the "what" — the one part
`item_use.rs` cannot see (it has no world or mob handle) — then hand a
resolved `ItemStack` to `try_pick_item`:

- **Block** — `crate::item_use::clone_item_stack_for_block(block_state)` is
  `BlockState.getCloneItemStack`'s **default** arm
  (`new ItemStack(this.asItem())`), via
  `lodestone_data::block_items::item_for_block(Block) -> Option<Item>` — the
  **inverse** of the existing item→block census (`block_for_item_id`),
  computed once behind a `OnceLock` from that same generated table rather
  than hand-rolled, so the two directions cannot drift. `None` for a block
  with no `BlockItem` at all (air, fluids, redstone wire, portal blocks).
  **Not modelled**: per-block `getCloneItemStack` overrides — crops clone to
  their seed, flower pots to the potted plant, banners/beehives/candle-cakes
  copy block-entity data. Each is a distinct vanilla override this crate has
  no per-block model for; `item_for_block`'s own doc comment lists them.
- **Entity** — `crate::item_use::spawn_egg_for_entity_type(entity_type)` is
  `Mob.getPickResult()`'s only modelled arm: a mob's own spawn egg, derived
  by name (`{entity path}` -> `{entity path}_spawn_egg`) the same way
  `crate::spawn_egg::entity_type_for_egg` derives the reverse, and checked
  against the real item registry so a misderived name refuses rather than
  proposing an egg that does not exist. `None` for every entity whose
  `getPickResult` also returns `null` by default (item entities, arrows, XP
  orbs, the player) and for the handful of non-`Mob` overrides not modelled
  (minecarts, boats, item frames, paintings, end crystals, leash knots,
  armour stands — each returns something other than a spawn egg).

`include_data` (`hasControlDown()` at pick time) gates two vanilla effects
neither of which has a consumer here: copying a block entity's NBT onto the
picked stack, and a game-master `FetchProfileCommand` debug print for an
avatar target. Decoded for wire fidelity, read by nothing — the same "not
modelled, no completion hook" scope cut `crate::item_use`'s module doc
already takes for other `Item.use` arms.

### Range gates

`crate::block_breaking::within_interaction_range` (already existed, reused
as-is) for the block case; `crate::item_use::within_entity_pick_range` (new,
same flattened-radius shape) for the entity case. Both are approximations of
vanilla's per-attribute `*_INTERACTION_RANGE` plus this packet's own extra
tolerance (`isWithinBlockInteractionRange(pos, 1.0)` /
`isWithinEntityInteractionRange(entity, 3.0)`), not an exact port — proportionate
to what a cheat-prevention gate needs, matching the existing block-break gate's
own documented simplification.

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
- **`SET_CREATIVE_MODE_SLOT`** — the next packet this model
  unblocks; needs `read_optional_item_stack`'s decode counterpart (see
  `crate::adapter::serverbound::write_optional_item_stack`'s doc comment for the existing
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
- `lodestone_data::block_items::item_for_block` — pick-block's block→item
  resolution, the generated-table inverse `block_for_item_id` already
  provides in the other direction.
- `crate::spawn_egg::entity_type_for_egg` — not called directly, but
  pick-entity's `spawn_egg_for_entity_type` is its mirror-image derivation
  and depends on the same name convention being exact.

## Verification

```bash
cargo test -p lodestone-server --lib --no-fail-fast -- inventory::
cargo test -p lodestone-v770 --lib --no-fail-fast -- inventory_decode_tests::
cargo test -p lodestone-v770 --test server_inventory_live
cargo test -p lodestone-server --lib --no-fail-fast -- item_use::
cargo test -p lodestone-data --test block_items -- item_for_block
cargo test -p lodestone-v770 --test serverbound_interaction_tier2
```

The last three cover the pick-block/pick-entity addition specifically: the
`item_use::` run is [`try_pick_item`](crate::item_use::try_pick_item)'s three
outcomes (hotbar hit, inventory swap with both an empty and an occupied
destination, creative create with and without a displaced stack, and the
survival-miss control), `item_for_block` is the generated-table inverse, and
`serverbound_interaction_tier2` is the wire round trip for both packets plus
`set_held_slot`.

The last one is a real `lodestone-client` (real `V770Adapter`) sending
`SET_CARRIED_ITEM` + `CONTAINER_CLICK` over an in-memory transport against
the real `V770ServerProtocol`/`serve_connection`, with the server's own
`PlayerInventory` (surfaced via `ServeSummary::inventory`, added for exactly
this reason) asserted afterward — not just a decode round trip. Watched
failing with the consumer temporarily neutered (`left: None, right:
Some(diamond_pickaxe)`), then restored via `cp` from a scratchpad backup
with an md5 check, per this repo's shared-checkout convention.
