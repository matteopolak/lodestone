# Fuzz/property-testing harness

## What it is

`crates/lodestone-fuzz` is a property-based fuzzing harness (issue #282) for lodestone's
wire decoders — the first one in the repo. It exists to check properties that need no
expected value at all: a decoder must never panic on arbitrary bytes, a truncated prefix
of a valid packet must error cleanly, and a length prefix must not force an allocation
disconnected from the bytes actually available. That last property is not hypothetical —
this harness found it already violated on its first run (see "Bug found" below).

This complements, not replaces, `decode(encode(x)) == x` round trips. `CLAUDE.md`'s own
record is why: hermetic chunk fixtures generated with our own encoder passed for months,
then a live server produced 49 × "unexpected end of input", and a shipped sheep-colour bug
(`Sheep.DATA_WOOL_ID` off by one) was invisible precisely because the tests encoded with
the same constants they decoded with. A round trip can only ever be as strict as the
shared misunderstanding between its two halves. Fuzzing doesn't have that blind spot —
"does this crash" is true or false independent of whether we understood the protocol
correctly.

## How it works

Uses `proptest`, not `cargo-fuzz`/libFuzzer: the toolchain here is stable 1.95.0
(`rust-toolchain.toml`), and `cargo-fuzz` needs nightly. `proptest` is a normal
dev-dependency, so every property test runs under a plain `cargo test`, no separate fuzz
job, no nightly toolchain, no corpus directory to manage. Bounded run time is a design
constraint throughout — see "Cost, in numbers" below.

Everything routes through two panic-catching entry points, both in
`crates/lodestone-fuzz/src/lib.rs`:

- `catch(f)` — a thin `std::panic::catch_unwind` wrapper that turns a panic into a
  readable `Err(String)` instead of `Err(Box<dyn Any>)`. Every property test calls a
  decoder through this, never directly; a bare call would abort the whole `cargo test`
  process on the first panic instead of reporting a (proptest-shrunk) failing case.
- `decode_clientbound(family, state, packet_id, payload)` — builds a fresh adapter for one
  of the four client families and calls `VersionAdapter::handle_packet` with a `NullSink`
  that discards every world write. Fresh per call because `V770Adapter` carries
  per-connection interior-mutable state (chunk shape, batch tracking, movement send state)
  that must not bleed between unrelated fuzz cases.

Each test file targets a different property or a different decoder direction:

| file | property | decoder(s) |
|---|---|---|
| `tests/harness_control.rs` | proves `catch` actually detects a panic | none (synthetic) |
| `tests/no_panic_arbitrary_bytes.rs` | no panic on arbitrary bytes | `handle_packet`, all 4 client families, clientbound |
| `tests/no_panic_v770_serverbound.rs` | no panic on arbitrary bytes | `V770ServerProtocol::decode`, serverbound |
| `tests/no_panic_net_codec.rs` | no panic, and terminates, on arbitrary bytes | `lodestone_net::Codec` (frame length + compression, ahead of every `VersionAdapter`) |
| `tests/truncation_is_clean.rs` | truncated valid packet errors cleanly | `handle_packet`, corpus-driven |
| `tests/length_prefix_allocation.rs` | length prefix must not over-allocate | `GameLogin::decode`, `RegistryData::decode` (proves the fix, issue #417) |

### The control (`tests/harness_control.rs`)

Per `CLAUDE.md`'s evidence rule, "assertions of an absence need a control proving the
detector works" — a `catch(f)` wrapper that always reports "no panic" regardless of `f`
would make every "never panics" assertion in this crate vacuous. `harness_control.rs`
decodes a minimal function with a textbook wire-decoding bug (`bytes[1 + bytes[0] as
usize]`, no bounds check) through `catch`, and asserts the wrapper *does* report a panic —
plus the boring inverse: an in-bounds call must **not** be reported as one. The buggy
decoder lives entirely in that one file, not in any shared or off-limits crate, so there is
nothing to neuter-and-restore and no window where this could be mistaken for someone else's
in-flight regression.

### Coverage: which decoders, which states

Both the deterministic sweep and the randomized property in `no_panic_arbitrary_bytes.rs`
read the real generated `packet_ids` tables (`lodestone_v{47,340,735,770}::packet_ids::
{state}::clientbound::ENTRIES`) — the same tables `cargo xtask connectedness` uses — so
"which packet ids get fuzzed" tracks the generator, not a hand-maintained list. This
covers:

- **All four client families' clientbound decode** (`v47`, `v340`, `v735`, `v770`), across
  all five `ConnectionState` phases each family declares packets for.
- **`v770`'s serverbound decode** (`V770ServerProtocol::decode`) — the attack surface our
  *integrated server* faces from a connecting client, not just what a server can do to us.
  It's the only family fuzzed on this side because it's the only one that implements
  `ServerProtocol` (`CLAUDE.md`: only `v770` can host).
- **`lodestone-net`'s frame-level `Codec`** (`feed`/`next_packet`) — the length-prefix and
  zlib-compression layer every packet passes through *before* any `VersionAdapter` sees it,
  shared by all four families. Issue #282's own body named this codec directly as the
  reason its scope could stay narrow ("the immediate risk is lower... but that conclusion
  currently rests on manual code reading"); `tests/no_panic_net_codec.rs` is that
  conclusion turned into a running check, including a termination bound (`next_packet` is
  drained up to 10,000 times per `feed` call — a codec that never returns `None`/`Err` and
  spins instead fails that cap rather than hanging the suite).

Deliberately **not** covered:

- **`v47`/`v340`/`v735` serverbound decode** — none of the three implement `ServerProtocol`,
  so there is no decoder there to fuzz; this isn't a coverage gap, it's "the function
  doesn't exist yet" (a legacy family becoming a host would need one first).
- **Anything below `handle_packet`/`decode` in isolation** — e.g. calling
  `GameLogin::decode` directly the way `length_prefix_allocation.rs` does is the exception,
  used only because it needs to isolate one field's allocation from the rest of the packet.
  Every other test goes through the adapter entry point real connections use, matching the
  precedent in `crates/protocol/v770/tests/entity_encoders.rs`.
- **NBT and the core `Reader` primitives in isolation** — not fuzzed directly here because
  reading them already shows they're bounds-checked before any allocation
  (`ensure_nbt_length_fits_remaining`, and `Reader::bytes`/`string`/`var_bytes` all validate
  against `remaining()` *before* the one allocation each makes, a plain `to_owned()`/`to_vec()`
  bounded by the already-validated slice). They're exercised transitively by every property
  above; a dedicated property suite for `lodestone-core` alone would be a reasonable follow-up
  but is not this harness's most valuable next dollar.

### Corpus: what's real and what's ours

Per `CLAUDE.md`, "an expected value must originate outside the code under test" — the same
rule applies to fuzz seeds, so `truncation_is_clean.rs` ranks its corpus explicitly:

- **Strong**: `crates/protocol/v770/tests/fixtures/*.hex` — six files of real bytes captured
  from a live vanilla 26.2 server, already used by that crate's own hermetic tests
  (`registry_data`, `item_entity_metadata`, `item_components`). Covers `registry_data`,
  `set_entity_data`, and `container_set_slot`. Loaded via `read_hex_fixture`/
  `v770_fixture_path` in `src/lib.rs`, which parse the same `#`-commented hex-dump format
  `crates/protocol/v770/tests/world_state.rs` established.
- **Weak, and named as such in the test**: every family's own `begin_login` output (the
  serverbound handshake/login bytes it would send on connect). Self-encoded — proves only
  that truncating *something packet-shaped* doesn't panic, not that we understood the
  protocol correctly. This is the only corpus source for `v47`/`v340`/`v735`: none of the
  three have a captured-fixture corpus today (no JVM-oracle capture exists for the legacy
  families). That gap is real; closing it would mean joining a legacy-protocol oracle and
  checking in fixtures the way `v770`'s live tests already do.

### Bug found and fixed: unbounded allocation from a length prefix (issue #417)

`length_prefix_allocation.rs` both stated and **measured** a real violation of the "must not
pre-allocate" property. `lodestone-macros`' `decode_vec` (`crates/lodestone-macros/src/lib.rs`,
in `decode_vec`, the non-`u8`-element and `#[mc(varint)]`-element branches) emitted
`Vec::with_capacity(len)` for a `Vec<T>` field *before* checking `len` against the bytes
actually remaining — unlike `lodestone-core`'s NBT reader, which bounds every array/list
length against `Reader::remaining()` first (`ensure_nbt_length_fits_remaining`). Any packet
field of that shape with no `#[mc(max = ...)]` attribute inherited the bug.

Confirmed, not just read: an 8-byte `GameLogin` payload (`entity_id = 0`, `hardcore = false`,
then a `levels` length prefix of 2,000,000 and nothing after it) drove a real
**48,000,000-byte** single allocation before the first per-element read failed with a clean
`UnexpectedEof` — measured with a counting `#[global_allocator]` scoped to that one test
binary (the only `allow(unsafe_code)` in the workspace; see the comment in that file for why
implementing `GlobalAlloc` needs it and why it's safe to allow narrowly). 2,000,000 was chosen
deliberately over pushing toward `i32::MAX` (~48 GB for this field): that would demonstrate a
full process-abort DoS but risks real memory pressure on a machine shared with other agents'
builds, which `CLAUDE.md`'s own Docker-memory incident already flags as a live hazard.

**Fixed** in `decode_vec` by capping the pre-allocation at `len.min(r.remaining())` instead of
trusting `len` outright. This is the *bound-by-what's-available* policy (option 1 of the three
the issue laid out — the other two were "don't pre-allocate at all", which pays a reallocation
cost on the hot decode path for no benefit here, and "require `#[mc(max = ...)]` everywhere",
a breaking change across four protocol crates that this fix avoids needing). The per-element
minimum used is the safe universal one — 1 byte — rather than a per-`T` minimum: every `Decode`
impl in this wire format consumes at least one byte (`fixed_codec!`'s primitives, `Decode for
String` via `Reader::string`, and any derived struct built from those), so capacity can never
exceed `r.remaining()` elements regardless of what `len` claims, and there is no per-type
minimum to get subtly wrong. `len` itself, the `#[mc(max = ...)]` check, and the `0..len` loop
bound are unchanged — only the up-front reservation is capped, so a field that legitimately has
enough bytes buffered still decodes every element in one allocation as before.

Measured after the fix: the same 8-byte payload now peaks at **0 bytes** (capacity capped to
`len.min(0) == 0`, and `Vec::with_capacity(0)` never allocates) — down from 48,000,000. A second
test proves the cap tracks `r.remaining()` rather than collapsing to "always zero": 100 trailing
garbage bytes after the same oversized length prefix caps the reservation at 100 elements and
peaks at exactly **2,400 bytes** (`100 * size_of::<String>()`), predicted before it was measured.
A positive control decodes a real captured `registry_data` fixture — one of the affected
field-shapes below — through `RegistryData::decode` and asserts it still decodes cleanly with no
trailing bytes, proving the cap doesn't reject legitimately large vectors.
`well_formed_small_payload_does_not_trigger_a_large_allocation` remains as the control that the
counting allocator itself isn't just reporting noise regardless of input — running it alongside
the huge-prefix test also surfaced a real test-isolation bug of this harness's own (see
"Gotchas" below).

Fields that were vulnerable before the fix, found by grepping every `Vec<T>` struct field
lacking `max`/`remaining`/`fixed`/`nbt` across all four families' `packets/*.rs` and excluding
`Vec<u8>` (safe — its `decode_vec` branch validates via `Reader::bytes` before allocating) and
`#[mc(len = "i16")]` fields (safe in practice — an `i16` length tops out at 32,767 elements
regardless of `max`):

- `v47`, `v340`, `v735`: `entity.rs`'s `entity_ids: Vec<i32>` (`#[mc(varint)]`).
- `v735`: `game.rs`'s `world_names: Vec<String>`.
- `v770`: `game.rs`'s `levels: Vec<String>` (the one measured above) and `entries:
  Vec<GameRuleEntry>` (×2); `registry.rs`'s `entries: Vec<PackedRegistryEntry>`;
  `player_info.rs`'s `entries: Vec<PlayerInfoEntry>` and `uuids: Vec<Uuid>`;
  `scoreboard.rs`'s `players: Vec<String>`; `configuration.rs`'s `packs: Vec<KnownPack>`
  (×2); `time.rs`'s `clocks: Vec<ClockUpdate>`; `login.rs`'s `properties: Vec<Property>`.

All 13 are fixed by the same macro change with no per-field edits, because the cap lives in
`decode_vec` itself rather than in an attribute each field would need to opt into. None of these
fields need an additional `#[mc(max = ...)]` bound for *safety* — the generic cap already closes
the allocation hole — though a real spec-derived `max` would still be a tighter, more meaningful
bound than "as many elements as fit in the remaining bytes" for fields where the protocol defines
one (e.g. a registry with a known maximum entry count). That tightening is optional follow-up
work, not a gap left open by this fix.

### Bug found and fixed: a remote panic from `multi_block_change`'s coordinate maths (issue #450)

`handle_packet_never_panics` found a real remote panic in `v340`'s `multi_block_change` decode
(`crates/protocol/v340/src/adapter.rs`, added by `714209b` for #349):

```rust
let x = chunk_x * 16 + rel_x;   // chunk_x is straight off the wire
```

An unchecked `i32` multiply on a wire-supplied chunk coordinate. For `|chunk_x| > i32::MAX / 16`
(= 134,217,727) it **panics in debug** and — worse — **silently wraps in release**, writing a
block at a position the packet never named. Note the direction, which is the opposite of #417's:
this is the *client* decoding a *server* packet, so the hostile party is a malicious or buggy
**server**.

**Fixed** with a two-part guard, `chunk_origin_block` in that same file, applied to both axes
*before* the record loop runs so nothing reaches the world sink for a packet that will be
refused:

- `checked_mul(16)` — the **structural** half. Cannot panic or wrap whatever the bound says, so
  a later edit that loosens the range check still cannot reintroduce either failure.
- a range check against `WorldBorder.absoluteMaxSize` (`WorldBorder.java:37`) at ±29,999,984
  blocks — the **semantic** half. A chunk coordinate past the border names a position no vanilla
  world can contain, so it is refused at the seam with an `AdapterError::Decode`. Deliberately
  **not clamped**: a clamp invents a position exactly the way the release-mode wrap did, and
  "wrote the wrong block" is not an improvement on "crashed".

**Reproduction was intermittent, and that was the more important half of the fix.** The full
crate run failed; the same target run alone with a name filter passed, twice. An intermittent
red is easier to write off as flake than a deterministic one, which is roughly what happened
before the issue was traced. Both durable fixes are committed:

- `tests/no_panic_arbitrary_bytes.proptest-regressions` — proptest's own seed file, so the
  generator case replays first on every run.
- `tests/fixtures/v340_multi_block_change_chunk_overflow.hex` plus `tests/fuzz_regressions.rs` —
  the literal twelve bytes proptest shrank to (`08 00 00 00 00 00 00 00 01 00 00 00`, i.e.
  `chunk_x = 134_217_728`, one record of air), asserted directly. Neither replaces the other:
  the seed file re-runs the whole strategy and would go stale if the strategy changed, while
  the fixture pins the payload and asserts *what* the decoder does with it.

The fixture gate asserts **refusal**, not survival, because "did not panic" is satisfied by the
release-mode wrap — the worse of the two original outcomes — and "did not panic, and clamped" is
satisfied by inventing a position. So it requires an `AdapterError::Decode` naming the offending
coordinate *and* **zero** `set_block` calls on a recording `WorldSink` (`lodestone_fuzz::NullSink`
discards writes and therefore cannot tell refused from wrapped). A separate test pins the bound
to the world border rather than to `i32` range: `chunk_x = 1_875_000` multiplies to 30,000,000,
which fits an `i32` fine — so a `checked_mul`-only fix accepts it — while `1_874_999` lands
exactly on 29,999,984 and must still be accepted.

Both halves were run as negative controls, restoring the pre-fix source from an md5-verified
backup:

| variant | overflow fixture | world-border pair | ordinary coordinate |
|---|---|---|---|
| pre-fix `chunk_x * 16` | `PANIC: attempt to multiply with overflow` | fails | passes |
| `checked_mul` only, no border check | passes | **fails** | passes |
| shipped fix | passes | passes | passes |

That middle row is the load-bearing one: without the border test, a `checked_mul`-only fix would
have looked complete.

**Sibling paths checked** (`/usr/bin/grep -rn '\* 16'` across all four families' `src/`), since
this is a shape rather than a one-off. Nothing else is reachable from the wire:

- `v340`'s own `block_change` reads a **packed** `Position` instead of chunk coordinates, so its
  x/z arrive pre-bounded at ±33,554,431 by the 26-bit field width, and it only ever does `>> 4`
  and `rem_euclid(16)` — no multiply, no overflow. (It can still name a position 3.5M blocks
  outside the border; that is a wrong-but-harmless write, not a panic, and out of #450's scope.)
- `v47/packets/chunk.rs:356`, `v340/packets/chunk.rs:470` (`block_z * 16 + block_x`) and
  `v735/packets/chunk.rs:272` (`cy_global * 16 + cz * 4 + cx`) all multiply **loop indices**
  bounded by `0..4`/`0..16` in the source, never wire values.
- `v340/flattening.rs:128` and `v340/canonical.rs:117,155` (`old_block_id * 16 + meta`) promote a
  `u8` to `usize` first; the maximum is `255 * 16 + 15 = 4095`.
- `v47` does store raw packed `(id << 4) | meta` composites, but that is a value, not a
  coordinate, and it is already range-checked against the 4,095-slot legacy table.

## How to change it, and the gotchas

- **Add a family or state to the sweep**: `Family::clientbound_entries` and `Family::STATES`
  in `src/lib.rs` are the single place both the deterministic sweep and the randomized
  property read from. A fifth family only needs a `Family` variant plus one `match` arm in
  `adapter()` and one in `clientbound_entries()`.
- **A process-wide `#[global_allocator]` in one test file measures every allocation in that
  test *binary*, including other `#[test]`s in the same file running concurrently — and a
  `Mutex` is the wrong fix.** This bullet used to recommend one, and it flaked; the corrected
  history is worth keeping because the second attempt looked airtight.

  `length_prefix_allocation.rs` hit the raw version while being written: the "small payload,
  small allocation" control failed only when run alongside the "huge payload" test, both
  reporting the same 48,000,000-byte peak, because `cargo test` runs `#[test]`s in parallel
  threads within one process. That was fixed with a `MEASUREMENT_LOCK: Mutex<()>` held for the
  reset-call-read span of every measurement — which serialises the *measuring* tests and does
  nothing whatever about the others. **A lock only excludes code that takes it.**
  `real_registry_data_fixture_still_decodes_cleanly_after_the_fix`, in the very same file, never
  calls `peak_alloc_during`, so it never takes the lock, and its fixture read plus
  `RegistryData::decode` allocate into the shared atomic from a parallel harness thread. Result:
  issue #417's own DoS regression gate passed alone and failed in a full parallel run — issue
  #450's second half, and `CLAUDE.md`'s *accumulator* species of vacuous test (a global
  outliving the gate's own window).

  **Fixed by scoping the counter, not by serialising the tests**: `PEAK_SINGLE_ALLOC` is now a
  `thread_local!` `Cell<usize>`, `const`-initialised so reading it from inside `alloc` cannot
  itself allocate and recurse, read with `try_with` so a teardown-time allocation records
  nowhere instead of panicking in the allocator. Other threads' allocations land in *their*
  cell and are structurally invisible — no cooperation needed from code that has never heard of
  this file. The `Mutex` is gone.

  **The property is now itself gated**, which is the durable part:
  `a_sibling_threads_allocation_does_not_contaminate_a_measurement` spawns a thread whose
  48,000,000-byte allocation is pinned by two `Barrier`s to the window strictly between this
  thread's reset and its read — deterministic, no scheduling luck — and asserts the reading
  stays under `SMALL_CEILING`. Observed against both older designs: `measured as 48000000
  bytes`. Against the thread-local: 0.

  **Do not widen `SMALL_CEILING` (4,096) to make a measurement pass.** It sits deliberately far
  below the old ~48,000,000-byte figure rather than merely under it, so a *partial* regression
  is still caught. Every reason so far to want it wider has been contamination of the
  measurement, not a genuinely larger correct allocation. A DoS guard that flakes gets muted,
  and a muted guard stops guarding.
- **When a target finds a bug, commit the input — a generated repro is not a gate.** Drop the
  bytes into `tests/fixtures/` in the same `#`-commented hex format (with a header recording
  where they came from — proptest's shrink line, a live capture, whichever) and assert them in
  `tests/fuzz_regressions.rs` via `lodestone_fuzz::regression_fixture_path`. Keep the
  `.proptest-regressions` seed file too; they fail for independent reasons. And assert what the
  decoder *does*, not merely that it survived: #450's release-mode behaviour was a silent wrong
  write, which every "no panic" assertion in this crate accepts by construction.
- **Raising `ProptestConfig::with_cases`** raises run time roughly linearly; see "Cost, in
  numbers" for the current baseline before changing it.
- **A new captured `.hex` fixture** (closing the `v47`/`v340`/`v735` corpus gap, or adding
  more `v770` packet types) drops into `crates/protocol/v770/tests/fixtures/` (or a sibling
  legacy-family `tests/fixtures/` once one exists) in the same `#`-commented format; wire it
  into `strong_corpus()` in `truncation_is_clean.rs` with its packet id and state. That
  directory belongs to `v770` (off-limits for direct edits per this task's ownership rules),
  so adding fixtures there was out of scope for this pass — flagged, not done.
- **Do not add a `cargo-fuzz` target as the primary mechanism.** If a nightly-only libFuzzer
  target is ever worth adding on top of this (e.g. for corpus-driven coverage-guided fuzzing
  proptest doesn't do), gate it behind an opt-in feature so `cargo test --workspace` on the
  stable 1.95.0 toolchain is unaffected.

## Configuration

- **Case cap**: `ProptestConfig::with_cases(512)` in `no_panic_arbitrary_bytes.rs`,
  `with_cases(256)` in `no_panic_v770_serverbound.rs`. Override per run with the
  `PROPTEST_CASES` environment variable without editing the source (proptest reads it
  directly).
- **Payload size cap**: `prop::collection::vec(any::<u8>(), 0..4096)` in both randomized
  properties — up to 4 KiB per generated payload.
- **Declared-vs-arbitrary packet id mix**: `prop::bool::weighted(0.875)` — seven of every
  eight cases pick a packet id the family/state actually declares (so most cases reach a real
  decode body instead of an immediate "unknown id" bail-out); the rest use a fully arbitrary
  `i32`, including negative and out-of-range values.
- **Allocation-magnitude constants** in `length_prefix_allocation.rs`: `CLAIMED_LEN =
  2_000_000` (the malicious length prefix, unchanged from when the bug was filed);
  `SMALL_CEILING = 4096` bytes (the post-fix ceiling for the zero-remaining-bytes case, whose
  predicted and measured peak is exactly 0); and `TRAILING = 100` bytes (the second test's
  padding, whose predicted and measured peak is exactly `100 * size_of::<String>() == 2400`
  bytes) — both post-fix figures four to seven orders of magnitude below the pre-fix
  ~48,000,000-byte measurement.

## Dependencies

- `proptest` (dev-dependency only) — property generation, shrinking, and the case-count
  environment-variable override.
- `lodestone-core`, `lodestone-model`, `lodestone-world`, and all four `lodestone-v{47,340,
  735,770}` crates — the decoders under test, plus `NullSink`'s `WorldSink` impl.
- `lodestone-server` (direct path dependency, not a `[workspace.dependencies]` alias — same
  convention `lodestone-v770`'s own `Cargo.toml` uses for it) — `ServerProtocol`/`ServerBound`
  for the `v770` serverbound property.
- `lodestone-net` — `Codec` for the frame-level property.

## Cost, in numbers

Measured with `--target-dir` pointed at a private, non-shared directory (see the note in the
crate's own test-running instructions about why that flag matters for build-cache hit rate):
the full `cargo test -p lodestone-fuzz --no-fail-fast` run — every file above, deterministic
sweeps plus both randomized properties at their case caps — completes in well under half a
second of test time (compilation aside). The deterministic sweep alone touches over 2,700
(packet id × fixed payload) combinations across the four client families. Cheap enough that
there's no reason this isn't part of the ordinary `cargo test --workspace` CI job.
