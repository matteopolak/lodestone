# Block survival capabilities

## What it is

`lodestone-data::block_survival` supplies the exact state facts used to decide whether a simple world-generation block can remain placed. The server converts its fixed 26.2 state-id bitsets into the version-free generator's canonical-state predicates.

## How it works

The generated table has one bit each for `solid_render`, full upward support, center downward support, and fire flammability across all 32,366 global block states. `lodestone_server::worldgen_data::survival_facts` walks those rows and emits the default answer for each base block plus every state whose answer differs; `StatePredicate` checks the exact state first, then its base default. This keeps waterlogged and connection-sensitive variants correct while avoiding any reliance on a generator-local interner id.

## How to change it

If a simple-block survival branch needs another compiled-state predicate, add it to `BlockSurvivalOracle.java`, the compact dump, generated bitset, `block_survival` API, and the `survival_facts` column list together. Keep the full-state drift test and add a control that differs from the nearest plausible approximation. Do not index a global capability bitset with `lodestone_worldgen::interner::StateId`; those handles are intentionally local and encounter-ordered.

## Configuration

`Resolver::block_survival_facts` is optional. A resolver that supplies no document returns `null`, which parses to empty predicates for fixtures. The bundled 26.2 resolver supplies the generated census through `TableResolver::with_block_survival_facts`.

## Dependencies

The source is the headless compiled 26.2 server registry, extracted by `crates/lodestone-data/oracle-java/BlockSurvivalOracle.java`. `lodestone-data` owns the compact global-id table; `lodestone-server` is the version seam that builds the canonical-state fact document; `lodestone-worldgen` consumes that document without depending on version data.
