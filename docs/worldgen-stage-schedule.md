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
* `SourceSchedule` describes the order in which neighbouring source chunks are
  admitted and completed by a mutable three-by-three feature dispatcher. This
  is not a column-stage order: it exists because one source can write into a
  neighbouring resident chunk. Fixed dimensions carry their offsets inside a
  `SourceCompletion::Fixed` value; admission-dependent dimensions carry no
  fixed permutation at all.
* `FeatureSchedule` describes the configured decoration ordinals inside one
  FEATURES pass. `DecorationStep` gives those ordinals names while preserving
  the numeric value needed by the seed derivation. External numeric values are
  decoded once at the data boundary.

`StageCursor` is wired into the canonical Overworld, Nether, and End entry
paths. It is a small debug guard for pipelines whose prefix and suffix live in
different helper methods: the immutable prefix calls `finish_prefix`, and the
packet-producing suffix resumes with `cursor_at` and asserts each named stage
in debug builds.

The schedule describes pass order only. It does not perform generation, choose
worker admission order, or replace the dimension-specific dependency logic.
Those operations remain in the corresponding generator so a schedule change
cannot silently alter terrain output.

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
proves both paths consume the same schedule.

## Configuration

The schedules have no runtime configuration. They are compile-time constants;
debug assertions are active in debug builds and have no effect on generated
block data.

## Dependencies

The module depends only on the Rust standard library and is re-exported as
part of `lodestone-worldgen`. Dimension generators and parity tools may depend
on it without introducing a dependency cycle.
