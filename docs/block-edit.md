# Block editing: dig, place, and a world that actually holds the change

## What it is

The piece that closes the loop a served session was still missing after
[`docs/served-session-liveness.md`](./served-session-liveness.md): a player on our own
server could walk around real generated terrain but could not change one block of it.
This is the decode → mutate → confirm path for the two serverbound editing packets —
`PLAYER_ACTION` (block-breaking) and `USE_ITEM_ON` (block placement) — plus the
retention that lets an edit survive a chunk being forgotten and re-sent.

**In scope:** decoding both packets into version-free `ServerBound` variants, applying
the edit to the server's own retained chunk data, confirming it back to the acting
client with a real `block_update` packet, and respecting the break *sequence*
(start/abort/finish) as three distinct events rather than collapsing them into one.

**Out of scope, deliberately:** break timing/hardness validation (the client's own
`lodestone-shell` predictor already gates when it sends `StopDestroy`, using real
per-state hardness — see `lodestone-shell/src/interact.rs`'s `drive_mining`), item
consumption from inventory (this crate has no inventory model at all — see "Placement
always places stone" below), drops, block-entity data, redstone/neighbour updates, and
any placement rule beyond "the state implied by clicking a face" (stairs, slabs, doors,
fences all need a cursor-derived orientation this does not compute). Interaction-range
and spawn-protection checks are also skipped: this crate tracks no player position
beyond the view-tracking column, so there is nothing to check a range against.

## How it works

### Where it lives

* `crates/lodestone-server/src/protocol.rs` — two new [`ServerBound`] variants
  (`BlockAction { action, pos, face, sequence }`, `UseItemOn { pos, face, sequence }`)
  and one new [`ServerProtocol`] method (`encode_block_update`), defaulted to emit
  nothing — the same "protocol crate that doesn't implement it just serves without it"
  contract every other method in this trait already follows.
* `crates/lodestone-server/src/chunk.rs` — the [`ChunkSource`] trait grows two more
  default methods (`block_state` read, `set_block` write, both no-ops/pass-through by
  default), and [`OverworldChunkSource`] gets the one implementation that actually
  persists a `set_block` call. See "Where edited state lives" below — this is the
  design question the task set out to answer, not an implementation detail.
* `crates/lodestone-server/src/server.rs` — `apply_block_action` and
  `apply_use_item_on`, the version-free handlers `dispatch_play_packet` calls for the
  two new `ServerBound` variants. Neither names a wire id or a protocol number.
* `crates/protocol/v770/src/server_protocol.rs` — the protocol-776 decoders for
  `player_action`/`use_item_on` and the encoder for `block_update`.

### Where edited state lives, and why that was the real work

Before this, **nothing retained a generated column at all**:
`OverworldChunkSource::column` called straight through to
`self.generator.column(cx, cz)` on every single request. That is fine for read-only
terrain — the generator is deterministic, so "regenerate every time" and "cache
forever" are observationally identical — but it means a `set_block` with nothing behind
it would vanish the moment its chunk left a client's view and came back
(`ViewTracker::recenter`'s forget/resend cycle in `server.rs`). Building that retention
*is* the task, not a preliminary to it.

`OverworldChunkSource` now holds `edits: Mutex<HashMap<(i32, i32), ChunkColumn>>`,
populated **only** by `set_block` — not by every `column()` read. An unedited column is
still regenerated fresh on every request, unchanged cost, unchanged behaviour (the
pre-existing `chunk_source_serves_generator_block_for_block` test still passes
unmodified, because it never edits anything). Only a column a player has actually
touched pays for a permanently-retained `ChunkColumn`. Caching *every* generated column
(touched or not) was the other option on the table; it was rejected because it would
make memory scale with how much of the world a session has merely looked at, not with
how much it has changed.

`WorldgenChunkSource` (the solidity-only stand-in kept for transport tests) gets none
of this — its `set_block` is the trait's no-op default, so edits against it are
silently discarded. That is intentional: it exists to prove the wire round-trip
deterministically, not to be an editable world, and the block-edit end-to-end test
(below) uses the real `OverworldChunkSource` specifically because it is the one type
whose retention is under test.

### The break sequence

`apply_block_action` tracks one `pending_break: Option<BlockPos>` per connection —
the version-free analogue of vanilla's `ServerPlayerGameMode.destroyPos` field:

* `StartDestroy` sets `pending_break = Some(pos)`. No terrain mutation, no packet sent
  — matching vanilla's own non-instamine path, and correct given hardness/timing
  validation is out of scope here.
* `AbortDestroy` clears `pending_break` **only if it matches `pos`**, mirroring
  vanilla's `pos.equals(this.destroyPos)` guard (`ServerPlayerGameMode.java:217`,
  `:239-248`). No mutation, no packet.
* `StopDestroy` only breaks the block **if `pending_break` still equals `pos`** —
  writes `minecraft:air` via `ChunkSource::set_block`, then sends one
  `encode_block_update` confirming it. A `StopDestroy` for a position nobody started
  (or one already aborted) is a no-op, same as vanilla.

This is what makes `Start` + `Stop` break a block while `Start` + `Abort` does not —
the block-edit end-to-end test drives exactly that sequence and asserts the abort left
the block untouched before ever sending the `Stop`.

### Placement

`apply_use_item_on` mirrors `BlockPlaceContext`'s replace-vs-relative choice
(`BlockPlaceContext.java`'s constructor): if the clicked cell's own state is
`is_air_or_fluid` (replaceable), the new block lands *there*; otherwise it lands at the
`face`-neighbour cell (`relative(pos, face)`, vanilla's `BlockPos.relative(Direction)`).
If that target cell is not itself replaceable, nothing is written — but two
`block_update` packets are sent regardless, for **both** `pos` and its neighbour,
matching vanilla's own `handleUseItemOn`
(`ServerGamePacketListenerImpl.java:1397-1398`), which sends both unconditionally: this
is what corrects a client that predicted a placement the server rejected, and what
happens to also upgrade the *other* cell's client-visible fidelity (see the wire gap
below) even when nothing about it actually changed.

**Placement always places `minecraft:stone`.** This crate has no inventory model at
all — no held-item tracking, no slot state — so there is no way to know what block the
clicked hand is holding. Rather than half-build an inventory seam for this one purpose,
placement always writes the one block a "plain full-cube block" claim can honestly make
regardless of what the client thinks it is holding. A real per-item placement needs a
server-side inventory model; that is a separate, larger piece of work.

### `is_air_or_fluid` doubles as "replaceable"

`chunk.rs`'s `is_air_or_fluid` (already used to compute `ChunkColumn::is_solid`) is
reused as the placement-replaceability test. Vanilla's real `canBeReplaced` covers more
(tall grass, snow layers, fire, …), but the generator this crate serves produces none
of that vegetation yet (`worldgen_data.rs`'s own "no caves/ores/trees" scope note), so
air-or-fluid is the whole replaceable set that can actually appear in served terrain
today.

## A discovered, pre-existing wire-fidelity gap (not fixed here)

While building the end-to-end test it became clear that
`V770ServerProtocol::encode_chunk`'s `build_world_column` — used for **every**
whole-column send, at join and at every `ViewTracker` resend, unrelated to this
change — collapses every solid block to a single `minecraft:stone` and everything
non-solid (air *and every fluid*) to air. It only ever calls
`ChunkSection::set_block(…, stone)` under an `is_solid` check and touches the section no
other way. So a real client's chunk store cannot see `deepslate`/`gravel`/`water`/etc.
at all through a whole-column send — only "solid" or "not". The server's own
`ChunkColumn` data is real (block-for-block verified by `worldgen_data.rs`'s existing
`chunk_source_serves_generator_block_for_block` test); it is only the *bulk terrain
encoder* that throws the variety away on the way to the wire.

This is orthogonal to block editing specifically: `encode_block_update` (this change)
resolves the real block-state string via `resolve_state_id`, so a break/place
confirmation always carries full fidelity — and, as a side effect, **upgrades** the
confirmed cells' client-visible state from the wire-collapsed stone/air to their real
value, even for a cell the edit didn't actually touch (see `block_edit.rs`'s
`dig_and_place_persist_through_forget_and_reload` test, which documents and exploits
this for its "clicked cell unchanged" assertion). Fixing `build_world_column` itself —
so a whole-column send shows real block variety — is a separate, larger change to a
path many other things depend on, and is not attempted here.

## How to change it, and the gotchas

* **A protocol crate that does not implement `encode_block_update` still compiles and
  still serves** — same contract as every other `ServerProtocol` method. It just never
  confirms an edit back to the client; the server-side mutation still happens.
* **`ChunkSource::block_state`'s default recomputes the whole column.** Cheap enough
  for an occasional dig/place (not the hot render path), but a future `ChunkSource`
  with expensive generation should consider overriding it directly rather than relying
  on the default, the way `OverworldChunkSource` could but currently does not need to
  (its `column()` already consults the edit cache, so the default is already correct
  and cheap enough there).
* **`resolve_state_id` (`server_protocol.rs`) is a linear scan over the ~32k-entry
  generated state table**, matching name *and* properties. Fine for an occasional
  confirmation packet; do not reach for it in a hot path (`encode_chunk`'s
  `build_world_column` does not use it, for exactly this reason plus the collapse
  described above).
* **`pending_break` is per-connection, not per-block.** Two different connections
  digging the same block concurrently is not modelled — the second `StartDestroy`
  simply overwrites the first connection's own tracked position, same as vanilla's
  single `destroyPos` field per `ServerPlayerGameMode` instance.
* **No `ClientboundBlockChangedAckPacket`.** Vanilla's `ackBlockChangesUpTo` answers
  every dig/place with a sequence-number ack so the client's own prediction can
  reconcile. `lodestone-client`'s adapter already decodes this packet into
  `ClientEvent::BlockChangedAck` (`adapter.rs`'s `BLOCK_CHANGED_ACK` arm), but nothing
  in the client stack consumes that event yet, so sending it would currently be inert.
  A future prediction-reconciliation feature needs both halves; only the decode half
  exists today.
* **Changing what gets placed:** `apply_use_item_on` in `server.rs` hardcodes
  `crate::chunk::STONE`. There is no seam to swap it per-item without first adding an
  inventory model — do not try to thread an item id through without one; the whole
  point of the current design is that it is honest about not having one.

## Configuration

No env vars or flags. The one constant worth knowing: `crate::chunk::{AIR, STONE}`
(`pub(crate)` string constants in `chunk.rs`) are what `set_block` ever writes through
this path.

## Dependencies

* `lodestone_model::{BlockActionKind, BlockFace, BlockPos}` — the version-free
  break-phase/face/position vocabulary already defined for the client's own
  `ClientAction`, reused here rather than duplicated for the serverbound side.
* `crate::packets::game::{PlayerAction, UseItemOn}` (`v770`) — already-derived
  `Encode`/`Decode` structs this work reuses for decode rather than hand-rolling a
  mirror (the same "derive over hand-roll" discipline `server_protocol.rs`'s own module
  doc explains); they existed already because `lodestone-shell`'s own sender already
  used them client-side.
* `.cache/mc/26.2/src/net/minecraft/server/level/ServerPlayerGameMode.java`,
  `.../server/network/ServerGamePacketListenerImpl.java`,
  `.../network/protocol/game/{ServerboundPlayerActionPacket,ServerboundUseItemOnPacket,
  ClientboundBlockUpdatePacket}.java`, `.../world/item/{BlockItem,context/
  BlockPlaceContext}.java`, `.../world/phys/BlockHitResult.java`,
  `.../network/FriendlyByteBuf.java` (`readBlockPos`/`readBlockHitResult` and their
  `write` counterparts) — the jar sources this behaviour and wire layout are measured
  against.
* `crates/protocol/v770/tests/block_edit.rs` — the real-client end-to-end test;
  `crates/lodestone-server/src/worldgen_data.rs`'s
  `set_block_persists_across_repeated_column_calls` — the hermetic retention proof,
  same fixture, no network/client involved.

[`ServerBound`]: ../crates/lodestone-server/src/protocol.rs
[`ServerProtocol`]: ../crates/lodestone-server/src/protocol.rs
[`ChunkSource`]: ../crates/lodestone-server/src/chunk.rs
[`OverworldChunkSource`]: ../crates/lodestone-server/src/chunk.rs
