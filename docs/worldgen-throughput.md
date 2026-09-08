# World-generation throughput

## What it is

`crates/lodestone-worldgen/examples/throughput.rs` measures the three bundled dimensions at the world-generation boundary. It compares each dimension's shaped terrain prefix with its fully decorated column over a deterministic grid, without persistence, lighting, or packet encoding.

## How it works

The executable builds one production generator per dimension and seed, walks a row-major `N × N` chunk grid, and reports cold and warm passes. Cold starts with a fresh generator; warm repeats the same grid after the first pass so memoized stage work is visible. Overworld and Nether use `column_shaped`; End uses its pre-surface `shape_field`; all decorated paths call the public `column` method.

Each pass reports chunks per second, process CPU utilization when the host exposes `getrusage`, and sampled resident-set growth. A separate 16-chunk allocation pass uses a benchmark-local counting allocator, so allocation counts do not distort the wall-clock throughput numbers. A rolling digest and non-air count keep generation work observable.

Run a 256-chunk sweep in release mode:

```text
cargo run --release -p lodestone-worldgen --example throughput -- 42 16 all
```

Use `32` for 1,024 chunks, or select one dimension with `overworld`, `nether`, or `end` as the third argument. The optional fourth argument selects `shaped` or `decorated`; this is useful for a focused sampling profile. Profile the selected release binary with `samply record` to identify hot functions.

## How to change it

Keep the coordinate order, seed, and grid size in the command when comparing runs. Add a new dimension by extending `DimensionName`, `Generator::new`, and `Generator::generate`. If a new generator has no shaped API, expose a base-terrain seam that does not run decoration rather than approximating shaped work by changing resolver data. Keep allocation counting separate from timed passes because the allocator hook changes the hot path.

The executable is intentionally not a server benchmark: do not add region-file I/O, light propagation, chunk packet encoding, or network scheduling to it. Those costs belong to their own measurements and would make the world-generation number ambiguous.

## Configuration

The positional arguments are `seed`, `grid-side`, `all|overworld|nether|end`, and `all|shaped|decorated`; defaults are `42`, `16`, `all`, and `all`. Release mode is required for representative throughput. The sample allocation count is capped at 16 chunks per mode so a full-grid run remains practical.

## Dependencies

The example uses the production constructors in `lodestone-server::worldgen_data`, the three generators in `lodestone-worldgen`, `memory-stats` for resident-set sampling, and `libc`'s process resource counter on Unix. It does not depend on persistence, lighting, packet, or transport modules.
