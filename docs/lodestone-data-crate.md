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
    no custom oracle program needed — the drift-guard test parses the report
    directly.
  - **JVM-walked tables** (`hardness`, `collision_shapes`, `block_solidity`,
    `entity_census`, `entity_dimensions`, `item_prototypes`, `outline_shapes`,
    `path_types`, `tools`) need an `oracle-java/*Oracle.java` program that boots
    the real 26.2 server headlessly (`SharedConstants.tryDetectVersion();
    Bootstrap.bootStrap();`) and walks a registry or `Block.BLOCK_STATE_REGISTRY`
    directly, because the fact in question (collision geometry, hardness,
    `forceSolidOn`/`forceSolidOff`, ...) has no getter and is absent from
    `blocks.json`. One exception: `collision_shapes`' dumper, `ShapeOracle.java`,
    stays in `crates/lodestone-physics/oracle-java/` — see "What did not move"
    below.
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
  invocation). `tools`/`particle_types`/`sound_events`/`menus`/`mob_effects`
  have no such test — they were never gated by a drift guard in `v770` either,
  and that did not change here.
- `v770`'s `adapter.rs` (`V770Adapter`, implementing
  `lodestone_model::VersionAdapter`) delegates every data-shaped trait method
  (`block_hardness`, `block_collision`, `block_outline`, `block_interaction`,
  `item_prototype`, `block_blocks_motion`, `entity_dimensions`,
  `entity_facts`, `tool_mining`) straight to `lodestone_data::*` — one line
  each, no logic. That is the seam a `&dyn VersionAdapter` consumer
  (`lodestone-shell`, `lodestone-physics`) already used before this move and
  still uses unchanged.

## How to change it, and the gotchas

- **Add a new census the same way the existing ones were added**: a dump
  program (or a registry-report parser) in the matching test file's generator
  function, a `src/generated/X.rs` raw table, and a `src/X.rs` lookup API. Wire
  it into `src/lib.rs`'s two `pub(crate) mod generated_X;` /
  `pub mod X;` declarations.
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
