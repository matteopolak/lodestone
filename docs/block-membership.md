# Exact block membership

## What it is

`lodestone_data::block::BlockMask` is an exact membership representation for
hot, repeatedly queried sets in the built-in block registry. It replaces hash
tables only where measurement justifies paying for the full fixed-width mask.

## How it works

The built-in registry is a contiguous `u16` domain of 1,196 entries. A mask
stores 19 `u64` words, so `contains` is one indexed load and bit test with no
hashing or per-set allocation. The type is exact: it cannot admit false
positives and does not represent plugin blocks. Vegetation tag closures use the
mask after resolver names have been converted to `Block` values.

## How to change it

Use `BlockMask` for hot reused sets after measuring the aggregate footprint and
lookup cost. Tiny or cold sets should remain direct matches or use a compact
sorted representation; callers that need iteration or plugin identities need a
different representation. Add registry-domain tests when the generated block
count changes. Do not replace the exact test with a Bloom filter: a false
positive changes generation output.

## Configuration

The mask width follows `lodestone_data::block::BLOCK_MASK_WORDS`, which is
derived from the generated block registry. No runtime setting changes it.

## Dependencies

`BlockMask` depends only on the generated block registry in `lodestone-data`.
World generation consumes it through the vegetation configuration and retains
its existing resolver boundary for loading tag definitions.
