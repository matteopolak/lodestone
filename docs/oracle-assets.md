# Oracle assets: what's on disk under `.cache/mc/`, and who reads it

## What it is

A census of `.cache/mc/` — the vanilla server jars, client jars, and booted-server world
directories this repo has fetched — cross-referenced against `crates/`, `xtask/` and `scripts/`
to say, for each version, exactly what test or script consumes it. It exists because
`docs/plans/multi-version-protocol-dedup.md`'s Stage 0 found eight jars on disk that nothing in
the tree reaches: fetched by `xtask version-table`, unknown to the protocol work. This document
is that finding turned into a maintained table, so the next agent reads it instead of re-deriving
it (or worse, assuming a fetched jar is therefore a used one).

Every row below was verified directly against the working tree at the commit named in the table
below (`git log -1 --format=%H`), by grepping for `.cache/mc/<version>` path literals in
`crates/`, `xtask/` and `scripts/` — not by trusting the plan document's own summary of itself.
Re-run the greps in "How to change it" before trusting a row that looks stale. Protocol numbers
are read from `crates/lodestone-registry/src/generated/version_table.rs`, not retyped by hand.

## How it works

### Census, measured at `6dd1ce92683e6d856f0ce0d1372de291b778c70d`

| version | protocol | jar | client.jar | booted `world/` | on-disk size | consumer(s) |
|---|---|---|---|---|---|---|
| 1.7.10 | 5 | **absent** | — | — | — | n/a — not fetched at all |
| 1.8.9 | 47 | yes | yes | yes | 21 MB | v1-8's `#[ignore]`d live gates (`live_chunk.rs`, `live_entity.rs`, `live_interaction.rs`; container `lodestone-mc189`, game `:25566`, RCON `:25576`); `crates/versions/1.8/tests/support/real_1_8_9_section_save.txt` + `oracle/extract_real_section.py` (extracted from this world); `crates/lodestone-anvil/tests/region_real_world.rs` (ignored, reads `world/region/r.0.0.mca` directly) |
| 1.9.4 | 110 | **absent** | — | — | — | n/a — not fetched at all |
| 1.10.2 | 210 | **absent** | — | — | — | n/a — not fetched at all |
| 1.11.2 | 316 | **absent** | — | — | — | n/a — not fetched at all |
| 1.12.2 | 340 | yes | yes | yes | 44 MB | `scripts/live-oracles/legacy-1.12.sh` (+ now also `legacy.sh 1.12.2`) backing v1-9's four `#[ignore]`d live gates (`live_chunk.rs`, `live_entity.rs`, `live_interaction.rs`, `live_canonical.rs`; container `lodestone-legacy-1-12`, game `:25568`, RCON `:25569`); `crates/lodestone-anvil/tests/region_real_world.rs` (ignored) |
| 1.13.2 | 404 | yes | no | no | 32 MB | **not a live gate** — a one-shot JVM-oracle dump: `crates/lodestone-canonical/tests/flattening.rs` + committed extract `tests/support/flattening_1_13_2_jvm.txt` (regenerate via `JAR=.cache/mc/1.13.2/server.jar`, `LODESTONE_REGEN=1`); `crates/versions/1.9/src/particle_ids.rs` + `tests/particle_ids.rs` (same jar, decompiled under `container` for the legacy particle-id table) |
| 1.14.4 | 498 | yes | no | yes | 34 MB | `scripts/live-oracles/legacy.sh 1.14.4` (container `lodestone-mc1144`, game `:25586`, RCON `:25587`) backing `crates/versions/1.14/tests/capture_join.rs`'s `#[ignore]`d recorder; the jar's own `--reports` dump is committed at `crates/versions/1.14/tests/support/{blocks,registries}_1_14_4_jar.json` and generates that protocol's block-state and entity tables |
| 1.15.2 | 578 | yes | no | yes | 35 MB | `scripts/live-oracles/legacy.sh 1.15.2` (container `lodestone-mc1152`, game `:25588`, RCON `:25589`), same recorder; same committed `--reports` dumps at `..._1_15_2_jar.json`. Also the source world for the vanilla upgrade oracle in `crates/versions/1.14/tests/support/state_upgrade_1_15_2_to_26_2.txt` |
| 1.16.5 | 754 | yes | no | yes | 39 MB | v1-14's `#[ignore]`d live gates (`live_chunk.rs`, `live_entity.rs`, `live_interaction.rs`; container `lodestone-mc1165`, game `:25573`, RCON `:25574`); `crates/versions/1.14/src/canonical.rs` + `tests/canonicalisation.rs` (jar decompiled under `container` for the 754 canonicalisation table); `crates/lodestone-anvil/tests/region_real_world.rs` (ignored) |
| 1.17.1 | 756 | yes | no | no | 42 MB | **referenced by nothing** |
| 1.18.2 | 758 | yes | no | no | 44 MB | **referenced by nothing** |
| 1.19.4 | 762 | yes | no | no | 45 MB | **referenced by nothing** |
| 1.20.1 | 763 | yes | no | no | 96 MB | **referenced by nothing, and not even one of the 16 version-table targets** — `1.20.6` is the table's entry in that slot; this directory is orphaned (see "The 1.20.1 oddity" below) |
| 1.20.6 | 766 | yes | no | no | 49 MB | **referenced by nothing** |
| 1.21.11 | 774 | yes | no | no | 54 MB | **referenced by nothing** for the jar itself — but see the note below: `vendor/minecraft-data/data/pc/1.21.11` (a *different* asset, not this jar) is heavily used |
| 26.2 | 776 | yes | yes | yes | 499 MB | the primary development target: `lodestone-v26-2`, `lodestone-server`, and every `creative.sh`/`survival.sh`/`terrain.sh` oracle |

Missing entirely (never fetched): **1.7.10, 1.9.4, 1.10.2, 1.11.2** — four of the sixteen
version-table targets. `xtask version-table --fetch-missing` would fetch them; nothing in this
repo currently asks it to.

**The "referenced by nothing" jars are the plan's finding, confirmed by a fresh grep rather
than copied from it.** The list was eight; the 1.14-era merge consumed two of them, leaving
six: `1.17.1, 1.18.2, 1.19.4, 1.20.1, 1.20.6, 1.21.11`. Each was checked with `grep -rF ".cache/mc/<version>" crates xtask scripts docs` — the
only hits for any of the six are this document and the dedup plan's own open-decision prose about
*fetching* Mojang reports from `1.21.11`'s jar in the future, which is a proposal, not a consumer.

### The 1.21.11 asset-identity trap this table exists to prevent

A bare `grep -rF "1.21.11"` (not path-scoped) returns dozens of hits in
`crates/lodestone-data/{src,tests}/light_props.rs`, `collision_shapes.rs`, `entity_dimensions.rs`,
and `crates/versions/26.2/tests/live_block_light.rs` / `live_chunk.rs` / `live_terrain_light.rs`.
**Every one of those reads `vendor/minecraft-data/data/pc/1.21.11/blocks.json`** — a community
dataset checked into a *different* vendor tree — never `.cache/mc/1.21.11/server.jar`. The two
are different oracle classes (CLAUDE.md's "Data sources, in order": Mojang's generator is #1,
minecraft-data is #3, cross-check-grade only), and conflating "1.21.11 is referenced" with "the
`.cache/mc/1.21.11` jar is referenced" would have hidden this row's real finding. Path-scope any
future grep the same way (`.cache/mc/<version>` as a literal, not the bare version string).

### The 1.20.1 oddity

`.cache/mc/1.20.1` is the largest of the eight unreferenced entries (96 MB — roughly double the
next largest) and has a different internal shape from every other version directory: a
`libraries/`/`versions/` tree alongside `server.jar`, rather than the flat
`server.jar` + `server.properties` + `world/` layout every fetched-and-run oracle has. It is not
one of the sixteen version-table targets at all (the table's neighbouring 1.20.x entry is
`1.20.6`, protocol 766) — it reads as a stray manual download rather than an `xtask fetch-version`
product, and is unreferenced under either reading of "referenced."

### Runtime this census does not cover

Whether `container` can currently boot any of these jars is a separate, time-varying fact — see
[`docs/oracles-and-benchmarks.md`](./oracles-and-benchmarks.md) ("Oracle runtimes: Apple
`container`") for the runtime itself, and `scripts/live-oracles/legacy.sh` /
`legacy-1.12.sh` / `creative.sh` / `survival.sh` / `terrain.sh` for the scripts that drive it.
That doc used to live at `docs/oracle-runtimes.md`; a concurrent documentation-consolidation
commit folded it into `oracles-and-benchmarks.md`, and `docs/README.md`'s generated index had not
been regenerated as of this writing, so it still links the now-dead `oracle-runtimes.md` path —
committed drift, not repeated here.

## How to change it

- **Adding a version's jar**: `cargo run -p xtask -- fetch-version --version <ver>` (see
  `--help`). Update this table's row in the same commit — an empty consumer column is only
  informative while it is current.
- **Wiring an unreferenced jar to a real consumer**: once a test or script reads
  `.cache/mc/<version>/...`, move that version's row out of "referenced by nothing" and name the
  file(s). Do this in the same commit that adds the consumer, not as a follow-up — this table
  drifting the same way `docs/README.md`'s link just did is exactly the failure mode it exists to
  avoid.
- **Re-verifying a row**: `grep -rF ".cache/mc/<version>" crates xtask scripts docs` (fixed-string,
  not a bare regex — an unescaped version number like `1.14.4` is a valid regex that matches
  unrelated hex/decimal noise via its dots, which is how the first pass at this table nearly
  mis-reported several rows as referenced).
- **A version can be "referenced" through a vendored dataset instead of its own jar** — see "The
  1.21.11 asset-identity trap" above. Grep `.cache/mc/<version>` specifically, not the bare
  version string, or a `vendor/minecraft-data` hit will read as a jar consumer it is not.

## Configuration

None. This is a census of fetched artefacts, not a configurable subsystem; `.cache/mc/` itself is
gitignored and reconstructed on demand by the `xtask` fetch commands above.

## Dependencies

- `xtask`'s `fetch-version`/`version-table` commands populate `.cache/mc/`.
- `scripts/live-oracles/legacy.sh` (1.8.9, 1.12.2, 1.16.5), `legacy-1.12.sh` (1.12.2, unchanged),
  `creative.sh`/`survival.sh`/`terrain.sh` (26.2) boot the jars listed above under Apple
  `container` — see `docs/oracles-and-benchmarks.md`.
- `crates/lodestone-canonical`, `crates/versions/1.9`, `crates/versions/1.14`,
  `crates/lodestone-anvil` are the crates whose tests read a jar or a booted world directly (as
  opposed to going through a live network gate).
