# The `lodestone-data` crate

## What it is

`lodestone-data` (`crates/lodestone-data`) owns the canonical 26.2 game-data
censuses that used to live under `crates/protocol/v770/src/generated/`, plus
the `oracle-java/` dump programs that produce them and the tests that
regenerate/drift-check them. Issue #361 extracted them: of the ~20 tables
`v770` generated, exactly one — `packet_ids.rs` — is wire format. The other
nineteen (`attribute_types`, `block_registry`, `block_solidity`,
`block_states`, `collision_shapes`, `data_component_types`, `entity_census`,
`entity_dimensions`, `entity_types`, `hardness`, `item_prototypes`, `items`,
`menus`, `mob_effects`, `outline_shapes`, `particle_types`, `path_types`,
`sound_events`, `tools`) describe **the game**, not **the protocol**, and now
live here instead.

Tables added since the extraction live here too, and `block_entity_types`
(issue #374) is the first: `state_id -> minecraft:block_entity_type` for all
32,366 states, 4,567 of which own one across 49 distinct types. It is what lets a
**block-state write** create or remove a block entity the way
`LevelChunk.setBlockState` does — see
[`block-entity-renderers.md`](block-entity-renderers.md). Its provenance is worth
noting as a case where *both* Mojang reports are insufficient rather than merely
stale: `blocks.json` is block properties only, and `registries.json` carries the
49 type ids but nothing about which blocks each type covers. Recovered from
`BlockEntityType.isValid(state)` (which *is*
`validBlocks.contains(state.getBlock())`) rather than by calling
`newBlockEntity`, so no live `BlockEntity` is constructed; the oracle throws
rather than emitting a row if a state's `hasBlockEntity()` and its claiming-type
count disagree, so the dump existing is evidence the invariant held everywhere.

The move is why #204 (`ChunkWorld` classifying every block solid/air instead
of reading the path-type census) is now a small wiring change rather than an
architectural dead end — see `docs/roadmap/server-entities.md`.

## How it works

- `src/generated/*.rs` — the raw tables: pure `&'static` rodata (arrays,
  slices, tuples), one file per census, `// @generated` and never hand-edited.
  Declared `pub(crate)` in `src/lib.rs` under `generated_*` names (e.g.
  `generated_hardness`), exactly as they were in `v770`.
- `src/*.rs` (one per table, e.g. `hardness.rs`, `collision_shapes.rs`,
  `path_types.rs`, `tool.rs`) — the hand-written lookup API over each raw
  table: bounds-checked accessors (`hardness(id) -> Option<Hardness>`), and
  for a few tables a version-free trait impl (`path_types::PathTypes` implements
  `lodestone_model::PathTypeRegistry`; `block_states::BlockStateTable`
  implements `lodestone_model::BlockStateRegistry`). Every public return type
  is a `lodestone_model` type (`BlockAabb`, `PathType`, `ItemPrototype`,
  `EntityBaseDimensions`, ...) — this crate never invents its own parallel
  types for something `lodestone-model` already names.
- `oracle-java/*.java` — the dump programs. Two provenance shapes, matching
  each table's own doc header:
  - **Registry-report tables** (`attribute_types`, `entity_types`,
    `block_states`, `sound_events`, `particle_types`, `menus`, `items`,
    `data_component_types`) are generated from Mojang's own
    `registries.json`/`blocks.json` reports (`.cache/mc/26.2/generated/reports/`),
    no custom oracle program needed. Two different mechanisms read that
    report, and they are not interchangeable: `attribute_types`, `entity_types`
    and `block_states` each have their own `tests/*.rs` drift-guard that
    parses the report directly (see below). `sound_events`, `particle_types`,
    `menus`, `items` and `data_component_types` have **no such test** — as
    documented under "How to change it" — so `cargo xtask gen-registries
    --check` (which `cargo xtask conformance` runs, pointed at this
    directory) is the *only* thing that regenerates or drift-checks them.
    Retiring that xtask step without first giving these five their own
    `tests/*.rs` guard would leave them with zero drift coverage, not
    redundant coverage.
  - **JVM-walked tables** (`block_entity_types`, `hardness`, `collision_shapes`, `block_solidity`,
    `entity_census`, `entity_dimensions`, `item_prototypes`, `outline_shapes`,
    `path_types`, `shade_brightness`, `snow_support`, `sound_types`, `tools`) need an
    `oracle-java/*Oracle.java` program that boots
    the real 26.2 server headlessly (`SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();`) and walks a registry or `Block.BLOCK_STATE_REGISTRY`
    directly, because the fact in question (collision geometry, hardness,
    `forceSolidOn`/`forceSolidOff`, ...) has no getter and is absent from
    `blocks.json`. One exception: `collision_shapes`' dumper, `ShapeOracle.java`,
    stays in `crates/lodestone-physics/oracle-java/` — see "What did not move"
    below.

    `snow_support` (issue #404's U2, world generation's `freeze_top_layer`) is the
    clearest case for why this list is JVM-walked rather than derived. Its four
    columns are `Block.isFaceFull(collisionShape, UP)`,
    `!getFluidState().isEmpty()`, `getFluidState().is(Fluids.WATER) && block
    instanceof LiquidBlock`, and `hasProperty(SNOWY)`. Measured on first run, the
    dump contradicted four separate hand guesses at once: only **6,359 of 32,366**
    states have a full UP collision face (against 3,287 whose collision shape is a
    single unit box — **159 blocks** are full-faced without being unit boxes, and a
    unit-box derivation would have refused snow on every one); the water predicate
    is true for **exactly one** state, `water[level=0]`, because flowing water is
    `Fluids.FLOWING_WATER` and a waterlogged block is not a `LiquidBlock`;
    **`snow[layers=8]` is not full-faced**, which is why
    `SnowLayerBlock.canSurvive` needs its explicit `layers == 8` clause; and
    `powder_snow` — not `chorus_plant` — is the one `dynamicShape()` block world
    generation exposes at a surface top. `tests/snow_support.rs` asserts each of
    those rather than restating them.
- `tests/*.rs` — one test per JVM-walked or registry-report table: hermetic
  consistency checks over the committed table, plus an `#[ignore]`d drift
  guard that regenerates the table from the committed dump
  (`tests/support/*_jvm.txt`, tracked in git) or oracle-java output
  (`oracle-java/*.txt`, gitignored) and asserts byte-for-byte equality against
  `src/generated/*.rs`. Regenerate with:

  ```text
  LODESTONE_REGEN=1 cargo test -p lodestone-data --test hardness \
      committed_table_matches_dump -- --ignored --nocapture
  ```

  (substitute the table name; each test file's own header has the exact
  invocation). `tools`/`particle_types`/`sound_events`/`menus`/`items`/
  `data_component_types`/`mob_effects` have no such test — they were never
  gated by a `tests/*.rs` drift guard in `v770` either, and that did not
  change here. `sound_events`, `particle_types`, `menus`, `items` and
  `data_component_types` are still drift-checked, just by a different
  mechanism: `cargo xtask gen-registries --check` (default `--out-dir` is
  this directory, `crates/lodestone-data/src/generated`, not a protocol
  family's `generated/` — these tables describe the game, not one
  protocol's wire format, so they live here once rather than once per
  family), which `cargo xtask conformance` also runs, unconditional of
  `--family`. `tools` and `mob_effects` have neither kind of drift guard
  today.
- `v770`'s `adapter.rs` (`V770Adapter`, implementing
  `lodestone_model::VersionAdapter`) delegates every data-shaped trait method
  (`block_hardness`, `block_collision`, `block_outline`, `block_interaction`,
  `item_prototype`, `block_blocks_motion`, `entity_dimensions`,
  `entity_facts`, `tool_mining`) straight to `lodestone_data::*` — one line
  each, no logic. That is the seam a `&dyn VersionAdapter` consumer
  (`lodestone-shell`, `lodestone-physics`) already used before this move and
  still uses unchanged.

### `block_states::state_id` — the one reverse map, and why it is derived rather than generated

`block_name`/`properties` answer id → (block, properties) in O(1). `state_id`
answers the other direction — a canonical state string such as
`"minecraft:oak_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]"`
→ its global 26.2 state id — and it is the only lookup here that is **not** a
direct index into a generated array.

It is not a second generated table either, deliberately. Its whole index is
derived from the tables already committed, once per process behind a `OnceLock`:

| structure | size | built from |
|---|---|---|
| `spans[block]` — `{first, last, default}` | 1,196 × 12 B | one walk of `STATES` plus `snow_support::is_default_state` |
| `by_name` — block indices sorted by name | 1,196 × 2 B | `BLOCK_NAMES`, sorted at build time |

That is ~17 KB and ~32k iterations, once. Generating an equivalent static table
through the `LODESTONE_REGEN=1` pattern the other dumps use would add a **drift
surface for data that is already in the tree** — the reverse map would then have to
be regenerated in lockstep with `block_states.rs` and `snow_support.rs`, and a
stale one fails in the worst possible way (plausible-looking wrong ids). Deriving
it means the only source of truth stays the jar dump.

Two things the build asserts, so a table that stops satisfying them fails loudly
at first use rather than resolving into a neighbouring block's states:

- **every block owns a *contiguous* id span** — true because vanilla builds
  `Block.BLOCK_STATE_REGISTRY` block by block, and `state_id` scans `first..=last`
  rather than all 32,366 rows *because* of it;
- **every block has at least one state.**

`by_name` is built rather than assumed sorted. `BLOCK_NAMES` happens to be
alphabetical today (it comes from a JSON object's keys), but nothing in the
generator promises that, and a silently unsorted array would make `binary_search`
return wrong answers instead of failing.

The resolver's three tiers — exact, default-plus-named-overrides, default alone —
and the reason the default is **not** the lowest id are documented on `state_id`
itself. That logic used to live in `lodestone-v770`'s `server_protocol.rs`; it
moved here so `lodestone-server`'s `ChunkColumn` could resolve its own block
palette without crossing the protocol-family feature seam, which is what took
98,304 string hashes per column out of the chunk encode path (see
[`chunk-column-encoding.md`](./chunk-column-encoding.md) for the measurement).

## How to change it, and the gotchas

- **Add a new census the same way the existing ones were added**: a dump
  program (or a registry-report parser) in the matching test file's generator
  function, a `src/generated/X.rs` raw table, and a `src/X.rs` lookup API. Wire
  it into `src/lib.rs`'s two `pub(crate) mod generated_X;` /
  `pub mod X;` declarations.
- **Never hand-duplicate `block_states::state_id`'s fallback.** Two test helpers
  copied an older version of it ("the lowest id sharing the block name") and became
  silent callers when the real one was corrected at `43a6e030`; one of them failed
  as a 30-second live timeout rather than a mismatch. If you need the id for a
  state string, call the function. `lodestone-server`'s `mobs::block_ids` used to
  keep a third hand-rolled inverse (a 32,366-entry `HashMap<String, u32>` built
  once from a per-state formatted string, for the pathfinder's terrain adapter);
  it is gone, and `block_state_id`/`block_state_id_or_default` are now both
  one-line calls into `state_id` itself.
- **`state_id_resolves_redstone_dust_by_power_not_to_the_lowest_id`** (this
  crate's own `block_states` test module) is the regression gate for the tiered
  fallback: `minecraft:redstone_wire[power=7]` must resolve to the state whose
  `power` is `7`, not to id 4011 (this block's lowest id, `power=0`) — the answer
  every caller got before `state_id` existed, for *every* power value, because a
  server-emitted dust string never names the other four connection properties and
  so never exact-matched anything. Any change to the tiers should keep this
  discriminating: an input where "lowest id" and "default plus overrides" agree
  would pass either implementation and prove nothing.
- **`block_registry` has no lookup-API wrapper file** — same as before the
  move. It is reached directly as `crate::generated_block_registry::*` from
  `block_states.rs` and `tool.rs`, the only two consumers.
- **This crate depends on nothing but `lodestone-model`.** If a new table
  needs a type `lodestone-model` does not have, add the type there first
  (version-free) rather than reaching for `lodestone-core` or
  `lodestone-world` here — pulling those in would widen this crate's
  footprint for every consumer, including `lodestone-server`, which is the
  whole point of the split.
- **A version crate other than `v770` needing one of these tables is not the
  same question as this crate needing to be version-generic.** Per #343, 26.2
  is the one canonical internal version; these are that version's data, full
  stop. `v47`/`v340`/`v735` keep their *own*, separate `entity_types.rs`
  tables in their own `generated/` folders, because for them that table is
  genuinely translation data (old wire id → canonical name), not a second copy
  of the canonical census — same principle as `v340`'s pre-Flattening
  `id:meta` table (`docs/protocol-340-flattening-table.md`). Do not try to
  make this crate parametric over protocol version; that is a different,
  unrequested project.
- **`VersionAdapter::block_hardness`/`entity_facts`/etc. were deliberately
  left as trait methods on `lodestone_model::VersionAdapter`, not moved or
  removed.** They are a real design smell — a *data* question sitting behind
  a *protocol* interface — but removing them touches the trait itself, its
  default impls, and every implementor, which is a design change wider than a
  house move. If you're tempted to have `lodestone-server` skip the trait and
  call `lodestone_data::hardness::hardness(...)` directly for a fact it needs
  and no adapter exposes yet (`path_types` is exactly this case, driving
  #204), that is fine and exactly what the extraction was for — but leave
  the existing `&dyn VersionAdapter` consumers (`lodestone-shell`,
  `lodestone-physics`) alone unless you're doing that redesign on purpose.
- **What did not move**: `ShapeOracle.java`
  (`crates/lodestone-physics/oracle-java/ShapeOracle.java`) produces
  `shape_java.txt`, which both `lodestone-data`'s `collision_shapes` drift
  guard *and* `lodestone-physics`'s own, independent
  `tests/collision_shapes.rs` (a hand-curated regression fixture, unrelated to
  this move) cite as their data source. Moving it into `lodestone-data` would
  have orphaned the physics test's documented provenance, so it stays put;
  `lodestone-data/tests/collision_shapes.rs` reaches it with one extra `../`
  hop instead.
- **`entity_variants.rs` (still in `lodestone-v770`) looks like a sibling
  table and is not one.** It resolves mob cosmetic-variant `Holder`s keyed by
  **entity-metadata serializer id** — a wire-decode detail, not a registry
  census — so it stayed in the protocol crate on purpose.

## Configuration

- `LODESTONE_REGEN=1` — see above; makes a drift-guard test overwrite
  `src/generated/*.rs` instead of asserting equality.
- No other env vars or feature flags; this crate has no Cargo features.

## Dependencies

- `lodestone-model` (only). Dev-dependency: `serde_json`, for the
  registry-report-driven drift guards.
- Depended on by `lodestone-v770` (`adapter.rs`'s `VersionAdapter` impl).
  Nothing else depends on it yet; `lodestone-server` gaining a dependency here
  is exactly what makes #204 a small change instead of a boundary violation —
  see `docs/roadmap/server-entities.md`.
