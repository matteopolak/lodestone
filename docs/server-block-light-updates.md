# Server block-light updates

## What it is

Why a torch placed after join emitted no light, what the served-light path was
actually measured to compute, and the column resend
`crates/lodestone-server/src/light.rs` gates so an emissive edit now relights.
Companion to [`server-chunk-light.md`](./server-chunk-light.md), which covers
producing the light bytes in the first place.

## How it works

### The audit first, because two plausible diagnoses were both wrong

The report was "torches don't emit light". The two candidate causes were *"the
server computes sky light only"* and *"it computes block light but the emission
source is not wired"*. Neither holds:

- `V770ServerProtocol::encode_chunk` calls `compute_served_light`, which is
  `lodestone_world::compute_column_light(column, &V770LightProps)`;
- that engine seeds **both** layers — sky from every cell open to the sky at `15`,
  block light from every cell whose block emits;
- `V770LightProps::emission` forwards to `lodestone_data::light_props::emission`,
  whose census carries `minecraft:torch` at **14** and gates it in
  `lodestone-data/tests/light_props.rs`;
- so a torch present when a column is *encoded* really does light that column.

What was missing is the *update*. Light is computed once per column at serve time,
and after join nothing re-sent it:

| link | state before this landed |
|---|---|
| `LIGHT_UPDATE` (packet 48) client decode | **present** — `v770/src/adapter.rs` reads all six fields and calls `World::merge_light` |
| `LightPatch::from_light_masks` three-state merge | **present**, gated |
| client re-mesh on a light change | **present** — the decode arm emits `ClientEvent::ChunkLoaded`, which doubles as "this region is dirty" |
| **server-side `LIGHT_UPDATE` encoder** | **absent — no `ServerProtocol` method, no v770 override** |

Eight of nine links green and the ninth never built. Placing a torch changed the
block (a `BLOCK_UPDATE` arrived and the client drew the torch) and structurally
could not change the light.

### The fix: a gated column resend

`crate::light::should_relight(old, new)` compares the two states' **emission**,
and `crate::server::resend_column_for_light` answers a hit by re-sending that whole
column through the `begin_chunk_batch`/`encode_chunk`/`end_chunk_batch` sequence
the move handler already uses. `encode_chunk` recomputes light from the column it
is handed — which now contains the torch — so correct light arrives with **no new
encoder and no new trait method**. The client's `level_chunk_with_light` arm
replaces the chunk and emits `ChunkLoaded`, the same re-mesh signal the
`LIGHT_UPDATE` arm emits.

Two call sites: `destroy_block` (breaking a torch must darken) and the placement
handler's tail (placing one must brighten).

The gate compares **values, not strings**. `minecraft:torch` and
`minecraft:wall_torch[facing=north]` both emit 14, so re-orienting a torch is not a
relight, while `redstone_torch[lit=false]` → `[lit=true]` (`0` → `7`) is. It also
cannot be `emission(new) > 0`: removing a light source has to relight too, and a
break writes `minecraft:air`.

## How to change it, and the gotchas

**This is a blunt instrument and the cost is the reason the gate is narrow.** A
full column is a few tens of KiB against the 2 KiB nibble array a `LIGHT_UPDATE`
would carry. It is affordable only because `should_relight` fires on torches,
lanterns, glowstone, sea lanterns, campfires and a furnace lighting — not on
ordinary placement. **Do not widen it to `dampening`**: that fires on nearly every
block placed and turns every placement into a column resend. If sky light needs to
follow an edit, build the encoder, not a wider predicate.

**Three gaps remain, and the first two need one brokered patch each.**

**1. Sky light does not follow an edit.** Mining a roof does not re-send the
column's sky light. The fix is the `LIGHT_UPDATE` encoder:

- `crates/lodestone-server/src/protocol.rs` — a new `ServerProtocol` method with a
  `ServerDirective::None` default, e.g.
  `fn encode_light_update(&self, cx: i32, cz: i32, light: &lodestone_world::ColumnLight) -> ServerDirective`;
- `crates/protocol/v770/src/server_protocol.rs` — the override. `ColumnLight::encode`
  already writes the exact `ClientboundLightUpdatePacketData` shape, so the body is
  a VarInt `cx`, a VarInt `cz`, then that payload, under
  `play::clientbound::LIGHT_UPDATE`. Note the wire order is sky/block/empty-sky/
  empty-block masks then the two array lists — **not** `LightPatch::from_light_masks`'
  argument order;
- then swap `resend_column_for_light` for a call to it, and widen `should_relight`
  to include `dampening`.

**2. Light does not cross a chunk border**, so a torch at local `x = 15` does not
light its eastern neighbour at all. This is not a gap in this module: it is
`compute_served_light` running the **isolated** compute, and it is the same open
item [`server-chunk-light.md`](./server-chunk-light.md) records as a measured
**Δ5** sky-light dark bias at column borders (7 of 10 chunks identical, worst case
121 of 212,992 cells, never brighter). Its five-step plan is still current — step 1
(promoting `lodestone-world` to a real dependency) is already done — **but it is
missing a trap that would make the fix look like it worked while serving stale
light:**

> If `ChunkColumn` carries a precomputed `light`, then `ChunkColumn::set_block`
> **and** `ChunkStore::set_block` must invalidate it. Both write blocks into a
> retained column without touching anything derived from them. A `column.light()`
> that survives an edit makes the resend above serve the light the column had
> *before* the torch was placed — a correct-looking wire, a re-meshed client, and
> no change on screen. The `Option<ColumnLight>` must be set to `None` by any
> block write, and refilled by whoever next serves the column.

Note the ordering consequence: with the seam fix in, an emissive edit within 15
blocks of a border has to resend the **neighbouring** columns too. Until then those
columns' light is byte-identical whether they are resent or not, which is why
`resend_column_for_light` sends one column and no more — sending the neighbours
today would be dead weight.

**3. Only the acting connection is told.** The resend rides that connection's own
`Connection`, like every other confirmation in `dispatch_play_packet`. On
singleplayer that is every player; on open-to-LAN a second player sees the torch
and not its light until they leave and re-enter the column.

## Configuration

None. No feature flags, no env vars. The relight is unconditional on an
emission-changing edit.

## Dependencies

- `lodestone-data` — `light_props::emission`, the 26.2 per-block-state census.
- `crate::chunk::resolve_palette_state_id` — the single definition of the
  state-string → id fallback, deliberately called rather than re-derived.
- `crate::server` — the two call sites and `resend_column_for_light`.
- `ServerProtocol::{begin_chunk_batch, encode_chunk, end_chunk_batch}` — the
  existing chunk-send seam, unchanged.
