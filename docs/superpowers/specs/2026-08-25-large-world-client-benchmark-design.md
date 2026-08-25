# Large-world client benchmark design

## Problem

The existing terrain and 64×32×64 showcase workloads are useful controls, but
they do not reproduce a live server where `world_encode_submit` reaches 7–8 ms
and `hud_ui_encode_submit` reaches 4–5 ms. A save's byte size alone cannot cause
that: render distance bounds the chunks considered by one frame. The next
workload must put dense, already-generated multiplayer chunks inside that bound
and preserve the counts needed to explain the timings.

## Chosen workload

Use the official Hermitcraft Season 10 Java world as a local Java 26.2 oracle.
It is a roughly 1.2 GB late-season multiplayer save with dense builds, entities,
block entities, signs, maps, storage/redstone areas, and ordinary terrain. The
installer downloads only from the official Hermitcraft endpoint, streams to a
temporary file, rejects unsafe ZIP paths, validates a Java world root, and
atomically installs the extracted cache under `.cache/mc/megaworld`. The world
and archive remain untracked benchmark inputs.

The oracle uses its own game/RCON ports and a copied 26.2 server jar. It never
mutates the smaller terrain or showcase worlds. Server conversion happens
before a measured client run; benchmark warmup excludes join, conversion, and
initial chunk settling.

## Measurement design

Add a third client workload named `megaworld`. It uses creative flight like the
terrain arm: a stationary segment measures a fully loaded dense view, and a
moving segment flies through pre-generated chunks. The player begins at the
save's authored spawn rather than `(0, 0)`, so the route remains tied to the
downloaded world.

Run two otherwise identical arms:

1. `debug_overlay=closed`, which measures normal play while the CSV profiler
   continues recording invisibly.
2. `debug_overlay=open`, which measures the F3 overlay the reported 4–5 ms HUD
   number was read from.

The overlay state is explicit CLI configuration and result metadata, never
inferred from timings. Existing CPU columns already split world work into
preparation, terrain cull/draw, other draws, encoder finish, and queue submit;
they split HUD work into debug gather, frame gather, HUD draw, container draw,
menu overlays, and GPU-timer closure. The once-per-second tracing detail also
carries sections visited and HUD chat/debug/menu counts. The runner will retain
and summarize those count snapshots so a duration is never reported without
its workload magnitude.

GPU whole-pass timestamps remain controls rather than fabricated totals. On
Apple Metal the block and first-person passes can be timed, but work inside a
pass requires an Xcode Metal capture.

## Alternatives rejected

- **Generate a huge vanilla map locally.** This is deterministic but mostly
  stresses terrain streaming, which the existing terrain arm already covers;
  it lacks years of player-built block/entity density.
- **Enlarge the synthetic plot.** This makes exact A/B comparisons easy but
  duplicates one artificial scene and misses the spatial/data-distribution
  behavior suspected on real servers.
- **Benchmark only the user's remote server.** It is the best final validation
  but cannot be reset, pinned, or redistributed, so it is unsuitable as the
  primary regression workload.

## Acceptance criteria

- Installation is idempotent and fails closed on partial, unsafe, or malformed
  downloads.
- The Java 26.2 oracle starts with the imported save and accepts a Lodestone
  client at render distance 24.
- Both overlay arms complete on the hardware built-in fullscreen display and
  record non-empty stationary/moving segments.
- Results identify which world and HUD subphases account for the reported
  totals, with section/HUD counts and GPU pass samples beside them.
- Any optimization is implemented only after that attribution, is guarded by a
  failing test or benchmark control, and wins a same-map, same-overlay A/B run.

## Change boundaries

World installation belongs in a standalone Python script; hosting belongs in a
new live-oracle shell script; client choreography remains in
`app/benchmark.rs`; CLI policy remains in `config.rs`; benchmark orchestration
and summaries remain in `client-frame-benchmark.py`. Do not add map parsing or
download behavior to the renderer.

## Configuration and dependencies

The source URL, expected archive name, cache destination, and ports are pinned
in the installer/oracle. Runtime requirements are Python 3, the official ZIP,
the existing Java 26.2 server jar, Apple's `container` runtime, and enough free
disk for the archive plus extracted world. No downloaded map data is committed.
