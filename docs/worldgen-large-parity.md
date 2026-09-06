# Large worldgen parity harness

## What it is

`scripts/worldgen-oracle/LargeParityOracle.java` is the scaffold for resumable compiled-26.2 oracle shards over exactly the 1001 by 1001 overworld chunk grid centred at `(0, 0)`: `cx, cz = -500..=500`. Version 2 stores the first 16 bits of SHA-256 for each packet payload. A complete manifest is 2,004,002 bytes of fingerprints plus a 160-byte header (about 1.91 MiB), not a million retained chunk snapshots.

## How it works

The recorded seed is `42`, matching the existing composed 26.2 fixture. The exporter writes row-major `(cz, cx)` shard payloads. Header v2 fixes protocol `776`, the full grid, shard rectangle, count, fingerprint algorithm and a SHA-256 schema-domain digest. It also carries SHA-256 of the entire payload, so validation detects a one-bit change to any fingerprint before merge or comparison.

Per-chunk SHA-256 v2 is defined over the exact uncompressed `level_chunk_with_light` packet payload bytes. This includes the packet body's chunk coordinates, heightmaps, section data, block entities, and light data. Packet id, VarInt packet-length framing, compression framing, encryption, and transport are excluded. The row-major manifest position identifies which coordinate each fingerprint belongs to. The finished exporter must obtain the payload only after the real server bulk chunk-status pipeline has produced the final chunk, including decoration, structure state, block entities, spawns and light.

`LargeParityOracle` now boots the bundled compiled server in-process, obtains each chunk through the real scheduled `FULL` status pipeline, runs the final post-processing step, and invokes the compiled chunk-with-light packet stream codec with the server's registry access. It therefore hashes the same uncompressed packet body a network connection would carry, without packet framing, compression, encryption, or transport. The exporter is deliberately separate from `lodestone-worldgen`: recreating packet serialization in the generator would compare two independently-maintained encoders rather than the delivered protocol.

## How to change it

First replace the fail-closed placeholder with a real scheduled-chunk packet source and connect the ignored Rust test to `lodestone-v26-2`'s production encoder. The current script deliberately fails instead of emitting a reduced dataset. Once those two consumers exist, run a bounded pilot first:

```text
bash scripts/worldgen-oracle/large-parity.sh --out /oracle/pilot.lwp --cx -1 0 --cz -1 0
python3 scripts/worldgen-oracle/large-parity-manifest.py validate scripts/worldgen-oracle/pilot.lwp
```

Use disjoint rectangles for parallel shards. The exporter schedules 256 chunks at a time by default, then post-processes and serializes that batch on the server thread before continuing. `LODESTONE_ORACLE_BATCH` can raise or lower the bounded future set, but a 1,024-future experiment failed after its first 256 completions, so keep the default unless a host-specific pilot proves a larger bound safe. The measured 256-chunk pilot (`0..=15` in both axes) sustained 35.3 chunks/s after server startup on the local Apple Container host; one serial JVM would therefore take roughly 7.9 hours for 1,002,001 chunks. Run disjoint shards in parallel to use more cores, and retain each completed shard. `--resume` authenticates a completed shard, or continues an interrupted file whose payload ends on a 2-byte record boundary and whose final checksum is still zero (the durable in-progress marker). It re-hashes the existing prefix before appending; a partial file carrying a final checksum is rejected rather than guessed at. Merge only complete coverage:

```text
python3 scripts/worldgen-oracle/large-parity-manifest.py merge --out /absolute/path/full.lwp shard-*.lwp
LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/full.lwp cargo test -p lodestone-v26-2 --test large_worldgen_parity -- --ignored
```

The merger rejects overlaps, holes, wrong bounds, malformed headers and a changed payload bit. A new packet layout or hash input requires a new schema version/domain string and matching Java exporter, Python validator and Rust reader; never reinterpret a v2 fingerprint.

## Configuration

The exporter accepts `--out`, `--cx LO HI`, `--cz LO HI`, and `--resume`. Ranges must remain inside `-500..=500`; the only full valid merge is the complete target grid. It runs using `scripts/worldgen-oracle/run.sh`, which mounts the local compiled 26.2 jar cache into the Apple container runtime.

## Dependencies

The oracle uses the locally cached compiled 26.2 server jar and assets under `.cache/mc/26.2`, the existing container-based Java runtime wrapper, and no decompiled implementation as an input. The verifier is Python standard library only; the Rust fixture reader has a small self-contained SHA-256 implementation so the test does not add a production dependency.
