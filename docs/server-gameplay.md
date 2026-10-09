# Server-authoritative gameplay: breaking, inventory, crafting, effects, advancements

## What it is

Gameplay surfaces where the server derives the real outcome itself and only compares a client's prediction against it: block breaking, inventory and container clicks, crafting, server-initiated effects, and advancements and statistics. A disagreement is corrected, never silently accepted.

## How it works

### Initial game mode

The world's default mode is shared `WorldStateHandle` configuration. Persistent worlds load it from the integer `GameType` in `level.dat` before spawning tasks, and autosave and shutdown write it back. Missing, malformed or unsupported values keep the current default (Survival if unconfigured); a returning player's saved mode wins.

Native online worlds load the same metadata into `LanConfig::world_state` before the listener starts. Browser hosts pass a typed `lodestone_model::GameMode` to `IntegratedServer::serve_with_transport_in_mode`. Keep `WorldStateHandle::load_level_data` and `WorldStateHandle::level_data_fields` together when changing the stored format.

The connection's play future is built inside `pin_future` and heap-pinned at the login handoff; poll timing borrows the pinned future. Keep that boundary when changing connection instrumentation, because the play and dispatch futures are large.

### Block-break validation

A dig is timed from block hardness and the held tool, using a lower-bound speed estimate with generous headroom: the server does not track enchantments or effects, so the aim is rejecting impossible digs, not reproducing exact timing.

- A start plus stop that clears the threshold breaks the block; a zero-hardness block breaks on start alone.
- A stop that arrives too early is deferred, never refused: progress keeps accruing on the server clock and the block breaks a tick or two late with no rollback. On a local server both packets land on the same tick, so refusing broke ordinary mining.
- Creative takes a start-only path with no hardness clock and no drops, but still runs range, known-state, non-air, unbreakable-state and plugin-proposal checks.

### Block-prediction acknowledgement

`ServerBound::BlockAction` and `UseItemOn` keep the raw prediction sequence; `UseItem::sequence` is optional because older protocols have none. The server validates it as nonnegative, converts to `PredictionSequence`, and acknowledges the greatest processed value once per connection tick after block updates. An acknowledgement means processed, not successful.

`ServerProtocol::encode_block_changed_ack` defaults to no output. Preserve absent legacy sequences as `None`.

### Inventory and container clicks

The server holds its own inventory model, with native slot numbering deliberately restated rather than shared with the client (this crate is client- and version-free). A join sends the restored inventory as an explicit snapshot.

A click's outcome is derived by replaying the click state machine server-side and comparing with the client's claimed diff. Agreement sends nothing; disagreement sends a full corrective resync. Trusting the client's diff would let any client mint any item into any slot.

`publish_open_container` runs every 50 ms in both the native and browser loops, diffing the open block entity's slots and properties against the last snapshot and sending changes (furnace progress updates while the screen is open). A new ticking menu must keep both call sites.

Beacon power keys from packets and saved NBT resolve into the closed `BeaconPower` domain before reaching `BeaconData`; built-in effects that are not beacon powers and custom keys are rejected there.

When a gameplay action does nothing, check that its wire value is in the server's decode table before suspecting anything downstream.

### Crafting

The server keeps its own grid and re-derives the result on every input change from the bundled recipe corpus. A client cannot write the result slot, but taking it (a click) is how crafting happens, consuming ingredients as a side effect.

Disagreement detection compares the client's claim against the whole derived menu state, not only the slots the client names; a client cannot predict a result it never computed.

A crafting table has no block entity: opening it creates a transient grid in per-connection state. Closing must return the grid and the cursor stack to the player (or drop them). The close handler collects slots written by `PlayerInventory::add`, deduplicates, and publishes final values in window-0 numbering so the client's closed menu receives them.

Recipe-book place requests use an opaque index into the recipe list the server sent at join. That list must be sent, and the index space must come from the same ordering used to resolve it, or the wrong recipe is placed.

### Server-initiated sounds, particles and level events

Anything the client cannot predict (mob hurt sounds, redstone doors, block breaks by others) must be sent explicitly. The gotcha: the client already predicts its own block break and placement sounds, so an effect published to the acting connection plays twice unless that player is excluded from the broadcast. Other connections still need it.

### Boxed protocol wrapper trap

Every gameplay encoder here (crafting and recipe book, world effects, advancements and statistics) is a defaulted `ServerProtocol` method and must also be forwarded by the generic boxed-protocol wrapper that singleplayer uses. A missing forward is not a compile error; it silently answers with the do-nothing default, and only singleplayer is affected.

### Advancements and statistics

A version-free model of the advancement tree, per-player criteria and a statistics counter, streamed over dedicated packets. An advancement is complete when every requirement group has a satisfied criterion. Visibility is a fixed shallow window around completed nodes.

Progress flushes incrementally, except that a join always sends the complete tree first. The open advancement tab is short-lived connection state: open, switch and close reports update the selected tab and emit a selection directive. Do not validate the tab identifier against the bundled tree, since a client may have a larger data-pack tree.

## How to change it

- **New inventory slot or equipment kind**: extend the inventory model and the menu-slot to native-slot mapping together, matching the client's table.
- **New container kind**: give it a real backing model and slot layout; click derivation is generic over the open menu, not per kind.
- **New effect**: add a case to the shared effect vocabulary and its one encoder, not a second transport lane.
- **New server-derived encoder**: add its boxed-wrapper forward in the same change. Prefer a test that enumerates the trait's methods against the wrapper over a hand-kept list.
- **Loosening the break-speed check**: only once the server tracks the real speed inputs; narrowing headroom first rejects legitimate players.

## Configuration

No runtime flags. Behaviour is fixed constants (break thresholds, effect ranges) or bundled data (recipe corpus, advancement tree).

## Dependencies

- Generated per-block-state and per-item data (hardness, tool speed, sound and particle registries).
- The bundled recipe corpus and the version-free recipe matcher shared with the client.
- The `ServerProtocol` seam ([dedicated-server.md](dedicated-server.md)); none of these modules names a packet id or protocol version.
