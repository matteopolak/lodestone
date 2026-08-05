# `lodestone-canonical`: the shared pre-Flattening → 26.2 block-state layer

## What it is

The crate every pre-1.13 protocol family maps its blocks through. It holds two things that
used to live inside `lodestone-v340`: the JVM-dumped `(old_block_id, meta)` → modern-block
table (vanilla's own 1.13.2 `DataFixerUpper` flattening fix), and the bridge from that
table's output to a concrete canonical **26.2** block-state id — the id space
`lodestone-world`'s palette consumers (the mesher's atlas, collision) are actually built
from. Extracted here as unit U1 of epic #343's dispatch plan
([`plans/multi-version-protocol.md`](./plans/multi-version-protocol.md)) so that the four
pre-1.13 families do not each carry a private copy of a 9,000-line generated table.

## How it works

Two modules, in series, both re-exported unchanged by `lodestone-v340` (so existing call
sites read `crate::flattening::…` / `crate::canonical::…` exactly as before):

| module | answers | provenance |
|---|---|---|
| `flattening` | `(id, meta)` → 1.13-era block name + properties | reflective dump of the real 1.13.2 server jar's own `DataFixerUpper` |
| `canonical` | that name + properties → a 26.2 state id (`lodestone_data::block_states`) | the 26.2 jar, via `lodestone-data`, plus a small hand-verified rename/property bridge |

Neither collapses a failure into air. `flattening` distinguishes *no table entry*,
*requires additional context* (flower pots, skulls) and the one structurally out-of-bounds
slot; `canonical` passes all three through and adds `Unmapped` as a drift guard. The
decision to substitute air is the **consuming family's**, made in its own `chunk.rs` and
counted in a `FallbackTally` so it stays visible.

Deep detail lives in the two documents this one summarises:
[`protocol-340-flattening-table.md`](./protocol-340-flattening-table.md) (the table, its
ambiguous cases, the `minecraft-data` cross-check) and
[`protocol-340-canonical-bridge.md`](./protocol-340-canonical-bridge.md) (the rename and
property bridges, entry by entry).

## Why one table serves every pre-1.13 version

The dumped table is the 1.13.2 DataFixer's, so it upgrades *1.12.2-space* ids. Older
versions' id spaces are a strict subset — ids were only ever added — so the same table
serves 1.7.10 through 1.12.2, and the per-version difference is only which slots are
populated, which `LegacyBlockState::NoTableEntry` already expresses.

The earlier doctrine deliberately denied `v340`'s table to `v47` to preserve per-crate
deletability. That trade is reversed here, for a reason that does not weaken deletability:
deletability applies to **families**, and deleting one is still its folder plus its
dependency line and feature line in `lodestone-registry` — exactly what `cargo xtask
check-deletable <vNNN>` simulates. This crate names no family and must never start; shared
game data living in a shared crate has precedent in `lodestone-data` (issue #361).

## How to change it, and the gotchas

- **`src/generated/flattening.rs` is generated. Never hand-edit it.** Re-dump from the jar
  with `oracle-java/FlatteningOracle.java`, replace `tests/support/flattening_1_13_2_jvm.txt`,
  then regenerate with
  `LODESTONE_REGEN=1 cargo test -p lodestone-canonical --test flattening committed_table_matches_dump -- --ignored --nocapture`.
- **The obfuscated class name is jar-build-specific.** The dump reads class `yp` in *that
  exact* 1.13.2 build; a different jar will not have it under that name. Rediscover it with
  the grep-then-decompile method in `FlatteningOracle.java`'s class doc before anything else.
- **The always-run drift guard is `committed_table_matches_the_committed_dump`**, which
  compares the committed generated file against the committed JVM dump — not against
  itself. Perturbing one `ResolvedEntry` name makes it fail naming the exact `old_id`/`meta`;
  that is the check that it is not vacuous.
- **`tests/canonical_states.rs` predicts values, not shapes.** Eight `(id, meta)` pairs with
  hardcoded 26.2 state-id literals, plus the negative control that the naive packed
  `(id << 4) | meta` composite names a *different* 26.2 block for every one of them. Adding
  a pair means adding it to the control too; a pair where the packed value coincides is
  worthless and the control says so in its failure message.
- **Adding a rename or property fixup is hand-written work and needs its own
  justification comment.** The generic `waterlogged=false` append is safe only because
  pre-1.13 has no waterlogging concept at all — that is a fact about the era, not a default.

## Configuration

`LODESTONE_REGEN=1` switches the flattening generator from assert to write. Nothing else.

## Dependencies

`lodestone-data` only, for the 26.2 block-state census. Consumed today by
`lodestone-v340`; every future pre-1.13 family (`v5`, `v47`, `v110`) is expected to consume
it rather than store raw packed ids, which is the defect `v47` is in today.
