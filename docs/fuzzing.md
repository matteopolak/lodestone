# Fuzzing

## What it is

Two independent fuzzing tracks. **Track A**
(`fuzz/`, a `cargo-fuzz`/libFuzzer workspace) is coverage-guided, in-process
fuzzing over pure parsing functions — packet decoders, NBT, loot-table JSON,
block-state strings, the density compiler, the unihex font parser, region-file
deserialization, and chat-text JSON/NBT. It finds panics, hangs and decode
crashes on malformed input; it has no oracle for correctness. **Track B**
(`crates/lodestone-fuzz/src/differential.rs`) is a tick-aligned
differential-fuzzing *harness* against a real vanilla oracle, for the class of
bug Track A structurally cannot see — wrong behaviour that never panics (the
motivating example: breaking a waterlogged block used to destroy the water
too, which is not the real mechanic). Track B is a narrow slice rather than a
finished fuzzer: fixed scripts run end to end against a live vanilla server,
while bounded generated scripts and semantic shrinking are proven only against
fresh in-memory oracles. Generated live runs remain deliberately unwired until
their reset and tick-boundary semantics can be made trustworthy. Its own
section below says exactly what is and is not there.

This complements, not replaces, `crates/lodestone-fuzz`'s existing
`proptest`-based harness (`docs/fuzz-harness.md`) — that harness runs under a
plain `cargo test`, no nightly, no corpus directory; Track A is coverage-guided
and can run far longer and explore far deeper, at the cost of needing the
nightly toolchain and a corpus/artifact directory. Both check "never panics on
arbitrary bytes" for overlapping but not identical surfaces; neither replaces
the other, matching that doc's own note that this is additive.

## How it works

### Track A: `fuzz/`, a `cargo-fuzz` workspace

`fuzz/` is its own Cargo workspace (own `Cargo.lock`, empty `[workspace]`
table in `fuzz/Cargo.toml`) — the same shape `web/` and
`crates/lodestone-wasm-host`'s guests already use, and for the same reason:
`cargo fuzz build` injects nightly-only sanitizer/coverage flags
(`-Cpasses=sancov-module`, `-Zsanitizer=address`) that must never leak into a
plain `cargo build --workspace` for the other ~290 crates. `rust-toolchain.toml`
pins `nightly-2026-08-07`, so no separate toolchain install is needed — a
`fuzz/`-rooted cargo invocation picks it up the same way any other command in
this repo does. `cargo install cargo-fuzz` (0.13.x tested) is the one thing
this needs beyond what the repo already pins.

Twelve targets, each a `[[bin]]` in `fuzz/Cargo.toml` under
`fuzz/fuzz_targets/`:

| target | what it fuzzes | crate |
|---|---|---|
| `v26_2_clientbound_decode` | `V770Adapter::handle_packet`, state+id from the input's own first bytes | `lodestone-v26-2` |
| `v26_2_clientbound_decode_by_id` | as above, but selects a *real* declared packet id by index rather than an arbitrary `i32` — reaches real decode bodies far more often | `lodestone-v26-2` |
| `v26_2_serverbound_decode` | `V770ServerProtocol::decode` — the attack surface a connecting *client* presents to our integrated server, the only family that hosts | `lodestone-v26-2`, `lodestone-server` |
| `nbt_decode` | `lodestone_core::read_named_nbt` | `lodestone-core` |
| `loot_table_json` | `lodestone_server::loot::LootTable::from_json` | `lodestone-server` |
| `block_state_string` | `lodestone_data::block_states::state_id` (`minecraft:oak_log[axis=y]` grammar) | `lodestone-data` |
| `density_function_json` | `lodestone_worldgen_core::density::Builder::build` — **known to panic today**, see that file's own doc | `lodestone-worldgen-core` |
| `unihex_font` | `lodestone_assets::font::read_hex_entries` (GNU Unifont `.hex` line format, reachable via a pushed resource pack) | `lodestone-assets` |
| `anvil_region_parse` | `lodestone_anvil::region::RegionFile::parse` + `read_chunk_nbt_bytes` over all 1024 chunk slots, chained into `read_named_nbt` | `lodestone-anvil`, `lodestone-core` |
| `text_chat_json` | `lodestone_model::text::Text::from_json` (pre-1.20.3 chat/sign/book text) | `lodestone-model` |
| `text_chat_nbt` | `lodestone_model::text::Text::from_nbt`, chained behind `read_network_nbt` so a crash localises to the `Nbt -> Text` fold and not the NBT reader `nbt_decode` already covers | `lodestone-model`, `lodestone-core` |
| `resource_pack_zip_source` | `lodestone_assets::ZipSource::from_bytes`, then `read` on every entry it indexed — the central-directory parse and the per-entry decompression path, both reachable from a `minecraft:resource_pack_push`-supplied archive | `lodestone-assets` |

Every target reuses `lodestone-fuzz`'s existing `decode_clientbound`/`Family`
plumbing where applicable, so "which packet ids exist" keeps tracking the
generated `packet_ids` tables rather than a second hand-maintained list — see
`crates/lodestone-fuzz/src/lib.rs`.

#### Recipes

| recipe | what it does |
|---|---|
| `just fuzz-build` | ASan build of every target, no run |
| `just fuzz-run <target> [flags]` | one target, from whatever `fuzz/corpus/<target>/` holds |
| `just fuzz-run-seeded <target> [flags]` | one target, starting from the **committed seeds** — prefer this for a real campaign |
| `just fuzz-repro <target> <artifact-file>` | replay one saved crash/timeout, deterministically |
| `just fuzz-smoke [seconds]` | bounded run of every gated target from the seeds; what CI runs |
| `just fuzz-seeds-regen` | rebuild `fuzz/seeds/**` from `.cache/mc` (needs a populated `.cache`) |

**Always pass `-max_total_time`** to `fuzz-run`/`fuzz-run-seeded`. An
unbounded libFuzzer run never terminates on its own, and CLAUDE.md's
disk/memory hazards apply to a growing fuzz corpus exactly as they do to
`target/` — bound `-rss_limit_mb` too on a machine running other agents'
builds. `just fuzz-smoke` bounds both itself.

```bash
just fuzz-run-seeded nbt_decode -max_total_time=600 -rss_limit_mb=1024
```

#### The seed corpus, and where every byte comes from

`fuzz/seeds/<target>/` is **committed**; `fuzz/corpus/`, `fuzz/artifacts/`,
`fuzz/target/` and `fuzz/coverage/` are gitignored and must stay that way.
libFuzzer writes coverage-increasing inputs only into the *first* corpus
directory it is given, so passing `corpus/<target>` before `seeds/<target>`
(what `fuzz-run-seeded` and `fuzz-smoke` both do) keeps the seeds read-only.

Seeds exist because an expected value — and a starting input — must originate
outside the code under test. A decoder seeded from its own encoder's output
only ever explores shapes that encoder already produces, and agrees with it by
construction. So every family is regenerated by `fuzz/seeds/generate-seeds.py`
(`just fuzz-seeds-regen`) from a producer this repository did not write:

| target | seeds | external producer |
|---|---|---|
| `v26_2_clientbound_decode`, `..._by_id`, `v26_2_serverbound_decode` | 36 each | packet payloads **captured off the wire** from a real vanilla 26.2 server (`crates/versions/26.2/tests/fixtures/*.hex`), framed with the packet id the vanilla generator's own `generated/reports/packets.json` gives that packet name in that state |
| `nbt_decode` | 5 | a **world save a real vanilla server wrote**: `level.dat`, one player `.dat`, and three chunk payloads decompressed out of a region file |
| `anvil_region_parse` | 1 | one real region file trimmed to its smallest single chunk — header encoding, compression byte, deflate stream and chunk NBT are all verbatim vanilla bytes; only the other 1023 header slots are zeroed |
| `loot_table_json` | 12 | the **vanilla data pack**'s own loot tables |
| `density_function_json` | 6 | the vanilla data pack's own density functions |
| `text_chat_json` | 25 | real text components out of vanilla advancements (plain `translate`), chat types (`with` parameter lists) and dialogs (click/hover events, nested `extra`) |
| `text_chat_nbt` | 26 | the same 25 components, NBT-encoded by a **small independent encoder written in that Python script** rather than by ours, plus a real `level.dat` with its root name stripped to network-NBT framing |
| `block_state_string` | 32 | strings spelled from the generator's own `generated/reports/blocks.json` — property names, value spellings and which properties a block even has all come from there, so a seed cannot inherit a misunderstanding from our parser |
| `unihex_font` | none | **honestly none available**: no GNU Unifont `.hex` file exists anywhere under `.cache/` (26.2 ships the unifont as a separately-downloaded pack), and there is nothing else to take one from offline. This target starts from an empty corpus. |
| `resource_pack_zip_source` | 1 | a zip built by this script from vanilla's own `assets/minecraft/lang/en_us.json` and `deprecated.json` (each truncated to `MAX_SEED_BYTES`) — the *contents* are real vanilla bytes, though (unlike the packet captures above) no vanilla `client.jar` is checked out under `.cache/` for the *container* itself to come from, so `zipfile` builds it |

`generate-seeds.py` treats a missing source as a hard error rather than a
quietly smaller corpus, and checks every fixture-to-packet mapping against the
vanilla report, so a renamed packet fails loudly instead of seeding a payload
under an id that never carries it.

#### Reproducing a crash

A failing run saves the exact input under `fuzz/artifacts/<target>/` (prefix
`crash-` or `timeout-`, suffixed with the input's SHA-1). CI's `fuzz` job
uploads that directory as the `fuzz-artifacts` workflow artifact on failure,
for exactly this reason. To reproduce:

```bash
just fuzz-repro density_function_json artifacts/density_function_json/crash-f2c9...
```

The path is relative to `fuzz/`, because the recipe `cd`s there. That is one
input, one execution, no mutation — deterministic, so it either reproduces or
the bug is environment-dependent and worth saying so. `cargo fuzz fmt <target>
<file>` (run from `fuzz/`) prints the input as the target's own `Arbitrary`
type where one is used, and `xxd` is enough for the `&[u8]` targets here.

**When a target finds a real bug, promote the input to a regression fixture**
rather than leaving it in `fuzz/artifacts/` — follow
`crates/lodestone-fuzz/tests/fuzz_regressions.rs`'s shape: a `#`-commented hex
fixture plus an assertion pinning what the decoder does with it, not merely
that it survives. Corpora and artifacts are gitignored by design; a crash left
only there is a finding nothing gates on.

#### CI: the `fuzz` job

`.github/workflows/ci.yml`'s **`fuzz`** job runs `just fuzz-smoke 30` on
every push to `main` and every pull request: `cargo fuzz build` for all
twelve targets, then **30 seconds per gated target** from the committed seeds
(eleven targets, so roughly five-and-a-half minutes of fuzzing). `timeout-minutes: 90`,
because on a cold cache the ASan release build of the whole crate graph at
`codegen-units=1` dominates the job.

Thirty seconds is a tripwire, not a campaign. What it actually gates: every
target still builds under ASan and links its entry point; every target still
loads its seeds and reaches real decode code (a collapsed exec count in the
`-print_final_stats=1` output is the tell if it stopped); the whole committed
corpus is replayed deterministically; and a *new* shallow panic reachable
within seconds of the seeds fails the push that introduced it. Long campaigns
stay a human's `just fuzz-run-seeded` job.

Leak detection is off (`-detect_leaks=0`). These targets' documented property
is "decoding attacker-controlled bytes must not panic"; a decoder that
allocates and then returns `Err` is a correct decoder, not a finding.

`fuzz/smoke-exclusions.txt` lists targets built but not gated, one per line
with a reason. Gating is **opt-out**, so a newly added target is covered the
moment it exists — and a stale exclusion naming a target that no longer
exists is a hard error, so an exclusion cannot silently keep a live target
ungated. One entry today: `density_function_json`, whose panic on any
non-object document is a documented "trusted embedded data" assumption pinned
by `crates/lodestone-fuzz/tests/density_builder_panics_on_non_object_json.rs`.
It goes back under the gate once that builder returns a `Result`.

#### Measured runs

90 seconds per target, from the seeded corpora, ASan, `aarch64-apple-darwin`,
one target at a time on a machine with other builds running:

| target | executions in 90s | result |
|---|---|---|
| `text_chat_json` | 14,146,197 | clean |
| `text_chat_nbt` | 7,688,512 | clean |
| `unihex_font` | 6,825,454 | clean |
| `nbt_decode` | 4,739,666 | clean |
| `block_state_string` | 2,641,448 | clean |
| `loot_table_json` | 1,459,842 | clean |
| `v26_2_clientbound_decode` | 853,708 | clean |
| `anvil_region_parse` | 672,593 | clean |
| `v26_2_serverbound_decode` | 644,495 | clean |
| `v26_2_clientbound_decode_by_id` | 462,697 | clean |
| `density_function_json` | 1,801 | **crashed in 1s** — the tracked non-object-JSON panic, reached by a one-byte mutation of a real vanilla density document (`y_factor` → `y_fa[tor`, so the `f()` helper's "missing/non-numeric field" arm). Not a new finding, and it is the control proving the harness reports a crash rather than swallowing one. |

Execution *rates* differ by three orders of magnitude across this table and
that is expected, not a defect: `text_chat_json` rejects most mutations at
`from_utf8` in nanoseconds, while `anvil_region_parse` walks 1024 chunk slots
and inflates a deflate stream per execution.

### Cranelift verification of the panic-to-finding boundary

`lodestone_fuzz::catch` (in `crates/lodestone-fuzz/src/lib.rs`) is one of this
workspace's two *production* `catch_unwind` panic-isolation boundaries per
CLAUDE.md (the other is `lodestone-ecs`'s `async_task`/`handle` job boundary) —
every property test in `crates/lodestone-fuzz/tests/` converts a decoder panic
into a reported test failure through it, so a silent stop-catching there means
a fuzzer/property test aborts instead of reporting a finding. CLAUDE.md
separately records a real, measured incident of a *nested* `catch_unwind`
failing to observe a panic specifically under Cranelift (this workspace's
debug codegen backend, `.cargo/config.toml`) while the identical test passed
under LLVM.

`crates/lodestone-fuzz/tests/catch_unwind_under_cranelift.rs` is the targeted
verification: a multi-frame, cross-crate panic (through `lodestone_core::Reader`,
via a `Vec` index rather than the tick-counter incident's slice index — a third,
independent panic source) caught both as a plain `#[test]` and inside an actual
`proptest!`-macro-expanded body, matching the exact nesting shape every
randomized property in this crate already runs under. **Measured while writing
this file: both cases pass identically under the default Cranelift build
(`cargo test -p lodestone-fuzz --test catch_unwind_under_cranelift`) and under
LLVM (`CARGO_PROFILE_DEV_CODEGEN_BACKEND=llvm cargo test -p lodestone-fuzz
--test catch_unwind_under_cranelift --target-dir target/llvm-check`)** — the
boundary catches under both backends for every shape this file exercises.
Re-run the LLVM comparison after any toolchain bump; the tick-counter incident
was discovered only because a codegen change surfaced it.

### Track B: the differential-fuzzing harness (`differential.rs`)

**Status: two fixed live comparisons, plus hermetic generation and shrinking.**
What exists:

- [`Action`]/[`ScriptStep`]/[`Script`] — the shared action-sequence type used
  by both hand-written live scripts and the hermetic generator.
- [`WorldOracle`] — the trait a "side" of the comparison implements:
  `apply`/`advance_tick`/`block_state`.
- [`run_differential`] — the actual tick-alignment mechanism: applies every
  action scheduled for a tick to **both** sides before ticking either, then
  compares a caller-named region **after every tick** and returns the *first*
  disagreement (tick, position, both values) rather than aggregating:
  comparing only the final state loses the signal that localises the bug, and
  the two most instructive bug classes here — a fluid spreading at the wrong
  rate, a piston committing on the wrong tick — are *timing* bugs that agree
  on the final state. `Divergence::tick` is the **0-based index of the tick
  that had just been run**, so `tick: 0` means "after exactly one elapsed
  tick", not "before anything ran". Proven correct against two in-memory fake oracles
  in `crates/lodestone-fuzz/tests/differential_harness_self_check.rs`: identical
  fakes never diverge, and a fake made to diverge at a *known* tick is caught at
  **exactly** that tick — the tick-localisation property this module exists
  for, with the divergence case serving as the negative control an
  always-`Agreed` harness would otherwise vacuously pass.
- `differential::rcon::RconOracle` (behind the `rcon-oracle` feature) — a
  `WorldOracle` over any Source-RCON endpoint, using
  `lodestone_testsupport::RconClient`. Works against real vanilla
  (`scripts/live-oracles/*.sh`) and against our own
  `lodestone_server::IntegratedServer::start_rcon` identically, since both
  speak the same wire protocol — this is what lets one oracle type serve
  either side of the comparison.

- `differential::fluid::FluidModelOracle` — the **our-side** oracle, driving
  this workspace's fluid model in-process through `lodestone_server::fluid`'s
  `run_scheduled_tick`/`ticks_after_edit`, the same entry point the world tick
  loop drains its scheduled block ticks into. Its world is a sparse store with
  a solid floor, built by the caller with `place_static` so a `/fill`ed wall
  is not itself a fluid trigger. Nothing here sleeps: `advance_tick`
  increments a counter and drains exactly the ticks due at that number, so our
  side's tick numbering is exact and the whole tick-alignment error budget
  sits on the RCON side alone.
- `differential::redstone::RedstoneModelOracle` — the **our-side** oracle over
  the redstone model, driving two production entry points rather than one:
  `lodestone_server::react_at_placement_with_entities` for a world edit, and
  `lodestone_server::block_tick_reaction::run_due_block_tick` for each entry
  the world tick loop's block-tick drain resolves. Both are needed because a
  redstone signal crosses dust inside the edit and then waits at a repeater,
  re-entering the cascade from a *drained* tick — a comparison driving only
  the edit path cannot see a delay or ordering bug at all.

  Its world is a map of whole `ChunkColumn`s, all resident, created on demand
  with a floor. That is load-bearing: the reaction dispatch's reach is decided
  by the `ChunkSource` it is handed, so a single-column rig answers air one
  cell past a seam and every cross-seam question comes back "no signal" with
  nothing distinguishing that from a real model bug.
  `RedstoneModelOracle::without_neighbours` is the same rig with residency
  denied — the single-column reach that existed before the cross-chunk work —
  and exists purely as the control that proves a cross-seam assertion has a
  detector behind it.
- `differential::state_matches` — gives the in-process side the vanilla side's
  matching semantics, so `minecraft:water` matches `minecraft:water[level=3]`
  on both and the two sides answer in one alphabet.
- `tests/support/differential_generation.rs` — a test-only generator and
  shrink driver over a finite position/state alphabet supplied by the caller
  outside the model under test. "External" describes that ownership boundary;
  the alphabet may be a small independently justified test domain and does
  not need a live fixture. It emits only `Action::SetBlock`: random raw
  commands would overwhelmingly be invalid, and the in-process oracles
  intentionally do not interpret them. Steps carry bounded tick gaps and are
  mapped to nondecreasing absolute ticks, beginning at tick 0.

  Generation uses `proptest`'s structured strategies and `ValueTree`, with a
  fixed ChaCha seed and fixed case/shrink-attempt budgets. The driver does not
  use an elapsed-time shrink limit. Every candidate gets a newly-created pair
  of oracles, an oracle failure aborts the search, and a shrink is accepted
  only when it preserves the original `(position, left state, right state)`
  divergence class. The tick may shrink because compacting idle time is part
  of making a reproducer useful.

  Before sampling or creating an oracle, the driver checks the domain's
  worst-case execution length, computed as `(max_steps - 1) * max_tick_gap +
  settle_ticks + 1`. The final `1` accounts for the inclusive tick-zero
  iteration.
  Arithmetic overflow and horizons above 4,096 oracle ticks are explicit
  configuration errors. Replay performs the same check against the decoded
  script, so hostile or stale JSON cannot turn into an overflow or an
  effectively unbounded run.

  A found case serializes as versioned JSON containing the scenario name,
  explicit minimal script, comparison region, settle ticks, seed/case
  provenance, and observed divergence. The explicit script is the durable
  reproducer; a seed alone can change meaning when a strategy or dependency is
  upgraded. The hermetic control deserializes that artifact, runs its explicit
  script against another fresh oracle pair, and requires the recorded
  divergence to recur. Unknown format versions are rejected.

  This does **not** use `arbitrary`. `Arbitrary` constructs typed values from a
  byte stream, but its structured `shrink` method no longer exists;
  libFuzzer's raw-input minimization is not semantic action deletion, tick
  compaction, or state/position minimization.
- **Two live runs, against a real vanilla 26.2 server**, both `#[ignore]`d.
  `crates/lodestone-fuzz/tests/differential_live_fluid_spread.rs` pairs
  `FluidModelOracle` with `RconOracle` over a water front spreading down a
  closed stone channel and requires agreement throughout the complete spread.
  `differential_live_redstone_contraption.rs` pairs
  `RedstoneModelOracle` with `RconOracle` over a repeater chain crossing two
  chunk seams, out to fourteen ticks, and agrees. See the two "live finding"
  sections below.

### Two measured facts about reading and stepping a live world

Both cost real time to establish and both fail in the *safe-looking*
direction, where a broken rig reports agreement.

- **The `execute if block … run say <marker>` probe does not work over RCON,
  and it fails silently.** `say` broadcasts to chat and sends the command
  source no feedback, so an RCON caller gets an **empty response body for both
  the matching and the non-matching case**. Measured against a live 26.2
  server, both arms: `''` and `''`. A `block_state` built that way answers "no
  candidate matched" for every position in every world, which makes two
  oracles agree unconditionally — an `Agreed` outcome that measures nothing.

  The working form is the **terminal** `execute if block <pos> <pattern>`,
  whose own feedback is the literal `Test passed` / `Test failed`. Measured on
  the same run: `'Test passed'` and `'Test failed'` respectively.
  `RconOracle::block_state` uses that, and treats **any other** response as an
  oracle failure rather than as "did not match", because conflating the two is
  how a broken rig reports agreement.
  `differential_live_fluid_spread.rs`'s
  `the_rcon_read_primitive_distinguishes_two_known_states` asserts both arms
  against a live server, so the comparison test's result is not vacuous.

- **`/tick step` does not advance scheduled block ticks, and a world being
  *frozen* rather than *paused* does not change that.**
  `scripts/live-oracles/creative.sh`'s own comment raises the possibility that
  the older folklore came from confusing a `pause-when-empty-seconds` pause
  with a freeze. Measured directly, on a world with
  `pause-when-empty-seconds=0`, with a control on the same rig in the same
  run: a water source in a closed channel advanced its front **one cell per 5
  ticks under real time** (250 ms per cell) and **zero cells across 25
  consecutive `/tick freeze` + `/tick step 1` pairs**. The folklore is
  confirmed, not void.

  So there is no exact single-tick primitive on the vanilla side.
  `RconOracle::advance_tick` polls `time query gametime` until the server's own
  counter reaches the next nominal tick; `differential::TICK_MILLIS` supplies
  only the poll cadence, not the verdict that a tick happened. Overshoots are
  recorded by `missed_deadlines` rather than silently adopted. The earlier
  direct measurement remains useful context: cell *N* first read as water at
  **249·*N* ms** across two independent trials against a 250·*N* ms prediction.

  `lodestone-server` still has no `/tick` command of its own, so an
  RCON-driven comparison of *our* server would face the same constraint — one
  more reason the in-process oracle above is the useful shape.

### Two more measured facts, about *aligning* a live comparison

Both were found by taking a comparison out to fourteen ticks. The fluid
comparison diverges on tick 0, so neither could show up there.

- **A torn-down circuit is not a clean one: block ticks outlive the blocks
  that scheduled them.** Filling a rig back to air does not retract the
  scheduled ticks its components had already queued at those coordinates, so a
  rebuild drops fresh repeaters onto a queue that still holds stale entries
  for them.

  What makes that destructive rather than untidy is the shape of a repeater's
  own scheduled-tick rule: an unpowered repeater that is ticked turns **on**,
  unconditionally, and only then schedules the tick that will turn it back off
  `2 · delay` later. (That is the pulse-lengthening behaviour, and it is
  correct.) So a stale entry does not expire harmlessly — it fires a spurious
  pulse. Measured: a comparison started during one reported the far cell of a
  20-cell row powered on its **first** tick, which no correct model of that
  row can produce; and the same test alone, on its own coordinates, measured
  the true 14. A fixed ten-tick sleep before energising was not enough (four
  failures in five runs), because the stale entries do not fire until the
  chunk itself starts ticking. The fix that works is a positive check —
  `differential_live_redstone_contraption.rs`'s `settle_until_quiescent` waits
  for twelve consecutive ticks with every repeater unpowered, one more than
  the longest pulse the row can hold — plus a distinct coordinate lane per
  test, which is what `RconOracle`'s `origin` parameter is for.

- **Real-time alignment has two separate error terms, and each shifts every
  tick label by a whole tick.** The first is *accumulated*: every
  `block_state` probe is a round trip between two sleeps, so a fixed
  `sleep(TICK_MILLIS)` per tick runs slower than the server and the server's
  tick count creeps ahead. Measured on a three-position, two-candidate region:
  an arrival whose true game tick is 10 was reported at harness tick 8.
  `RconOracle::advance_tick` therefore sleeps to a **schedule** anchored at
  the first call, absorbing probe cost into the same 50 ms budget the server
  uses, and `RconOracle::missed_deadlines` counts the ticks that were already
  overdue — assert it is zero before believing any tick label.

  The second is *constant*: the phase between the harness's sleeps and the
  server's tick boundaries. Land on the boundary and a millisecond of jitter
  decides whether a probe sees `k` or `k + 1` ticks; observed as the same test
  alternating between agreeing and reporting a lead, flipped by three extra
  round trips. The fix is to start mid-tick — wait for `time query gametime`
  to advance, then sleep half a tick — which puts a 25 ms margin either side
  of every sample. `time query gametime` is also the right instrument for a
  one-off arrival measurement, being the server's own monotonic tick counter
  rather than elapsed wall time.

### Track B's second live finding: our redstone matches vanilla across two chunk seams

`crates/lodestone-fuzz/tests/differential_live_redstone_contraption.rs`
(`#[ignore]`d, four tests) compares a 20-cell row — source, dust, three
repeaters at `delay` 1, 4 and 2, dust between them — laid out so it crosses a
chunk boundary on its **first** hop and again at cell 17. Three cells are
probed after every tick, each with a two-candidate alphabet holding only its
predicted power and zero, so a side writing any other power answers `None` and
is reported at that position rather than matching something looser.

Measured on a live 26.2 server against its own tick counter, three trials,
identical each time: cell 1 reaches power 15 on tick 0, cell 16 reaches power
**8** on tick 10, cell 18 reaches power 15 on tick 14. Our model produces
exactly that, and the live comparison agrees over the whole run. The two
plausible wrong timelines land nowhere near: reading a repeater's delay as
`delay` game ticks rather than `2 · delay` gives 1/5/7, and reading the flat
one-tick on-place delay gives 1/2/3.

Both halves have a watched failure rather than a description of one. In
process, `redstone_contraption_ticks.rs` runs the same layout against
`RedstoneModelOracle::without_neighbours` and every probed cell stays at power
0 for the whole trace; against a live server,
`a_model_with_no_cross_column_reach_is_caught_on_the_first_tick` requires the
comparison to catch that same model on tick 0 at cell 1, naming the tick and
the position. And because every probe here reads the same *block* and differs
only in a numeric property, the read primitive needs its own control on a
property rather than on a block name —
`the_rcon_read_primitive_discriminates_a_dust_power_level` asserts both arms,
since a probe matching the base name alone would report the predicted power
everywhere and agree unconditionally.

`redstone_contraption_ticks.rs` is the half that runs with no server, no
network and no feature flag, so it is what keeps those numbers from
regressing; the layout and the predictions are shared between the two files
(`crates/lodestone-fuzz/tests/contraption/mod.rs`) so neither can drift from
the other.

### Track B's live fluid-cadence comparison

The external expectation was measured before the comparison code existed:
with a water source at one end of a closed channel, the reference front reaches
cell *N* on tick 5·*N*. The in-process model follows the same schedule: an edit
seeds only positions that already hold fluid, using each fluid's own tick
delay, and newly wetted cells schedule their later work through the normal
fluid queue.

`our_fluid_model_matches_vanilla_s_water_front` compares every channel cell
after every tick through the complete spread and requires agreement. The live
result is accepted only when `RconOracle::missed_deadlines()` is zero, so a
host that bursts through unobserved reference ticks cannot manufacture either
agreement or disagreement.

### What Track B still does not do
- **No validation against a reverted historical fix** — revert a committed fix
  in a scratch worktree and require the harness to rediscover it (flowing
  water waterlogging a slab, a door dropping nothing, …). Each target still
  needs a caller-owned action alphabet and independently sourced expected
  values, plus deterministic reset/tick control before generated scripts can
  be trusted against a live oracle.
- **No client-state comparison.** Comparing what our own *client* believes
  about blocks/entities/inventory after replaying a packet stream (rather than
  comparing rendered pixels, which two different renderers will differ on in
  ways that are not bugs) is entirely unaddressed.
- **Only one dimension, and only fluids.** The comparison covers block states
  over a caller-named region. The entity list, the player's inventory and the
  scheduled-tick queue are all named in the design and none is implemented.
- **Our side of the live comparison is the fluid model, not the whole
  server.** `FluidModelOracle` drives `lodestone_server::fluid`'s production
  scheduled-tick entry point over a sparse world, not a running
  `IntegratedServer`. That is enough for a fluid comparison and not enough
  for redstone or pistons; an `IntegratedServer`-backed `WorldOracle` (via
  `start_rcon`, following
  `crates/lodestone-server/tests/rcon_live_oracle.rs`'s pattern) is the next
  oracle to write. Note our `execute if block` reduces away some properties by
  design, so such an oracle wants a direct `ChunkSource::block_state` read
  rather than a command probe.

## How to change it, and the gotchas

- **Add a fuzz target**: drop a `fuzz_targets/<name>.rs` with
  `#![no_main]` + `libfuzzer_sys::fuzz_target!`, add a matching `[[bin]]` to
  `fuzz/Cargo.toml` (`test = false, doc = false, bench = false`, matching every
  existing entry), and add any new crate dependency to that file's
  `[dependencies]`. `just fuzz-build` compiles everything in one shot; a
  missing `[[bin]]` entry compiles the crate but produces no runnable binary,
  which is easy to miss — check `cargo fuzz list` (run from `fuzz/`) names your
  new target.
  A new target is gated by CI automatically — gating is opt-out via
  `fuzz/smoke-exclusions.txt` — so nothing else needs remembering there.
- **Seed the new target from something this repo did not author**, by adding a
  family function to `fuzz/seeds/generate-seeds.py` and running
  `just fuzz-seeds-regen`, then committing the files it writes under
  `fuzz/seeds/<target>/`. `.cache/mc` carries three usable producers: the
  vanilla generator's own reports (`26.2/generated/reports/*.json`), the
  vanilla data pack (`26.2/src/data/minecraft/**`), and world saves a real
  vanilla server wrote (`survival/world/**`); captured wire bytes live in
  `crates/versions/26.2/tests/fixtures/*.hex`. If there is genuinely no
  external source, say so in the table above rather than seeding from our own
  encoder — `unihex_font` is the honest example.
- **When a target finds a bug, promote the input to a regression fixture, not
  a corpus entry** — corpora and artifacts are gitignored by design
  (`fuzz/.gitignore`), and a crash that lives only in `fuzz/artifacts/` is a
  finding nothing gates on. Follow
  `crates/lodestone-fuzz/tests/fuzz_regressions.rs`'s existing shape: a
  `#`-commented hex fixture plus an assertion pinning what the decoder does
  with it, not merely that it survives.
- **`fuzz/`'s own `Cargo.lock` is real and committed** (`cargo-fuzz`'s default),
  separate from the root workspace's — a dependency bump there needs its own
  `cargo update` run from inside `fuzz/`.
- **Extend `differential.rs`'s action alphabet** by adding an `Action` variant
  and handling it in every `WorldOracle` impl (`RconOracle`, `FluidModelOracle`,
  and the fakes in the self-check test) — `Action::RunCommand` is the escape
  hatch for anything not worth its own variant yet, and is deliberately a
  no-op on the in-process side rather than a second command parser.
- **Extend generated scripts through their `GenerationDomain` first.** Keep
  positions and states finite and caller-owned, order entries from simplest to
  most specific so shrinking has a useful direction, and repeat an entry when
  it needs more generation weight. The state list and comparison candidates
  must be selected outside the model under test; a small independently
  justified test domain is sufficient, while a generated report or measured
  fixture is useful when the scenario needs one. Raw `RunCommand` remains
  replayable but is not generated.
- **Add another `WorldOracle`** by implementing the trait directly. Answer in
  the caller's candidate alphabet via `differential::state_matches` even when
  you have a real read primitive, or the two sides of a comparison will
  disagree about spelling rather than about behaviour.
- **A live differential test must assert its read primitive discriminates,**
  in both arms, against the live server — not just that the comparison
  returned `Agreed`. An always-`None` probe produces a perfectly green
  `Agreed` over any world.

## Configuration

- **`-max_total_time=N`** (seconds) and **`-rss_limit_mb=N`** — pass both on
  every `just fuzz-run` invocation on a shared machine; see the Track A section
  above.
- **`just fuzz-smoke [seconds]`** — seconds per target, default `30`; CI
  passes `30` explicitly so the workflow states its own budget.
- **`LODESTONE_DIFFERENTIAL_RCON`** (`host:port`) — the live endpoint
  `differential_live_fluid_spread.rs` drives, defaulting to the flat/creative
  oracle's `127.0.0.1:25571`. Any oracle in `scripts/live-oracles/` works: the
  rig is `/fill`ed from scratch, so the world's own terrain never
  participates. Whichever one you use needs `pause-when-empty-seconds=0` —
  with nobody logged in, a paused world runs no scheduled block ticks and the
  comparison reports agreement on a frozen world.
- **`differential::TICK_MILLIS`** — the poll cadence used while
  `RconOracle::advance_tick` waits for the server's own game-time counter,
  currently `50` ms. The counter, not elapsed wall clock, decides whether the
  next nominal tick exists.
- **`SearchBudget`** in the generated-script test support — `seed`, `cases`,
  and `shrink_attempts` are all explicit integers. Its proptest configuration
  fixes the RNG to ChaCha, disables failure-persistence-by-seed, and sets
  `max_shrink_time` to zero; the versioned explicit JSON case is the replay
  artifact. `MAX_ORACLE_TICKS` caps each generated or replayed candidate at
  4,096 oracle ticks after checked horizon arithmetic.
- **`rcon-oracle` Cargo feature** on `lodestone-fuzz` — gates the
  `differential::rcon` module and its `lodestone-testsupport` dependency; off
  by default, unlike the four protocol-family features, because it pulls in a
  network-reaching dependency edge `cargo test --workspace` must not need
  just to build this crate.

## Dependencies

- **`cargo-fuzz`** (host tool, `cargo install cargo-fuzz`) and the nightly
  toolchain `rust-toolchain.toml` already pins — see that file's own comment
  for exactly which nightly and why it is pinned to a specific date.
- **`libfuzzer-sys`** (`fuzz/Cargo.toml`) — the `fuzz_target!` macro and
  libFuzzer's runtime driver.
- **`lodestone-fuzz`, `lodestone-core`, `lodestone-model`, `lodestone-server`,
  `lodestone-v26-2`, `lodestone-data`, `lodestone-worldgen-core`,
  `lodestone-assets`, `lodestone-anvil`** — one leaf/near-leaf dependency per
  Track A target, pulled in for exactly one parse entry point each rather than
  any of these crates' heavier subsystems.
- **`lodestone-testsupport`** (optional, `rcon-oracle` feature) — `RconClient`,
  reused rather than re-derived; see `crates/lodestone-testsupport/src/lib.rs`'s
  own doc on the one-`read()`-per-request RCON framing constraint.
- **`python3`** — `fuzz/seeds/generate-seeds.py` only, and only when
  regenerating seeds. Nothing in `just health`, `just fuzz-smoke` or CI runs
  it; the seeds it produces are committed.
- **A live vanilla 26.2 oracle under Apple `container`** — Track B's
  `#[ignore]`d live tests only. See `docs/oracles-and-benchmarks.md` and
  `scripts/live-oracles/`.
- **`proptest`** — a dev dependency used by the existing decoder properties
  and by Track B's hermetic structured generator/shrinker. Track A does not use
  it. `serde` and `serde_json` are also dev-only here and encode the explicit
  replay case; they are not pulled into the differential library or cargo-fuzz
  targets.
