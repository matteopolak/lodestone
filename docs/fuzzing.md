# Fuzzing

## What it is

Two independent fuzzing tracks. **Track A**
(`fuzz/`, a `cargo-fuzz`/libFuzzer workspace) is coverage-guided, in-process
fuzzing over pure parsing functions — packet framing and decoders, NBT, loot-table JSON,
block-state strings, the density compiler, the unihex font parser, region-file
deserialization, and chat-text JSON/NBT. It finds panics, hangs and decode
crashes on malformed input; it has no oracle for correctness. **Track B**
(`crates/lodestone-fuzz/src/differential.rs`) is a tick-aligned
differential-fuzzing *harness* against a real vanilla oracle, for the class of
bug Track A structurally cannot see — wrong behaviour that never panics (the
motivating example: breaking a waterlogged block used to destroy the water
too, which is not the real mechanic). Track B is a narrow slice rather than a
finished fuzzer: fixed scripts run end to end against a live vanilla server,
and bounded generated fluid and redstone scripts now run against that oracle
with per-case reset, timing-boundary checks, semantic shrinking, and replay.
The generator's general properties are also proven against fresh in-memory
oracles. Its own section below says exactly what is and is not there.

This complements, not replaces, `crates/lodestone-fuzz`'s existing
`proptest`-based harness — that harness runs under a
plain `cargo test`, with no sanitizer setup or corpus directory; Track A is coverage-guided
and can run far longer and explore far deeper, at the cost of needing the
nightly toolchain and a corpus/artifact directory. Both check "never panics on
arbitrary bytes" for overlapping but not identical surfaces; neither replaces
the other.

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

Fourteen targets, each a `[[bin]]` in `fuzz/Cargo.toml` under
`fuzz/fuzz_targets/`:

| target | what it fuzzes | crate |
|---|---|---|
| `v26_2_clientbound_decode` | `V770Adapter::handle_packet`, state+id from the input's own first bytes | `lodestone-v26-2` |
| `v26_2_clientbound_decode_by_id` | as above, but selects a *real* declared packet id by index rather than an arbitrary `i32` — reaches real decode bodies far more often | `lodestone-v26-2` |
| `v26_2_serverbound_decode` | `V770ServerProtocol::decode` — the attack surface a connecting *client* presents to our integrated server | `lodestone-v26-2`, `lodestone-server` |
| `v26_2_serverbound_decode_by_id` | the same serverbound decoder, selecting a declared per-state packet id so mutations reach real decode arms | `lodestone-v26-2`, `lodestone-server` |
| `packet_frame_codec` | `Codec::feed` plus `next_packet`, one byte fragment at a time, across cleartext and compressed frames | `lodestone-net` |
| `nbt_decode` | `lodestone_core::read_named_nbt` | `lodestone-core` |
| `loot_table_json` | `lodestone_server::loot::LootTable::from_json` | `lodestone-server` |
| `block_state_string` | `lodestone_data::block_states::state_id` (`minecraft:oak_log[axis=y]` grammar) | `lodestone-data` |
| `density_function_json` | `lodestone_worldgen_core::density::Builder::build` — malformed JSON shapes must return a typed error | `lodestone-worldgen-core` |
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
| `v26_2_clientbound_decode`, `..._by_id`, `v26_2_serverbound_decode`, `..._serverbound_decode_by_id` | 37 each from the current capture set | packet payloads **captured off the wire** from a real 26.2 server (`crates/versions/26.2/tests/fixtures/*.hex`), framed with a state selector and either the captured direction's generated id or a valid index into the selected direction's generated table; the arbitrary clientbound target also retains three targeted nesting probes |
| `packet_frame_codec` | 8 | four captured 26.2 packet payloads, with their packet ids taken from the generator report and cleartext/zlib stream frames written by Python's independent VarInt/zlib implementation; this checks the receive codec against bytes it did not encode |
| `nbt_decode` | 5 | a **world save a real vanilla server wrote**: `level.dat`, one player `.dat`, and three chunk payloads decompressed out of a region file |
| `anvil_region_parse` | 1 | one real region file trimmed to its smallest single chunk — header encoding, compression byte, deflate stream and chunk NBT are all verbatim vanilla bytes; only the other 1023 header slots are zeroed |
| `loot_table_json` | 12 | the **vanilla data pack**'s own loot tables |
| `density_function_json` | 6 | the vanilla data pack's own density functions |
| `text_chat_json` | 25 | real text components out of vanilla advancements (plain `translate`), chat types (`with` parameter lists) and dialogs (click/hover events, nested `extra`) |
| `text_chat_nbt` | 26 | the same 25 components, NBT-encoded by a **small independent encoder written in that Python script** rather than by ours, plus a real `level.dat` with its root name stripped to network-NBT framing |
| `block_state_string` | 32 | strings spelled from the generator's own `generated/reports/blocks.json` — property names, value spellings and which properties a block even has all come from there, so a seed cannot inherit a misunderstanding from our parser |
| `unihex_font` | none | **honestly none available**: no GNU Unifont `.hex` file exists anywhere under `.cache/` (26.2 ships the unifont as a separately-downloaded pack), and there is nothing else to take one from offline. This target starts from an empty corpus. |
| `resource_pack_zip_source` | 1 | a zip built by this script from vanilla's own `assets/minecraft/lang/en_us.json` and `deprecated.json` (each truncated to `MAX_SEED_BYTES`) — the *contents* are real vanilla bytes, though (unlike the packet captures above) no vanilla `client.jar` is checked out under `.cache/` for the *container* itself to come from, so `zipfile` builds it with fixed archive timestamps |

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
thirteen targets, then **at most 30 seconds or 10,000 executions per gated
target**, whichever limit libFuzzer reaches first, from the committed seeds.
`timeout-minutes: 90`,
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
ungated. An empty target directory or exclusions covering every target also
fail before building, because neither executes the property being gated.

The runner rejects zero, negative, malformed and overflowing time or execution
budgets before invoking Cargo: zero disables libFuzzer's time limit. Run
`python3 fuzz/test_smoke.py` to exercise these controls, verify the ordered
corpus arguments, and prove target failures propagate while later targets
still run. This contract test substitutes a recording Cargo executable; it
checks orchestration and does not claim decoder or sanitizer coverage.

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

**Status: two fixed live comparisons, plus generated and shrunk fluid cases on
both hermetic and live oracle paths, and a fixed-seed scheduled-tick queue
model check.**
What exists:

- `tests/support/tick_corpus.rs` and
  `scripts/live-oracles/capture-differential-ticks.py` — a deliberately small
  companion lane for a one-to-sixteen-tick Java-oracle trace. The recorder
  takes a SetBlock-only JSON plan, force-loads only its action and probe
  chunks for the capture, waits for the server's own game-time value,
  and writes the observed candidate-alphabet state for every probe after every
  tick. It rejects a counter jump rather than silently labelling two elapsed
  ticks as one. The Rust consumer accepts only a versioned artifact marked
  `real-java-rcon`, requires exact contiguous observations and an exact-one
  game-time increment, and compares an in-process production oracle to those
  externally authored expected values. Its hermetic detector control corrupts
  the Rust-side read at a known second tick and requires the corpus runner to
  return that tick, position, expected state and actual state. The reviewed
  `tests/fixtures/tick_corpus_26_2_stone.json` capture is the smallest live
  corpus: one force-loaded stone placement observed after one elapsed tick and
  replayed through `fluid::FluidModelOracle`. It proves the capture-to-model
  route without pretending one static cell exercises delayed scheduling.
  Re-capture it with `tick-corpus-example.json`, inspect the JSON and its
  redacted command provenance, then replace the reviewed artifact. Longer
  scenarios remain bounded by the recorder's strict counter-jump rejection;
  an RCON round trip that straddles a tick is a rejected capture, never a
  silently relabelled observation.

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
- `tests/differential_client_state.rs` also includes a hermetic
  `IntegratedServer`-backed `WorldOracle`. Its fixed two-tick proof applies
  edits to the retained `ChunkSource`, reads the same source directly, and
  waits for `server_tick_count()` before each comparison. A deliberate source
  read fault is caught at the first differing tick, so the proof exercises the
  real server tick loop without routing reads through command parsing.
- `tests/scheduled_tick_queue_model.rs` is a bounded, fixed-ChaCha-seed model
  check over the production `ScheduledTickHandle` used by the world tick loop
  and by the save path's column snapshot. Each generated script starts with a
  cross-chunk, same-tick priority conflict, then adds at most 32 shrinkable
  schedule, drain, and cancellation operations over both block and fluid
  queues. An independent vector model predicts acceptance, due ordering,
  hard drain caps, `(position, kind)` deduplication, cancellation, queue
  membership, and persisted insertion order. The wrong-priority control is
  required to fail and shrink, so a permanently agreeing harness cannot pass.
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

- `tests/differential_container_click_model.rs` — a bounded, hermetic
  server-side transaction-state comparison. It drives the production
  `lodestone_server::container_click::do_click` consumer over a three-slot
  generic container and compares every pickup, quick-move, swap, throw,
  pickup-all and drag packet with an independent item/count/cursor model. A
  fixed prefix exercises merge-before-place ordering and multi-slot drag
  distribution; a fixed ChaCha seed adds at most 32 shrinkable clicks per case
  across 160 cases. The detector control removes quick-move's merge pass and
  must fail on the fixed partial-stack witness, proving that the comparison
  reads real slot state rather than accepting an inert model.

- `tests/differential_generated_text_nbt.rs` — a bounded fixed-seed model
  check for the modern NBT text fold. A grammar independently builds scalar,
  list, and compound `text`/`extra` values, and a separate plain-text fold is
  compared with `lodestone_model::Text::from_nbt`. Those values enter the live
  26.2 chat, inventory, and scoreboard adapters, so the property checks a
  shared production consumer rather than an isolated format helper. A reader
  that deliberately drops an `extra` child must disagree, proving the detector
  observes the folded output instead of accepting every generated input.

- `tests/resource_key_model.rs` — a bounded fixed-ChaCha-seed grammar check
  over `lodestone_model::ResourceKey`, the shared identifier parser packet
  adapters use before retaining registry, sound, channel, and command-parser
  keys. The model independently classifies empty components, extra separators,
  first invalid namespace/path characters, and implicit namespaces. A wrong
  default namespace is required to fail and shrink to a one-character bare
  key, proving the detector observes the production result rather than merely
  accepting generated input.

- `tests/command_tree_redirect_model.rs` — a bounded fixed-ChaCha-seed model
  check over `lodestone_model::command_tree::CommandTree::effective_children`,
  the redirect expansion consumed by client chat completion. An independent
  flat-graph walk covers child order, redirect chains, and cycles without
  constructing a production tree for expected output. Three literal graphs
  pin the no-redirect, chain, and cycle cases; generated graphs shrink their
  child lists and redirects. A redirect-omission model is required to fail and
  shrink with a redirected child still present, proving the consumer sees the
  same-token redirect edge.

- `tests/legacy_status_model.rs` — a bounded fixed-ChaCha-seed model check over
  `lodestone_net::parse_legacy_status`, the legacy server-list ping consumer.
  An independent UTF-16BE framer and field model cover both supported layouts,
  trimmed numeric fields, optional protocol metadata, and non-BMP text. The
  control drops a modern protocol value and must fail after shrinking, proving
  the comparison observes parsed status fields rather than merely accepting
  generated packets.

- `tests/differential_client_state.rs` — a bounded hermetic client-state
  comparison. It replays fixed block, entity and inventory packet scripts
  through the public `ClientBuilder` over `lodestone_net::memory_pair`, with a
  scripted adapter writing block results through `WorldSink` and entity/menu
  events through the normal event route. Independent maps are compared with
  the client-owned `ClientHandle::block_at`, `ClientHandle::entities` and
  `ClientHandle::player_menu` values after every tick; one-tick state faults
  prove first-divergence reporting for all three dimensions. The same fixture
  also runs deterministic generated campaigns: 24 block scripts with 12
  packets each, 8 entity scripts with 8 packets each, and 8 inventory scripts
  with 8 packets each, plus 8 generic-container scripts with 6 packets each.
  That is 464 packet replays against independent maps, with a
  first-divergence control for every dimension.
- `tests/differential_captured_client_state.rs` — a fixed captured-packet
  lane through the real 26.2 adapter and the public client read model. It
  replays server-authored pickaxe and potion slot packets from checked-in
  fixtures, compares the resulting item identity and count to each fixture's
  external annotation, and includes a wrong-item detector control.
- `tests/differential_captured_block_updates.rs` — a fixed captured terrain
  sequence through the real 26.2 adapter and the public client world read
  model. It replays an externally captured one-cell update followed by a
  two-cell section update into an all-air resident chunk, compares every
  commanded position after its packet, and proves that omitting the bulk
  packet leaves both cells at the all-air baseline.
- `tests/differential_captured_chunk_lifecycle.rs` — a fixed externally
  captured full chunk load, block update and chunk unload through the real
  26.2 adapter and public client world read model. It verifies the loaded
  chunk and command-authored block state, then proves the unload makes both
  unavailable; an omitted-unload control remains loaded with the update.
- `tests/differential_generated_gravity.rs` — a bounded generated gravity
  action domain through `IntegratedServer`, covering sand, red sand and gravel.
  It injects the public `BlockTickFeed` schedule, reads the server's retained
  `ChunkSource`, and compares every tick with an independent model of the
  two-tick delay, gravity, drag and landing on a fixed floor; a wrong-read
  control proves the detector reports the first divergence.
- `tests/differential_generated_waterlogging.rs` — a bounded source-water
  action sequence through `IntegratedServer` that fills a dry slab between two
  source blocks. It injects the public `BlockTickFeed` schedule, reads the
  retained `ChunkSource`, and compares the slab and surrounding trench against
  an independent map; a wrong-read control proves first-divergence reporting.
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
  of in-memory oracles, while the evaluator form lets a live test perform and
  validate its own reset before each candidate. An oracle failure aborts the
  search, and a shrink is accepted only when it preserves the original
  `(tick, position, left state, right state)` divergence class. Keeping the
  first-divergence tick stable matters more than compacting idle time: a
  timing failure moved to another tick is a different failure, even when its
  eventual states are identical.

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
- `differential_live_generated_fluid.rs` takes the same generator and shrinker
  through the real RCON oracle. It generates one to three edits of a channel's
  source cell from the caller-owned air/water alphabet, then compares the
  source and three downstream cells after every tick. The in-process side is
  `FluidModelOracle`, so generated candidates still reach the production fluid
  scheduling and tick entry points rather than a test copy of the mechanic.

  Every candidate reuses one dedicated coordinate lane only after an explicit
  reset: force-load, clear the whole rig to air, re-anchor the game-time
  counter, let six ticks elapse while every fluid position is empty, rebuild
  the channel, and positively verify all four watched cells are air plus every
  corresponding floor, roof and side-wall cell is stone. Each query offers the
  intended state and a discriminating alternative, so an unrecognised response
  cannot look like the desired baseline. Six is one more than the overworld
  water scheduler's five-tick delay, so a pending water tick from a previous
  candidate fires against air and cannot reach the rebuilt rig. Cleanup always
  attempts clear, counter reset, drain and force-load removal in that order;
  failure in an earlier step is recorded but cannot skip the removal attempt.
  Setup, teardown, transport and tick-wait failures are returned as oracle
  failures; a tick timeout is typed separately from other oracle failures and
  neither is serialized as a gameplay divergence. A candidate that loses a
  tick boundary is retried from a fresh reset up to three total attempts;
  other oracle failures are not retried, and a third timing failure aborts the
  search. Each RCON connect has a five-second deadline, and each complete frame
  read or write shares one five-second wall-clock deadline across every partial
  socket operation. The remaining budget is installed as the socket timeout
  before each operation, so a peer cannot extend the deadline by drip-feeding
  bytes. Platform timeout errors are normalised to the typed timeout outcome.
  This absorbs occasional boundary jitter without accepting a run whose tick
  labels cannot be trusted or allowing a slow or silent endpoint to stall the
  retry loop forever.

  The ignored live test runs a deliberately faulty read wrapper first and
  requires generation to find, shrink, serialize and replay its divergence.
  That is the negative control for an accidentally always-agreeing generator.
  `historical_reversion_is_found_shrunk_and_replayed_against_live_vanilla` is
  the production-regression control: it is ignored in the normal tree and is
  run only by `just fuzz-historical-fluid-reversion`, which checks out a
  detached worktree and applies the reviewed delay-one fluid-seed mutation
  there. Before that mutation, the wrapper runs the matching fixed-tree
  control over the exact same bounded stream and requires `NoDivergence`.
  The control requires the real first-tick downstream mismatch, requires the
  semantic shrinker to preserve its complete divergence signature, and then
  replays the serialized minimal case from a fresh live lane. The wrapper
  removes the worktree on every exit path, so it neither edits nor relies on
  uncommitted changes in the shared checkout.
  A real divergence is likewise replayed immediately after decoding its JSON
  before the test reports the artifact. Set `LODESTONE_DIFFERENTIAL_REPLAY` to
  that JSON file on a later run to bypass generation and execute the explicit
  minimized script from a fresh reset. Replay files are untrusted input: before
  opening RCON, the live entry point requires the generated-fluid scenario and
  settle horizon, the exact four-cell probe lane, `SetBlock` actions only,
  positions and states from the generator's domain, and a recorded divergence
  inside the probe alphabet and execution horizon. A replay cannot use the
  general `RunCommand` action to escape the reset lane.
- `differential_live_generated_redstone.rs` uses the same generator over the
  existing two-seam repeater contraption. Its finite domain toggles only the
  source between air and a redstone block, with one to three actions and gaps
  no larger than three ticks. It compares the contraption's three existing
  dust-power alphabets, so an unexpected power is a mismatch rather than a
  loosely matched success. Generated candidates and the independent property
  probe use separate dedicated lanes; each candidate force-loads and rebuilds
  its generated lane, then requires all three repeaters to remain unpowered for
  twelve consecutive observed ticks before the game-time counter is anchored.
  A missed RCON tick deadline is retryable only after a full fresh reset, and
  no accepted comparison has a missed deadline. The ignored target proves the
  powered dust probe accepts `power=15` and rejects `power=0`, then requires a
  faulty model read to be generated, semantically shrunk without changing its
  first-divergence signature, JSON-decoded, and replayed from another reset.
  The fixed stream must return `NoDivergence`. Replay JSON is rejected before
  RCON setup unless its scenario, source-only action domain, probe region,
  state alphabets, and execution horizon exactly match this lane.
- **Four live integration targets, plus two historical controls, against a
  real vanilla 26.2 server**, all `#[ignore]`d.
  `crates/lodestone-fuzz/tests/differential_live_fluid_spread.rs` pairs
  `FluidModelOracle` with `RconOracle` over a water front spreading down a
  closed stone channel and requires agreement throughout the complete spread.
  `differential_live_redstone_contraption.rs` pairs
  `RedstoneModelOracle` with `RconOracle` over a repeater chain crossing two
  chunk seams, out to fourteen ticks, and agrees. The generated fluid and
  redstone runs are the reset-and-replay paths described immediately above.
  See the two fixed "live finding" sections below.

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

### Captured entity lifecycle

`differential_captured_entity.rs` replays three unmodified server-authored
payloads from `tests/fixtures/entity_lifecycle_26_2.json`: an armor stand's
spawn, absolute position update, and removal. The expected UUID and fractional
coordinates come from the capture commands, independently of the decoder.
Each packet goes through the real adapter, driver and ECS before the test
reads `ClientHandle::entity`. An omitted-movement control must retain the
spawn position and reject the commanded endpoint; removal must still erase
the entity. No synthesized spawn prefix participates in this lane.

To acquire a fresh sequence, start the headless creative oracle, then run:

```bash
CARGO_TARGET_DIR=/private/tmp/lodestone-batch-549 cargo test -p lodestone-fuzz \
  --no-default-features --features v26-2,rcon-oracle \
  --test differential_captured_entity acquire_entity_lifecycle_from_external_server \
  -j 2 --no-fail-fast -- --ignored --nocapture
```

The capture test prints `ENTITY_LIFECYCLE_CAPTURE=` followed by reviewable
JSON after checking both live-acquired replay paths. Replace the fixture's
protocol/id/packet fields with that output while keeping its provenance
description. Session entity ids may differ across captures; do not normalize
the bytes with our encoder. The workflow uses only ports `25570`/`25571`, the
test's generated player name and the `lodestone_fuzz_lifecycle` entity tag;
do not run two acquisitions concurrently. It bounds connection, join and
capture waits to 5, 30 and 20 seconds respectively, with the existing bounded
RCON transport. It removes the tagged stand after capture or a capture timeout.
The ordinary replay needs only `v26-2`, no oracle or network, and bounds every
expected event wait to five seconds.

### Captured block-update sequence

`differential_captured_block_updates.rs` replays two unmodified payloads from
`tests/fixtures/block_updates_26_2.json`: a one-cell update at `(8, 100, 8)`
and a section update covering `(9, 100, 8)` and `(10, 100, 8)`. The external
capture commands name gold and diamond blocks, so those state names are the
expected values; the replay resolves them through the independently generated
block-state report, not a local packet encoder. Before replay the normal
client owns one all-air chunk at `(0, 0)`. This makes the read model's
loaded-chunk precondition explicit while keeping the subject narrow: each
captured packet must cross the production adapter and driver before
`ClientHandle::block_at` sees its write.

The ordinary test compares every commanded cell immediately after its packet.
Its control replays only the first packet and proves each cell owned by the
omitted section update remains air and differs from the commanded diamond
state. That is a correctness detector, not a malformed-input or robustness
test.

To acquire a fresh sequence, start the local headless creative oracle, then
run:

```bash
CARGO_TARGET_DIR=/private/tmp/lodestone-batch-549 cargo test -p lodestone-fuzz \
  --no-default-features --features v26-2,rcon-oracle \
  --test differential_captured_block_updates acquire_block_updates_from_external_server \
  -j 2 --no-fail-fast -- --ignored --nocapture
```

The acquisition uses only the local game and RCON ports, loads the capture
client's chunk, captures the two server-sent payloads after the two annotated
commands, then restores the three cells and releases the forced chunk. It
prints `BLOCK_UPDATE_CAPTURE=` followed by reviewable JSON. Replace the
fixture with that output without re-encoding or normalizing packet bytes.

This is deliberately **not** a captured full-column lane: the initial
all-air resident chunk is a small test setup, while the state-changing packets
and expectations are external. It therefore proves update routing and public
world visibility, but does not yet differentially verify chunk-packet palette,
light, biome, heightmap or block-entity contents.

### Captured chunk lifecycle

`differential_captured_chunk_lifecycle.rs` replays three unmodified payloads
from `tests/fixtures/chunk_lifecycle_26_2.json`: the full chunk-and-light
packet for `(0, 0)`, a one-cell update at `(8, 100, 8)`, and the later unload
for `(0, 0)`. The packet bytes came from one bounded local creative-oracle
session. The source commands force-loaded the initial chunk, set the named
position to a gold block, then moved that capture client beyond its view range.
The gold block name is the independent expected value; the replay resolves its
state ID through the generated block-state report rather than encoding a local
packet.

The fixture stores each raw payload as zlib-compressed base64 to keep the
checked-in JSON reviewable. The test restores those exact bytes before passing
them unchanged to the production adapter and `ClientBuilder` driver. It first
observes `ClientEvent::ChunkLoaded` and `ClientHandle::is_chunk_loaded`, then
the block-update event and `ClientHandle::block_at`, and finally the matching
unload event with both public reads absent. Its required control omits only the
unload and observes that the captured chunk and gold-block update remain
visible. This is a deterministic correctness detector, not a malformed-input
or general robustness test.

To refresh the bounded capture, start the local headless creative oracle, then
run:

```bash
LODESTONE_CHUNK_LIFECYCLE_CAPTURE_OUT=/absolute/path/to/lodestone/crates/lodestone-fuzz/tests/fixtures/chunk_lifecycle_26_2.json \\
CARGO_TARGET_DIR=/private/tmp/lodestone-batch-549 cargo test -p lodestone-fuzz \\
  --no-default-features --features v26-2,rcon-oracle \\
  --test differential_captured_chunk_lifecycle acquire_chunk_lifecycle_from_external_server \\
  -j 2 --no-fail-fast -- --ignored --nocapture
```

The output path is deliberately explicit because the test binary's working
directory is not the repository root. Without it, acquisition prints the JSON
instead. The acquisition uses only the local game and RCON ports, removes the
gold block, and releases the forced chunk after capture.

The lane proves folding of this particular external chunk lifecycle through the
public state model. It does not yet compare the chunk packet's initial palette,
light, biome, heightmap or block-entity contents against an independently read
world snapshot, nor does it cover a broader range of chunk positions or
transitions.

### Captured initial chunk content

`differential_captured_chunk_content.rs` replays one unmodified
`level_chunk_with_light` payload from a bounded protocol-776 creative-oracle
session. Before the capture client joins, RCON force-loads chunk `(0, 0)` and
places two differently oriented logs and glowstone at `y = 300`. The fixture
records those source-command state names and their three `MOTION_BLOCKING` tops;
they are external expectations, not bytes or decoded values produced by our
encoder.

The ordinary replay sends the exact fixture payload through `V770Adapter` and
the normal `ClientBuilder` driver. It then reads each named state through both
`ClientHandle::block_at` and its public palette-backed `section_at` snapshot,
and reads the public `ClientHandle::column_heightmap` value after translating
the stored height above `min_y` back to an absolute top. The corruption control
changes one expected state annotation in memory and requires the public block
comparison to report its named mismatch; a comparison that never reads the
decoded chunk state cannot pass that control.

To refresh the fixture, start the local headless creative oracle and run:

```bash
LODESTONE_CHUNK_CONTENT_CAPTURE_OUT=/absolute/path/to/lodestone/crates/lodestone-fuzz/tests/fixtures/chunk_content_26_2.json \\
CARGO_TARGET_DIR=/private/tmp/lodestone-batch-549 cargo test -p lodestone-fuzz \\
  --no-default-features --features v26-2,rcon-oracle \\
  --test differential_captured_chunk_content acquire_chunk_content_from_external_server \\
  -j 2 --no-fail-fast -- --ignored --nocapture
```

The capture path is explicit because test binaries do not run from the
repository root. Acquisition clears its three blocks and releases the forced
chunk after it records the packet. This lane covers palette-backed block states
and the motion-blocking heightmap from initial chunk content. The public client
surface still has no narrow biome or initial block-entity query, so those
remain coverage gaps rather than reasons to read protocol-private state in the
test.

### What Track B still does not do
- **The client-state packet corpus is still small.** The captured lane covers
  three inventory slot payloads, one block-update sequence, one full chunk
  load/update/unload lifecycle, and palette-backed block states plus one
  heightmap from an initial chunk, but no broader inventory or chunk packet
  sequence. The captured armor-stand lane covers spawn, movement and removal,
  but not metadata. The two committed item-entity
  metadata fixtures remain unpaired with their own session's spawn, so they
  cannot independently drive an item-entity lifecycle. The generated
  block/entity/inventory/container campaign remains
  hermetic and bounded; it compares client state rather than rendered frames,
  because renderer differences are not themselves client-state bugs.
  The inventory component lane replays explicit-tool, plain-tool, potion,
  and plain-tool replacements into the same public menu slot. Independent
  expectations from capture annotations cover tool rules and defaults, potion
  holder/color/effects/name, and clearing the old component values. A stale-tool
  and stale-potion control must each produce a named field mismatch. The check
  reads the public game stack through its shared-model conversion; it does
  not stop at the decoder's emitted event.
- **Generated live cases cover fluids and redstone.** Both have bounded
  generated action domains. Falling blocks and source-water waterlogging each
  additionally have a bounded hermetic `IntegratedServer` action proof. The
  container click lane is hermetic and drives the production transaction
  consumer directly; no generated live piston or container action domain
  exists. Every generated live comparison still covers only block states over
  a caller-named region. The client read-model has no scheduled-tick queue to
  compare: inbound ticking metadata folds into session server information,
  while world reactions belong to the server-side scheduler rather than the
  client state exposed by `ClientHandle`. The server-side queue itself is
  covered by the separate fixed-seed model lane above; this does not claim a
  live client packet oracle for scheduled ticks.
- **Entity effect simulation has a bounded hermetic model lane.**
  `tests/differential_generated_effects.rs` generates at most 24 operations
  after a fixed stronger/shorter stacking prefix, then compares the production
  `lodestone_server::mob_effects::ActiveEffects` surface after every apply,
  tick, remove, and clear against an independent hidden-chain model. Durations
  are finite or the explicit infinite sentinel, health and effect ids come
  from small caller-owned alphabets, and ChaCha seed/case/shrink budgets are
  fixed. A detector control that stops ticking hidden durations must fail and
  shrink, proving the comparison can observe the resurfaced effect's shortened
  remaining duration. This is hermetic and does not claim a live entity-packet
  or JVM differential oracle.
- **The live comparison still uses the fluid model for its our-side world.**
  `FluidModelOracle` drives `lodestone_server::fluid`'s production
  scheduled-tick entry point over a sparse world. The hermetic
  `IntegratedServer` proofs read a retained `ChunkSource` directly: one proves
  tick alignment for edits, one exercises falling-block scheduling and landing,
  and one exercises two-source waterlogging of a slab. They do not replace the
  live RCON path or cover the broader redstone, piston or container domains.
  A direct source read also preserves full canonical state properties that an
  `execute if block` command probe intentionally reduces.

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
- **Use `search_and_shrink_with` for a live generator.** Its evaluator owns
  reset, baseline validation, timing alignment and cleanup for one candidate,
  and returns their failures as `DifferentialOutcome::OracleFailed`. The
  factory-based `search_and_shrink` remains the smaller form for fresh
  in-memory oracles. A generated live replay uses
  `ReplayCase::replay_generated_with`, which validates the scenario, probe
  lane, action kind, positions and state alphabet before invoking the same
  resettable evaluator used by generated and shrink candidates.
  Search rejects a zero-case budget before evaluating any candidate. Search
  and replay both require a nonempty probe region and at least one candidate
  state per probe, so missing coverage cannot produce a successful comparison.
  These structural guards do not replace the scenario's independent read
  discriminator: an oracle that always returns `None` still needs a control
  proving that it can observe a known state.
- **Run the historical fluid-reversion control** with a separately started
  `./scripts/live-oracles/creative.sh`, then `just
  fuzz-historical-fluid-reversion`. The wrapper starts from committed `HEAD`
  in a uniquely named detached worktree, applies the exact former delay-one
  seven-cell seeding behavior from commit `40d39c13` without committing it,
  confirms the old seed is present, and executes the fixed-tree and historical
  ignored generated-live tests in that order. It exits successfully only after
  the fixed tree has no divergence and the mutated tree's historical test
  finds the expected live disagreement, shrinks it, and replays it; it
  removes that worktree even when the test fails. Do not run `git revert` in
  the shared checkout: its uncommitted edits belong to other work.

## Configuration

- **`-max_total_time=N`** (seconds) and **`-rss_limit_mb=N`** — pass both on
  every `just fuzz-run` invocation on a shared machine; see the Track A section
  above.
- **`just fuzz-smoke [seconds]`** — seconds per target, default `30`; CI
  passes `30` explicitly so the workflow states its own budget. The argument
  must be a positive decimal integer no larger than `2147483647`.
- **`FUZZ_SMOKE_RUNS`** — execution limit per smoke target, default `10000`,
  passed as libFuzzer's `-runs` alongside the time limit. It has the same
  positive-integer validation. For a short local run use
  `CARGO_BUILD_JOBS=2 FUZZ_SMOKE_RUNS=100 just fuzz-smoke 2`.
  Limits apply to target execution, not compilation; corpus initialization
  and an in-progress input may finish beyond a time or execution budget.
- **`LODESTONE_DIFFERENTIAL_RCON`** (`IP:port`) — the live endpoint
  `differential_live_fluid_spread.rs` drives, defaulting to the flat/creative
  oracle's `127.0.0.1:25571`. Any oracle in `scripts/live-oracles/` works: the
  rig is `/fill`ed from scratch, so the world's own terrain never
  participates. The endpoint requires a numeric IPv4 or bracketed IPv6 address;
  hostnames are rejected before connection work so DNS cannot evade the hard
  connection deadline. Whichever oracle you use needs
  `pause-when-empty-seconds=0` — with nobody logged in, a paused world runs no
  scheduled block ticks and the comparison reports agreement on a frozen
  world.
- **`LODESTONE_DIFFERENTIAL_REPLAY`** (path) — when running either generated
  live differential target, bypasses generation and replays the
  versioned JSON's explicit minimized script against a freshly reset live
  lane. The file must satisfy the live scenario's exact generated-domain and
  probe-lane policy before any oracle connection is attempted. The test then
  requires the recorded tick, position and states to recur.
- **`differential::TICK_MILLIS`** — the poll cadence used while
  `RconOracle::advance_tick` waits for the server's own game-time counter,
  currently `50` ms. The counter, not elapsed wall clock, decides whether the
  next nominal tick exists.
- **`differential::RCON_IO_TIMEOUT`** — the hard wall-clock bound applied to
  each RCON connection attempt and each complete frame read or write, currently
  five seconds. Partial socket operations share the frame's original deadline;
  `WouldBlock` from a platform socket timeout is normalised to `TimedOut` so the
  generated live runner's bounded retry policy sees one portable kind.
- **`SearchBudget`** in the generated-script test support — `seed`, `cases`,
  and `shrink_attempts` are all explicit integers. Its proptest configuration
  fixes the RNG to ChaCha, disables failure-persistence-by-seed, and sets
  `max_shrink_time` to zero; the versioned explicit JSON case is the replay
  artifact. A shrink preserves the complete first-divergence signature,
  including tick. `MAX_ORACLE_TICKS` caps each generated or replayed candidate
  at 4,096 oracle ticks after checked horizon arithmetic.
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
- **`python3`** — the seed generator and `fuzz/test_smoke.py`'s isolated
  orchestration controls. Nothing in `just health`, `just fuzz-smoke` or CI
  runs the seed generator; the seeds it produces are committed.
- **A live vanilla 26.2 oracle under Apple `container`** — Track B's
  `#[ignore]`d live tests only. See `docs/oracles-and-benchmarks.md` and
  `scripts/live-oracles/`.
- **`proptest`** — a dev dependency used by the existing decoder properties
  and by Track B's hermetic structured generator/shrinker. Track A does not use
  it. `serde` and `serde_json` are also dev-only here and encode the explicit
  replay case; they are not pulled into the differential library or cargo-fuzz
  targets.
