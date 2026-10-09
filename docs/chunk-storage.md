# Server-side chunk column storage and wire encoding

## What it is

How a server-side `ChunkColumn` holds block-state data in memory and how those states reach a real client as the `level_chunk_with_light` body, including the three typed heightmaps, per-section fluid counters and resident-neighbour lighting.

## How it works

### In-memory storage

A column keeps one column-wide palette of canonical `StateId`s. Each 16-row section stores cells as a single repeated value (no allocation) or a bit-packed array sized to the widest id the section uses; it widens when a write needs more bits and never narrows (a column is built once and edited a handful of times). This is simpler than the client's per-section local palette, whose remap-and-rewrite on growth would risk silently serving wrong blocks; the shared threshold and packing rules are in [architecture](architecture.md) ("World storage and memory").

`SectionedBlocks` keeps the shared `CompactBlockStorage` section spine. Generated handoff consumes the storage and drops any dense carrier; payloads already shared with another immutable product stay shared. `ChunkColumn::worldgen_block_read` freezes the payload with its typed palette (a small copy preserving index order) for Nether neighbour probes, and resident mutation copies on write so an in-flight generation keeps its captured state. Storage may have a different Y origin than the column's `min_y`; packed read handles keep the server origin and translate rows, and partial top sections expose only real rows.

`PalettedContainer::palette_values` borrows the single value or indirect palette without scanning or allocating. An indirect palette may retain entries after the last cell is overwritten, so it is a conservative superset of live values, not a census; direct storage returns `None` (unknown, not empty). The fluid mesher uses it to prove a centre section dry from typed states.

Loading reconstructs the representation from the region file's per-section palette and packed indices rather than replaying a block-set per cell (about 98,000 string comparisons per column otherwise).

Generated columns derive palette metadata, random-tick section counts and the three client heightmaps during the same analysis that finds section uniformity and maximum id, so there is no separate metadata scan and no unpacking of new storage. Nether and End raw-window adapters remap one section-sized scratch field at a time, apply ordered upper spills before packing, and initialise ticking counts from the final-cell histogram. Air is index zero; every incoming palette entry is interned before cells are read, preserving unused entries and encounter order. Nether takes the fused client maps; End installs its supplied generation maps at the handoff boundary. With no maps requested and no randomly ticking states in the palette, transient summaries are skipped and zero section counts retained (unused ticking entries keep the histogram path because palette history is preserved). Proven air sections return a uniform index without per-cell observation, absent padding with no spill skips scratch filling, and a spill with a nonzero index keeps ordinary analysis including the last partial section.

Full-map derivation and dirty-cell repair start below the conservative packed-storage air ceiling, computed once per derivation or write batch (no cached ceiling): a section edge above every nonzero palette index, assuming index zero is ordinary air (other air variants overestimate). Partial top sections clamp the scan, while heightmap values and encoding use the original `min_y` and full height.

The `gen-counters` feature exposes raw-window source reads, final-cell observations, constructed sections, heightmap queries and omitted upper-air rows through the shared worldgen snapshot, aggregated per constructor or map lane. The strict production benchmark prints them as `storage_work`; run counters apart from timing. `StoneFloorSource` (`chunk_stone_floor.rs`: stone below a fixed surface, air above, default biome, `set_block` panics) is the test fixture, kept apart so it cannot become a production terrain path.

### Wire encoding

The column resolves each distinct state string once into a validated `lodestone_data::block_states::StateId`, and the encoder writes the raw integer at the protocol boundary. This is once per palette entry (dozens per column), not per cell. An earlier encoder skipped resolution and collapsed every solid block to one stand-in and everything else (fluids too) to air. A fluid's bare name has no valid state by itself (every real fluid state has a property), so resolution needs a third tier beyond exact match and air: the same-name default state, matched against the jar-marked default, not the lowest id (they disagree for most multi-state blocks and coincide only for water and lava).

Per-section block and fluid counts derive from the same ids (a client trusts the wire and never recomputes). The encoder writes all three client heightmaps from those ids and takes light from the resident 3x3 neighbourhood when available; the one-column encoder is an isolated fallback.

Biome palette entries are holder indices in the ordered `minecraft:worldgen/biome` registry sent in Configuration. The server derives that order by decoding the exact captured registry fixture it sends (a sorted overworld subset would shift later ids, since the table includes other dimensions' entries).

### Heightmaps

World generation may retain a `MOTION_BLOCKING` snapshot, but packet encoding does not trust it: at serve time the encoder scans the current column and sends `WORLD_SURFACE`, `MOTION_BLOCKING` and `MOTION_BLOCKING_NO_LEAVES` (registry ids 1, 4, 5), each the first block from the top satisfying its predicate, relative to the dimension minimum. No matching block gives height zero, which is different from no heightmap (an absent one makes the client compute its own; a wrong one is trusted). Using the same resolved ids as the sections, fresh, generated, imported, loaded and edited columns all describe the bytes sent. The no-leaves predicate excludes leaf states explicitly, not by subtracting the ordinary answer. Fluid-bearing states (including waterlogged) are counted in the section prefix from the exact palette written after it, with level properties kept in the block-state palette.

## How to change it

- Compact packing and snapshot detachment live in `lodestone_worldgen::generated_storage::CompactBlockStorage`; keep the server adapter's relative-row translation and captured palette together. With `gen-counters`, `ResidentDenseImport`, `ResidentPacked` and `ResidentPayloadCopy` separate full-height imports, packed probes and copy-on-write traffic.
- Never resolve a block-state string per cell on a hot path; route through the pre-resolved `StateId` palette and convert to the raw integer only for a packet (a string-hash memo still costs measurably).
- Check a block's jar-marked default state before extending the same-name fallback to another property-requiring block.
- A new heightmap kind takes its registry id and predicate from its own definition, not a neighbour's or an ordinal (the no-leaves kind is not the general one plus a filter; a plausible wrong predicate is worse than no map).
- Promoting a single-value section to packed must fill the new array with the original value before applying the triggering write, or every other cell silently becomes the wrong block.
- Biome holder ids come from the registry packet order, never a sorted or dimension-specific list.

## Configuration

None; widths, thresholds and predicates derive from the chunk format and game data.

## Dependencies

The shared world-storage crate (paletted container, sections, packing), `lodestone-data` (string-to-id resolution), the worldgen crate (the real `MOTION_BLOCKING`, carried across a seam and only serialised here), and [architecture](architecture.md) for container thresholds, index order and the long-array framing rule.
