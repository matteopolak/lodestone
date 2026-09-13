# Worldgen Schedule Guard

## What it is

`cargo xtask check-worldgen-schedule` is a source-level architecture guard for
the production world-generation entrypoints. It keeps orchestration tied to
the central typed schedule for each dimension and requires option gates to be
declared in `lodestone_worldgen::stage_schedule` metadata.

## How it works

The guard parses every production Rust file under `crates/lodestone-worldgen/src`
with `syn`. Typed pass sequencing is allowed only in the three dimension
executors, the shared Overworld fill executor, and the schedule module itself;
a new `cursor`, `enter`, `run`, or completion call in another module is a
violation. Functions whose names identify stage implementations may still do
their pass-local work, but they do not get an exception for owning a second
typed sequence. The audit also watches the small set of public/batch source
entrypoints for nested `dx`/`dz` or `x`/`z` offset/radius loops, which would
duplicate the central source-neighbourhood schedule. Test modules are skipped,
including compound `cfg` expressions, and ordinary geometry loops are not
classified as source loops.

The same run checks that `StageOption`, `StageGate`, the three per-dimension
gate tables, and `StageSchedule::with_gates` remain present. The audit fails if
a required source file moves or if its parser cannot read the production
syntax, so an empty scan cannot pass silently.

## How to change it

Add a new source entrypoint name to `SOURCE_ENTRYPOINTS` only when it is a
genuine public or batch neighbourhood owner, and add a stage-like operation to
`STAGE_LIKE_METHODS` when a new helper can sequence a generation pass. If a
new executor is intentional, add its exact path to `APPROVED_STAGE_FILES` and
give it a central dimension schedule; do not broaden the list to silence a
finding from a pass implementation. If a stage can be a no-op because its
data is absent, declare its typed `StageGate` beside the dimension's schedule
and extend the schedule validation tests.

## Configuration

The guard has no runtime flags. It scans all production Rust under
`crates/lodestone-worldgen/src`, requires the five fixed schedule/executor
anchors to exist, and runs as `cargo xtask check-worldgen-schedule` or
`just check-worldgen-schedule`. It also checks the shared 256-chunk Distant
Horizon setting in the shell configuration.

## Dependencies

The scanner uses `syn`, `proc-macro2`, and the standard filesystem API. The
typed metadata lives in `lodestone-worldgen::stage_schedule`; no runtime
generation code depends on `xtask`.
