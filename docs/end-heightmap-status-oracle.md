# End heightmap status oracle

## What it is

`EndP06LifecycleCapture` is the bounded external adapter for the End generation boundary that affects
cross-chunk feature writes and the three client-visible heightmaps. It runs inside the stream oracle
with seed `42`, holds target chunk `(280,78)` at its pre-feature stage, and authenticates each raw
resident transition before the Rust materializer builds the packet.

## How it works

The adapter requests the target at the pre-feature stage, then admits the nine source chunks in
canonical X-major/Z-minor order. At each FEATURES boundary it captures the source and the observed
target resident status plus the three raw client heightmaps. The stream frame authenticates this
sidecar independently from the raw chunk packet, and the Rust parser installs those values before
running its source body; it never reconstructs them from the final block field.

The stream parser has small controls for the canonical target focus `(2,0)`: authenticated raw motion
heightmaps of `56` are retained by the canonical transition, while a negative control carrying `58`
is retained as `58`. A reversed source order is rejected before materialization. These controls keep
source admission order distinct from the target's own stage boundary without materializing a large
world.

Run it from the repository root with the external jar cache available:

```bash
scripts/worldgen-oracle/stream-parity.sh \
  --dimension end --cx 280 280 --cz 78 78
```

The runner compiles the capture adapter beside `LargeParityOracle` inside a disposable Java 25
container. It does not read or modify Lodestone generation code.

The validated seed-42 witness is source-order sensitive: the target focus `(2,0)` has raw client map
values `(68,56,56)` at the authenticated FEATURES boundary, even though the final resident terrain
would otherwise suggest `(68,58,58)`. The Rust control replays that canonical value and separately
proves the negative `(68,58,58)` payload is not silently normalised. The external one-chunk gate
currently reports identical End terrain, biomes, entities, and all three heightmaps; any remaining
raw-packet difference is isolated to the light payload.

## How to change it

Keep the target, resident radius, stage mapping, and source order explicit and bounded. If a new
witness is needed, extend the authenticated sidecar schema and its parser/materializer controls
together; do not derive a transition from a final block or light-free record. Re-run the one-chunk
external gate whenever the server lifecycle request changes, and retain a negative-order or negative-
value control before interpreting a parity result.

## Configuration

`ORACLE_ARGS` is populated by `run.sh` from the command-line arguments. The stream target bounds are
passed to `stream-parity.sh`; the seed remains the fixed `42` used by the runner's server properties.
The capture requires the 26.2 cache and Apple `container`.

## Dependencies

The probe uses the real 26.2 server jar and its libraries through `scripts/worldgen-oracle/run.sh`,
the shared `LargeParityOracle` server lifecycle, and the JDK block and chunk APIs exposed by that jar.
It has no dependency on the Rust crates or on a generated fixture.
