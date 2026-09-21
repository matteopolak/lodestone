# Compiled worldgen configuration cache

## What it is

The bundled Overworld source factory retains a bounded cache of immutable,
compiled generator configurations. Repeated production source/lease creation
for the same seed and world configuration reuses parsed templates, pools,
structures and density programs while each source receives fresh mutable
column state.

## How it works

`OverworldGenerator::compile` builds an opaque
`CompiledOverworldGenerator`. `OverworldGenerator::from_compiled` attaches a
new staged-column store and preliminary-surface cache to that configuration.
`worldgen_data::overworld_chunk_source_of_type` keys a four-entry LRU by seed,
world type, settings fingerprint, resolver fingerprint and production executor
version. Compilation is serialized while a missing key is filled, so two
workers cannot duplicate the same expensive build. Cache eviction only drops
immutable configuration handles; live sources retain their own configuration
until they finish.

## How to change it

Change `OverworldGenerator::compile`/`from_compiled` when a new immutable
generator input or runtime cache is introduced. Keep request-local stores out
of `CompiledOverworldGenerator`, and bump the executor-version component of the
server cache key when the production request contract changes. Use
`bundled_generator_cache_stats` to verify hit/miss/compilation behavior in a
production benchmark; the counters do not affect generation.

## Configuration

`BUNDLED_GENERATOR_CACHE_CAPACITY` bounds retained compiled configurations at
four entries. `BUNDLED_GENERATOR_EXECUTOR_VERSION` identifies the immutable
production request contract. The cache uses the embedded resolver's complete
asset fingerprint, so a changed bundle cannot reuse an older configuration.

## Dependencies

The cache spans `lodestone-server::worldgen_data`,
`lodestone-server::chunk::OverworldChunkSource` and
`lodestone-worldgen::overworld::OverworldGenerator`. It relies on
`TableResolver`'s embedded asset fingerprint and on the generator's existing
bounded staged store; it does not alter lifecycle/session ownership or the
worldgen density, aquifer or surface algorithms.
