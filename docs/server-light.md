# Server-side light

## What it is

How the integrated server computes the sky and block light it puts on the wire for a served chunk, and keeps that light current after an edit.

## How it works

### Engine and census

Sky and block light share one flood fill losing one level per step; sky seeds every cell open to the sky at full level, block light seeds every emitter. Per-state dampening and emission come from an injected lookup built from a per-state census, so the engine has no registry dependency. A column can be computed alone (exact except near its edge) or against its 3x3 neighbourhood (radius 15 never reaches past adjacent chunks).

- Sky seeding enqueues only boundary cells next to unlit transmitting cells (`lodestone_world::lighting::compute_sky`); when changing it compare against a full-source flood over mixed roofs and partial opacity.
- Live relighting reads canonical state IDs from resident columns through a read-only block view. The Overworld initial pass borrows the centre and eight neighbours (`InitialLightVolume`, `compute_overworld_initial_lights_borrowed`), clipped to the served window; compare every layer against the buffered control in `initial_light_view_tests`.
- Unseeded scans skip block lookups above a column's air ceiling, derived from packed section indices (not the heightmap: air variants exist above it); a `BlockVolume` override must be a conservative upper bound. When every tile is plain air the shared pass stops 15+ cells above the highest ceiling and emits full sky; retained sky or an unusual ceiling keeps the full-height path.

An absent wire light section means the dimension default (full daylight in the Overworld), not darkness. The sky payload keeps a bounded run of full-sky sections above the highest non-air section; update packets always keep explicit zero block-light sections so they clear client values.

### Dimension sky rules

Sky seeding is a dimension property, not a column shape (Nether and End share 0..256, only the Nether has no skylight).

| Dimension | Initial chunk form |
|---|---|
| Overworld | one-section sky form, uniform zero block light elided |
| Nether | no sky sections; zero block-light only in sections the centre plus loaded 3x3 allocates; block light from centre and cardinal neighbours, diagonals wait for a later update |
| End | sky from the settled `ColumnLight` snapshot; a generated source without a snapshot keeps computed sky and drops sections the 3x3 would not allocate |

The allocation mask comes from block occupancy in the admitted columns, never coordinates or names. `ChunkSource::dimension` carries the dimension to protocol hooks (unlabelled defaults to Overworld); any dimension wrapper must forward its label.

### Retained snapshots

A protocol returning `true` from `ServerProtocol::retains_initial_column_light` (default `false`) settles the whole 3x3 before encoding and consumes the centre's exact `ColumnLight`.

- `compute_initial_column_lights_with_neighbours_in_dimension` returns a `ColumnLightSettlement`: a mandatory centre plus optional relative-coordinate dependency entries; `ChunkStore` captures them, checks revisions and forwards them as one batch.
- `ChunkColumn` has a lifecycle stage: `DependencyInitialized` (a seed made while settling another centre) or `CentreSettled` (own admission finished). The fast path checks the stage, not light values; a dependency never downgrades `CentreSettled`; any block mutation clears stage and light.
- Both skylit dimensions recompute a fresh centre's sky from terrain, never from retained sky. The End restores retained dependencies verbatim afterwards; the Nether seeds from retained block light and treats a not-yet-admitted neighbour as an opaque seam. A reload serves the persisted `CentreSettled` snapshot verbatim (imported End snapshots with omitted zero arrays get explicit `Uniform(0)`).
- `column_to_nbt`/`column_from_nbt` store sky and block arrays including the two boundary sections; `LodestoneLightSections` records the window when it differs from terrain count plus two. The native record path stores only canonical `CentreSettled` light and recomputes a dependency-only column; the NBT decoder rejects a lifecycle marker with no arrays. `ColumnLight::new` takes a block-section count (subtract the two boundary sections when cloning an empty shape).
- A column whose storage height differs from the protocol's wire height (a plugin dimension under standard framing) cannot retain light. A retaining source must serve a resident backing column before generating, or a resident 3x3 becomes eight opaque seams. The bounded cache must sit above a persistence-capable source (`ChunkStore::store_resident_columns` forwards the whole batch).
- `ChunkStore` uses a per-coordinate revision check from snapshot to commit with the global cache mutex and coordinate gates released during compute; a later write to the centre or a dependency invalidates the result, and a bounded retry ends in an exclusive footprint transaction. Only a write that lands bumps a revision.

The End reload behaviour is pinned by an external sealed-world capture.

### Keeping light current after an edit

An edit triggers recompute-and-resend only if old and new blocks differ in **resolved** emission or dampening: decorative changes must not resend, while equal-opacity blocks of different dampening must (sky crossing depends on it).

- Block updates are always sent; only light-changing ones enqueue destinations on the per-connection FIFO relight queue, deduplicated across batches.
- Tick-driven changes use the connection's delivered-column ledger (a pending join column needs nothing; a delivered neighbour still gets cross-boundary light) and need a fully resident footprint (`ChunkSource::resident_column`), sending nothing otherwise so a timer never generates a cold 3x3.
- Direct player edits send the block change immediately; nearby destinations share a solve on the bounded worker dispatcher, sent only if the snapshot is still current and the destination still delivered. See [Resident relighting](resident-relighting.md).
- Producers that know both states decide at publish time; an unknown prior state relights conservatively; scheduled fluid writes capture the replaced state per write. Tick-feed block updates are sent even while the join stream is in flight.

### Cross-chunk propagation

A family opts in via `ServerProtocol::uses_cross_column_light` and `compute_column_light_with_neighbours`. The server gathers the edited column and its eight neighbours, recomputes all nine and replaces retained snapshots only once the whole result exists (`ChunkColumn::set_block` invalidates immediately). The update goes to the acting connection only. Initial encoding assembles the 3x3 in fixed relative order, and the result must not depend on direction, coordinate or holder ordinal; a partial neighbour list counts as deferred fresh emission while still deriving masks.

Sky light matches an independent reference server's on laterally varied terrain; a small block-light residual is a known census gap for a couple of block types.

## How to change it

- Block light behaviour: edit the census entry from the real per-version source, never by analogy.
- Cross-column light for a family: implement both hooks, keep the relative order, use the ordinary unloaded result for absent columns, add a solver control and a client-observed edit test. The fixture must supply all eight neighbours: with an opaque roof and open east column, east-only and full-3x3 both give sky 14 at the east border while seven neighbours without east give 7, catching assembly that works with only one entry.
- Retained initial light: opt in only when the encoder consumes the exact snapshot; exercise source, cache, save and reload, and a write landing between capture and commit (including a dependency).
- A dimension or sky rule goes through the `*_in_dimension` hooks with decode tests of an initial chunk and a light update. Never infer skylight from `min_y`, height or section count.
- Off-task chunk encoders carry the 3x3; initial and live paths must agree at borders.
- Do not widen the relight predicate to every placement; keep timer relights resident-only; re-check the retained centre before sending a detached result; keep the decision at the producer (LAN relays forward unchanged). Never compute light on the client's live multiplayer path.

## Configuration

No runtime setting. Capability boundaries: `ServerProtocol::retains_initial_column_light`, `uses_cross_column_light`, optional `detached_light_compute` (pure function for native tick and direct-edit relights); `detached_resident_light_compute` and `compute_resident_light_batch` share grouped computation between native and browser (26.2 implements them).

Timing sink phases: `resident-light-settlement`, `resident-light-compute`, `resident-light-encode`, `connection-relight` (occupied polls only), nested inside `connection-poll`; initial lighting is `packet-lighting`. For the borrowed vs buffered initial-light comparison run `borrowed_overworld_initial_light_matches_buffered_roof_seam_and_air` in release with `--nocapture` and `LODESTONE_LIGHT_VIEW_PERF_ITERATIONS` in `1..=128` (prints `LIGHT_VIEW_PERF` sums, no assertion); the `light_shared_initial_3x3` benchmark has a full-scan control ([oracles and benchmarks](oracles-and-benchmarks.md)).

## Dependencies

The world-storage crate's lighting engine and wire format; the generated per-state light census; [chunk storage](chunk-storage.md) and [chunk lifecycle](chunk-lifecycle.md); the native bounded worker dispatcher; an independent reference server for oracle comparison only.
