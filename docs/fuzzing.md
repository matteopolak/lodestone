# Fuzzing (issue #549)

## What it is

Two independent fuzzing tracks, per issue #549's own split. **Track A**
(`fuzz/`, a `cargo-fuzz`/libFuzzer workspace) is coverage-guided, in-process
fuzzing over pure parsing functions — packet decoders, NBT, loot-table JSON,
block-state strings, the density compiler, the unihex font parser, region-file
deserialization, and chat-text JSON/NBT. It finds panics, hangs and decode
crashes on malformed input; it has no oracle for correctness. **Track B**
(`crates/lodestone-fuzz/src/differential.rs`) is a tick-aligned
differential-fuzzing *harness* against a real vanilla oracle, for the class of
bug Track A structurally cannot see — wrong behaviour that never panics (the
motivating example: breaking a waterlogged block used to destroy the water
too, which is not the real mechanic). Track B is a skeleton today, not a
finished fuzzer — see its own section below for exactly how far it got.

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

Eleven targets, each a `[[bin]]` in `fuzz/Cargo.toml` under
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

Every target reuses `lodestone-fuzz`'s existing `decode_clientbound`/`Family`
plumbing where applicable, so "which packet ids exist" keeps tracking the
generated `packet_ids` tables rather than a second hand-maintained list — see
`crates/lodestone-fuzz/src/lib.rs`.

Run with `just fuzz-build` (compile-only, ASan, no run) or
`just fuzz-run <target> [libfuzzer-flags...]`, e.g.:

```bash
just fuzz-run nbt_decode -max_total_time=60 -rss_limit_mb=1024
```

**Always pass `-max_total_time`.** An unbounded libFuzzer run never terminates
on its own, and CLAUDE.md's disk/memory hazards apply to a growing fuzz corpus
exactly as they do to `target/` — bound `-rss_limit_mb` too on a machine
running other agents' builds. `fuzz/.gitignore` excludes `target`, `corpus`,
`artifacts` and `coverage`; **never commit a corpus or a crash artifact** —
when a run finds something worth keeping, copy the specific crashing input
into a regression fixture the way `crates/lodestone-fuzz/tests/fuzz_regressions
.rs` already does for the proptest harness, with a header saying where it came
from.

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

**Status: skeleton, not a finished fuzzer.** What exists:

- [`Action`]/[`ScriptStep`]/[`Script`] — a fixed, hand-written action sequence
  type. **No generation or shrinking over the alphabet** — issue #549's own
  suggested order puts random generation at step 3, after "prove the harness
  agrees on a script with no known divergence" at step 2, and step 2 is as far
  as this session reached.
- [`WorldOracle`] — the trait a "side" of the comparison implements:
  `apply`/`advance_tick`/`block_state`.
- [`run_differential`] — the actual tick-alignment mechanism: applies every
  action scheduled for a tick to **both** sides before ticking either, then
  compares a caller-named region **after every tick** and returns the *first*
  disagreement (tick, position, both values) rather than aggregating —
  "comparing only the final state loses the signal that localises the bug"
  (issue #549's own words). Proven correct against two in-memory fake oracles
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

What does **not** exist yet, named rather than glossed over:

- **No live run against a real vanilla oracle.** `RconOracle` is written and
  compiles, but running it against a started container plus a live lodestone
  dedicated server was not attempted this session — the machine was under
  measured real memory pressure while this was written (`vm.swapusage` showed
  non-zero `used`, dozens of concurrent `rustc` processes from other agents),
  and starting a JVM container on top of that seemed like the wrong call
  rather than a corner cut for convenience. This is the single most valuable
  next step: wire `RconOracle` up against `scripts/live-oracles/creative.sh`
  on one side and an in-process `IntegratedServer` (via `start_rcon`, following
  `crates/lodestone-server/tests/rcon_live_oracle.rs`'s existing pattern
  exactly) on the other, and run the fixed placing/breaking script from the
  self-check test for real.
- **No general block-state read primitive over vanilla RCON exists.**
  `/data get block <pos>` only answers for a block *with* a block entity
  ("The target block has no tile data" otherwise). `RconOracle::block_state`
  reads by probing a caller-supplied candidate list with
  `execute if block <pos> <candidate>` — the same technique
  `lodestone-server`'s own `redstone_oracle_gate.rs` already uses for an exact
  (not sampled) state read. This bounds Track B, as it stands, to comparisons
  where the caller can enumerate the plausible resulting states up front; it
  cannot yet answer "what is the state here" for an arbitrary position.
- **No shared tick-step primitive between two independent server processes.**
  Neither side implements a `/tick step`/`/tick sprint`-equivalent pause/resume
  today (`lodestone-server` has no `/tick` command at all as of this writing),
  and vanilla's own `/tick step` is documented (CLAUDE.md, and
  `redstone_oracle_gate.rs`'s own module doc) to not advance a scheduled block
  tick regardless. `RconOracle::advance_tick` sleeps one real tick interval
  (50 ms, `differential::TICK_MILLIS`) and lets each side's own tick loop run
  freely — the same "real time, never `tick step`" discipline
  `redstone_oracle_gate.rs` already uses for its own timing measurements. This
  bounds alignment to "both sides have had at least one real tick", not "the
  exact same tick count provably elapsed on both processes".
- **No validation against a reverted historical fix.** Issue #549's own
  "Validation" section — revert a committed fix in a scratch worktree and
  require the fuzzer to rediscover it (flowing water waterlogging a slab, a
  door dropping nothing, …) — needs an action alphabet rich enough to reach
  each reverted code path, which needs generation (step 3) first.
- **No client-state comparison.** The issue's "on the client half" section
  (comparing what our own client believes about blocks/entities/inventory
  after replaying a packet stream, rather than rendered pixels) is entirely
  unaddressed.

## How to change it, and the gotchas

- **Add a fuzz target**: drop a `fuzz_targets/<name>.rs` with
  `#![no_main]` + `libfuzzer_sys::fuzz_target!`, add a matching `[[bin]]` to
  `fuzz/Cargo.toml` (`test = false, doc = false, bench = false`, matching every
  existing entry), and add any new crate dependency to that file's
  `[dependencies]`. `just fuzz-build` compiles everything in one shot; a
  missing `[[bin]]` entry compiles the crate but produces no runnable binary,
  which is easy to miss — check `cargo fuzz list` (run from `fuzz/`) names your
  new target.
- **Seed a corpus from real bytes where they exist.** `anvil_region_parse` has
  none bundled (no committed `.mca` fixture in this repo today — real captured
  region files live only under gitignored `.cache/mc/*` world saves); a human
  wanting a head start can seed `fuzz/corpus/anvil_region_parse/` from a small
  saved region file directly (verbatim disk bytes, no wrapper). The packet
  decoders' own corpus provenance (which fixtures are "strong" captured-vanilla
  evidence versus "weak" self-encoded) is `docs/fuzz-harness.md`'s table, not
  repeated here.
- **When a target finds a bug, commit the input as a regression fixture, not
  a corpus entry** — corpora and artifacts are gitignored by design
  (`fuzz/.gitignore`). Follow `crates/lodestone-fuzz/tests/fuzz_regressions.rs`'s
  existing shape: a `#`-commented hex fixture plus an assertion pinning what
  the decoder does with it, not merely that it survives.
- **`fuzz/`'s own `Cargo.lock` is real and committed** (`cargo-fuzz`'s default),
  separate from the root workspace's — a dependency bump there needs its own
  `cargo update` run from inside `fuzz/`.
- **Extend `differential.rs`'s action alphabet** by adding an `Action` variant
  and handling it in every `WorldOracle` impl (today, just `RconOracle`) —
  `Action::RunCommand` is the escape hatch for anything not worth its own
  variant yet.
- **Extend `WorldOracle` to a non-RCON oracle** (an in-process `ChunkSource`,
  say) by implementing the trait directly; `block_state`'s `candidates`
  parameter is a *permission* to probe, not a requirement — an oracle with a
  real read primitive can ignore it and return the true state.

## Configuration

- **`-max_total_time=N`** (seconds) and **`-rss_limit_mb=N`** — pass both on
  every `just fuzz-run` invocation on a shared machine; see the Track A section
  above.
- **`differential::TICK_MILLIS`** — the real-time sleep `RconOracle::advance_tick`
  uses per tick, currently `50` (vanilla's own tick length).
  **`rcon-oracle` Cargo feature** on `lodestone-fuzz` — gates the
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
- **`proptest`** — only via `crates/lodestone-fuzz`'s existing dependency;
  Track A does not use it (see `docs/fuzz-harness.md` for why that harness uses
  `proptest` rather than `cargo-fuzz`, a decision this doc's own existence
  makes narrower than it used to be: both now coexist deliberately).
