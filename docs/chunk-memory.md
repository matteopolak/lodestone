# Loaded chunk memory

## What it is

The integrated server retains generated chunk columns in a bounded cache. This
document records the representation choices that keep those resident columns
small without changing block reads, mutations, or packet bytes.

## How it works

Each column stores palette indices in 16-row sections. Uniform sections carry a
single index, while varied sections use the narrowest `u64` packing width. The
column also retains its textual palette for persistence and a few derived
per-palette tables for ticking, registry ids, and redstone dispatch.

Built-in block-state text used by `block_state_arc` is shared through one lazy
process-wide canonical table keyed by `StateId`. The column-owned palette stays
available for persistence and extension states; only unknown plugin/data-pack
state text gets a second `Arc<str>` allocation. This avoids duplicating the
same state bytes in every retained column while preserving an allocation-free
hot lookup.

## How to change it

Keep the section index order and palette index meaning stable: the chunk encoder,
region writer, ticking counters, and worldgen parity all consume those values.
When changing a field, update the constructors (`new`, `from_generated`, and
dimension adapters), `recalc_ticking_counts`, and `intern` together. Extend the
two `chunk_memory` sharing tests before changing custom-state handling.

The representation's direct storage cost is measured by
`ChunkColumn::blocks_heap_bytes`; RSS measurements in `chunk_store` remain the
authoritative check for allocator overhead and cache residency. Do not infer a
process RSS saving from a capacity count alone: run the retained and dropped
arms under the same release binary and compare their deltas.

Generation is a separate lifetime from retention. The worldgen result still
arrives as a dense `u16` block grid, so an Overworld column temporarily owns
`16 * 16 * 384 * 2 = 196608` block bytes while `ChunkColumn::from_generated`
packs its sections. Those bytes are transient and are released before the
column enters the resident cache; they must not be added to the retained
column total. The same distinction applies to palettes, decoration products,
and feature scratch buffers: an allocation count or peak during generation is
not evidence that the allocation remains in a loaded chunk. The ignored
`chunk_memory_census` test reports retained capacity per dimension, while
generation profilers should measure the producer separately.

## Configuration

There is no runtime switch. The canonical state table is initialized only when
the first non-air built-in `Arc<str>` lookup is requested; air uses the existing
singleton directly. Cache capacity is controlled
by `ChunkStore`'s view-radius policy, independently of the representation.

## Dependencies

The representation uses `lodestone-data`'s generated `StateId` table, the
server's `ChunkColumn` and `SectionedBlocks`, and the existing `ChunkStore`
retention policy. No protocol family or wire-format dependency is introduced.
