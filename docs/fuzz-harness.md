# Fuzz harness

## What it is

`lodestone-fuzz` holds the hermetic decoder properties and the Track B
tick-aligned differential harness. Track B's fixed replay is a deterministic,
bounded action script that compares a caller-named block-state region after
every tick and retains the seed and script alongside the first divergence.

## How it works

`differential::FixedActionReplay` owns an opaque replay seed, ordered
`ScriptStep`s, a `BlockStateRegion`, and a trailing settle horizon. Construction
rejects duplicate probes, empty candidate alphabets, unordered actions, and
work beyond the documented step/probe/tick limits before it creates an oracle.
`FixedActionReplay::run` delegates to `run_differential`, which applies each
tick's actions to both sides, advances both worlds once, then compares every
probe in deterministic order. `ReplayReport` keeps the complete replay case;
its `DifferentialOutcome::Diverged` value is the first tick and position that
disagreed, not an end-of-run aggregate.

`WorldOracle` is the common seam. Hermetic tests use a tiny in-memory map
oracle, while a real-server run can use `differential::rcon::RconOracle` when
the `rcon-oracle` feature is enabled. Both return a state only from the
region's caller-supplied candidate list, so the constrained server probe and
the in-process reader compare the same alphabet.

## How to change it

Add a new fixed case by constructing `BlockStateRegion` and
`FixedActionReplay`, then run it against fresh oracle instances. Keep the seed
with the script even though this slice does not generate from it yet: it is the
stable identifier a later generator or a bug report can reuse. Add a hermetic
fake-oracle detector control for any change to replay ordering, region
validation, or divergence reporting; agreement-only coverage cannot prove the
comparison is capable of observing a mismatch.

Do not create a second replay loop for live tests. Implement `WorldOracle` or
use `RconOracle`, so real and hermetic cases share the tick ordering and
first-divergence semantics. Random generation and shrinking belong above this fixed
replay layer and are intentionally out of scope here.

## Configuration

`MAX_FIXED_REPLAY_STEPS`, `MAX_FIXED_REPLAY_PROBES`,
`MAX_FIXED_REPLAY_CANDIDATES`, and `MAX_FIXED_REPLAY_TICKS` bound fixed replay
work. `settle_ticks` is part of the replay and allows delayed world reactions
to be compared after the last action. Enable Cargo feature `rcon-oracle` only
for a real-server `RconOracle` run; hermetic replay tests require no container
or network service.

## Dependencies

The core replay abstraction depends only on `lodestone-fuzz`'s
`differential` module and the caller's `WorldOracle` implementations. The
optional real-server path uses `lodestone-testsupport` for RCON framing; it is
not a dependency of the hermetic fake-oracle tests.
