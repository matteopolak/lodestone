# Worldgen Stage Schedule

## What it is

`lodestone_worldgen::stage_schedule` names the ordered passes that turn a
dimension's density field into a packet-ready chunk. The table is shared by
the three generators and is intended to be the vocabulary used by parity
replay and production dispatch code.

## How it works

There are four intentionally separate typed schedules:

* `StageSchedule` describes one column's pass order. The Overworld and Nether
  resolve structure starts, references, and terrain influence before fill;
  structure placement then happens after carving. The End has no terrain
  influence stage and resolves starts while placing structures after
  materialisation. Biomes, surface rules, decoration, and the final output are
  named separately. The Overworld has the final top-layer pass, while the
  Nether and End do not. Its `shaped_boundary` is explicit because `Shaped`
  does not mean the same final pass in every dimension: Overworld and End
  include structure placement, while Nether resumes with placement at source
  completion.
* `LifecycleSchedule` describes the common externally observable status graph:
  empty → structure starts → structure references → biomes → noise → surface
  → carvers → features → initialize light → light → spawn → full → packet
  finalization. Every `PhaseContract` gives its prerequisite phase and radius,
  plus its block-write radius. A dimension with no actual carver still advances
  through that no-op lifecycle status; this is intentionally distinct from its
  internal `StageSchedule`. Packet finalization is a Lodestone serving boundary
  that depends on both a full centre and the radius-one settled-light footprint.
* `SourceSchedule` describes the candidate source window for a mutable
  three-by-three feature dispatcher. Nether completion order is derived from a
  `ChunkRequest`: its admitted rectangle is tiled from the request's halo
  minimum, then ordered by tile-z, tile-x, local-z, and local-x. The resulting
  `AdmissionWavefront` sorts each target's source window; it is not a global
  centre-first/centre-last permutation. This is not a column-stage order: it
  exists because one source can write into a neighbouring resident chunk.
* `FeatureSchedule` describes the configured decoration ordinals inside one
  FEATURES pass. `DecorationStep` gives those ordinals names while preserving
  the numeric value needed by the seed derivation. External numeric values are
  decoded once at the data boundary.

`StageCursor` is a small debug guard for pipelines whose prefix and suffix live
in different helper methods: a cursor can resume at a cached-prefix boundary
and asserts the next named stage in debug builds.

The schedules describe pass order and the named request-order vocabulary. They
do not perform generation or replace dimension-specific dependency logic.
The Nether production dispatcher and parity replay both consume
`ChunkRequest::admission_order`, so a schedule change cannot silently leave
one comparator with a copied tile loop.

The tables are based on the external runtime's generation pyramid and task
dispatch, read as two separate sources: the pyramid defines statuses,
dependency radii, and write radii; the task dispatch defines what work each
status performs. Internal names deliberately describe behavior instead of
copying external implementation identifiers. This distinction matters when a
single external status contains several Lodestone helper passes, or when a
status is a no-op for one dimension.

## How to change it

Add a `ColumnStage` variant only when a pass has an observable worldgen
boundary. Add
it to each affected dimension's static schedule in the exact order consumed by
that generator, update its `shaped_boundary`, then extend the schedule tests.
When wiring a generator whose
pipeline is split across cached helpers, use `cursor_at` at the prefix boundary
through `shaped_boundary_index`; do not repeat a stage name at each resumption
site. Call `enter` or `run` at each helper boundary. Keep derived values such as
heightmaps inside their consuming pass rather than promoting them to stages. Add
a `DecorationStep` variant only when the external ordinal has a stable named
meaning; do not use a column stage to represent a source admission or a feature
ordinal.

Change `LifecycleSchedule` only after checking both the external pyramid and
its task dispatcher. A new phase needs a typed `LifecyclePhase`, one
`PhaseContract`, and independently derived dependency/write radii. Never infer
those radii from Lodestone's current neighborhood loops: that would make the
contract repeat the implementation it is meant to check. Keep strings out of
all of these tables; diagnostic text is derived from enum variants only at the
display boundary.

Do not use this table to paper over an unknown parity mismatch. First capture
the external order, then add a focused production and replay assertion that
proves both paths consume the same request/admission/completion model. A
request whose source window crosses a tile boundary is a useful control: it
must produce a different local source permutation than a request wholly inside
one tile.

## Configuration

The schedules have no runtime configuration. They are compile-time constants;
debug assertions are active in debug builds and have no effect on generated
block data. `LifecycleSchedule::validate` also checks that dependencies point
strictly backward and that packet finalization remains terminal.

## Dependencies

The module depends only on the Rust standard library and is re-exported as
part of `lodestone-worldgen`. Dimension generators, the server scheduler, and
parity tools may depend on it without introducing a dependency cycle.
