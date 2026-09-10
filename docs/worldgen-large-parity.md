# Large worldgen parity harness

## What it is

`scripts/worldgen-oracle/LargeParityOracle.java` is the resumable, 251,001-chunk parity oracle for the 501 by 501 grid centred at `(0, 0)`. It freezes one generated reference world first, then records a full SHA-256 digest of each chunk's canonical semantic record; the old v2 raw 16-bit packet fingerprints are explicitly rejected.

## How it works

The reference seed is `42` and the target coordinates are `cx, cz = -250..=250`, for `501 × 501 = 251,001` chunks. A v3 record has a fixed semantic schema: chunk coordinates; heightmaps sorted by numeric type with 256 decoded heights each; 24 sections of resolved global block-state ids and biome ids; block entities sorted by relative position and type with recursively key-sorted NBT; and all 26 sky then block light sections, distinguishing missing, empty, and present 2048-byte arrays. Packet palettes, packet map traversal order, and packet framing do not enter the record.

For every compared coordinate, the Rust gate obtains the centre and its eight adjacent columns through the normal generation dispatcher, then calls the production neighbour-aware initial-chunk encoder. This is required for light too: an emissive block in a neighbouring column can illuminate the centre across its border, while the one-column encoder deliberately has no such input. The gate keeps that light in the canonical record; it never substitutes a synthetic or all-missing light payload. The frozen-world materialization halo supplies the corresponding settled neighbours on the reference side.

Raw P06 comparison replays the serial light-admission lifecycle, not the
read-only export batch size. The first target is the bootstrap admission and
sees only its centre as a light source. Each later centre uses the retained
block-light layers that existed before its admission, while the packet encoder
still receives the complete 3 by 3 terrain footprint. A generated dependency
whose light layer has not been admitted remains an opaque seam; its terrain is
not allowed to leak an emission or propagation result into an earlier centre.
After a centre is settled, missing queued dependencies receive their own
retained light and allocation records. Each newly initialized dependency is
settled from its own remapped three-by-three view of the terrain supplied by
the admission. Its fresh terrain emitters are active, while retained layers
are seeded only for already initialized columns; an unavailable outer column
remains an opaque seam. This lets a source in a dependency's future neighbour
contribute to that dependency without leaking through an uninitialized slot
into the original centre. When a dependency-initialized column
later becomes the centre, its retained block-light values are applied to the
new centre computation without copying the dependency's storage masks. The
new centre keeps its own `Missing` versus allocated representation, including
explicit zero layers, while the shared admission still computes and retains any
newly touched dependencies. Only the resulting centre snapshot becomes
eligible for the fast path. The deterministic materialization-order
control in `large_worldgen_parity.rs` guards this bootstrap-to-retained
transition.

End raw replay carries light-engine storage independently from terrain. Each
admission keeps the real three-by-three block footprint, seeds the flood from
any retained layers, keeps the centre snapshot separate from dependency
storage, and retains allocated-zero dependency layers separately before holder
eviction. A
later packet therefore distinguishes populated and allocated-zero layers; it
never substitutes an all-air column for missing light storage or resets the
layer map at a batch boundary. The authenticated controls cover an empty
dependency footprint and a later admission with retained northwest, north,
west, and centre layers plus an allocated-zero east layer.
Fresh End dependency layers use the computed allocation mask for the complete
admitted footprint: an all-air footprint keeps both layers Missing, while an
all-air selected column with admitted terrain keeps only that terrain-induced
vertical corridor. This control is coordinate-independent and does not encode
retention policy in the parity test.
The comparator replays those admissions through the production
`encode_chunk_with_source` seam, then exports from the same source; the test
does not maintain a parallel light-storage map. The generated End raw arm
explicitly flushes the `RegionChunkSource` through its `WorldSaveHandle`, drops
that source, reopens a fresh `RegionChunkSource`, preloads each target's full
three-by-three batch, and encodes through the same seam again. Its diagnostics
are labelled generated save/reopen parity and remain separate from the frozen
world import diagnostic below. The focused persistence control saves a centre
and its eight dependencies and requires the reopened source to report zero
generated columns and at least one disk load; it does not add a retention layer
to the test.

Set `LODESTONE_LARGE_PARITY_PHASE_PROFILE=1` on a bounded generated End run to
report reusable phase timings and counters. The profile separates manifest and
reference I/O, centre materialization, save, reopen, batch halo loads, target
loads, packet hashing, and the combined source-aware light-settlement/packet
encode seam. Each line includes wall time, calls, unique coordinates, cache
hits/misses, and source-generated/source-loaded deltas; the latter expose
discarded generated reads inside the production seam rather than counting only
the outer admission. For a content-only calibration that measures admissions
without settling light or encoding packets, also set
`LODESTONE_LARGE_PARITY_CONTENT_ONLY=1`. This diagnostic intentionally skips
save/reopen and exact comparison, so it cannot establish parity or persistence
correctness.

End raw comparisons bound this replay to the requested export prefix. Export
records are x-fastest/z, while admissions are tile-z/tile-x/z/x; for each
requested target, the comparator finds the latest admission whose centre or
one of its eight light dependencies can write that target's retained state,
then replays that exclusive admission prefix. Later admissions cannot affect
those targets. On the authenticated 51×51 End shard, prefixes ending at
export indices 890 and 1050 therefore require 1,631 and 1,646 admissions
respectively, instead of the full 53×53 halo's 2,809. The complete export
still uses the complete admission stream.

For a persisted-world import/encoder diagnostic, set
`LODESTONE_LARGE_PARITY_FROZEN_WORLD_ROOT` to a writable copy of the validated
sealed End world. The gate recomputes the oracle tree digest, checks the
dimension-specific freeze stamp and authenticated manifest identity, then
opens one fresh `RegionChunkSource` over the sealed root's `world/` persisted
Anvil columns. It reads
the x-fastest/z export prefix in batches of 256 by default (override with
`LODESTONE_LARGE_PARITY_PERSISTED_BATCH_SIZE`), requires zero generated-column
fallbacks, and encodes each retained centre through the production V770 End
encoder. This arm is labelled persisted-world import/encoder parity and keeps
its mismatch inventory separate from generated-world replay acceptance; a
persisted pass cannot establish generated-world parity. The batch loop models
the oracle's one-server sequential export: ticket removal changes residency,
not immutable sealed bytes, so the source is opened once rather than reset for
every batch. The minimal external controls are index 0 `(-25,-25)` and index
890 `(-2,-8)` in a `-25..25` shard; packet references can be supplied with the
existing reference-packet directory. A fresh reopen may legitimately load no
retained light for the first dark target, producing empty masks; later targets
can carry persisted sky/block layers and neighbour-derived masks. The
diagnostic therefore delegates missing-layer handling to the production
encoder and never infers masks from occupancy or performs an independent
target recomputation. Set `LODESTONE_LARGE_PARITY_PERSISTED_ONLY=1` to stop
after this diagnostic and avoid entering the generated replay arm. Set
`LODESTONE_LARGE_PARITY_PERSISTED_PACKET_OUT` to retain the first actual
persisted-arm mismatch packet for component decoding; the hook is bounded and
does not select coordinates.

The dimension-aware v4 format is additive. It keeps the same 501 by 501 bounds and 32-byte per-chunk digests, but authenticates one of the three dimension identities (`minecraft:overworld`, `minecraft:the_nether`, or `minecraft:the_end`) in header bytes `168..200` as SHA-256 of its resource-location string and uses that dimension's decoded window: overworld `min_y=-64` with 24 sections, Nether/End `min_y=0` with 16 sections. The v5 End format keeps that header shape but has new manifest and record domains. Its export reads the persisted settled light from each requested chunk in the sealed world copy; it does not reset or recompute light, or load an export-only neighborhood. Normal per-batch ticket removal is safe because the sealed world is not mutated during export. It also elides only a trailing all-15 sky-light layer, because an initial chunk enables its column after queueing supplied layers and a missing top sky layer resolves to the same full value. Mixed sky arrays and all block-light tags remain exact. This avoids treating allocation history as terrain while preserving a real light-value mismatch. V3 and v4 records remain readable and byte-for-byte unchanged; a merge or duplicate-read acceptance rejects different dimensions or semantic versions.

The workflow has two mandatory phases. `--mode materialize` generates the requested rectangle **plus its one-chunk halo** (the complete baseline therefore materializes `-251..=251`, or `253,009` chunks), completes the real chunk-status work, post-processes it, saves it, and writes a tree-digest seal. The launcher fixes the generation executor at one worker, pauses an empty server by default, and the oracle refuses to start if the worker setting is absent. A genuinely new root is marked initialized before the server thread starts, so its process-seeded startup spawn search cannot write an unordered 11-by-11 overlap; existing roots, including authenticated roots, keep their saved startup state. Materialization then visits centres in tile row-major order and admits them serially while retaining every earlier ticket in the tile. Before each admission it resets the level random source from the seed and coordinate, clears the nearest-biome accelerator on both the caller and generation worker, waits for the full result, drains scheduler and deferred-light work, and releases the tile as one fence. This is a deterministic construction contract, not a throughput knob: independent forward roots now agree on light-free records, while a reverse traversal remains an intentional negative control because cross-column feature writes are order-sensitive. It advances only the scheduler (never resident chunk ticks), checks that resident full chunks report correct light, and does not add an export-specific loading halo or change tile geometry. Each bounded epoch runs in a fresh container/JVM against the same persistent root; the shell drives the epochs automatically. This matters because the server retains point-of-interest section data beyond ticket removal, and an in-process server restart closes shared executors instead of yielding a reusable clean heap. A progress journal records the exact geometry, epoch size, next tile, and an in-flight tile range. Its `lodestone-large-parity-materialization-v2-<dimension>` filename and marker bind that journal and its seal to the serial, one-worker construction contract rather than the semantic record version. Earlier v3/v4 seals and journals are refused for every dimension because they may have been produced under concurrent construction; create a new empty root instead. It advances only after the prior JVM exited cleanly; an interrupted epoch, a changed range or epoch size, a missing journal, or an already sealed root fails closed rather than being resumed ambiguously. For the complete baseline, omit the range flags so the requested grid is `-250..=250` and its halo is `-251..=251` in both axes. `--mode export` accepts only that sealed world through a read-only mount, copies it into the container's ephemeral server-access directory, and exports semantic digests from the restarted persisted content. The manifest carries the frozen-world digest, schema digest, geometry, full digest width, and SHA-256 payload checksum; merge refuses a shard from a different frozen world. A root sealed before this startup and admission contract is not an acceptable baseline and must be regenerated.

This split prevents two independent failure modes. A generated chunk can receive a later neighbour feature write, so the old batch-immediate capture could observe transient state. Freezing before capture removes that lifecycle race; canonical records remain the semantic acceptance authority, while the raw diagnostic path now fixes packet-visible ordering before invoking the compiled codec.

`--packet-out` remains a one-chunk raw packet diagnostic. It is never used for the baseline, but is useful when the Rust comparison identifies a semantic mismatch and needs its existing detailed packet diff. Before encoding, the oracle rebuilds block-state and biome palettes by a fixed section traversal, orders heightmap entries and block entities explicitly, and recursively orders compound-tag keys while preserving list order. `LargeParityOracle --determinism-selftest` uses the compiled 26.2 packet codec on one sealed target: a deliberately reversed packet-order control must differ, while two stabilized encodes must have an empty differing-offset list. Set `ORACLE_DETERMINISM_X` and `ORACLE_DETERMINISM_Z` to choose the target; both default to zero.

The P06 raw-packet format is an independent opt-in path: pass `--raw-packet` (or `--format v6`) to materialize and export the 1001 by 1001 target grid `-500..=500`, with its complete 1003 by 1003 halo `-501..=501`. The materialization wrapper selects the matching v6 seal and does not confuse it with the v2 semantic provenance stamp. Each manifest record is the first two bytes of the SHA-256 digest of the exact stabilized compiled-codec packet body, emitted in x-fastest then z order. The manifest carries the v6 dimension, frozen-world and materialization identities; each export also writes a `.packet-audit` sidecar containing a matching identity header and one full 32-byte packet digest per coordinate. The sidecar is checked during resume and is required by `large-parity-manifest.py validate`, which authenticates its checksum and cross-checks every full digest's two-byte prefix against the main manifest. P06 does not reinterpret or regenerate v3-v5 semantic manifests.

Structure terrain adaptation is part of the authenticated full-world scope. Header bytes `70..72` are the explicit structure-beard scope field; value `0` means `production-real`, and every P06/P07 manifest and audit sidecar is emitted and accepted with that value. Thus Java and Rust compare generated structure terrain under the same production scope. The small composed stage fixture is a different diagnostic contract: its text record carries `beardScope empty` because that oracle intentionally omits structure starts. A composed fixture must not be used as a packet-baseline substitute, and changing the scope field is a fail-closed negative control rather than a valid way to explain terrain differences.

The comparator replays the initial-light admission boundary before it asks the packet codec for bytes. Materialization admits rows in z-major/x-major order, so a target's first saved snapshot sees only its north row and west cell; the packet may still carry all eight terrain neighbours after that snapshot is fixed. This keeps a future east or south column from changing an already-saved initial fallback, while later live relight remains free to use the complete footprint. The same relative rule is used for one-record diagnostics and bounded prefixes, so the control never depends on a particular world coordinate.

The live comparator models dependency completion separately from packet-stream order. Nether and End requests replay their admitted dependency wavefront with the request's tiled key (tile-z, tile-x, local-z, local-x); a later adjacent request schedules only the previously unseen edge. This is not the z-major order used to emit packet records, and it is not one universal three-by-three source permutation. Overworld streaming retains the admitted dependency state across bounded frame batches, matching the external stream's removal of only the requested centre ticket; replaying each halo afresh can invent a cross-target vegetation spill that the corresponding external record cannot contain. The `(0,-1)` wavefront fixture and the one-record Nether P07 glowstone witness continue to guard the halo lifecycle. Immutable shaped prefixes remain eligible for parallel preparation; only stateful decoration commits use these deterministic orders.

### Live streaming comparison

When a full frozen root is unnecessary, `scripts/worldgen-oracle/stream-parity.sh` connects the external JVM oracle to the Rust comparator through a temporary file stream. It admits the requested rectangle in z-major/x-fastest order and compares one light-free content record at a time: terrain state IDs, 4×4×4 biome cells, the three client heightmaps, and canonical block entities. Neither side constructs or compares light, so a one-chunk check does not pay packet encoding or light settlement, and no neighbouring columns are regenerated solely for comparison.

During content discovery, `LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS=1` reports and continues past a record whose only difference is a client heightmap. Terrain, biomes, and block entities must still match byte-for-byte. This is an iteration aid for the separately tracked incremental-heightmap lifecycle; it is never an acceptance mode.

Client heightmaps follow the replayed generation lifecycle rather than being
rescanned from a completed block field. A resident column primes its three maps
when its own FEATURES event begins. Feature writes then update a destination
only if that destination has already crossed the same boundary; writes that
arrived earlier are included when the destination later primes itself. This
distinction matters for cross-chunk vegetation and structure writes because the
final blocks alone cannot reveal whether a write preceded or followed map
initialization.

End placement admission is evaluated at every sampled origin. A source chunk
whose centre is not `minecraft:end_highlands` can still place a chorus plant
when a sampled position resolves to that biome, so source-centre biome checks
must not suppress the feature's RNG stream before its per-origin filter runs.

The stream is intentionally ephemeral. There is no ledger, corpus, resume prefix, or packet dump. The container appends to a disposable bridge file only because its shared-filesystem transport cannot carry the stream directly over a host FIFO. The launcher forwards that open file into a host FIFO, unlinks its pathname once the Rust reader is attached, and removes the surrounding temporary directory on every exit. Consequently a long scan retains only the producer's open stream plus the consumer's bounded frame batch; it never leaves a reusable baseline or named multi-gigabyte artifact. Defaults select `(0,0)`; pass `--cx` and `--cz` for a bounded batch. A mismatch fails with coordinate and digest information. Set `LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS=1` only when a first differing record offset is needed; normal mismatches do not rerun lifecycle materialization or write temporary packets.

The Rust comparator retains at most a bounded frame batch before generating and
comparing records. End defaults to 64 frames and calls
`EndChunkSource::generate_batch`, which deduplicates immutable dependency
windows while returning decorated columns in the stream's z-major/x-fastest
order. Overworld and Nether retain their default one-frame batch because their
feature lifecycle is an ordered read-after-write replay. Set
`LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE` to an explicit positive value when
running a controlled comparison; the launcher forwards it to the comparator.
Nether batches admit their complete two-chunk feature-write halo in z-major,
x-fastest order before replay, but emit records only for requested targets.
Overworld stream batches retain the shaped halo and completed centre source
bodies across frame batches. Each requested centre runs its FEATURES body once;
the surrounding admitted columns are lower-status dependencies only. A centre
may still leave a cross-boundary write in a resident neighbour, so retaining
those completed centre bodies preserves the external one-target admission
boundary without replaying a neighbour's grass/tree spill into a later record.
For example, a short End control is:

```text
LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE=64 \
  scripts/worldgen-oracle/stream-parity.sh \
  --dimension end --cx -5 5 --cz -5 5
```

The stream is an iteration aid, not a replacement for accepted LWP manifests. It uses the same v7 light-free record schema as the standalone content comparison while keeping the live path bounded and disposable. Use a small rectangle first:

```text
scripts/worldgen-oracle/stream-parity.sh \
  --dimension overworld --cx -2 2 --cz -2 2
```

The stream's binary header is `LWS26S01`; the format marker is `7` for light-free content. The launcher waits for a readiness marker before attaching the Rust reader, so a boot failure is reported instead of leaving it blocked. A zero-length frame is the normal end marker; the separate completion marker makes an early producer exit distinguishable from a complete stream.

The launcher forwards `LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS` and
`LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS` to the Rust comparator. Keep
both unset for an acceptance run; they are bounded first-mismatch aids and do
not change what the external side generates.

P07 is the light-free content path: pass `--light-free` (or `--format v7`) to export the same 1001 by 1001 `-500..=500` grid with two-byte main records and a required `.light-free-audit` sidecar of full 32-byte SHA-256 records. Its explicit schema is domain/coordinates/dimension, the three client heightmaps (registry ids `1`, `4`, and `5`), section block-state ids and biome ids, and sorted canonical block entities. It never constructs a packet, settles light, or includes light bytes. The sidecar is authenticated in lockstep with the main payload and binds the same seed, dimension, frozen-world identity, geometry, schema, and x-fastest ordering. P07 export accepts either a P07 seal or an existing authenticated P06 frozen-root seal, so the active 1001² content pass reuses the already materialized P06 root rather than requiring another world-generation pass. `full-parity-worker.sh` selects it with `LODESTONE_ORACLE_FORMAT=v7` (or `LODESTONE_ORACLE_LIGHT_FREE=1`); resume, merge, duplicate-read `accept`, and independent-root `reproducible` retain bounded streaming and mmap coverage checks. The P06 full-packet path remains unchanged for the later light comparison.

The external P07 controls are `python3 scripts/worldgen-oracle/large-parity-manifest.py selftest`, the ignored `java_and_rust_light_free_records_agree` one-chunk cross-language test, and `scripts/worldgen-oracle/light-free-read-control.sh`. The last control runs two fresh read-only exports from the same sealed P06/P07 root, compares both main and sidecar files, validates them, and hashes the source tree before and after to prove that a light-free read does not mutate the frozen root. A small export can be produced without packet diagnostics:

For a mismatching one-chunk cross-language control, set
`LODESTONE_LARGE_PARITY_RUST_RECORD_OUT` to retain Lodestone's canonical
light-free record beside the externally exported `--record-out` file. A binary
comparison can then attribute every differing four-byte state ID to its exact
section and local coordinate; the option does not affect either digest.

`light_free_target_matches_serial_lifecycle` accepts an authenticated P07
manifest plus `LODESTONE_LARGE_PARITY_TARGET_INDEX`. It reconstructs the
sealed world's 16-by-16 tiled admission order with the complete two-chunk
Nether feature-write halo, then replays only the selected target's causal
feature closure. The halo is lifecycle input; manifest iteration still emits
only requested target coordinates. This boundary matters for the first target
row, whose source effects can begin two chunks earlier. This control distinguishes a generator defect
from the isolated-column comparator accidentally discarding earlier neighbour
writes. The full streaming comparator must preserve the same lifecycle while
bounding retained rows; independent per-column generation is not equivalent.

```text
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/to/p06-root \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/light-free-pilot \
  scripts/worldgen-oracle/large-parity.sh --mode export --light-free \
  --out /oracle-out/pilot.lwp --cx 0 0 --cz 0 0
```

`large-parity-manifest.py merge` uses bounded external storage: it streams and authenticates one shard at a time, writes records into a fixed-size memory-mapped slot file, and tracks coverage with one bit per target coordinate. The final payload is read back in z-major rows with x-fastest records, so shard argument order cannot affect output bytes. A duplicate coordinate remains an overlap error and any unset coverage bit remains a gap error; payload checksums, frozen-world identities, dimensions, and schema digests are checked before the merged file is emitted. This keeps the P06 1,002,001-record merge independent of Python dictionary size while retaining the exact v3-v6 output format.

For the complete three-dimension run, use `scripts/worldgen-oracle/full-grid-p06.py`. It requires three distinct absolute world roots (`--overworld-root`, `--nether-root`, and `--end-root`) and one absolute output root (`--output-root`); every path must be outside the repository. The coordinator creates a small state file in the output root before starting, then runs the v6 materializer, two read-only export passes, a complete shard merge, duplicate-read acceptance, and the Rust comparator for each dimension in Overworld, Nether, End order. End uses 501 two-row shards and is always exported serially, even when `--workers` is greater than one. A rerun reuses the state file and each `--resume` shard; it never replaces an incompatible state file, world provenance, partial shard, merged manifest, or accepted manifest.

The coordinator performs disk, RAM, and batch-size preflight before invoking an oracle. The default floors are 8 GiB free disk and 4 GiB RAM; `--min-free-bytes`, `--min-ram-bytes`, and `--batch-size` make the operator's chosen limits explicit. The dry plan is useful for review and CI command-construction tests: it prints every materialization, export, merge, duplicate-read, validation, and comparator command without starting Java or Cargo.

Example (all generated state stays under `/private/tmp`):

```text
scripts/worldgen-oracle/full-grid-p06.py \
  --overworld-root /private/tmp/lodestone-p06-overworld \
  --nether-root /private/tmp/lodestone-p06-nether \
  --end-root /private/tmp/lodestone-p06-end \
  --output-root /private/tmp/lodestone-p06-outputs \
  --batch-size 256
```

Use `scripts/worldgen-oracle/tests/test-full-grid-p06.sh` and
`python3 -m unittest discover -s scripts/worldgen-oracle/tests -p 'test_full_grid_p06.py'`
for command construction and refusal controls. They use a dry plan and never run
the external oracle or the Rust comparator.

For a Nether P06 comparison, the Rust gate partitions the selected z-major,
x-fastest target prefix into deterministic one-row windows before wrapping the
source in its retained cache. Each window prepares only its target row plus the
packet-light halo and the wider immutable prefix read closure, compares every
record in manifest order, then calls `ChunkSource::reset_packet_replay` before
the next row. A full-width row therefore retains at most `1007 × 7 = 7,049`
pre-decoration entries rather than a `1007 × 1007` full-grid closure. This
changes retention only: target/admission order, generated bytes and the default
first-mismatch stop are unchanged. The pure window planner control proves that
the first two rows concatenate to the original target stream and that the
retention bound is independent of full-grid height. The ignored raw-byte seam
control compares one-shot preparation with preparation/reset across a row
boundary, including full packet digests.
For a Nether P06 comparison, the Rust gate generates the terrain columns needed
by the serial admission prefix and then applies a persistent light store. The
packet's 3×3 light neighbourhood is independent of read-only export batch
selection. Worldgen preparation and cache-retention controls remain separate
from the packet lifecycle and must not alter the authenticated packet bytes.

The section-header non-empty count is a wire field, not the version-neutral
column occupancy count. `packet_non_empty_block_count` excludes `air`,
`cave_air`, and `void_air` by resolving each emitted state to its block kind;
the section payload still carries those states. `ChunkSection::non_air_count()`
is therefore not suitable for this field. Keep this count separate from the
fluid count. Focused encoder controls read the header directly and then decode
the payload, so a zero count cannot hide an omitted cave- or void-air state.

### Lifecycle replay for the accepted 16 by 16 manifests

The accepted partial manifests at `/private/tmp/lodestone-worldgen-parity-overworld-16-outputs/overworld-partial.lwp` and `/private/tmp/lodestone-worldgen-parity-nether-16-outputs/nether-partial.lwp` have an independent full-run lifecycle capture beside them at `/private/tmp/lodestone-worldgen-lifecycle-capture-20260907-r1/out`. The Rust gate authenticates `provenance.txt`, the 324-row `replay.tsv`, and the 4,608-row `replay-completion-order.tsv` before generating a packet. The capture schema is `lodestone-worldgen-lifecycle-capture-v1`, with seed `42`, a one-chunk halo, and tile-z-major/tile-x-major admission order. The accepted manifest SHA-256 values are `54f3a7e62ed8dbd0d976a27eefef64f6d11152d3e26b94e162e2561192f81071` (Overworld) and `cb4d341f6826ebf7bce49ee195618d48e314729b6bd4251b3b97124e4f948789` (Nether). The canonical replay-sequence SHA-256 is `4e2eeb0217c06e7ed68e3976df04ebef5648ff1b14516141c49095d8ef15498f`; the source-capture SHA-256 is `b47516b6f47ec74aba6201cd8d54401deb12edf94cc4272c0dd9c2b52845f9a3`; and `replay.tsv` is `b8678c8a8b847a94d82bba31c9250aeace0d43e02fc309c923838e327c209986`. The Overworld completion file is `4af54ab035bc7febad74da7a6dffdd9c79a6b9e10c90a164523d98c5e995b1e0`; the Nether completion file is `7115c80a42320ed2ca7c3b8fe7160ea4516cdc6436a10633e212f405f2f0b52a`.

The completion rows repeat observations for every target, so the parser first deduplicates one global event per `(source, stage, completion_seq)`. It requires exactly one successful `FEATURES` event for each of the 324 replay admissions, verifies that its source and admission index agree, and rejects any inversion between `FEATURES` completion-sequence rank and replay admission order. `LifecycleMaterializer` enforces the stricter one-application identity `(source, stage)`; a changed completion sequence remains diagnostic telemetry and is rejected before the source body can run again. Effects are then applied once per source in authenticated `replay.tsv` admission order. The complete 18 by 18 halo is shaped before any feature body runs, so an early source may write to a neighbour whose explicit ticket appears later; independent shaped columns may use the process-wide worker dispatcher, but their results are committed in capture order. Every later write remains in the mutable resident state and read-after-write override map. For Overworld, each source's production top-layer spill pass then runs with those current overrides immediately after its `FEATURES` result and before the next admission; Nether and End have no corresponding stage. End uses the same materializer rather than rebuilding a pristine target-centred 3 by 3 world: its source adapter adopts the production immutable terrain/structure prefix, primes client heightmaps at each source's own FEATURES boundary, applies source-filtered absolute writes in order, and retains return-gateway sidecars in their destination columns. `FULL` and `before_fence` are parsed as telemetry diagnostics only: they do not establish causal ordering or freeze a target. After the FEATURES bodies and any source-local top-layer passes finish, the gate snapshots and encodes the targets and accepts only a digest match. The reusable `LifecycleMaterializer` and source adapters live in `lodestone-worldgen-parity::lifecycle`; the ignored comparator owns only capture authentication and manifest comparison. The adapters invoke the production shaped-prefix and source-filtered decoration dispatchers, including their read-after-write override inputs and generated sidecars.

The capture is deliberately external and is not checked into the repository. Its provenance fields and fixed digests prevent silently replaying a changed artifact; the accepted root freeze digests are `ade151a2bd6a5840c0548d70dd763a3f5060b301fbf2b2cf043af5de365ea4e8` (Overworld) and `c56e42d8ac751d348ff8461b4284c783f24702437039b682b37dca426b497048` (Nether), and the accepted manifest digest is checked against the manifest supplied to the test as well.

`LifecycleReplayPlan` is a static per-target optimisation over that authenticated full replay. It walks events backwards from the packet's 3 by 3 light domain, selecting direct writers within Chebyshev distance two and recursively including earlier mutable dependencies within the audited distance-four bound. Destination admission is a separate clipped closure. Events remain in authenticated order, source-local top-layer work remains immediately after FEATURES, and spills are applied only to admitted destinations. The Nether adapter computes only the unique pre-decoration columns in the selected closure rather than the full replay's closure. The ignored `full_and_pruned_lifecycle_replay_have_identical_target_packet_bytes` control compares raw packet bytes for the audited corner using exactly its 3 by 3 packet-light neighbourhood; its audited closure counts remain a control on the plan itself, not an assumption in the production comparator. The streaming comparator selects this pruned plan automatically for a one-chunk fail-fast prefix (`LODESTONE_LARGE_PARITY_MAX_CHUNKS=1` with `LODESTONE_LARGE_PARITY_SCAN_ALL` unset). `LODESTONE_LARGE_PARITY_TARGET_INDEX` is the explicit arbitrary-target form: it selects one zero-based row from the authenticated 16 by 16 manifest, seeks directly to that 32-byte digest, and builds the plan for `capture.target_order[index]`. It requires the 16 by 16 Overworld or Nether lifecycle manifest, cannot be combined with scan-all, and accepts only an unset limit or `LODESTONE_LARGE_PARITY_MAX_CHUNKS=1`; invalid, out-of-range, or ambiguous combinations fail closed. Multi-target and scan-all runs retain full replay as acceptance authority. End manifests never enter this lifecycle-only branch and use the retained-source comparator. The lifecycle unit negative control mutates event order and observes plan construction reject it; geometry, event count, and order mismatches fail closed.

For Overworld replay, `prepare_lifecycle_replay` creates one bounded slot per authenticated admission. A FEATURES completion lazily fills its slot with only immutable 5 by 5 pre-ore handles, stitched heights, biome eligibility, and source feature/ore selections; resident overrides, placement writes, and RNG state remain per-completion and stay serial. Ordinary production generation does not retain these cloned selection contexts for every explored chunk. The byte-identity comparator remains the authority: enabling the prepared slots must not change packet bytes or the authenticated event order. Lifecycle controls compare serial and dispatched shaped admissions byte-for-byte, while a deliberately reversed FEATURES commit is required to change a spill-dependent control state; this catches accidental completion-order semantics.

An out-of-rectangle spill is not copied into a resident packet column, but it is still retained in the shared read-after-write override map. A later source can therefore observe the spill through its wider feature context without causing an unadmitted destination chunk to enter the final packet set.

To run the bounded lifecycle gate:

```text
LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/overworld-partial.lwp \
LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT=/private/tmp/lodestone-worldgen-lifecycle-capture-20260907-r1/out \
LODESTONE_LARGE_PARITY_MAX_CHUNKS=1 \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity \
parity_manifest_streams_before_rust_comparison -- --ignored --nocapture
```

To inspect an arbitrary target without replaying the other 323 lifecycle
admissions, set its zero-based manifest index. For example, Nether index `7`
is `(-1,-8)` in the accepted target order:

```text
LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/nether-partial.lwp \
LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT=/private/tmp/lodestone-worldgen-lifecycle-capture-20260907-r1/out \
LODESTONE_LARGE_PARITY_TARGET_INDEX=7 \
LODESTONE_LARGE_PARITY_MAX_CHUNKS=1 \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity \
  parity_manifest_streams_before_rust_comparison -- --ignored --nocapture
```

The target index is a selector, not a request to consume a prefix: the reader
authenticates the complete payload first, then seeks to and reads exactly the
selected digest. This keeps the manifest acceptance check intact while making
the replay cost depend on the selected target's closure.

`nether_target_index_7_full_and_pruned_packet_bytes_match` is the corresponding
raw-byte identity control for index `7`. It is ignored because its full side
replays all 324 admissions; run it only when the long-running control has been
reviewed.

The default is fail-fast at the first target digest mismatch. `LODESTONE_LARGE_PARITY_SCAN_ALL=1` consumes the bounded prefix and retains the existing packet component report, while `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT` writes the bounded coordinate/state evidence. The Nether invocation uses its Nether partial manifest and the same capture root; set `LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE` to one dimension-specific capture directory when a root contains more than the two named directories.

The lifecycle capture's `targets.tsv` intentionally stores packet hashes and sizes, not packet bodies. The Rust gate does not use that file as acceptance input: the sealed manifest digest is the authority. To obtain a reference body for the first Nether target for component diagnosis, export that one chunk from the supplied accepted root into a mounted output directory, then pass the resulting packet to the Rust gate:

```text
mkdir -p /private/tmp/lodestone-worldgen-reference-packet
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/private/tmp/lodestone-worldgen-parity-nether-16-v4 \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/lodestone-worldgen-reference-packet \
LODESTONE_ORACLE_DIMENSION=nether \
  bash scripts/worldgen-oracle/large-parity.sh --mode export \
  --out /oracle-out/nether-one.lwp --cx -8 -8 --cz -8 -8 \
  --packet-out /oracle-out/nether-reference.packet

LODESTONE_LARGE_PARITY_MANIFEST=/private/tmp/lodestone-worldgen-parity-nether-16-outputs/nether-partial.lwp \
LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT=/private/tmp/lodestone-worldgen-lifecycle-capture-20260907-r1/out \
LODESTONE_LARGE_PARITY_REFERENCE_PACKET=/private/tmp/lodestone-worldgen-reference-packet/nether-reference.packet \
LODESTONE_LARGE_PARITY_MAX_CHUNKS=1 \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity \
  parity_manifest_streams_before_rust_comparison -- --ignored --nocapture
```

The accepted-root export is only diagnostic evidence; the manifest digest remains the acceptance authority. The lifecycle `targets.tsv` hash is not a final-state acceptance signal and its body is not retained, so it must not be confused with the accepted-root packet used for component diagnosis. A failing one-target run reports the first final-state digest mismatch and, when a reference packet is supplied, exact terrain, biome, heightmap, block-entity, sky-light, and block-light component differences.

## How to change it

The Java exporter and Rust comparator must change together whenever a semantic field changes. Bump the schema/domain strings and manifest version rather than reinterpreting existing data. Keep decoded ids in their existing packet-cell order, sort only collections without semantic order (heightmaps, block entities, compound keys), and keep NBT lists in order. The raw diagnostic encoder additionally stabilizes packet palettes before constructing the packet and uses insertion-ordered packet maps; preserve that boundary when changing packet diagnostics. For v4/v5, pass the dimension identity through packet decoding and source selection; do not infer it from a column's height because Nether and End intentionally share a window. V5's light normalization is only for an initial chunk's trailing full-sky tail; never apply it to block light, a mixed sky array, or a later light-only update.

The Rust gate in `crates/versions/26.2/tests/large_worldgen_parity.rs` generates a complete 3 by 3 local input, sends it through the neighbour-aware production initial-chunk encoder, then decodes the resulting packet and applies the schema-selected canonical record before comparing the full digest. Its north-neighbour glowstone control proves that the old one-column path has zero block light at the centre border while the neighbour-aware path carries the expected value. The test support reader authenticates the entire manifest before generation; for P06 it also authenticates the complete two-byte payload and its required full-digest packet-audit sidecar, rejecting a matching prefix whose full digest differs. Its v2 refusal is intentional: a short raw hash cannot be converted into a semantic hash.

When extending lifecycle replay, preserve the separation between capture facts and generator behavior: add a provenance field and a fixed digest for each new external stream, validate the 324 admissions and the one-FEATURES-per-source admission mapping, reject completion-sequence inversions, and deduplicate global completion events before dispatch. Add the corresponding override argument to the production source dispatcher so a later feature reads the resident state produced by earlier events, and carry generated block entities through the production column boundary in event order. Keep the reusable source/materializer behavior in `lodestone-worldgen-parity::lifecycle`; the version-specific comparator should retain only capture parsing and acceptance reporting. Do not use target fences or `targets.tsv` as final-state acceptance. Keep the first final-manifest mismatch and its component report as the acceptance evidence.

Run a small persisted control before any broad job. This proves both the frozen-world seal and duplicate read-only export:

```text
mkdir -p /absolute/path/parity-world
LODESTONE_ORACLE_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize --cx -8 7 --cz -8 7
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode export --out /oracle/pilot-a.lwp --cx -8 7 --cz -8 7
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode export --out /oracle/pilot-b.lwp --cx -8 7 --cz -8 7
cmp scripts/worldgen-oracle/pilot-a.lwp scripts/worldgen-oracle/pilot-b.lwp
python3 scripts/worldgen-oracle/large-parity-manifest.py validate scripts/worldgen-oracle/pilot-a.lwp scripts/worldgen-oracle/pilot-b.lwp
```

When changing the semantic schema, also run the cross-language control. It makes Java emit one packet body and its un-hashed canonical record, then requires Rust to decode that same packet into byte-identical record bytes and the manifest digest.

```text
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode export --out /oracle/cross.lwp \
  --packet-out /oracle/cross.packet --record-out /oracle/cross.record --cx 0 0 --cz 0 0
LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET=/absolute/path/to/cross.packet \
LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_RECORD=/absolute/path/to/cross.record \
LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST=/absolute/path/to/cross.lwp \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity \
  java_and_rust_canonical_records_agree -- --ignored
```

Only after this control passes, materialize the full grid twice from **separate empty roots**. Export and merge each root independently, then require `reproducible` to authenticate equal semantic payloads before either root can contribute an accepted baseline. This gate intentionally permits the two frozen-world identities to differ, but requires each input to be the complete 501 by 501 grid and rejects schema, dimension, geometry, checksum, and payload drift.

```text
python3 scripts/worldgen-oracle/large-parity-manifest.py reproducible /absolute/path/full-materialized-a.lwp /absolute/path/full-materialized-b.lwp
```

After that gate passes, select one of those reproducible roots and export it twice into distinct shard directories. `accept` then proves its read-only export is repeatable. `full-parity-worker.sh` divides work into 32-chunk-wide shards, refuses to start without `LODESTONE_ORACLE_FROZEN_WORLD_ROOT`, and uses `LODESTONE_ORACLE_SHARD_DIR` to choose its relative output directory. Legacy overworld shards remain directly below that directory; v4 Nether/End shards add a dimension subdirectory so existing v3 merge globs continue to work. End v5 workers must run sequentially: simultaneous reference servers can alter scheduler timing enough to invalidate the persisted-light reproducibility contract even when memory and disk capacity are sufficient. Other formats may use parallel workers only after the host has measured enough space for their independent copies.

```text
LODESTONE_ORACLE_WORLD_ROOT=/absolute/path/parity-world \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize
for read in baseline-read-a baseline-read-b; do
  for worker in 0 1 2 3; do
    LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/parity-world \
      LODESTONE_ORACLE_SHARD_DIR="$read" \
      bash scripts/worldgen-oracle/full-parity-worker.sh "$worker" 4
  done
done
python3 scripts/worldgen-oracle/large-parity-manifest.py merge --out /absolute/path/full-read-a.lwp /absolute/path/baseline-read-a/shard-*.lwp
python3 scripts/worldgen-oracle/large-parity-manifest.py merge --out /absolute/path/full-read-b.lwp /absolute/path/baseline-read-b/shard-*.lwp
python3 scripts/worldgen-oracle/large-parity-manifest.py accept --out /absolute/path/full-v3.lwp /absolute/path/full-read-a.lwp /absolute/path/full-read-b.lwp
LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/full-v3.lwp \
  LODESTONE_LARGE_PARITY_REQUIRE_FULL_GRID=1 \
  cargo test -p lodestone-v26-2 --test large_worldgen_parity -- --ignored
```

`LODESTONE_LARGE_PARITY_MAX_CHUNKS` makes the Rust gate compare a bounded prefix. By default the gate fails at the first digest mismatch and, when set, includes the existing single-packet diagnosis. Set `LODESTONE_LARGE_PARITY_SCAN_ALL=1` to consume the whole selected prefix before failing and report the complete chunk-coordinate set rather than stopping at the first light difference. In scan-all mode, `LODESTONE_LARGE_PARITY_REFERENCE_PACKET` supplies one external packet body; `LODESTONE_LARGE_PARITY_REFERENCE_PACKET_DIR` supplies a directory of `reference*.packet` bodies, keyed by their decoded chunk coordinates. Inputs must be regular files below the diagnostic size bound and must decode inside the selected manifest prefix; stale or out-of-prefix captures are rejected. Every supplied pair is decoded and compared independently for terrain states, biomes, heightmaps (with map keys ordered canonically), block entities, sky light, block light, and present/empty/missing masks. The packet aids are unauthenticated local evidence; the manifest digest remains the acceptance authority. Mask rows are diagnostic-only because v5 canonicalization may intentionally elide a trailing full-sky layer. Set `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT` to retain the full coordinate/value report (with bounded examples per component); the test failure remains a compact count summary. `LODESTONE_LARGE_PARITY_PACKET_OUT` retains the first raw packet only for mismatch diagnosis. `LODESTONE_LARGE_PARITY_PACKET_OUT_DIR` is the bounded multi-packet equivalent: it is raw-packet-only, creates or requires an empty directory, and writes every selected actual packet as `x{chunk_x}_z{chunk_z}.packet`, including packets emitted before a fail-fast mismatch. The directory is limited to 4,096 packets and 64 MiB total; use a new empty directory for each run.

`LODESTONE_LARGE_PARITY_START_INDEX` selects the first zero-based x-fastest/z export record of a contiguous comparison window. Pair it with `LODESTONE_LARGE_PARITY_MAX_CHUNKS` (or the scan-all-only `LODESTONE_LARGE_PARITY_BATCH_SIZE`) for the window length; for example, `START_INDEX=890 MAX_CHUNKS=160` compares records `890..1050` and still replays the causal generation/light prefix through record 1049. The manifest and packet-audit readers seek directly to the selected payload, while the production source retains the earlier replay state. A `MAX_CHUNKS` range fails fast at its first mismatch unless `SCAN_ALL=1` is set; `BATCH_SIZE` retains its existing scan-all requirement. Starts beyond the authenticated count, overruns, lifecycle target selection, and an unbounded start are rejected.

The fastest fail-fast invocation for a raw manifest window is:

```text
LODESTONE_LARGE_PARITY_MANIFEST=/private/tmp/lodestone-worldgen-v6-end-51-export-a/end-51x51.lwp \
LODESTONE_LARGE_PARITY_START_INDEX=890 \
LODESTONE_LARGE_PARITY_MAX_CHUNKS=160 \
cargo test -p lodestone-v26-2 --test large_worldgen_parity parity_manifest_streams_before_rust_comparison -- --ignored
```

For a multi-thousand-chunk issue inventory, set `LODESTONE_LARGE_PARITY_BATCH_SIZE` to an integer of at least `2000` and set `LODESTONE_LARGE_PARITY_SCAN_ALL=1`. The batch size must not exceed the authenticated manifest count and, when `LODESTONE_LARGE_PARITY_MAX_CHUNKS` is also present, the two values must agree. The comparator still authenticates the complete manifest payload before selecting the batch, holds one expected digest and one generated packet at a time, and records every mismatching target in the diagnostic output. If authoritative packet bodies are supplied for the batch, the report additionally groups retained terrain, biome, heightmap, light, mask, and block-entity signatures with their target coordinates; packet bodies are indexed by path and loaded one at a time, with an aggregate 64 MiB diagnostic input bound. Per-packet signature and example caps keep that optional census bounded. A digest-only batch remains a valid acceptance comparison, but it cannot claim a component-level cause without an independent packet body.

`scripts/worldgen-oracle/diagnose-parity.sh` automates the raw-packet form of this workflow. It performs one authenticated scan, writes a bounded mismatch inventory containing the manifest and packet-audit identities plus each expected and generated full digest, and selects only those coordinates for the second phase. The first phase may retain generated packet bytes for the selected batch, but the helper copies only mismatching bodies into its bounded working set. The JVM diagnostic mode then reads the sealed root once and exports reference packet bodies for those coordinates in batches; it never exports the surrounding matching chunks. A file-only Rust pass authenticates both packet digests and clusters exact terrain, biome, heightmap, block-entity, sky-light, block-light, and storage-mask signatures. The manifest and sidecar remain the acceptance authority throughout; the inventory, packet directories, and report are disposable diagnostic evidence. The helper refuses stale inventory identities, duplicate or out-of-grid coordinates, sidecar tampering, more than 4,096 mismatch bodies, and more than 64 MiB of packet evidence.

For example, a bounded 2,000-target raw scan for one dimension is:

```text
LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/accepted.lwp \
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/sealed-world \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/parity-diagnostics-output \
LODESTONE_ORACLE_DIMENSION=nether \
  scripts/worldgen-oracle/diagnose-parity.sh
```

The command currently targets the authenticated P06 packet path. Light-free P07 records intentionally remain a separate content comparison because they do not contain a packet light section to decode; their mismatch inventory is still useful for selecting a later record-component adapter.

The local negative controls for the inventory and sealed-root binding are:

```text
python3 -m unittest discover -s scripts/worldgen-oracle/tests -p 'test_parity_diagnostics.py'
```

The legacy scout files under `/private/tmp/lodestone-parity-scout-*` use the rejected `LWP26P02` raw-fingerprint format and are not inputs to this gate. To produce a fresh authenticated shard after a sealed dimension root is available, use the wrapper below. Its default range is exactly 2,000 targets; pass `--cx`, `--cz`, and `--out` to choose another rectangle or output name:

```text
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/to/sealed-world \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/lodestone-parity-batch \
  scripts/worldgen-oracle/batch-parity.sh \
  --dimension overworld \
  --out overworld-2000.lwp
```

The wrapper invokes the existing read-only exporter and then `large-parity-manifest.py validate`; for P06 that validation also requires the adjacent `.packet-audit` sidecar and authenticates its full-digest payload. It estimates a 2,000-target export at the exporter’s measured shard rate, while the Rust comparison time is workload-dependent. It refuses ranges below 2,000, ranges outside `-500..500`, missing paired bounds, path traversal, and any output that the authenticated v4/v5/P06 reader rejects. Run the comparator against the resulting shard with `LODESTONE_LARGE_PARITY_BATCH_SIZE=2000`, `LODESTONE_LARGE_PARITY_SCAN_ALL=1`, and `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT=/private/tmp/lodestone-parity-batch/report.txt`.

## Configuration

`NetherPlacementOracle` replays only a placed feature's modifier pipeline
against an authenticated frozen Nether world. Set `ORACLE_SOURCE_X`,
`ORACLE_SOURCE_Z`, `ORACLE_PLACED_FEATURE`, `ORACLE_FEATURE_INDEX`, and
`ORACLE_FEATURE_STEP` to inspect candidate origins and RNG order without
mutating the sealed baseline. `run.sh` compiles it with `LargeParityOracle`,
whose existing server bootstrap and frozen-root validation it reuses.
Set `LODESTONE_ORACLE_TRACE_NETHER_PLACEMENT=1` while materializing a disposable
reference root to place the instrumented `CountOnEveryLayerPlacement` ahead of
the server jar. It logs every attempt for `LODESTONE_ORACLE_SOURCE_X` and
`LODESTONE_ORACLE_SOURCE_Z`; never use an instrumented run as an accepted
baseline until its output has been checked against an uninstrumented control.

`LODESTONE_ORACLE_BATCH` controls only the bounded read-only export batch; materialization is intentionally one centre at a time. `LODESTONE_ORACLE_EPOCH_TILES` controls the clean-JVM materialization epoch in 16 by 16 tiles and defaults to `32` (at most 8,192 nominal chunks before edge clipping). It is durable provenance: keep it unchanged while resuming a materialization. `LODESTONE_ORACLE_PAUSE_WHEN_EMPTY_SECONDS` defaults to `1` for `large-parity.sh` materialization and may be set explicitly when diagnosing startup; it fences resident server ticks after readiness. `LODESTONE_ORACLE_TRAVERSAL=reverse` is a disposable negative control only and must not produce an accepted root; unset or `forward` is the canonical construction order. `LODESTONE_ORACLE_WORLD_ROOT` is a writable host directory mounted at `/world` only for materialization. `LODESTONE_ORACLE_FROZEN_WORLD_ROOT` is mounted read-only at `/frozen` for export and for `--determinism-selftest`. The source seal is checked before it is copied into the ephemeral server-access directory. `--raw-packet` or `--format v6` selects the P06 1001-square geometry and its v6 materialization contract; without it, the existing v3-v5 semantic defaults remain. `LODESTONE_LARGE_PARITY_PACKET_OUT_DIR` selects the empty, bounded directory for retaining the Rust comparator's actual raw packet bodies; it is rejected for semantic manifests and is independent of the authoritative reference-packet directory. `ORACLE_DETERMINISM_X` and `ORACLE_DETERMINISM_Z` select the selftest target and default to `0`. `scripts/worldgen-oracle/reproducibility-control.sh --side 21` (or `--side 51`) leaves two independent forward roots and a reverse-order negative control under an explicit work directory, comparing only authenticated payload records. `LargeParityOracle --provenance-selftest` proves that a legacy v3 seal cannot export and a legacy v4 progress file cannot resume; `LargeParityOracle --determinism-selftest` proves the reversed-order negative control and repeated stabilized packet equality. The format versions of their manifests remain unchanged, but their construction provenance is not accepted.

The lifecycle gate uses `LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE` for one explicit capture directory or `LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT` for the `overworld-full-accepted-sequence` and `nether-full-accepted-sequence` subdirectories. `LODESTONE_LARGE_PARITY_MAX_CHUNKS` bounds the final target prefix after the full 324-source replay, `LODESTONE_LARGE_PARITY_SCAN_ALL` changes fail-fast to bounded diagnostics, and `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT` retains the report. These variables do not alter the authenticated admission-order replay. Before Nether admission, the comparator derives a replay-only pre-decoration cache capacity from the admitted rectangle plus its 5x5 read radius; the production cache remains at its smaller demand-ordered bound. The control reports 484 unique pre-decoration computations for the accepted 18x18 capture, so cache retention changes cost only and cannot change source order or packet bytes.

The manifest tools accept `validate`, `merge`, `reproducible`, `accept`, and `selftest`. `reproducible` is the independent-root gate: it requires both manifests to cover the complete grid and have equal schema, dimension, and payload while deliberately allowing different frozen-world identities; for P06 it also requires equal authenticated packet-audit payloads. Use it on the two complete merged materializations before `accept`. `accept` is the final read-only export gate: it requires two complete byte-identical exports from one frozen-world identity and carries the P06 sidecar beside the accepted manifest. `validate` never treats `.packet-audit` as a standalone manifest: it derives the sidecar from each v6 main path, rejects missing or malformed sidecars, and has controls for sidecar checksum tampering and record misalignment. `selftest` covers full-grid merge ordering, independent-root payload equality plus bit-flip/incomplete/schema negatives, duplicate-read acceptance, payload tampering, missing/tampered/misaligned P06 sidecars, different-world and different-dimension merge/accept refusal, schema rejection, and v2 rejection. Existing v3 overworld and v4 dimension exports continue to validate and merge byte-for-byte. End exports now select v5 automatically and must be kept separate from v4 shards; accepted v4 artifacts are not regenerated by this migration. The `batch-parity.sh` wrapper is an authenticated 2,000-plus target export path; it does not upgrade legacy raw fingerprints.
`LODESTONE_LARGE_PARITY_LIGHT_TRACE_ADMISSIONS` enables a bounded Nether admission trace for the first requested admissions, with a hard cap of 64. Each record reports the centre and its eight relative dependency slots before the light callback, every returned settlement entry, and the committed source state; populated sections include their sky/block state, raw-array SHA-256, non-zero count, maximum value, and allocation metadata. Set `LODESTONE_LARGE_PARITY_LIGHT_TRACE_OUT` to write the trace to a file; otherwise it is written to stderr. A zero limit or an unset limit disables the trace, and the trace never changes materialization or packet output.

The manifest tools accept `validate`, `merge`, `reproducible`, `accept`, and `selftest`. `reproducible` is the independent-root gate: it requires both manifests to cover the complete grid and have equal schema, dimension, and payload while deliberately allowing different frozen-world identities. Use it on the two complete merged materializations before `accept`. `accept` is the final read-only export gate: it requires two complete byte-identical exports from one frozen-world identity. `selftest` covers full-grid merge ordering, independent-root payload equality plus bit-flip/incomplete/schema negatives, duplicate-read acceptance, payload tampering, different-world and different-dimension merge/accept refusal, schema rejection, and v2 rejection. Existing v3 overworld and v4 dimension exports continue to validate and merge byte-for-byte. End exports now select v5 automatically and must be kept separate from v4 shards; accepted v4 artifacts are not regenerated by this migration. The `batch-parity.sh` wrapper is an authenticated 2,000-plus target export path; it does not upgrade legacy raw fingerprints.

P07 uses `--light-free` or `--format v7`; `LODESTONE_ORACLE_FORMAT=v7` and `LODESTONE_ORACLE_LIGHT_FREE=1` select it for sharded workers. Its export accepts a matching v7 seal or the v6 seal for the same dimension, while an optional v7 materialization uses the separate v7 construction contract. `LODESTONE_LARGE_PARITY_LIGHT_FREE_AUDIT` selects a comparator sidecar path; otherwise the manifest's `.light-free-audit` suffix is required. Dimension roots are independent worlds and must never share a materialization directory. Start a Nether or End materialization with an empty writable root (the command resumes clean epochs until its dimension-specific seal appears):

```text
mkdir -p /private/tmp/lodestone-nether-501-seed42
LODESTONE_ORACLE_WORLD_ROOT=/private/tmp/lodestone-nether-501-seed42 \
LODESTONE_ORACLE_DIMENSION=nether \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize --dimension nether

mkdir -p /private/tmp/lodestone-end-501-seed42
LODESTONE_ORACLE_WORLD_ROOT=/private/tmp/lodestone-end-501-seed42 \
LODESTONE_ORACLE_DIMENSION=end \
  bash scripts/worldgen-oracle/large-parity.sh --mode materialize --dimension end
```

Use `LODESTONE_ORACLE_OUTPUT_ROOT` for shard files so exports do not write under the repository's `/oracle` mount. For example, a Nether worker writes to `/private/tmp/lodestone-nether-shards/baseline-tiles/nether/` while reading the sealed root read-only:

```text
mkdir -p /private/tmp/lodestone-nether-shards
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/private/tmp/lodestone-nether-501-seed42 \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/lodestone-nether-shards \
LODESTONE_ORACLE_DIMENSION=nether \
  bash scripts/worldgen-oracle/full-parity-worker.sh 0 4
```

## Dependencies

The exporter uses the locally cached compiled 26.2 server and assets under `.cache/mc/26.2`, through the container runtime wrapper. The Python validator uses only the standard library. The Rust comparator uses the production protocol decoder and a small test-only SHA-256 implementation.

`LODESTONE_ORACLE_FROZEN_CACHE_ROOT` optionally selects a persistent cache outside the sealed source (the default is `<sealed-root>.oracle-cache`). The launcher copies the sealed root into a read-only seed once, then creates a fresh writable APFS clone for each JVM. Each clone is removed when that JVM exits, including an interrupted export, so server lock, region, and cache writes cannot cross shards. On hosts without APFS clone support the launcher uses a normal copy for correctness and reports no clone-level I/O saving. The Java exporter authenticates the clone against the source seal before opening the server and re-authenticates the sealed source after the read; a stale seed or a source mutation fails closed. Run `scripts/worldgen-oracle/frozen-world-clone.sh selftest` for the independent source-before/after, read-A/read-B isolation, and interrupted-read cleanup controls. For the 501 serial End P06 shards, this changes the root transfer from 501 full copies to one full seed plus 501 metadata-only clones (approximately `(501 - 1) / 501`, or 99.8%, of root-byte copy I/O avoided before server writes).
