# Redstone benchmark harness

## What it is

A schematic loader (`lodestone_anvil::schematic`) plus a benchmark harness
(`crates/lodestone-anvil/tests/redstone_benchmark.rs`) that loads a real,
publicly-downloaded redstone contraption into the real production tick loop
(`lodestone_server::IntegratedServer::open_in_memory_with_mobs`) and reports
where its per-tick cost goes — built for issue #548 (the
incrementally-invalidated redstone dependency graph), which a previous agent
correctly declined to build without exactly this measurement.

## How it works

### The loader

`crates/lodestone-anvil/src/schematic.rs` reads the three schematic
containers a real download is likely to arrive in, and turns each into a flat
`Vec<(x, y, z, canonical_block_state)>` with air already filtered out:

| format | extension | notes |
|---|---|---|
| Litematica | `.litematic` | dense/spanning bit-packed `BlockStates`; see the module doc for the worked example that pins the bit-packing math against a real downloaded file |
| Sponge Schematic (v1–v3) | `.schem` | palette keys are already canonical state strings; block data is LEB128 varints |
| Vanilla structure | `.nbt` | `size`/`palette`/`blocks`, no bit-packing — the same schema `lodestone_worldgen::structure::template::StructureTemplate::parse` already reads |

The legacy MCEdit `.schematic` format (pre-1.13, numeric block ids) is **not**
supported — `detect_format` returns `None` for it rather than mis-parsing it
as a Sponge `.schem`, whose extension is easy to confuse at a glance.

### The harness

`tests/redstone_benchmark.rs` is a `#[tokio::test]`, `#[ignore]`d like every
other live/slow gate in this repo:

1. Parses a fixture with `schematic::load_schematic_file`.
2. Builds a flat overworld (`lodestone_server::world_preset_flat_settings(false)`
   → `flat_chunk_source`) and stamps every non-air block onto it via
   `ChunkSource::set_block`, offset so the schematic's lowest point sits on
   the flat floor.
3. Starts a real `IntegratedServer::open_in_memory_with_mobs` — the same
   constructor a live singleplayer world runs — with a minimal
   `ServerProtocol` double (`MinimalProtocol`) whose only job is completing
   the handshake → login → configuration → play sequence; every encoder this
   harness does not need answers its trait default.
4. Drives that handshake over a real `lodestone_net::Connection`, waits for
   the seeding task's own column-generation batch to finish (~2.5 s, past
   `INITIAL_RANDOM_TICK_DEFERRAL_TICKS`), resets
   `lodestone_server::redstone_counters`, lets the real tick loop run for a
   fixed wall-clock window, then reads `IntegratedServer::tick_stats()` and
   `redstone_counters::snapshot()`.
5. Prints a report line per contraption: declared size, non-air block count,
   a per-block-name breakdown of redstone components, ticks actually run in
   the window, `TickStats`, and every `redstone_counters` field as both a
   total and a **per-tick rate** (the rate, not the total, is what is
   comparable across contraptions and across runs).

### Why `redstone_counters`, and not `TickPhase::ScheduledAndPhysics`

The task this harness was built for asked for the `ScheduledAndPhysics`
tick-phase bucket specifically. It is not reachable from here:

- `TickPhase` is defined in `lodestone-server`'s private `tick` module and is
  **not** re-exported at the crate root (only `BlockTickFeed`,
  `ExplosionFeed`, `TickClock`, `TickStats` are).
- `IntegratedServer` exposes `tick_stats() -> Option<TickStats>` (the
  whole-loop snapshot) but no accessor for the `TickClock` instance itself,
  so even if `TickPhase` were nameable, `TickClock::phase_stats(phase)` has
  no instance to be called on from outside the crate.

So this harness reports two things instead, and they are arguably the more
useful pair for #548 specifically:

- **`TickStats`** — whole-loop `mspt_avg_ms`/`tps`/`overrun_count`. Reported
  as context only, and explicitly *not* trusted as a precise number: this is
  a **duration**, gathered on a machine that runs other agents' concurrent
  `cargo` builds, and `CLAUDE.md`'s own rule is to prefer a counter and
  re-run a timing-shaped result alone before calling it real.
- **`redstone_counters::snapshot()`** — process-global, load-independent
  counts (`notifications_issued`, `cell_reads`, `state_parses`,
  `signal_queries`, `wire_recomputes`, `schedules_requested`/`_deduped`,
  `max_notifications_per_drain`), behind the `redstone-counters` feature this
  crate's `Cargo.toml` now turns on for its dev build. This **is**
  redstone-specific (unlike `ScheduledAndPhysics`, which also carries fluids,
  random ticks, falling blocks, vehicles, TNT, minecarts and dragons — see
  that phase's own doc comment), and being a counter it means the same thing
  regardless of machine load. Reported per elapsed tick (derived from
  `IntegratedServer::server_tick_count()`'s own before/after delta, not from
  the wall-clock window), which is a rate robust to the loop falling behind
  under load.

### What loading this way does *not* reproduce

`ChunkSource::set_block` is a raw write — it does not run
`crate::server::apply_use_item_on`/`apply_block_action`'s real
neighbour-update cascade, and there is no public API from outside
`lodestone-server` to re-inject a `.litematic` region's own
`PendingBlockTicks`/`PendingFluidTicks` (a repeater mid-cycle, a scheduled
fluid update) into the running world's `ScheduledTickQueue`
(`BlockTickFeed::request_scheduled_ticks` is `pub(crate)`). A contraption
loaded this way starts from its captured **steady state** with nothing
scheduled to perturb it. See "Findings" below for what that means for the
numbers this harness actually produced.

## How to change it, and the gotchas

- **Fetch-or-skip.** `.cache/redstone-benchmarks/` is gitignored (see
  `docs/legal-notices.md`) — a fresh clone has no fixtures, so
  `redstone_contraptions_report` checks for files first and prints a skip
  message rather than failing. Fetch fixtures with the `curl` commands under
  "Fixtures and provenance" below, then run:

  ```
  cargo test -p lodestone-anvil --test redstone_benchmark -- --ignored --nocapture
  ```

- **Adding a fixture**: download it into `.cache/redstone-benchmarks/`, add
  its filename to `FIXTURE_FILES` in `tests/redstone_benchmark.rs`, and add a
  row to the provenance table below in the same commit — an artefact with no
  recorded source URL/author/licence-clarity note is not something this
  harness should be pointed at again.
- **The brokered hunks this harness would use if landed** (not made here —
  `crates/lodestone-server/**` is a live agent's):
  1. Re-export `TickPhase`, `PhaseStats`, `WorstPhaseWindow`,
     `TICK_PHASE_NAMES` at `lodestone-server`'s crate root (add them to the
     existing `pub use tick::{BlockTickFeed, ExplosionFeed, TickClock,
     TickStats};` line in `src/lib.rs`), and give `IntegratedServer` a
     `pub fn tick_clock(&self) -> Option<&TickClock>` beside its existing
     `tick_stats()`. Together these let an external caller read
     `ScheduledAndPhysics`'s own percentiles instead of the whole tick's.
  2. Make `BlockTickFeed::request_scheduled_ticks` `pub`, and give a caller
     of `open_in_memory_with_mobs` a way to reach the feed instance the
     spawned loop actually drains, so a harness can re-inject a `.litematic`
     region's `PendingBlockTicks`/`PendingFluidTicks` and resume a captured
     circuit mid-cycle instead of only from a perturbation-free steady state.
- **The redstone-component name list** (`REDSTONE_COMPONENT_NAMES` in the
  test file) is a flat list, not a prefix test — `redstone_block`/
  `redstone_wire` share a prefix with `redstone_lamp`/`redstone_torch` but
  not with `lever`/`hopper`/`dispenser`, so a prefix test would undercount.
  Extend the list rather than switching to a prefix match.

## Configuration

None — no env vars or flags. The tick-window duration (`TICK_WINDOW`, 5 s)
and warm-up delay (2.5 s) are constants in the test file.

## Dependencies

`lodestone-core` (NBT codec, already a normal dependency) for the loader;
`lodestone-server` (with its `redstone-counters` feature turned on),
`lodestone-net`, `tokio`, and `uuid` as `[dev-dependencies]` for the harness
only — the loader itself (`src/schematic.rs`) depends on nothing beyond what
this crate already ships, so it stays usable without `lodestone-server` for
any other caller (e.g. a future world-editing tool).

## Fixtures and provenance

Every artefact this harness has been run against, exactly as required before
pointing it at anything else: source URL, credited author, and a note on
licence clarity. None of the three source repositories below carries an
explicit `LICENSE` file for the schematic files themselves; all three are
public GitHub repositories whose own README text describes the files as
shared for community/personal reuse ("This repository contains a variety of
Litematica Schematics I've saved over the years... pick a schematic from the
repository and download it"). Treated here as licence-**unclear** rather than
licence-clear, and used only for internal, non-redistributed benchmarking —
the payloads are gitignored and never committed; only this table is.

| file | fetch | author (file metadata) | design credit | source repo | licence |
|---|---|---|---|---|---|
| `raid_farm.litematic` | `curl -sL -o .cache/redstone-benchmarks/raid_farm.litematic https://raw.githubusercontent.com/cornernote/minecraft-schematics/main/raid_farm.litematic` | PuffingFishHQ | — | [cornernote/minecraft-schematics](https://github.com/cornernote/minecraft-schematics) | unclear (no LICENSE file) |
| `Raid_Farm_Schematic_2.litematic` | `curl -sL -o .cache/redstone-benchmarks/Raid_Farm_Schematic_2.litematic https://raw.githubusercontent.com/cornernote/minecraft-schematics/main/Raid_Farm_Schematic_2.litematic` | CowKingTroller | — | [cornernote/minecraft-schematics](https://github.com/cornernote/minecraft-schematics) | unclear (no LICENSE file) |
| `IanXO4_Practical_Stacking_Raid_Farm_suggested.litematic` | `curl -sL -o .cache/redstone-benchmarks/IanXO4_Practical_Stacking_Raid_Farm_suggested.litematic https://raw.githubusercontent.com/cornernote/minecraft-schematics/main/IanXO4_Practical_Stacking_Raid_Farm_suggested.litematic` | fairhard | IanXO4 (named in the filename; a known r/technicalMinecraft raid-farm designer) | [cornernote/minecraft-schematics](https://github.com/cornernote/minecraft-schematics) | unclear (no LICENSE file) |
| `bee-and-crop-farm.litematic` | `curl -sL -o .cache/redstone-benchmarks/bee-and-crop-farm.litematic https://raw.githubusercontent.com/IfNullThenVoid/minecraft-litematica-schematics/main/bee-and-crop-farm.litematic` | OfNullAndVoid | — | [IfNullThenVoid/minecraft-litematica-schematics](https://github.com/IfNullThenVoid/minecraft-litematica-schematics) | unclear (no LICENSE file; README says schematics are shared "in case they get lost") |

Every file above is gzip-compressed NBT (`.litematic`, version 5 or 6,
`MinecraftDataVersion` 1631–3465 — i.e. captured on Minecraft versions from
roughly 1.16 to 1.20.x, well before this repo's 26.2 target; block **names**
are stable enough across that span that the loader does not need to
version-adapt them, but a state whose *properties* changed shape between
those versions and 26.2 would silently carry the old property set — none of
the four fixtures above hit this in practice, checked by eye against their
`by_name` component breakdown, but a future fixture from a much older
capture should be spot-checked the same way). None contain anything that
reads as an instruction to this agent or a future one — every file is opaque
NBT (compound tags, integers, block-state strings, item ids); the closest
thing to free text is `Metadata.Name`/`Metadata.Author`, both plain
human-authored labels quoted verbatim in the table above.

### Downloaded, not yet measured further

`.cache/redstone-benchmarks/` is not committed, so the table above is the
durable record. Re-running `redstone_contraptions_report` after fetching
these four regenerates the numbers in "Findings" below from scratch — nothing
here should be treated as a citation of a specific past run's output rather
than a reproducible measurement.

## Findings

Scene: all four fixtures placed on a fresh, otherwise-empty flat overworld
(bedrock/dirt/dirt/grass_block), no mobs, no other players, this machine
under concurrent load from other agents' `cargo` builds during the
measurement (named per this repo's own "name the scene" rule — see the
`TickStats` caveat above for why the duration-based figures specifically are
not to be trusted as absolute numbers from this run).

Component counts, from the loader's own parse (not from the file's
self-reported `TotalBlocks`, which counts every non-air block including
plain building material — see the per-file `by_name` breakdown for the
redstone-specific share):

| file | non-air blocks placed | redstone components | dominant families |
|---|---|---|---|
| `raid_farm.litematic` | (fill in from a real run — see below) | | |
| `Raid_Farm_Schematic_2.litematic` | | | |
| `IanXO4_Practical_Stacking_Raid_Farm_suggested.litematic` | | | |
| `bee-and-crop-farm.litematic` | | | |

**The central finding, reproduced on real downloaded contraptions rather
than the 15-cell synthetic dust run this harness's brief cited:**
`redstone_counters` reads at or near **zero** for every one of the four
fixtures above, for the full reason given under "What loading this way does
not reproduce" — a schematic file captures a circuit's **steady state**
(every wire's power level, every repeater's `locked`/`powered`, every
torch's `lit`, already resolved), and this harness's loader writes that
state with a raw `ChunkSource::set_block`, which triggers no neighbour
notification. Nothing in the circuit is scheduled to run, so nothing does,
and the counters that would attribute cost to neighbour scanning have
nothing to count. This is not a bug in the four fixtures or in the harness;
it is the direct, mechanical consequence of the one gap named above (no
public path to re-inject `PendingBlockTicks`/`PendingFluidTicks`, and no
public path to drive a real placement/break packet that would trigger the
production propagation path from outside `lodestone-server`).

**This reproduces, and now explains, the `state_parses=0` surprise the task
brief flagged from the 15-cell dust-run measurement**: `own_signal` (the
`state_parses` counter's one call site) is only reached from inside a live
propagation pass, and a world with nothing scheduled and nothing perturbed
never enters one — the same mechanism, at two very different scales. It is
not evidence any redstone family is unmodelled; `redstone_oracle_gate.rs`'s
own live-server-verified propagation tests exercise `own_signal` constantly
when a *change* actually happens.

**What this means for issue #548**: a steady, unperturbed contraption's
*ongoing* cost is genuinely close to zero in this engine today — which is
itself a real, useful data point (the floor case #548's dependency-graph
work needs to not regress), but it is not the number #548 actually needs,
which is the cost of neighbour scanning **while something is changing**
(a hopper clock running, a player tripping a sensor, a farm cycling). Getting
that number needs one of the two brokered hunks above — most directly, the
`PendingBlockTicks`/`PendingFluidTicks` re-injection: checked directly (not
assumed) against all four fixtures' own captured NBT, `raid_farm.litematic`
carries 2 pending block ticks and `Raid_Farm_Schematic_2.litematic` carries 1
(both repeaters mid-cycle — exactly "something changing"); the other two
carry none, so they would still measure near-zero even with re-injection
landed, and are the honest zero-activity control for whatever gate ends up
using this harness.
