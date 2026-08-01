# Chunk column encoding: real block states on the wire (issue #363)

## What it is

`V770ServerProtocol::encode_chunk`'s `build_world_column` turns one
`lodestone-server` [`ChunkColumn`](../crates/lodestone-server/src/chunk.rs) — the
server's own per-block-state terrain data, whatever produced it (the real
generator, an edit, a stand-in) — into the version-free
[`lodestone_world::ChunkColumn`](../crates/lodestone-world/src/column.rs) that
`encode_column_body` serialises as the `level_chunk_with_light` packet body: the
whole-column send every client gets at join, and that `ViewTracker` resends as a
player walks (`crates/lodestone-server/src/server.rs`).

Until this fix (issue #363), `build_world_column` **collapsed** every solid block
to `minecraft:stone` and everything non-solid — air *and every fluid* — to air,
via `if source.is_solid(...) { section.set_block(..., stone) }`. The server's own
terrain data was, and is, real (grass, dirt, deepslate, gravel, water, ores as the
generator grows to produce them — verified block-for-block against a real 26.2
JVM oracle over 98,304 blocks/chunk); only the *encoder* threw the variety away on
the way to the wire. A client joining our own server had never actually seen it.

**A second, independent bug was hiding behind the first, and fixing only the
collapse would not have fixed water.** `lodestone-worldgen`'s
`OverworldGenerator` writes its default fluid as the bare literal
`"minecraft:water"` (`crates/lodestone-worldgen/src/overworld.rs`'s
`default_fluid`), with no `level` property — but real water has **no
propertyless state at all** (every id in `86..=101` carries `level=0..15`; see
`.cache/mc/26.2/generated/reports/blocks.json`'s own entry for
`minecraft:water`). So even after `build_world_column` started calling
`resolve_state_id` for every cell, a fluid cell's exact-match scan found
nothing and fell straight through to `resolve_state_id`'s *own* pre-existing
no-match fallback: air. This crate's own hermetic gate
(`encode_chunk_carries_real_block_states_including_a_fluid`) caught this the
first time it ran — its `assert_ne!(water_id, air_id())` sanity check failed —
which is exactly why that assertion is there: issue #363's own brief predicted
"a fix that only thinks about solids will leave \[fluids\] broken and still
look like progress," and a first pass at this fix did precisely that, from an
unanticipated angle. See "Two bugs, not one" below.

## How it works

`build_world_column` now resolves each cell's **real** block-state id, via
`ServerChunkColumn::block_state(x, y, z) -> &str` (already used, at a single
coordinate, by `V770ServerProtocol::encode_block_update`) and this module's
existing `resolve_state_id`, a linear scan over the generated ~32k-entry
protocol-776 state table matching both block name and property values.

```rust
let state = source.block_state(lx as i32, wy, lz as i32);
let id = *seen.entry(state).or_insert_with(|| resolve_state_id(state));
if id != shape.air_id {
    section.set_block(lx, ly, lz, id);
}
```

`resolve_state_id` alone is too expensive to call once per block: a column is
16×16×384 = 98,304 cells, and a linear scan is `O(STATE_COUNT)` (~32k), so an
unmemoized call site would be billions of comparisons per column, on every join
and every view-tracker resend. `seen: HashMap<&str, u32>` memoizes by the
block-state string itself, local to one `build_world_column` call: a real
column's *distinct* strings number in the dozens
([`docs/chunk-memory-pool-footprint.md`](./chunk-memory-pool-footprint.md)
records live sections as 4-bit indirect palettes with at most 6 entries each), so
the expensive scan runs once per distinct string, not once per block. The map is
not carried across calls — different columns are different data, so there is
nothing durable to cache across them without the source outliving one
`encode_chunk` call.

Once a real id reaches `ChunkSection::set_block`
(`crates/lodestone-world/src/section.rs`), the underlying
[`PalettedContainer`](../crates/lodestone-world/src/container.rs) already handles
arbitrary content correctly — single-value, 4-bit-minimum indirect (widening as
needed, up to 8 bits), and direct-at-15-bits strategies transition automatically
as `set` is called with more distinct values. **No change was needed there**: it
was already generic and jar-verified: this bug was purely "the encoder never fed
it anything but two values," not a container limitation.

### Two bugs, not one

Issue #363's title names the encoder's collapse, but closing it required
fixing a second, independent bug in `resolve_state_id` (also in this file) —
without which fluids specifically would have stayed broken even after the
collapse itself was gone:

1. **The collapse** (`build_world_column` writing only `stone`/air, described
   above) — fixed by making it resolve real per-cell state.
2. **`resolve_state_id` had only two fallback tiers**: an exact
   name-and-properties match, or air. A **bare** block name for a block that
   *requires* properties (water chief among them — there is no propertyless
   water state) never matched anything and fell to tier two: air. This bug
   predates issue #363 — `encode_block_update` could already have hit it for
   any bare, property-requiring state string, though nothing before this
   change ever exercised that path with one.

`resolve_state_id` now has three tiers: exact match, then a **same-name
default** (the lowest-id state sharing the block name), then air only if the
name itself has no match at all.

**"Lowest id" is not a general vanilla-registration guarantee — checked, not
assumed.** A one-off scan of `blocks.json` found the lowest-id state
disagrees with the block's actual marked `"default": true` state for 661 of
797 multi-state blocks (e.g. `minecraft:acacia_button`'s default is id
`10780`, not its lowest id `10771`). It happens to hold for both fluids this
fallback can currently reach — water (`86` lowest = `86` default) and lava
(`102` lowest = `102` default), each confirmed directly against `blocks.json`'s
own marker, not inferred from a pattern. The fallback resolving bare
`"minecraft:water"` to the *right* state is a fact about water specifically,
not a property this fallback derives correctly in general. **Do not extend
its coverage to a new bare, property-requiring block name without checking
`blocks.json`'s own `"default"` marker for that specific block first.**

### Nothing else depended on the collapse

Checked, not assumed, before changing this:

- **Heightmaps** are sent empty (`Heightmaps::new()`) and **light** is sent as
  all-`Missing` (`ColumnLight::new(...)`) regardless of block content — a
  pre-existing, separately documented gap, untouched by this fix either way.
- **The per-section `nonEmptyBlockCount`/`fluidCount` wire shorts**
  (`encode_column_body`) are handled differently by the two decoders, and this
  fix happens to make both of them *more* correct, not less:
  - **This crate's own decoder** (`crates/protocol/v770/src/packets/chunk.rs`'s
    `read_sections`) consumes and discards the leading shorts, then recomputes
    the block count straight from the container
    (`ChunkSection::from_containers`'s non-air scan) — so it never depended on
    what this encoder wrote for that field, before or after the fix.
  - **A real vanilla client does not recompute**: `LevelChunkSection.read`
    (`.cache/mc/26.2/src/net/minecraft/world/level/chunk/LevelChunkSection.java:163-166`)
    stores the wire `nonEmptyBlockCount` directly, and
    `LevelChunk.replaceWithPacketData`
    (`.../world/level/chunk/LevelChunk.java:523-532`) never calls
    `recalcBlockCounts` afterward — so a real client trusts whatever count we
    send. Vanilla's own definition
    (`LevelChunkSection.recalcBlockCounts`, `LevelChunkSection.java:122-153`)
    counts every state where `!state.isAir()`, i.e. **fluids count as
    non-air** — the same "non-air-id" test `ChunkSection::set_block`'s
    `non_air_count` bookkeeping already used. The pre-fix encoder derived this
    count from the `is_solid` collapse, which excludes fluids, so it was
    already sending an **undercount** to any real client for a fluid-bearing
    section — a second, narrower symptom of the same root cause. This fix's
    count is now the real one, computed off the same real ids the container
    holds; no separate change was needed for it to become correct.
- **`is_solid`/`solid_count`** (`crates/lodestone-server/src/chunk.rs`) remain
  used directly by server-side mob pathing
  (`crates/lodestone-server/src/mobs.rs`'s `ChunkWorld::is_solid`), which reads
  the source `ChunkColumn` itself, never the wire-encoded copy this module
  produces — unaffected by this change in either direction.

The collapse was, as its own function name suggested, a bring-up shortcut that
outlived its purpose — not load-bearing for anything else in the pipeline.

## How to change it, and the gotchas

- **Don't call `resolve_state_id` per-block without memoizing.** See the
  performance section above; the whole point of `seen` is amortizing the linear
  scan. If a future change needs per-block resolution somewhere else *not*
  already inside a `build_world_column`-style loop, either add the same
  local-`HashMap` pattern or push a name→id index into `lodestone_data` — don't
  reach for the bare linear scan in a true hot path.
- **`resolve_state_id` has three fallback tiers now, not two** — exact match,
  same-name lowest-id state, then air; see its own doc comment ("Two bugs, not
  one" above). Only a name with *zero* matches in the table (an unknown block,
  or a name typo) reaches air; a known block with the wrong/missing properties
  gets its lowest-id same-name state instead. Never a panic either way — a bad
  string degrades to a visibly-wrong-at-worst block rather than crashing the
  connection.
- **"Lowest id" is not "default" in general — verified false for 661/797
  multi-state blocks.** It is only confirmed correct for the two fluids this
  fallback can currently reach (water, lava). **Do not extend this fallback's
  reach to a new bare, property-requiring block name without checking
  `blocks.json`'s own `"default": true` marker for that specific block
  first.** If a future caller wants a bare name to resolve to a block's *real*
  default and that block is not water or lava, verify it the same way — do
  not assume the lowest id is right.
- **`stone_id()` is now `#[cfg(test)]`-only.** It was `encode_chunk`'s literal
  fallback before this fix; the only remaining reference is
  `encode_block_update_wire_layout`'s pinning assertion. Do not resurrect it as
  a non-test fallback without a reason — the whole point of this fix is that
  `build_world_column` no longer needs a single hardcoded block.
- **Heightmaps and light are still not computed for real** — a separate,
  pre-existing, documented gap (see `encode_column_body`'s own doc comment).
  Fixing that is unrelated future work, not part of this change.

## Configuration

No env vars or flags. `ChunkShape::overworld_1_21()`
(`crates/protocol/v770/src/packets/chunk.rs`) fixes the overworld's `min_y`,
section count, and palette kinds (`PaletteKind::block_states()`: 4-bit-minimum
indirect, 15-bit direct — sized for the ~32k-entry protocol-776 global registry).

## Dependencies

- `crates/lodestone-server/src/chunk.rs`'s `ChunkColumn::block_state` — the real
  per-cell string source this encoder now reads instead of `is_solid`.
- `crates/lodestone-data/src/block_states.rs`'s `block_name`/`properties`/
  `STATE_COUNT` — the generated protocol-776 state table `resolve_state_id`
  scans.
- `crates/lodestone-world/src/{container.rs,section.rs}` — the paletted-container
  strategies and non-air bookkeeping that already handled arbitrary content
  correctly; unmodified by this fix.
- `.cache/mc/26.2/src/net/minecraft/world/level/chunk/{PalettedContainer,
  LevelChunkSection}.java` — the wire-format and count-recomputation reference
  this doc's claims above are checked against.
- `.cache/mc/26.2/generated/reports/blocks.json` — Mojang's own block report,
  the source for water's `id`/`level`/`"default"` facts `resolve_state_id`'s
  same-name-default fallback relies on.
- `crates/lodestone-worldgen/src/overworld.rs`'s `default_fluid` — the bare
  `"minecraft:water"` literal that made the fallback necessary in the first
  place; not modified by this fix (out of this crate's scope — see the task
  report), but load-bearing context for why `resolve_state_id` needed a third
  tier.
- `crates/protocol/v770/tests/block_edit.rs`'s
  `dig_and_place_persist_through_forget_and_reload` — the real-client end-to-end
  gate (an actual `lodestone-client` reading `handle.block_at` after a whole
  -column send, not a round-trip through this crate's own encoder); and this
  crate's own `server_protocol.rs::block_edit_tests::
  encode_chunk_carries_real_block_states_including_a_fluid` — the fast hermetic
  complement, decoding a real `encode_chunk` send back through the real wire
  codec and checking deepslate/gravel/water ids individually, water (a fluid)
  included since that is the case the old collapse mapped to air rather than
  stone.
