# Worldgen Stage Schedule

## What it is

`lodestone_worldgen::stage_schedule` names the ordered passes that turn a
dimension's density field into a packet-ready chunk. The table is shared by
the three generators and is intended to be the vocabulary used by parity
replay and production dispatch code.

## How it works

There are three intentionally separate typed schedules:

* `StageSchedule` describes one column's pass order. The Overworld and Nether
  resolve structure starts, references, and terrain influence before fill;
  structure placement then happens after carving. The End has no terrain
  influence stage and resolves starts while placing structures after
  materialisation. Biomes, surface rules, decoration, and the final output are
  named separately. The Overworld has the final top-layer pass, while the
  Nether and End do not.
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

The schedule describes pass order and the named request-order vocabulary. It
does not perform generation or replace dimension-specific dependency logic.
The Nether production dispatcher and parity replay both consume
`ChunkRequest::admission_order`, so a schedule change cannot silently leave
one comparator with a copied tile loop.

## How to change it

Add a `ColumnStage` variant only when a pass has an observable worldgen
boundary. Add
it to each affected dimension's static schedule in the exact order consumed by
that generator, then extend the schedule tests. When wiring a generator whose
pipeline is split across cached helpers, use `cursor_at` at the prefix boundary
and call `enter` or `run` at each helper boundary. Keep derived values such as
heightmaps inside their consuming pass rather than promoting them to stages. Add
a `DecorationStep` variant only when the external ordinal has a stable named
meaning; do not use a column stage to represent a source admission or a feature
ordinal.

Do not use this table to paper over an unknown parity mismatch. First capture
the external order, then add a focused production and replay assertion that
proves both paths consume the same request/admission/completion model. A
request whose source window crosses a tile boundary is a useful control: it
must produce a different local source permutation than a request wholly inside
one tile.

## Configuration

The schedules have no runtime configuration. They are compile-time constants;
debug assertions are active in debug builds and have no effect on generated
block data.

## Dependencies

The module depends only on the Rust standard library and is re-exported as
part of `lodestone-worldgen`. Dimension generators and parity tools may depend
on it without introducing a dependency cycle.
