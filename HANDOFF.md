# Lodestone — Deferred Work Handoff

**Status of this document:** everything below was **deliberately descoped**, not abandoned or
found broken. Each area is left in a *working, committed, test-covered* state. This file exists so
the work can be picked up by someone who was not present when it was built.

**Current active scope** is v770 only (protocol 776 / MC 26.2), across four workstreams:
packets, UI, entities, lighting. Everything in this document is **outside** that scope.

**Rule for anyone resuming:** every number in this file was measured on real data, not estimated.
Where something is unproven it says so explicitly. Please preserve that distinction — the single
most expensive recurring failure on this project has been a claim that outran its evidence.

**See also:** [`DESIGN.md`](./DESIGN.md) — full architecture and rationale. Its **§12 validation
log** is the highest-value part: ~20 entries recording beliefs that were confidently held and
empirically false, and how each was caught. This file is self-contained, but §12 is what stops the
same mistakes being made again.

---

## Table of contents

1. [Multi-version protocol families (v47 / v340 / v735)](#1-multi-version-protocol-families)
2. [WebAssembly / browser target](#2-webassembly--browser-target)
3. [Audio](#3-audio)
4. [Worldgen performance](#4-worldgen-performance)
5. [Online-mode authentication](#5-online-mode-authentication)
6. [Allocator selection (closed — no action needed)](#6-allocator-selection-closed)
7. [Never started](#7-never-started)
8. [Traps that are expensive to rediscover](#8-traps-that-are-expensive-to-rediscover)

---

## 1. Multi-version protocol families

### Decision

The original target was 17 protocol families spanning 1.8.9 → 26.2. That was cut first to four
(v770, v735, v340, v47) and then to **v770 only**. The other three families **remain in the tree
and must not be deleted** — they are the empirical proof that the version-isolation architecture
works, and re-deriving that proof is far more expensive than carrying the code.

### Why the reduction happened (this is the load-bearing part)

Two independent findings converged, and together they mean 17 families was the *wrong plan*
rather than merely an ambitious one:

- **Adapter dispatch cannot be generated.** ID routing is mechanical, but lowering/raising to
  `ClientEvent`/`ClientAction`, world side effects, registry lookups, teleport replies and
  chunk-shape state are semantic per-version work.
- **Wire-shape migration cannot be generated either.** `xtask new-version` cloned v340 → v735
  correctly and mechanically, and the result was a 1.12.2 client wearing 1.16 packet IDs.

So codegen covers packet IDs and registry tables — the cheap part — and covers **neither dispatch
nor shape migration**, which are the bulk *and* the risk.

### Measured cost of a family

Do **not** use the `cargo xtask codegen-ratio` "hand-written lines" figure to plan with; it counts
docs, blanks and derived struct declarations. The real measurement, taken on v735:

| bucket | lines |
|---|---|
| generated (`packet_ids` 841 + `entity_types` 123) | 964 |
| hand-written total | 3007 |
| · doc/comments | 997 |
| · blank | 181 |
| · **actual code** | **1829** |

And within that 1829:

| file | lines | nature |
|---|---|---|
| `adapter.rs` | 712 | dispatch / choreography / lower / raise — **irreducible** |
| `chunk.rs` | 191 | paletted decode, biomes prefix, light split, flattening — **irreducible** |
| `metadata.rs` | 211 | typed union + per-version type-id table — semi-reducible |
| hand codecs | ~200 | JoinGame/Respawn NBT, slot, position — macro-closable |
| derived decls | ~515 | `#[derive]` + field lists — mechanical |

**Genuine irreducible per-version knowledge is ~900 code lines.** A fifth family is roughly a day
of work, not a project. Budget accordingly if resuming.

### State of each family

| family | version | protocol | live-verified | `ClientAction` encode |
|---|---|---|---|---|
| `v770` | 26.2 | 776 | yes — active scope | 42/43 |
| `v735` | 1.16.5 | 754 | yes, chunk decode against a real 1.16.5 server | 17/43 |
| `v340` | 1.12.2 | 340 | yes | 17/43 |
| `v47` | 1.8.9 | 47 | yes — 81 columns via `map_chunk` + `map_chunk_bulk`, 0 trailing bytes | 16/43 |

Deletability is **measured**, not asserted — `cargo xtask check-deletable <family>` simulates
removal and reports the true fallout. All families are cleanly deletable (v47 5 manifest lines,
v340 4, v770 8).

### What is left if you resume

1. **Action encode breadth is the biggest gap.** v47/v340/v735 sit at 16–17 of 43 while v770 is at
   42. Concretely, **a 1.8.9 client still cannot break a block.** `BlockAction`, `UseItemOn`/
   `UseItem` and `InteractEntity` are partial/lossy; `ContainerClick` is absent on all three.
2. **Critically — some of that gap is correct by design and must not be "fixed."** The canonical
   model is shaped by the newest protocol and older adapters translate *upward*, so
   `SetPlayerInput`, `EndClientTick` and `ChatAck` genuinely have no 1.8.9 form. **Any resumed work
   must first produce a table distinguishing *absent by design* from *not done yet*,** because a
   table where those look identical is exactly how v735 shipped registered-but-unreviewed.
3. **v47 place-interaction cannot be gated in the current lab.** The 1.8.9 container is survival
   with no RCON and no console, so the player has nothing to place. Break-only is the maximum until
   that container gets an RCON channel. This is documented in-crate rather than silently absent.

### The `SHAPE_REVIEW.toml` gate — do not remove it

`xtask new-version` clones a family and prints a residue list telling you the packet structs are
still the *source* family's wire shapes. On first use, that warning went to stdout and evaporated
while the same command wired the new family into the registry as **supported**. One command emitted
a true signal and an opposite fact, and only the fact survived.

The fix is that a family is **not registerable** while `SHAPE_REVIEW.toml` has undischarged
entries. v735 was de-registered until all 62 packet entries were audited. **Residue printed is
residue lost** — if you extend the tooling, keep the failure closed.

Also: **never clone a live test.** A cloned live gate pointing at the *source* family's server is
worse than no test, because it manufactures evidence for the wrong version. v735 shipped with a
cloned `live_chunk.rs` still pointing at the 1.12.2 container on port 25568.

---

## 2. WebAssembly / browser target

### State: working spike, goal met, frozen

Verified end to end in Chrome:

```
[status] REAL terrain from real server bytes — 16 chunks, 16 sections, 250 greedy quads
         backend: BrowserWebGpu | select_strategy(): PerDraw     ~119–121 fps
[net]    relay probe OK — browser WebSocket → relay → live server
         version.name = "26.2" | {"version":{"name":"26.2","protocol":776}, …}
```

Browser → WebSocket relay → **live vanilla 26.2 server**, round-tripping real status JSON, with
real vanilla textures decoded in-browser. `trunk` 0.21.14 is installed and `trunk serve` works.

`scripts/wasm-check.sh` passes for all wasm targets and is the regression guard.

### Architecture notes worth keeping

- **The one true blocker is networking, permanently.** Browsers cannot open raw TCP; vanilla
  servers speak only raw TCP. A browser build **strictly requires** a WebSocket↔TCP relay. No
  browser API removes this (WebTransport/WebRTC don't speak to a vanilla TCP listener either).
- **The relay is ~150 lines and protocol-blind.** Because the codec is byte-transparent framing, it
  never parses a packet, so **one relay serves all versions and all servers**. The moment it parses
  a packet it becomes a per-version component and you need one per family. Keep it dumb.
- **Payload: 933 KB brotli** (raw 3.71 MiB, gzip 1.21 MiB) at last measurement. **Report brotli** —
  servers ship wasm brotli-compressed and gzip overstates real cost by ~26%. Attribution:
  wgpu + naga + glow ≈ 1.19 MiB, i.e. the graphics stack, not our code.
- **`wasm-opt -Oz` is counterproductive for download size** — it shrinks the raw module ~10% but
  makes the *brotli* artefact 4 KB larger. It trades download for parse/instantiate time. Trunk's
  `data-wasm-opt="0"` is correct if you are optimising bytes.
- **`opt-level = "z"` (1.21 MiB) beats `"s"` (1.30) and `"3"` (1.62).** `"3"` is a +28% regression
  for speed a 250-quad scene does not need.

### `webgl` feature was removed, and it is **not** a toggle

It cost 537 KB brotli — **68% of the entire download** — for a path that **panicked before frame
0**. The terrain pipeline binds a vertex-stage storage buffer (`block.rs`, `ShaderStages::VERTEX` +
`BufferBindingType::Storage`), which WebGL2 categorically lacks, so `create_bind_group_layout`
panics at construction.

**Re-adding WebGL2 costs a downlevel-compatible render path (no vertex-stage storage), not a
feature flag.** That is recorded in `web/Cargo.toml` beside the removal. Before pricing any
fallback, *run it* — a fallback that has never executed is not a fallback.

### Traps

- **COOP/COEP asymmetry.** `trunk serve` sets both headers → `crossOriginIsolated === true`; a
  plain static server sets neither. Anything depending on cross-origin isolation (threaded meshing
  via `wasm-bindgen-rayon`) **works under trunk and fails mysteriously elsewhere.** Documented in
  `web/README.md`.
- **A 2-D `getImageData` readback of the WebGPU canvas returns all-black.** That is the un-retained
  drawing buffer, *not* a blank scene. Use the **composited screenshot** to verify. This is the
  inverse of the project's usual failure mode and just as misleading.
- **`std::fs` compiles for `wasm32-unknown-unknown` and fails only at runtime.** A `cfg` gate
  removes *existing* entry points but does nothing about a newly added ungated `fs::read`. The
  enforcement is confinement to a single gated file (`lodestone-assets/src/source_native.rs`) plus
  a grep guard in `scripts/wasm-check.sh`. Keep both.
- **Cargo features are advisory, not architectural boundaries.** Features unify across the whole
  graph, so a downstream crate taking a dependency with default features on silently overrides
  `default-features = false` elsewhere. For a hard boundary use `cfg(target_arch)`.
- **Run `scripts/wasm-check.sh` whenever a dependency is added or bumped.** Dependency changes are
  the only way this breakage class enters the tree — an `rsa 0.9` addition once pulled in a third
  major of `getrandom` via a `rand_core 0.6` pin and broke the browser build, and nothing anyone
  edited mentioned `getrandom`.

### What is left

Browser **singleplayer** is unblocked (`lodestone-server`'s tokio is target-split) but not wired.
The browser is **not** the limiting factor for multiplayer — adapter dispatch breadth is. Do not
optimise the wasm layer in response to multiplayer feeling thin.

---

## 3. Audio

### State: complete and working, consumed only partially

`lodestone-audio`: 63 tests. `lewton` 0.10.2 (pure-Rust Vorbis) + `cpal` 0.18.1 (native-only,
`cfg`-gated). Sample-driven clock with **no `Instant::now()` anywhere**, enforced by a crate-wide
guard with an empty allowlist.

`SOUND`, `SOUND_ENTITY`, `LEVEL_EVENT` and `LEVEL_PARTICLES` all dispatch, so the packet seam is
connected. Playback is gated behind `LODESTONE_ASSET_ROOT`.

### Validation approach worth preserving

Decode validation deliberately avoids self-comparison: **libsndfile encodes** the fixture,
**ffmpeg decodes** the golden PCM, **lewton** is under test — `max_abs_diff 3.1e-5`, `rms 1.8e-5`.
The test has teeth: negated, channel-swapped, and one-frame-shifted goldens are each asserted to
**fail** the tolerance.

**Trap:** a genuinely-silent vanilla ogg gives a worthless all-zeros "match" — two silent buffers
agree perfectly. Guarded by a `peak > 0.3` assertion on the fixture.

### Parity facts (transcribed with call-site citations)

- **`SoundSource` has 11 buses in 26.2** (don't forget `UI`).
- Range = `max(instanceVolume, 1.0) × attenuationDistance` (default 16).
- `AL_LINEAR_DISTANCE` rolloff 1.0 ref 0.0 → `gain = max(0, 1 − dist/maxDist)`.
- MASTER is **not** squared. Pitch clamped `[0.5, 2.0]`.
- Only MONO spatialises; stereo plays flat with no downmix.

### Known limitation, honestly graded

**Panning geometry is not parity.** Vanilla delegates stereo placement to OpenAL-Soft's HRTF. Ours
is equal-power panning, documented as an approximation rather than claimed exact.

### Sound asset layout (non-obvious)

**`sounds.json` is not in `client.jar`.** It, like every `.ogg`, lives in the external asset-object
store addressed by `asset-index-<n>.json` at `objects/<sha1[0..2]>/<sha1>`.

Corpus: **1968 events, 8024 entries (7963 file, 61 event-refs), 4843 distinct files.** Entry type
is `"file" | "event"` (**not** `"sound" | "event"`). All 61 refs are depth-1 and acyclic, but
**vanilla ships no cycle guard**, so a malicious pack would stack-overflow at play time — we bound
it with a visited set and depth cap. A `type: event` entry contributes the *referenced* event's
total weight to the parent's selection sum.

---

## 4. Worldgen performance

### State: correctness complete and verified; performance unmeasured

`lodestone-worldgen` has the strongest evidence in the codebase, all bit-exact against a JVM oracle,
element-wise, naming the divergent coordinate on failure:

```
noise router      34048 / 34048   whole region
final density     98304 / 98304   whole chunk, interpolated (4×8×4 cells, trilerped)
carvers           98304 / 98304   × 2 chunks
surface + aquifer land and ocean profiles
ore features      whole-chunk exact BOTH directions, 3 fixtures / 2 seeds / 2 terrain profiles
```

It is now genuinely on screen — wiring it into the shell moved spawn Y from 46 (a sine+hash
placeholder) to **71** (real vanilla surface height), and meshed sections from 610 to 831.

### The one open question

**Debug-build generation measured ~1.1 s/chunk (169 chunks in 3m09s).** Release-mode per-chunk time
was never measured. Before optimising, measure — and note the constraint: **generation
parallelisation must not break per-chunk RNG determinism**, which the ore-feature parity depends on.
Do not trade parity for speed.

### The bug class to watch

Buried ores draw a `nextFloat` inside `shouldSkipAirCheck` **before** the 6-neighbour air test.
Short-circuiting them desynchronises the shared RNG stream and **three ore families silently
vanish.** A wrong draw *count* is invisible to any test asking "did ores appear" and instantly fatal
to whole-chunk parity.

This is why both gate styles are needed: **exact-match on one chunk catches a wrong draw order;
count bands catch a plausible-but-wrong distribution. Neither catches the other.**

### Architecture note

Worldgen is **data, not code** — vanilla 26.2 ships its noise router as **963 JSON files** under
`data/minecraft/worldgen/`. The generator is a ~700-line version-free interpreter over per-version
JSON, not ~10k lines of ported logic.

The proof that it is data is better than the claim: the Rust interpreter reads **disk JSON** while
the oracle evaluates the **running server's live `RandomState` router**. If disk JSON were an
incomplete picture the two would diverge. They agree 100%.

### Deferred architectural step

The shell currently calls the generator **directly**. Vanilla runs singleplayer as an integrated
server, so the faithful destination is generate → loopback → client-consumes, sharing the
multiplayer path. This was deliberately deferred: closing the island today by a direct call was
worth more than the correct architecture arriving later, and **the generator itself does not have
to change when the call site is replaced.** Recorded so the shortcut is a decision with a named
successor rather than drift.

---

## 5. Online-mode authentication

### State: crypto path works end to end; a full authenticated join is untested and not claimed

```
$ cargo test -p lodestone-net --test online_handshake -- --ignored --nocapture
post-encryption disconnect reason: {"translate":"multiplayer.disconnect.unverified_username"}
test result: ok. 1 passed
```

**The measurement is the failure, and it is a strong one.** That disconnect arrived **encrypted**
and decrypted cleanly. So the server accepted our RSA-wrapped shared secret, matched the verify
token we echoed, switched on its cipher, and its AES-128-CFB8 reply round-tripped against ours. The
only thing that failed is the session-server ownership lookup, which needs a Microsoft account we
do not have.

A framing or decrypt error would mean broken crypto; a clean protocol-level "unverified username"
means the crypto is right. **When you cannot reach success, choose a failure that discriminates.**

### What exists

- `lodestone-auth`: Microsoft device-code OAuth (`flow.rs`) with token caching (`cache.rs`).
- `lodestone-net`: `Cfb8Cipher`, SRV record resolution (`resolve.rs`), legacy/status ping (`ping.rs`).
- Encryption is outermost on the wire: `encode = frame(compress(body))` then encrypt; `feed =
  decrypt` then buffer. One cipher per connection, separate CFB8 feedback registers per direction,
  key == IV == the 16-byte secret. It lives in the sans-IO codec, so the browser path inherits it.

### External vectors (an authority we did not write — keep these)

Minecraft's server-ID hash is a **signed** SHA-1: `BigInteger.toString(16)` over the raw digest, so
negatives get a leading `-` and a leading-zero digest loses a character. A naive hex digest passes
the first and fails the other two:

```
Notch  4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48
jeb_   -7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1     ← the negative case
simon  88e16a1019277b15d58faf0541e11910eb756f6      ← 39 digits, leading zero
```

CFB8-AES128 is checked against NIST SP800-38A F.3.7.

**The cipher is stateful across the whole connection, not per packet.** There is a deliberate
"per-packet-reinit-is-wrong" test that proves statefulness matters rather than merely asserting it.

### What is left

A real authenticated join, which needs Microsoft credentials. `rsa`/`rand` are native-only
(`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`) — deliberately, since the
session-server call is native-only anyway. The docs name that seam as where a browser auth story
would land.

---

## 6. Allocator selection (closed)

**No action needed. Decision: keep the system allocator.** Benchmarked in
`crates/lodestone-allocbench` — one binary per allocator, mutually-exclusive features, peak RSS via
`/usr/bin/time -l`, median of 5.

| vs. system baseline | throughput (geomean) | mean RSS |
|---|---|---|
| mimalloc 0.1.52 | 94% | **130%** |
| snmalloc-rs 0.7.4 | 79% | 104% |
| tikv-jemallocator 0.7.0 | **113%** | 111% |

No candidate is both faster *and* leaner than macOS `libmalloc`, and the top-end wins are within
measured noise. Each costs a C/C++ toolchain dependency. **Not justified.** If meshing throughput is
later *proven* by profiling to be the bottleneck, jemalloc is the only candidate with a consistent
edge — revisit then, not before.

Two findings worth keeping:

- **Cross-thread free inverts the ranking.** Local-free order is `jemalloc > system ≈ mimalloc ≫
  snmalloc`; cross-thread free at 8–10 threads is `snmalloc > jemalloc ≈ mimalloc > system`.
  Benchmarking with same-thread free — the obvious thing to write — ranks snmalloc last and produces
  the opposite conclusion.
- **Methodology trap:** `vec![0u8; n]` routes to `alloc_zeroed`, letting an allocator skip the
  memset on fresh OS-zeroed pages — it showed jemalloc at a bogus 4×. Use `with_capacity` + a real
  fill so the benchmark matches how sections and meshes are actually written.

**Rule: library crates must never set `#[global_allocator]`.** That is an application-level
decision; a library that hijacks it breaks every downstream consumer.

---

## 7. Never started

- **Scripting host.** WASM (`wasmtime`, sandboxed) vs Lua (`mlua`, ergonomic). Leaning WASM for
  untrusted plugins with a capability-based API. No code exists.
- **The other 13 protocol families.** See §1 for the real per-family cost (~900 irreducible lines).
- **Entity model ports.** ~130–150 base mob meshes are hand-written `LayerDefinition`/
  `MeshDefinition` classes in vanilla with **no data path** — nothing in the generated reports or
  minecraft-data exposes mesh geometry. The version-free primitive (`CubeDef`/`PartPose`/`PartDef`
  → `bake_entity`) exists in `lodestone-assets`; the per-mob data does not. Meshes are largely
  stable across versions, so this is author-once, tweak-per-version.

---

## 8. Traps that are expensive to rediscover

These cost real time to find. They are not specific to the deferred work, but they are the things
most likely to bite someone resuming it.

### Four species of vacuous test

A test can be green, well-written, live, and prove nothing. Two of these species **cannot be found
by reading the test** — the source is exemplary and the flaw is a property of what the test was
pointed at.

| species | flaw lives in | readable? | example |
|---|---|---|---|
| **assertion** | the assert | yes | `let _ = walk_to(...)`; position printed, never asserted |
| **precondition** | the setup | yes | missing fixture → `skip` instead of fail; gate passed in 0.00s |
| **duration** | test lifetime vs system counters | **no** | server stops sending chunks after 10 unacked batches; every gate disconnects first |
| **world** | the input data | **no** | light propagation gated on **superflat**, where sky light never spreads sideways |

Audit questions to carry forward:
- *Does any server-side counter accumulate past our gate's lifetime?*
- *Does the input actually contain the structure the code under test exists to handle?*

### An expected value must originate outside the code under test

`decode(encode(x)) == x` is satisfied by two symmetric misunderstandings. v735's hermetic chunk
fixtures were generated with **our own encoder** and passed throughout, then the live gate produced
49 × "unexpected end of input" — the decoder was missing the 1.16.2 biomes varint length-prefix.
Encoder and decoder shared one wrong mental model, so the round trip closed perfectly on bytes no
server would ever send.

Use captured server bytes, a JVM oracle, or a hand-decoded spec example. Where a live capture is
impractical, check the fixture in **as bytes** the first time it is validated against reality.

The same trap at the oracle level: a self-authored JVM oracle validates *the behaviour you chose to
model in it*, not vanilla's. Three implementations once agreed bit-for-bit across 16 scenarios and
all three were wrong, because all 16 happened to be flush contacts where two competing formulations
coincide. **Agreement across ports is weak evidence when the ports share an author.**

### Assertions of an absence need a control proving the detector works

"No corrective teleport", "no trailing bytes", "no dropped packet" are each only as good as the
evidence the mechanism *would* have fired. The live physics gate asserts zero corrective
`player_position` packets — and the server only validates movement once `hasClientLoaded()` is true,
so without sending `player_loaded` it silently ignores movement and returns a false green. The
permanent negative control (one 30-block teleport that **must** be snapped back) is what makes the
absence meaningful.

### Test-suite health

- **`cargo build --workspace` is not a health check** — it does not build test targets. Use
  **`cargo check --workspace --all-targets`**.
- **A test total gathered during concurrent edits is a sample, not a measurement.** The meaningful
  invariant is *zero failures **and** zero non-compiling targets*, never the absolute count. A run
  once reported "1406 passed, 0 failed" while exiting 1, because a crate failed to compile.
- **A live gate behind both a feature flag and `#[ignore]` compiles to zero tests without the
  feature** and reports `ok. 0 passed`, which is indistinguishable from success at a glance. Put the
  full invocation in the docs at every call site.

### Live-server hazards

- **Offline mode derives the account UUID from the *username*, ignoring the UUID the client sends.**
  Every test sharing a name shares one persisted player file. A mob killed that player once, vanilla
  persisted the dead state, and every subsequent join was held on the death screen — **which sends
  no chunks.** A dead player is a silent, total chunk blackout while join, keep-alives and entity
  movement all continue perfectly. Use `lodestone-testsupport`'s `unique_username`.
- That helper must be unique **by construction** (an `AtomicU64` plus pid), never derived from a
  clock. A `nanos % 1e9` version reads as a 10⁹ space and delivered ~10⁶ because the platform clock
  had microsecond resolution. The counter goes **first** in the string so the server's hard 16-char
  limit truncates the *timestamp*, not the discriminator.
- **A freshly summoned entity is not selector-visible until the next server tick.** Poll for it;
  never assert immediately. `Invulnerable:1b` additionally makes an entity **un-targetable** —
  vanilla's `TargetingConditions` rejects it — so use `NoAI:1b` for a stationary lure.
- **Vanilla's RCON client performs exactly one `read()` per request** and closes the socket unless
  `pktsize == read - 4`. Sending the frame as two `write_all` calls silently closes the connection
  after a few commands. **Write the entire frame in one call.**
- **`tick step N` does not advance entity physics; only `tick sprint N` does** — and a
  `tick sprint 1` used for registration silently consumes a tick, presenting as a phantom +1 offset.

### Resource hygiene — read this before running anything

**This machine is shared with an unrelated project.** Docker holds images, volumes and build cache
belonging to the user's other work (`mht-*`, postgres, valkey, seaweedfs).

**`docker system prune`, `docker volume prune` and `docker builder prune` would each destroy it.
Never run them.** Every cleanup action must name its target explicitly. Note also that Docker's
`name=` filter is a **substring** match, not a prefix match.

Containers are named `lodestone-<purpose>`; prefer `docker run --rm`. Reclaim disk by deleting
`target/debug/incremental` first (pure regenerable cache — it does **not** force a dependency
rebuild), then stale **own-crate** artefacts in `target/debug/deps` by mtime. **Never third-party
artefacts, never `deps` wholesale** — `cargo sweep --time N` is actively wrong here, because the
oldest mtimes belong to stable third-party deps that are still current, while our own crates
accumulate one dead content hash per rebuild.

---

## Appendix: authoritative data sources, in order

1. **Mojang's own generator** (`packets.json`, `registries.json`, `blocks.json`) — authoritative,
   works for every version ≥1.14 including 26.x.
2. **Decompiled source** — reference for behaviour only; never transliterated. 26.2 ships
   de-obfuscated, so class and method names are real.
3. **minecraft-data** — bootstrap and cross-check for **1.8–1.21.11 only**; it has no 26.x data.
4. **minecraft.wiki protocol pages** — human documentation.

**Prefer interrogating the real jar over any community dataset.** `blocks.json` contains no
collision geometry at all, and minecraft-data's `blockCollisionShapes.json` measured **stale and
incomplete for 26.2** — 92.29% of states reliably covered, 30 blocks missing by name. A spot check
would have said "looks fine." The replacement boots the real server headlessly and dumps
`getCollisionShape(...).toAabbs()` for all 32,366 states.

Where minecraft-data is still the practical choice, record why.

---

## Addendum — "Islands": subsystems that are correct but not plugged in

This is the **dominant defect class in this project** and the single most useful thing
to know when picking it up. In every case below the subsystem is individually built,
individually tested, and reaches **zero pixels** because nothing calls it. They do not
show up as failures — the tree is green, the tests pass, and the game runs.

A test suite cannot see an island. Only a **pixel gate** can: assert coverage inside the
subject's screen rect, plus a negative control (empty state, or an opposite-corner
reading) that must fail the same assertion. `bulk-models` established the pattern for
entities and `impl-net` reproduced it for status effects; copy those, don't invent one.

### Island 1 — Lighting (highest value, engine is finished)

The light engine is **exact against real vanilla**, verified cell-for-cell on a live
26.2 server: sky `0/24576` disagreements, block `0/12288`, `diff_column_light` block
`0/32768` + sky `0/32768`. Each has a negative control that genuinely fired (`5120`
suppressed-sky, `298` contaminated-world). This is the best-validated subsystem here.

**It reaches no pixels.** `lodestone-render`'s `build_batch` fills its light grid with
`UniformLight::pre_light_bridge()` — full-bright — at **3 sites**.

Fixing it is two-sided and **ordering matters**: the producer (`lodestone-client`
populating `MeshJob`'s light grid from `sections_and_light_at`) must land **before or with**
the consumer (retiring the 3 bridge sites onto the existing `WorldSectionLight`/`SkyDefault`
adapter). Retiring the bridge first turns the world black — worse than full-bright.

- **Trap:** full-bright and correct lighting *both* render a visible world, so "it still
  draws" proves nothing. Assert shadowed interiors are measurably darker than open sky.
- **Trap:** light section indexing is off-by-one **by design** — light section 0 is the
  boundary *below* the world, light section `i` covers block section `i-1`, 26 light
  sections for 24 block sections. `sections_and_light_at` takes an explicit `(n, n+1)`.
  "Correcting" this into alignment is a regression that looks like a fix.
- **Trap:** the Nether has no sky light, so `SkyDefault` must not be a blanket 15.

### Island 2 — Vanilla block textures (contradicts an explicit requirement)

The playable shell renders a **procedural colour atlas**, not vanilla textures.
`lodestone-shell/src/gpu.rs:393`, inside `RenderState::new`, unconditionally calls
`crate::blocks::build_atlas()` — a generated per-sprite base colour with a deterministic
dither so surfaces "read as textured". There is no vanilla path, no feature flag and no
fallback logic at that call site.

Meanwhile `lodestone-assets` carries a real `Atlas` / `AtlasBuilder` / `AtlasDefinition`,
and **`lodestone-render` already uses it** (`block_resolver.rs`, `texture.rs`, plus
`tests/model_census.rs` and `tests/live_gate.rs`). Vanilla assets are on disk:
`.cache/mc/26.2` (412 MB).

So this island is **narrow** — the loader exists, the consumer exists, and the two are
wired together on the render side. The shell simply never asks for it. Note
`cargo test -p lodestone-assets --lib` runs **0 tests**; the coverage lives in
`lodestone-render`'s tests, so don't read the empty result as "untested".

### Island 3 — UI surfaces (partially resolved)

Five surfaces are modelled and folded in `lodestone-game` — tab list, scoreboard,
container/inventory, boss bar, status effects. Only **status effects** has been proven
to pixels (`overlay_rasterises_to_pixels`: empty frame `0`, populated widget rect `2160`,
opposite corner `0`). The rest fold state correctly and draw nothing.

`impl-game` built a `Menus` aggregate routing the 7 container events through the proven
`ClientMenu::reconcile` seam — **consume it, do not rebuild it**. Two hazards it already
solved, both of which produce a plausible-looking *wrong* inventory rather than an error:

- Container packets are in **menu order**; `SET_PLAYER_INVENTORY` is **native order**.
  `ClientMenu::set_player_native` exists with a known-value guard asserting native slot 0
  lands at menu index **36**, not 0.
- Container size comes from **server truth** (`content_len - 36`), never a hand-written
  menu-type→size table.
- Slot layout differs per menu: window 0 is `0` result / `1..=4` craft / `5..=8` armour /
  `9..=35` main / `36..=44` hotbar / `45` offhand, while `Generic{n}` is `0..n` container /
  `n..n+27` main / `n+27..n+36` hotbar — **no armour, no offhand, hotbar not at 36**.

## Addendum — `PlayerLoaded` is encoded but never sent

`ClientAction::PlayerLoaded` exists (`lodestone-model/src/action.rs:290`) and v770 encodes
it (`adapter.rs:3225`). **Nothing produces it.**

Vanilla's server seeds a ~60-tick (~3 s) `clientLoadedTimeoutTimer` after join **and after
respawn**, and **silently ignores movement packets until it elapses** unless the client
zeroes it early (`ServerGamePacketListenerImpl.hasClientLoaded()`). Vanilla sends it
automatically with no game or UI dependency. We never do — so for the first ~3 s of every
session the server discards our movement, and any gate measuring movement in that window
measures nothing and returns a **false green**.

Three live gates work around this by sleeping ~5 s each, with comments asserting the
capability is absent (`live_physics_bot.rs:45` and `:242`, `live_second_observer.rs:317`,
`live_session.rs:110`). Those comments were true when written; the variant landed later.

- **Do not** strip the waits wholesale. `live_second_observer` waits on a genuinely
  different condition — the *observer* client receiving our entity — and collapsing the two
  yields a gate that passes on latency.
- The **`minecraft:brand` custom payload** is in the same state: encoded, tested, never sent,
  where vanilla sends it at join.

## Addendum — staleness is the most common defect here

**Five separate instances surfaced in a single session.** Every one was *true, evidenced
and correct when written*, then quoted or relied upon after the world changed underneath it:

1. A "~40 of 141 packets handled" metric quoted while ~50 packets landed (real: 91) — this
   steered an entire fleet of agents at the wrong bottleneck.
2. `lodestone-render` believing `handle.section_light` didn't exist. It does — this is what
   keeps the best-verified subsystem off screen (Island 1).
3. An assumption that a 158-type entity-geometry census existed. No such table exists
   anywhere; the thing named "census" is the *spawn* census for mob-cap.
4. A gate docstring asserting "no adapter emits any entity event", written before ~50
   packets landed.
5. Three test files asserting `ClientAction::PlayerLoaded` doesn't exist (above).

**Standing rule, and the cheapest safeguard available:** when work is gated on *"X doesn't
exist yet"*, **re-verify that X still doesn't exist** before routing around it. Staleness
needs its own check precisely *because* the original claim was honest and correct — nothing
about it looks wrong on inspection, which is why it survives review.

Corollary: prefer `cargo xtask connectedness` over any hand-derived coverage number. The
hand-derived version was wrong four times, in four different ways.

## Addendum — model-layer gap: `ItemStack` has no components

`lodestone_model::ItemStack` carries **no components** — no custom name, enchantments,
durability or max-stack. Drawing stays correct because container reconcile is
server-authoritative; only *offline click-stacking prediction* is affected. Needs a
component patch at the model event layer. Documented at the `From<&model::ItemStack>` impl.

## Addendum — neighbour-aware relight is singleplayer-only (divergence trap)

Caught during the lighting handover and worth preserving, because getting it wrong
produces a bug that **survives every test in the suite**.

On **multiplayer**, `merge_light` already carries the server's seam-complete,
cross-chunk-propagated light — the server has the whole region loaded, so its values are
authoritative and complete. Firing `compute_column_light_with_neighbours` on MP chunk
arrival would **overwrite server-authoritative light with our own partial recompute**.
That is a divergence bug, and it looks like a lighting *improvement* while it happens.

The trigger predicate is therefore **"chunk arrived AND we generated its light"** (i.e.
singleplayer / integrated server), **not** "chunk arrived".

When the SP relight does get wired, its trigger is **bidirectional**: on column `N`
arriving, relight `N` *and* every already-loaded orthogonal neighbour `A` — `A`'s facing
seam was baked while `N` was absent (treated as opaque), so it is now stale. Miss that
second half and you get a **permanent dark stripe on `A`'s face that never revisits**,
survives every geometry test, and only shows up visually.

A genuine loaded-edge (neighbour actually absent) staying dark is **correct**, and
self-heals when that neighbour streams in — provided the bidirectional trigger exists.

**What to watch:** `diff_column_light_full`'s split. Interior must stay `0`; an *edge*
count that **changes on neighbour arrival** is the fix working, not a regression.

**Routing, once light is visible and a seam artifact appears:** MP shadowed seams belong
to `merge_light` plumbing / mesher sampling; SP seams belong to the neighbour-aware
relight. Cost is ~12.5 ms/column, and the wire lives in the chunk-arrival/load path, not
in the mesher — the mesher only reads whatever merged light the handle exposes.

## Addendum — traps found during final wrap-up

### `UseItemOn` needs the 26.2 `world_border_hit` bool

`use_item_on` in v770 requires a trailing `world_border_hit` boolean added in 26.2.
**Without it the live server disconnects on decode.** This is a wire-format bug that only
a live server surfaces — a hermetic round-trip test passes happily, because our encoder and
decoder agree with each other while both disagreeing with vanilla. Fixed and covered by
`use_item_on_is_byte_exact`.

Generalisation worth keeping: for any packet where we control **both** sides of a
round-trip test, the test cannot detect a shared misunderstanding of the wire format. Only
a real server can. Prefer at least one live assertion per packet family.

### `V770ServerProtocol` must stay stateless (aliasing trap)

`IntegratedServer::bind` wraps the protocol in `Arc<P>` and clones **that same `Arc`** into
every accepted connection's spawned task. Any interior-mutable "last sent" state placed
inside `V770ServerProtocol` would therefore be **silently shared across independent
clients**.

This passes every test today, because singleplayer has exactly one connection — and
corrupts as soon as a second player joins. The fix already applied: keep the protocol a
zero-sized stateless unit struct and pass `(prev, current)` snapshots as parameters, with
the caller owning per-connection state:

```rust
fn encode_add_entity(&self, entity: &EntitySnapshot) -> ServerDirective;
fn encode_entity_update(&self, prev: Option<&EntitySnapshot>, current: &EntitySnapshot)
    -> Vec<ServerDirective>;
fn encode_remove_entity(&self, ids: &[i32]) -> ServerDirective;  // batched, matches REMOVE_ENTITIES
```

`encode_remove_entity` takes a slice and returns **one** directive because `REMOVE_ENTITIES`
is genuinely batched on the wire (VarInt count + VarInt ids) — one packet per id would be
valid but wrong-shaped.

### An empty UI panel may legitimately draw

The tab list's empty-state control is **not zero** — the empty panel itself renders a
background. Its pixel gate therefore asserts a populated-vs-empty **delta**
(`552 → 1380` bright pixels), not an absolute zero.

This matters because the intuitive gate ("expect 0 pixels when empty") **fails against
correct code**, and the natural response is to "fix" the renderer to satisfy it — removing
a background that vanilla actually draws. Check what the surface is supposed to look like
when empty before choosing between a delta assertion and an absolute one.

Surfaces proven to pixels so far, with their evidence:

| surface | populated | empty control | corner control |
|---|---|---|---|
| status effects | 2160 px in rect | 0 (whole frame) | 0 |
| tab list | 1380 bright px | 552 (panel draws) | — |
| scoreboard | 7216 px | 0 | — |
| container (chest) | 23271 px in rect | 0 | 0 |

## Addendum — THE BIGGEST ISLAND: live server terrain never renders

**Read this before believing any screenshot.** When the client is run with `--window --live`,
the terrain on screen is the **locally generated demo world**, not the server's. The live
connection streams events (entities, chat, health, sounds) correctly and independently, but
**the server's chunks are never meshed**.

Two independent causes, and the second is the real one:

1. `Sim::mark_column_dirty` early-returns for live columns; the meshing pipeline reads
   `self.world`, which is the singleplayer worldgen world.
2. **The shell meshes with `DemoClassifier`, whose palette is block ids `0..=9`**
   (`AIR`, `STONE`, `DIRT`, `GRASS`, `SAND`, `WATER`, `LOG`, …). A live 26.2 server streams
   *vanilla* block-state ids in the tens of thousands. `block(id)` returns `None` for all of
   them, which `DemoClassifier::classify` maps to a **non-occluding, surface-less cell** —
   i.e. air. Meshing live chunks through it renders **near-nothing**.

**The trap, and it is a serious one:** wiring live meshing *without* first swapping the
classifier produces a pipeline that runs, produces no geometry, and **passes a lighting gate
vacuously** — an empty world is trivially "not full-bright". Any gate over live terrain must
therefore first assert *non-trivial geometry exists* (quad count, coverage) before asserting
anything about its lighting.

**Correct order:** vanilla `state_id → sprite` classifier first (`impl-render`'s
`blocks_json_registry` + `BlockAtlas`, both landed and ready — see Island 2), then the
`mark_column_dirty` live rewrite. Lighting then rides along for free, because the light read
(`sections_and_light_at`) and column geometry (`world_dimensions`) are already wired.

**What is genuinely done:** singleplayer terrain is meshed *and correctly lit* as of `3870ae1`,
gated by `shadowed_meshes_darker_than_open_sky_and_the_bridge_cannot_tell` — a test whose
full-bright control renders shadowed and open cells identically at 255 and therefore fails.
That is real, verified lighting; it just currently applies to the generated world only.

### Correction to an earlier belief in this document

An earlier framing held that "multiplayer renders full-bright terrain". **That was wrong** —
MP terrain does not mesh at all. The distinction matters: "lit incorrectly" suggests a
lighting fix, while "not meshed" points at the classifier. Chasing the former would have
produced a vacuous green.

## Addendum — the obvious `player_loaded` live gate is vacuous (design note)

`PlayerLoaded` is now auto-sent (`06adf98`), policy-suppressible via `PlayerLoadedPolicy`
(`a9ae6a2`), and re-armed on **`ClientEvent::Respawned`** as well as `Death` (`6613751`) so
portal / dimension-change / `/respawn` no longer silently re-enter the ignore-movement
window. Hermetic coverage: `cargo test -p lodestone-client --test driver` → 20/20, including
`player_loaded_suppressed_under_manual_policy` and
`player_loaded_rearms_on_respawn_without_death`.

**The live gate is deliberately not written, and the intuitive design for it does not work.**

The tempting gate is: "move immediately after join with `PlayerLoaded` suppressed, observe
the server rubber-band us back." **It never fires.** When `hasClientLoaded()` is false,
vanilla **silently drops** `MovePlayer` packets — it does *not* send a correcting teleport.
So a lone walker observes **no correction at all** during the window: local prediction
diverges freely while the server keeps us at spawn. A correction only appears *after* the
window, when the accumulated catch-up move looks illegal (too fast).

A gate built on the naive design therefore passes whether or not the fix works — the classic
vacuous green.

**Two designs that actually work:**

1. **Second observer client** — confirm the *entity* did not move server-side during the
   window, from a different connection. This is the stronger option, because it observes
   authoritative state rather than inferring from our own packets.
2. **Assert on the post-window catch-up correction spike** — real, but timing-sensitive;
   gate carefully so it cannot pass merely on latency.

`PlayerLoadedPolicy::Manual` is the negative-control lever. Put the gate in
`live_physics_bot.rs` behind the `live-v770` feature.

**General lesson worth carrying:** "the server will correct us if we're wrong" is an
assumption, not a mechanism. Before building a gate on an expected server *reaction*,
confirm vanilla actually reacts — several of its validation paths **drop silently** rather
than responding, and a gate waiting for a response that never comes cannot distinguish
success from failure.
