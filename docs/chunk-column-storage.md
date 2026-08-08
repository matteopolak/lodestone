# Server-side chunk column storage

## What it is

`SectionedBlocks` (`crates/lodestone-server/src/chunk_blocks.rs`) is how a
server-side `ChunkColumn` stores its block-state indices: one bit-packed 16-row
section at a time, each either a single repeated value or a packed array sized to
the palette ids that section actually uses. It replaced a flat `Vec<u16>` over the
column's full height, which was **192 KiB of the 195.5 KiB** the chunk store
measured per retained column. It is the storage half of unit **U8** of
[`plans/chunk-lifecycle.md`](./plans/chunk-lifecycle.md), issue #551, and it cut
per-column residency to **31.1 KiB, measured** — so the singleplayer store at
`render_distance` 32 went from **867 MiB to 139.2 MiB**, both measured under
`/usr/bin/time -l`.

## How it works

A column's palette is still a column-wide `Vec<String>` of block-state strings
(`palette[0] == "minecraft:air"`), and cells are still indices into it in
`(y_local * 16 + z) * 16 + x` order. Only the *cells* changed. One `Section` per
16-row window counted from `min_y` — the same windows `SECTION_ROWS` governs and
`ChunkColumn::section_ticking` indexes:

| variant | holds | heap bytes |
|---|---|---|
| `Uniform(id)` | every cell is `id` | **0** |
| `Packed { bits, longs }` | ids at `bits` wide, `64 / bits` per `u64`, none spanning a boundary | `longs.len() * 8` |

`bits` is `ceil(log2(max_id + 1))`, floored at 1. It only grows, and only on a
write the current width cannot hold; the widening rebuilds that one section (4,096
reads) and nothing else. A section never narrows and never collapses back to
`Uniform` — both are bookkeeping for a case that does not recur, since a column is
built once and then edited a handful of times.

Two independent savings, and the first is much the larger:

- **An all-one-value section allocates nothing.** A full overworld column is 24
  sections and terrain occupies roughly the lower half, so about half of every
  column was 4,096 cells of `0`. Vanilla's own chunk format has exactly this case
  and writes no `data` array for it; so does our *client*
  (`lodestone_world::Storage::Single`).
- **A populated section packs to the width its ids need.** A deep section
  referencing ids `0..16` is 4 bits; an air/stone section is 1 bit.

Measured on four real generated columns: **22,640 / 23,328 / 23,728 / 26,752 bytes
(mean 24,112)** against the flat grid's 196,608 — an **8.2×** cut on the grid, and
**6.3×** on the whole retained column once the palette, biome grid and map entry
are counted. The paired `/usr/bin/time -l` arms are in
[`chunk-store.md`](./chunk-store.md).

### The API this changed

`ChunkColumn::raw_blocks() -> &[u16]` could not survive: there is no longer one
contiguous grid to borrow, and materialising one would reintroduce the 192 KiB the
change exists to remove. It is replaced by three accessors, and every caller was
already walking the flat grid section-by-section:

| new | replaces | callers |
|---|---|---|
| `append_section_cells(s, &mut Vec<u16>)` | slicing `raw_blocks()` per section | `chunk_nbt`'s region-file writer, `random_tick`'s definitional scan, `tests/random_tick_section_counters.rs` |
| `section_count()` | `height.div_ceil(16)` at each call site | as above |
| `blocks_heap_bytes()` | — (new) | `tests/chunk_memory.rs` |

`ChunkColumn`'s own `block_state`/`set_block`/`solid_count`/`is_solid` and every
`ChunkSource` signature are unchanged, so nothing outside this crate — including
`crates/protocol/v770`'s `encode_chunk`, which resolves cells through
`block_state` one string at a time — needed a patch.

## How to change it, and the gotchas

**The one invariant is that `get` after `set` returns what was written, for every
cell at every width.** The gates in `chunk_blocks.rs`'s own test module drive the
width transitions explicitly (`1 → 2 → 4 → 5 → 8 → 9 → 12 → 16`) and re-check the
*whole* section after each one, because a packing bug that corrupts a neighbour
rather than the written cell is the failure mode with no local symptom. Every
expected value comes from a `Flat` reference implementation in the test module —
the representation this replaced — never from `SectionedBlocks`'s own earlier
output, which `decode(encode(x)) == x` would satisfy under two symmetric bugs.

**The seeding branch is the one bug here with no loud symptom.** Promoting a
`Uniform(id)` where `id != 0` to `Packed` must fill the new buffer with `id`
before applying the write. Forgetting it leaves 4,095 cells reading as air — a
silent terrain deletion, not a panic. `promoting_a_non_zero_uniform_preserves_every_other_cell`
is that gate.

**A partial top section collapses to `Uniform(first)`, whose surplus cells read
back as `first` rather than the flat grid's implicit 0.** Sound because nothing can
reach them: `section_rows(s)` bounds every bulk read and `get` is only called with
a `y_local` inside `height`. If you add a reader that indexes a section directly,
bound it by `section_rows`.

**Fixtures that measure this are content-sensitive now, and one was already
falsified by the change.** `chunk_store`'s `touched_column` wrote one cell per 8
y-rows, which faulted every page of a flat 192 KiB allocation but packs to ~12 KiB
here — it would have reported a saving no real column gets, while still running and
still producing a plausible delta. Read its doc comment before writing a new
memory fixture; it is a worked example of CLAUDE.md's *world* species of vacuous
test.

**Why not a per-section palette, unlike the client's container.**
`lodestone_world::PalettedContainer` keeps a local palette per section, so a
section holding one high-id block among stone stays at 4 bits where this stays at
whatever that id needs. That is a real difference and it was judged not worth
having: the column palette is already deduplicated column-wide (tens of entries,
so ≤ 7 bits), the remaining gap is single-KiB per section, and a local palette
costs a remap table plus an index rewrite on every palette growth — the one
operation here that must not have a bug, because a wrong remap silently serves the
wrong block rather than failing.

**Why not reuse `PalettedContainer` outright.** It is a `u32` container that would
need a 4,096-entry `Vec<u32>` marshalling buffer at every construction and repeats
its 32-byte `PaletteKind` in all 24 sections, and it lives in a crate
`lodestone-server` deliberately keeps out of its normal dependency graph
(`lodestone-world` is a dev-dependency there; see `src/ecs/schedules.rs` for why
the browser bundle cares). The *design* is copied; the code is not shared.

**What is left on the table.** The block grid is no longer the largest per-column
term. In order, what remains is the 3-D biome grid (~3 KiB, a flat `Vec<u16>` over
`height / 4 * 16` cells — issue #512, and the same treatment applies), the palette
`String`s, and the `HashMap` entry. The other half of U8 is `Arc<ChunkSection>`
copy-on-write sharing between the store and the wire encoder, which needs
`ChunkSource::column`'s signature to change.

## Configuration

None. `CELLS` and `SECTION_ROWS` are the chunk format and the widths are derived;
no constant in this module is a tuning knob.

## Dependencies

`crate::chunk::SECTION_ROWS` and nothing else — not even `std` beyond `Vec`.
Deliberate: this is the hottest data structure in the server, and the crate's
dependency graph is load-bearing for the browser bundle.

## Gates

| gate | file | what it pins |
|---|---|---|
| `a_real_generated_column_is_cell_identical_to_the_flat_grid` | `tests/chunk_memory.rs` | every one of 98,304 cells matches `GeneratedColumn::into_raw()`'s flat grid — an expected value from another crate |
| `a_column_read_back_off_disk_is_cell_identical` | `tests/chunk_memory.rs` | the same, after a `column_to_nbt`/`column_from_nbt` round trip, so `chunk_nbt`'s newly section-ordered writer is covered |
| `the_packed_grid_costs_a_fraction_of_the_flat_one_on_a_real_column` | `tests/chunk_memory.rs` | a byte count, with a floor as well as a ceiling — a *zero* would mean the terrain vanished |
| `every_write_reads_back_across_all_width_transitions` | `chunk_blocks.rs` | each widening preserves every other cell, and the width tracks the largest id present |
| `a_scrambled_full_height_column_round_trips_cell_for_cell` | `chunk_blocks.rs` | 98,304 pseudo-random writes, no sampling, both construction paths |
| `promoting_a_non_zero_uniform_preserves_every_other_cell` | `chunk_blocks.rs` | the seeding branch (see above) |
| `from_flat_collapses_air_and_sizes_the_rest_to_its_ids` | `chunk_blocks.rs` | predicted uniform count *and* exact widths, so an over-wide packing fails too |
| `append_section_cells_reproduces_the_flat_slice_exactly` | `chunk_blocks.rs` | the order `chunk_nbt` and the region file depend on |
| `incremental_counters_match_an_independent_recount_through_a_mutation_storm` | `tests/random_tick_section_counters.rs` | a packing bug that dropped or shifted cells shows up as a random-tick counter disagreement, via a recount that reads the sections in its own separate loop |

Both `chunk_memory.rs` arms assert a **variety precondition** first — distinct
states, non-air cells, and *some* air — because a pure-air or single-state column
would compare equal under a completely broken packer. That is the failure mode you
cannot find by reading the assertions.
