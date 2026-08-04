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

## How to change it, and the gotchas

- **Add a family or state to the sweep**: `Family::clientbound_entries` and `Family::STATES`
  in `src/lib.rs` are the single place both the deterministic sweep and the randomized
  property read from. A fifth family only needs a `Family` variant plus one `match` arm in
  `adapter()` and one in `clientbound_entries()`.
- **A process-wide `#[global_allocator]` in one test file measures every allocation in that
  test *binary*, including other `#[test]`s in the same file running concurrently.**
  `length_prefix_allocation.rs` hit this directly while writing it: its "small payload, small
  allocation" control failed only when run alongside the "huge payload" test, both reporting
  the same 48,000,000-byte peak, because `cargo test` runs `#[test]`s in parallel threads
  within one process by default and the counting atomic is necessarily global. Fixed with a
  `Mutex` held for the whole reset-call-read span of each measurement — serializes just the
  two tests in that file rather than needing `--test-threads=1` for the entire binary. If you
  add a third test to that file that also measures allocation, it gets the serialization for
  free through the same lock; if you add allocation-measuring code to a *different* file,
  it's a separate test binary and cannot see this one's counter at all.
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
