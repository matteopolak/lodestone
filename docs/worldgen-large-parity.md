# Large worldgen parity harness

## What it is

`scripts/worldgen-oracle/LargeParityOracle.java` is the resumable, 251,001-chunk parity oracle for the 501 by 501 grid centred at `(0, 0)`. It freezes one generated reference world first, then records a full SHA-256 digest of each chunk's canonical semantic record; the old v2 raw 16-bit packet fingerprints are explicitly rejected.

## How it works

The reference seed is `42` and the target coordinates are `cx, cz = -250..=250`, for `501 × 501 = 251,001` chunks. A v3 record has a fixed semantic schema: chunk coordinates; heightmaps sorted by numeric type with 256 decoded heights each; 24 sections of resolved global block-state ids and biome ids; block entities sorted by relative position and type with recursively key-sorted NBT; and all 26 sky then block light sections, distinguishing missing, empty, and present 2048-byte arrays. Packet palettes, packet map traversal order, and packet framing do not enter the record.

For every compared coordinate, the Rust gate obtains the centre and its eight adjacent columns through the normal generation dispatcher, then calls the production neighbour-aware initial-chunk encoder. This is required for light too: an emissive block in a neighbouring column can illuminate the centre across its border, while the one-column encoder deliberately has no such input. The gate keeps that light in the canonical record; it never substitutes a synthetic or all-missing light payload. The frozen-world materialization halo supplies the corresponding settled neighbours on the reference side.

Raw P06 prefixes also replay the initial-light admission boundary. The first
target is the bootstrap admission: its retained initial-light snapshot uses the
centre only. Every later target adopts the four cardinal source columns before
its initial snapshot is retained, while the encoder still receives the complete
3 by 3 packet footprint. This separates source admission state from packet
neighbour input and is intentionally coordinate-independent. The deterministic
two-record control is `LODESTONE_LARGE_PARITY_MAX_CHUNKS=2` with the
authenticated manifest, packet-audit sidecar, and a reference-packet directory;
it must match both the bootstrap record and the following cardinal-admission
record before a longer prefix is attempted. The admission-order unit control
in `large_worldgen_parity.rs` guards the bootstrap-to-cardinal transition.

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
opens one fresh `RegionChunkSource` over the persisted Anvil columns. It reads
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
target recomputation.

The dimension-aware v4 format is additive. It keeps the same 501 by 501 bounds and 32-byte per-chunk digests, but authenticates one of the three dimension identities (`minecraft:overworld`, `minecraft:the_nether`, or `minecraft:the_end`) in header bytes `168..200` as SHA-256 of its resource-location string and uses that dimension's decoded window: overworld `min_y=-64` with 24 sections, Nether/End `min_y=0` with 16 sections. The v5 End format keeps that header shape but has new manifest and record domains. Its export reads the persisted settled light from each requested chunk in the sealed world copy; it does not reset or recompute light, or load an export-only neighborhood. Normal per-batch ticket removal is safe because the sealed world is not mutated during export. It also elides only a trailing all-15 sky-light layer, because an initial chunk enables its column after queueing supplied layers and a missing top sky layer resolves to the same full value. Mixed sky arrays and all block-light tags remain exact. This avoids treating allocation history as terrain while preserving a real light-value mismatch. V3 and v4 records remain readable and byte-for-byte unchanged; a merge or duplicate-read acceptance rejects different dimensions or semantic versions.

The workflow has two mandatory phases. `--mode materialize` generates the requested rectangle **plus its one-chunk halo** (the complete baseline therefore materializes `-251..=251`, or `253,009` chunks), completes the real chunk-status work, post-processes it, saves it, and writes a tree-digest seal. The launcher fixes the generation executor at one worker and the oracle refuses to start if that setting is absent. Materialization visits centres in tile row-major order, but within each tile it admits exactly one centre, waits for its full result and deferred-light settlement, then removes that centre's ticket before admitting the next. Both boundaries are provenance rather than throughput knobs: fresh roots produced different semantic records without the one-worker setting, and concurrent admissions likewise changed sealed feature state. It advances only the scheduler (never resident chunk ticks), fences deferred light work, and checks that the resident full chunk reports correct light. Normal eviction and clean epoch shutdown persist that settled state; materialization does not force a whole-cache flush for every centre. It does not add an export-specific loading halo or change tile/order geometry. Each bounded epoch runs in a fresh container/JVM against the same persistent root; the shell drives the epochs automatically. This matters because the server retains point-of-interest section data beyond ticket removal, and an in-process server restart closes shared executors instead of yielding a reusable clean heap. A progress journal records the exact geometry, epoch size, next tile, and an in-flight tile range. Its `lodestone-large-parity-materialization-v2-<dimension>` filename and marker bind that journal and its seal to the serial, one-worker construction contract rather than the semantic record version. Earlier v3/v4 seals and journals are refused for every dimension because they may have been produced under concurrent construction; create a new empty root instead. It advances only after the prior JVM exited cleanly; an interrupted epoch, a changed range or epoch size, a missing journal, or an already sealed root fails closed rather than being resumed ambiguously. For the complete baseline, omit the range flags so the requested grid is `-250..=250` and its halo is `-251..=251` in both axes. `--mode export` accepts only that sealed world through a read-only mount, copies it into the container's ephemeral server-access directory, and exports semantic digests from the restarted persisted content. The manifest carries the frozen-world digest, schema digest, geometry, full digest width, and SHA-256 payload checksum; merge refuses a shard from a different frozen world. A root sealed before this per-centre light settlement is not an acceptable End baseline and must be regenerated.

This split prevents two independent failure modes. A generated chunk can receive a later neighbour feature write, so the old batch-immediate capture could observe transient state. Freezing before capture removes that lifecycle race; canonical records remain the semantic acceptance authority, while the raw diagnostic path now fixes packet-visible ordering before invoking the compiled codec.

`--packet-out` remains a one-chunk raw packet diagnostic. It is never used for the baseline, but is useful when the Rust comparison identifies a semantic mismatch and needs its existing detailed packet diff. Before encoding, the oracle rebuilds block-state and biome palettes by a fixed section traversal, orders heightmap entries and block entities explicitly, and recursively orders compound-tag keys while preserving list order. `LargeParityOracle --determinism-selftest` uses the compiled 26.2 packet codec on one sealed target: a deliberately reversed packet-order control must differ, while two stabilized encodes must have an empty differing-offset list. Set `ORACLE_DETERMINISM_X` and `ORACLE_DETERMINISM_Z` to choose the target; both default to zero.

The P06 raw-packet format is an independent opt-in path: pass `--raw-packet` (or `--format v6`) to materialize and export the 1001 by 1001 target grid `-500..=500`, with its complete 1003 by 1003 halo `-501..=501`. The materialization wrapper selects the matching v6 seal and does not confuse it with the v2 semantic provenance stamp. Each manifest record is the first two bytes of the SHA-256 digest of the exact stabilized compiled-codec packet body, emitted in x-fastest then z order. The manifest carries the v6 dimension, frozen-world and materialization identities; each export also writes a `.packet-audit` sidecar containing a matching identity header and one full 32-byte packet digest per coordinate. The sidecar is checked during resume, so a matching 16-bit payload cannot hide a missing or misaligned full audit stream. P06 does not reinterpret or regenerate v3-v5 semantic manifests.

For a Nether P06 comparison, the Rust gate prepares the immutable pre-decoration
cache from the selected target prefix before wrapping it in the retained source.
The packet's 3×3 light neighbourhood expands the generator's 5×5 prefix read
closure by one chunk on each side. Thus a 51×51 target shard derives a 57×57
prefix capacity (3,249 entries); a bounded pilot derives the smaller rectangle
it actually visits. Preparation changes retention only. The worldgen counter
control reports prefix computations and whole-cache evictions, and the ignored
raw-byte control compares a fresh unprepared source with the prepared source
before this path is used for an external packet comparison.

The section-header non-empty count is a wire field, not the version-neutral
column occupancy count. `packet_non_empty_block_count` excludes `air`,
`cave_air`, and `void_air` by resolving each emitted state to its block kind;
the section payload still carries those states. `ChunkSection::non_air_count()`
is therefore not suitable for this field. Keep this count separate from the
fluid count. Focused encoder controls read the header directly and then decode
the payload, so a zero count cannot hide an omitted cave- or void-air state.

### Lifecycle replay for the accepted 16 by 16 manifests

The accepted partial manifests at `/private/tmp/lodestone-worldgen-parity-overworld-16-outputs/overworld-partial.lwp` and `/private/tmp/lodestone-worldgen-parity-nether-16-outputs/nether-partial.lwp` have an independent full-run lifecycle capture beside them at `/private/tmp/lodestone-worldgen-lifecycle-capture-20260907-r1/out`. The Rust gate authenticates `provenance.txt`, the 324-row `replay.tsv`, and the 4,608-row `replay-completion-order.tsv` before generating a packet. The capture schema is `lodestone-worldgen-lifecycle-capture-v1`, with seed `42`, a one-chunk halo, and tile-z-major/tile-x-major admission order. The accepted manifest SHA-256 values are `54f3a7e62ed8dbd0d976a27eefef64f6d11152d3e26b94e162e2561192f81071` (Overworld) and `cb4d341f6826ebf7bce49ee195618d48e314729b6bd4251b3b97124e4f948789` (Nether). The canonical replay-sequence SHA-256 is `4e2eeb0217c06e7ed68e3976df04ebef5648ff1b14516141c49095d8ef15498f`; the source-capture SHA-256 is `b47516b6f47ec74aba6201cd8d54401deb12edf94cc4272c0dd9c2b52845f9a3`; and `replay.tsv` is `b8678c8a8b847a94d82bba31c9250aeace0d43e02fc309c923838e327c209986`. The Overworld completion file is `4af54ab035bc7febad74da7a6dffdd9c79a6b9e10c90a164523d98c5e995b1e0`; the Nether completion file is `7115c80a42320ed2ca7c3b8fe7160ea4516cdc6436a10633e212f405f2f0b52a`.

The completion rows repeat observations for every target, so the parser first deduplicates one global event per `(source, stage, completion_seq)`. It requires exactly one successful `FEATURES` event for each of the 324 replay admissions, verifies that its source and admission index agree, and rejects any inversion between `FEATURES` completion-sequence rank and replay admission order. `LifecycleMaterializer` enforces the stricter one-application identity `(source, stage)`; a changed completion sequence remains diagnostic telemetry and is rejected before the source body can run again. Effects are then applied once per source in authenticated `replay.tsv` admission order. The complete 18 by 18 halo is shaped before any feature body runs, so an early source may write to a neighbour whose explicit ticket appears later; every write remains in the mutable resident state and read-after-write override map. For Overworld, each source's production top-layer spill pass then runs with those current overrides immediately after its `FEATURES` result and before the next admission; Nether has no corresponding stage. `FULL` and `before_fence` are parsed as telemetry diagnostics only: they do not establish causal ordering or freeze a target. After all 324 `FEATURES` bodies and any source-local top-layer passes finish, the gate snapshots and encodes the 256 targets and accepts only a digest match to the authenticated final manifest. The reusable `LifecycleMaterializer` and source adapters live in `lodestone-worldgen-parity::lifecycle`; the ignored comparator owns only capture authentication and manifest comparison. The adapters invoke the production shaped-prefix and source-filtered decoration dispatchers, including their read-after-write override inputs and generated block entities.

The capture is deliberately external and is not checked into the repository. Its provenance fields and fixed digests prevent silently replaying a changed artifact; the accepted root freeze digests are `ade151a2bd6a5840c0548d70dd763a3f5060b301fbf2b2cf043af5de365ea4e8` (Overworld) and `c56e42d8ac751d348ff8461b4284c783f24702437039b682b37dca426b497048` (Nether), and the accepted manifest digest is checked against the manifest supplied to the test as well.

`LifecycleReplayPlan` is a static per-target optimisation over that authenticated full replay. It walks events backwards from the packet's 3 by 3 light domain, selecting direct writers within Chebyshev distance two and recursively including earlier mutable dependencies within the audited distance-four bound. Destination admission is a separate clipped closure. Events remain in authenticated order, source-local top-layer work remains immediately after FEATURES, and spills are applied only to admitted destinations. The Nether adapter computes only the unique pre-decoration columns in the selected closure rather than the full replay's closure. The ignored `full_and_pruned_lifecycle_replay_have_identical_target_packet_bytes` control compares raw packet bytes for the audited corner using exactly its 3 by 3 packet-light neighbourhood; its audited closure counts remain a control on the plan itself, not an assumption in the production comparator. The streaming comparator selects this pruned plan automatically for a one-chunk fail-fast prefix (`LODESTONE_LARGE_PARITY_MAX_CHUNKS=1` with `LODESTONE_LARGE_PARITY_SCAN_ALL` unset). `LODESTONE_LARGE_PARITY_TARGET_INDEX` is the explicit arbitrary-target form: it selects one zero-based row from the authenticated 16 by 16 manifest, seeks directly to that 32-byte digest, and builds the plan for `capture.target_order[index]`. It requires the 16 by 16 Overworld or Nether lifecycle manifest, cannot be combined with scan-all, and accepts only an unset limit or `LODESTONE_LARGE_PARITY_MAX_CHUNKS=1`; invalid, out-of-range, or ambiguous combinations fail closed. Multi-target and scan-all runs retain full replay as acceptance authority. End manifests never enter this lifecycle-only branch and use the retained-source comparator. The lifecycle unit negative control mutates event order and observes plan construction reject it; geometry, event count, and order mismatches fail closed.

For Overworld replay, `prepare_lifecycle_replay` creates one bounded slot per authenticated admission. A FEATURES completion lazily fills its slot with only immutable 5 by 5 pre-ore handles, stitched heights, biome eligibility, and source feature/ore selections; resident overrides, placement writes, and RNG state remain per-completion and stay serial. Ordinary production generation does not retain these cloned selection contexts for every explored chunk. The byte-identity comparator remains the authority: enabling the prepared slots must not change packet bytes or the authenticated event order.

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

After that gate passes, select one of those reproducible roots and export it twice into distinct shard directories. `accept` then proves its read-only export is repeatable. `full-parity-worker.sh` divides work into 16-chunk-wide shards, refuses to start without `LODESTONE_ORACLE_FROZEN_WORLD_ROOT`, and uses `LODESTONE_ORACLE_SHARD_DIR` to choose its relative output directory. Legacy overworld shards remain directly below that directory; v4 Nether/End shards add a dimension subdirectory so existing v3 merge globs continue to work. End v5 workers must run sequentially: simultaneous reference servers can alter scheduler timing enough to invalidate the persisted-light reproducibility contract even when memory and disk capacity are sufficient. Other formats may use parallel workers only after the host has measured enough space for their independent copies.

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

`LODESTONE_LARGE_PARITY_MAX_CHUNKS` makes the Rust gate compare a bounded prefix. By default the gate fails at the first digest mismatch and, when set, includes the existing single-packet diagnosis. Set `LODESTONE_LARGE_PARITY_SCAN_ALL=1` to consume the whole selected prefix before failing and report the complete chunk-coordinate set rather than stopping at the first light difference. In scan-all mode, `LODESTONE_LARGE_PARITY_REFERENCE_PACKET` supplies one external packet body; `LODESTONE_LARGE_PARITY_REFERENCE_PACKET_DIR` supplies a directory of `reference*.packet` bodies, keyed by their decoded chunk coordinates. Inputs must be regular files below the diagnostic size bound and must decode inside the selected manifest prefix; stale or out-of-prefix captures are rejected. Every supplied pair is decoded and compared independently for terrain states, biomes, heightmaps (with map keys ordered canonically), block entities, sky light, block light, and present/empty/missing masks. The packet aids are unauthenticated local evidence; the manifest digest remains the acceptance authority. Mask rows are diagnostic-only because v5 canonicalization may intentionally elide a trailing full-sky layer. Set `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT` to retain the full coordinate/value report (with bounded examples per component); the test failure remains a compact count summary. `LODESTONE_LARGE_PARITY_PACKET_OUT` retains the first raw Lodestone packet only for mismatch diagnosis.

For a multi-thousand-chunk issue inventory, set `LODESTONE_LARGE_PARITY_BATCH_SIZE` to an integer of at least `2000` and set `LODESTONE_LARGE_PARITY_SCAN_ALL=1`. The batch size must not exceed the authenticated manifest count and, when `LODESTONE_LARGE_PARITY_MAX_CHUNKS` is also present, the two values must agree. The comparator still authenticates the complete manifest payload before selecting the batch, holds one expected digest and one generated packet at a time, and records every mismatching target in the diagnostic output. If authoritative packet bodies are supplied for the batch, the report additionally groups retained terrain, biome, heightmap, light, mask, and block-entity signatures with their target coordinates; packet bodies are indexed by path and loaded one at a time, with an aggregate 64 MiB diagnostic input bound. Per-packet signature and example caps keep that optional census bounded. A digest-only batch remains a valid acceptance comparison, but it cannot claim a component-level cause without an independent packet body.

The legacy scout files under `/private/tmp/lodestone-parity-scout-*` use the rejected `LWP26P02` raw-fingerprint format and are not inputs to this gate. To produce a fresh authenticated shard after a sealed dimension root is available, use the wrapper below. Its default range is exactly 2,000 targets; pass `--cx`, `--cz`, and `--out` to choose another rectangle or output name:

```text
LODESTONE_ORACLE_FROZEN_WORLD_ROOT=/absolute/path/to/sealed-world \
LODESTONE_ORACLE_OUTPUT_ROOT=/private/tmp/lodestone-parity-batch \
  scripts/worldgen-oracle/batch-parity.sh \
  --dimension overworld \
  --out overworld-2000.lwp
```

The wrapper invokes the existing read-only exporter and then `large-parity-manifest.py validate`; it estimates a 2,000-target export at the exporter’s measured shard rate, while the Rust comparison time is workload-dependent. It refuses ranges below 2,000, ranges outside `-250..250`, missing paired bounds, path traversal, and any output that the authenticated v4/v5 reader rejects. Run the comparator against the resulting shard with `LODESTONE_LARGE_PARITY_BATCH_SIZE=2000`, `LODESTONE_LARGE_PARITY_SCAN_ALL=1`, and `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT=/private/tmp/lodestone-parity-batch/report.txt`.

## Configuration

`LODESTONE_ORACLE_BATCH` controls only the bounded read-only export batch; materialization is intentionally one centre at a time. `LODESTONE_ORACLE_EPOCH_TILES` controls the clean-JVM materialization epoch in 16 by 16 tiles and defaults to `32` (at most 8,192 nominal chunks before edge clipping). It is durable provenance: keep it unchanged while resuming a materialization. `LODESTONE_ORACLE_WORLD_ROOT` is a writable host directory mounted at `/world` only for materialization. `LODESTONE_ORACLE_FROZEN_WORLD_ROOT` is mounted read-only at `/frozen` for export and for `--determinism-selftest`. The source seal is checked before it is copied into the ephemeral server-access directory. `--raw-packet` or `--format v6` selects the P06 1001-square geometry and its v6 materialization contract; without it, the existing v3-v5 semantic defaults remain. `ORACLE_DETERMINISM_X` and `ORACLE_DETERMINISM_Z` select the selftest target and default to `0`. `LargeParityOracle --provenance-selftest` proves that a legacy v3 seal cannot export and a legacy v4 progress file cannot resume; `LargeParityOracle --determinism-selftest` proves the reversed-order negative control and repeated stabilized packet equality. The format versions of their manifests remain unchanged, but their construction provenance is not accepted.

The lifecycle gate uses `LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE` for one explicit capture directory or `LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT` for the `overworld-full-accepted-sequence` and `nether-full-accepted-sequence` subdirectories. `LODESTONE_LARGE_PARITY_MAX_CHUNKS` bounds the final target prefix after the full 324-source replay, `LODESTONE_LARGE_PARITY_SCAN_ALL` changes fail-fast to bounded diagnostics, and `LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT` retains the report. These variables do not alter the authenticated admission-order replay. Before Nether admission, the comparator derives a replay-only pre-decoration cache capacity from the admitted rectangle plus its 5x5 read radius; the production cache remains at its smaller demand-ordered bound. The control reports 484 unique pre-decoration computations for the accepted 18x18 capture, so cache retention changes cost only and cannot change source order or packet bytes.

The manifest tools accept `validate`, `merge`, `reproducible`, `accept`, and `selftest`. `reproducible` is the independent-root gate: it requires both manifests to cover the complete grid and have equal schema, dimension, and payload while deliberately allowing different frozen-world identities. Use it on the two complete merged materializations before `accept`. `accept` is the final read-only export gate: it requires two complete byte-identical exports from one frozen-world identity. `selftest` covers full-grid merge ordering, independent-root payload equality plus bit-flip/incomplete/schema negatives, duplicate-read acceptance, payload tampering, different-world and different-dimension merge/accept refusal, schema rejection, and v2 rejection. Existing v3 overworld and v4 dimension exports continue to validate and merge byte-for-byte. End exports now select v5 automatically and must be kept separate from v4 shards; accepted v4 artifacts are not regenerated by this migration. The `batch-parity.sh` wrapper is an authenticated 2,000-plus target export path; it does not upgrade legacy raw fingerprints.

Dimension roots are independent worlds and must never share a materialization directory. Start a Nether or End materialization with an empty writable root (the command resumes clean epochs until its dimension-specific seal appears):

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
