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

Until this fix, `build_world_column` **collapsed** every solid block
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
which is exactly why that assertion is there: this fix's own brief predicted
"a fix that only thinks about solids will leave \[fluids\] broken and still
look like progress," and a first pass at this fix did precisely that, from an
unanticipated angle. See "Two bugs, not one" below.

## How it works

`build_world_column` resolves each cell's **real** block-state id, and since the
string-work unit below it does so without touching a string at all:

```rust
let id = source.block_state_id(lx as i32, wy, lz as i32);
if id != shape.air_id {
    section.set_block(lx, ly, lz, id);
}
```

`ChunkColumn::block_state_id` (`crates/lodestone-server/src/chunk.rs`) is a range
check plus two array indexes: the column stores its blocks as indices into a
small per-column palette, and `palette_state_ids[i]` is `palette[i]` already
resolved to a global 26.2 state id. The resolution happens **once per palette
entry**, when the entry is appended (`intern`) or when a whole generated palette
is adopted (`from_generated` → `recalc_ticking_counts`) — the same append-only
argument that makes `palette_ticking` sound.

The string resolver, `lodestone_data::block_states::state_id`, is still what
computes those ids, and `V770ServerProtocol`'s `resolve_state_id` is now a
one-line wrapper around it that supplies the air fallback. Its remaining callers
are per-*edit*, not per-block: `encode_block_update` echoing a neighbour cell's
existing state, and `encode_block_update_body`. Because the palette and the
encoder resolve through the same function, they cannot drift into two different
understandings of what a bare block name means.

Once a real id reaches `ChunkSection::set_block`
(`crates/lodestone-world/src/section.rs`), the underlying
[`PalettedContainer`](../crates/lodestone-world/src/container.rs) already handles
arbitrary content correctly — single-value, 4-bit-minimum indirect (widening as
needed, up to 8 bits), and direct-at-15-bits strategies transition automatically
as `set` is called with more distinct values. **No change was needed there**: it
was already generic and jar-verified: this bug was purely "the encoder never fed
it anything but two values," not a container limitation.

### Two bugs, not one

This fix's title names the encoder's collapse, but closing it required
fixing a second, independent bug in `resolve_state_id` (also in this file) —
without which fluids specifically would have stayed broken even after the
collapse itself was gone:

1. **The collapse** (`build_world_column` writing only `stone`/air, described
   above) — fixed by making it resolve real per-cell state.
2. **`resolve_state_id` had only two fallback tiers**: an exact
   name-and-properties match, or air. A **bare** block name for a block that
   *requires* properties (water chief among them — there is no propertyless
   water state) never matched anything and fell to tier two: air. This bug
   predates this fix — `encode_block_update` could already have hit it for
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
    (`.cache/mc/26.2/src/net/minecraft/world/level/chunk/LevelChunkSection.java`)
    stores the wire `nonEmptyBlockCount` directly, and
    `LevelChunk.replaceWithPacketData`
    (`.../world/level/chunk/LevelChunk.java`) never calls
    `recalcBlockCounts` afterward — so a real client trusts whatever count we
    send. Vanilla's own definition
    (`LevelChunkSection.recalcBlockCounts`)
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

## The per-block string work, and how it was removed

**Measured, both arms in one process, release, seed 1234, 8 real generated
columns × 5 repeats** (`crates/protocol/v770/tests/chunk_encode_cycles.rs`,
`cargo test --release -p lodestone-v770 --test chunk_encode_cycles -- --ignored
--nocapture`). Instructions retired, not wall clock: `DESIGN.md` §12.130 measured
this machine at 0.16–0.21% for instructions against 11.6–19.1% for wall clock,
with other agents always compiling.

| arm | insn/column | ns/column | spread |
|---|---|---|---|
| cell loop, string path (before) | 39,524,010 | 1,460,864 | 0.07% |
| cell loop, integer path (after) | 14,936,763 | 569,974 | 0.26% |
| whole `encode_chunk`, after | 39,963,080 | — | 0.08% |

24,587,247 instructions per column removed — **250 per cell**, a 2.65× cut on the
cell loop and **38% of what a served column cost end to end**. The version that
paid it did three things per cell that are all gone:

1. read a block-state **`&str`** (98,304 per column);
2. probed it through a per-column `HashMap<&str, u32>` — std's SipHash, per cell;
3. resolved each *distinct* entry through a scan of all 32,366 rows of the
   generated state table with a property-vector compare per name match. The
   8-column fixture has 225 palette entries, so that alone is ~900k row visits
   per column.

It survived the 21-unit worldgen optimisation drive
([`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md)) because the
generation cost metric excludes protocol encode **by definition** — no instrument
in that drive could see this code.

Two gates hold it:

- `src/server_protocol/chunk_encode_identity.rs` keeps the pre-change body
  *verbatim* as a control and asserts the two paths encode **byte-identical**
  `level_chunk_with_light` payloads for real columns, reporting a cell coordinate
  when they differ. Negative control run: perturbing
  `lodestone_data::block_states::state_id` by one id fails it at
  `chunk (0, 0) cell (0, -64, 0): integer path says 84, string path says 85
  (state string "minecraft:bedrock")`.
- `palette_state_ids_agree_with_the_jar_derived_dump` in the same file checks the
  ids against `block_name`/`properties`/`is_default_state` — jar dumps, read
  directly, never through either resolver. That is the outside expectation a
  two-implementation diff cannot supply.

## How to change it, and the gotchas

- **Never resolve a state string per block again.** The palette is the boundary:
  if you need a per-cell id, call `ChunkColumn::block_state_id`, or take
  `palette_state_ids()` and walk the index grid with `append_section_cells`. A
  local `HashMap<&str, u32>` memo is *not* good enough — hashing 98,304 strings
  was itself a measurable part of the 250 insn/cell removed above, independent of
  the scan it was memoizing.
- **`resolve_state_id`'s semantics live in `lodestone-data` now, and it is not
  "lowest id".** A bare block name resolves to the block's **jar-marked default
  state** with the caller's named properties written over it, per
  `lodestone_data::block_states::state_id`'s three tiers. The older "lowest id
  sharing the name" version shipped three bugs at once — snowy spread grass,
  wrong-facing directionals, and redstone dust rendering as
  climbing rather than flat. **Do not hand-duplicate the fallback.** Two test
  helpers did, became silent callers when it changed at `43a6e030`, and one failed
  as a 30-second live timeout rather than a mismatch.
- **A property no real state of the block carries is *synthetic* and dropped**
  (this server's `minecraft:comparator[…,output=N]`, which vanilla keeps in a
  block entity). That drop is in `state_id`, so every future synthetic property is
  covered without a new special case.
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

- `crates/lodestone-server/src/chunk.rs`'s `ChunkColumn::block_state_id` /
  `palette_state_ids` — the real per-cell **integer** source this encoder reads,
  and the palette-resolution point that keeps it integer. `block_state` (the
  `&str` form) is still there for the save, NBT and debug seams.
- `crates/lodestone-data/src/block_states.rs`'s `state_id` (the reverse map) plus
  `block_name`/`properties`/`air_state_id` — the generated protocol-776 state
  table and the resolver over it. `crates/lodestone-data/src/snow_support.rs`'s
  `is_default_state` supplies the jar's own default-state column, which is what
  makes tier 2/3 right.
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
