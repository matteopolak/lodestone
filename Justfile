# Lodestone task runner — the canonical-COMMAND layer, not a script host.
#
# Division of labour (see docs/repo-tooling.md for the full writeup):
#   xtask   keeps anything that parses Rust/workspace structure, generates a
#           committed artifact, or needs its own test (the drift gates).
#   just    holds the canonical INVOCATIONS — one to three lines each. No
#           script body moves into this file.
#   scripts/* keep their bodies and their current paths; recipes here only
#           delegate to them, so every doc that already says `scripts/…` stays
#           correct.
#
# Every recipe below is also a literal command documented in CLAUDE.md's
# "Build and test" section — that section names the recipe first and keeps
# the raw command beside it as the definition, so this file is never the only
# record of what a recipe actually runs.

# Per-agent private target dir (docs/repo-tooling.md). Deliberately NOT a
# CARGO_*-prefixed name: sccache hashes CARGO_* env vars into its cache keys,
# and the env-var form of --target-dir measured 0% cache hits vs 78-94% for
# the FLAG form. just interpolates {{tdir}} into the command line BEFORE
# cargo runs, so cargo always sees the flag form — never rename this to
# CARGO_TARGET_DIR, and never add `set export`, which would leak it into the
# environment for cargo to read as one. Default "target" preserves today's
# behaviour exactly for anyone (including CI) who sets nothing.
tdir := env("LODESTONE_TARGET_DIR", "target")

# Overridable job cap, defaulting to EMPTY — not hardcoded to 4. A fixed -j
# here would silently throttle CI and any idle-machine run. Set
# LODESTONE_JOBS=4 yourself for local multi-agent courtesy (docs/repo-tooling.md).
jobs := env("LODESTONE_JOBS", "")
jflag := if jobs != "" { "-j " + jobs } else { "" }

# Default endpoints for the STANDALONE `lodestone-relay` binary via `run-relay`
# only — `run-wasm` below no longer uses this at all (its own binary,
# `lodestone-web-server`, has its own baked-in defaults matching this file's
# host/port, since there is no longer a second literal anywhere to keep in
# sync with). Held in a variable rather than inline in the signature purely so
# `just --list` can still fit the recipe's doc comment on the same line as its
# name — an inline default this long pushes the description onto a line of
# its own.
relay_defaults := "--listen 127.0.0.1:25580 --target 127.0.0.1:25565"

# Private target dir for the PGO recipes at the bottom of this file. Separate
# from {{tdir}} on purpose: those recipes set RUSTFLAGS, and cargo keys its
# cache on RUSTFLAGS, so pointing them at the shared dir would cost every
# other build on this machine a full cold rebuild each time you switch.
pgo_dir := env("LODESTONE_PGO_DIR", "target/pgo")

# --- Health checks (CLAUDE.md "Build and test") ---------------------------

# cargo check --workspace --all-targets
check:
    cargo check --workspace --all-targets {{jflag}} --target-dir {{tdir}}

# cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench
# The --exclude is NOT a workaround: lodestone-allocbench has a deliberate
# compile_error!() when more than one allocator feature is on, because each
# installs its own #[global_allocator] — plain --all-features structurally
# cannot pass for that one crate. With it excluded, the whole rest of the
# workspace is clean under --all-features.
check-all:
    cargo check --workspace --all-features --all-targets --exclude lodestone-allocbench {{jflag}} --target-dir {{tdir}}

# cargo check -p lodestone-shell --no-default-features — the version-seam
# check. No protocol family is enabled by default; this is the only thing
# that proves the shell still compiles with NONE of them.
#
# The second line is a different claim from the first. Compiling without a
# family proves the seam; it does not prove the windowing stack is gone,
# because an unused dependency still compiles fine. check-no-winit-headless
# asks the resolved dependency graph instead, so a headless or server build
# cannot quietly start linking winit again.
check-seam:
    cargo check -p lodestone-shell --no-default-features {{jflag}} --target-dir {{tdir}}
    cargo run -p xtask -- check-no-winit-headless

# cargo test --workspace --no-fail-fast. Plain `cargo test` stops at the
# first failing test BINARY, hiding every alphabetically-later crate's
# failures — --no-fail-fast is not optional here.
test:
    cargo test --workspace --no-fail-fast {{jflag}} --target-dir {{tdir}}

# cargo xtask check-comment-voice — fails on issue references and
# change-voice comments ("this change", "this commit", ...) in .rs/.md/.wgsl
# comments and doc comments that are not covered by
# xtask/check-comment-voice.toml. See that file's header and
# xtask/src/comment_voice.rs's module doc for what counts and why.
check-comment-voice:
    cargo run -q -p xtask {{jflag}} --target-dir {{tdir}} -- check-comment-voice

# All five checks above, in order.
health: check check-all check-seam test check-comment-voice

# --- Running the game -------------------------------------------------------

# The [[bin]] is `lodestone`, not `lodestone-shell`, and `default-members`
# makes a bare `cargo run` target the shell anyway; -p is spelled out so this
# recipe does not depend on that. Release is not a preference here — a debug
# build is unplayable. The `live` feature (which turns on v26-2, and nothing
# else) is ON by default now, so there is no --features flag: use
# `cargo run --no-default-features` to reproduce a version-family-free build.
# cargo run --release -p lodestone-shell --bin lodestone — launch the game
run *args:
    cargo run --release -p lodestone-shell --bin lodestone {{jflag}} --target-dir {{tdir}} -- {{args}}

# scripts/run-wasm.sh — keep the browser build rebuilding on change (`trunk
# watch`) AND serve it, page plus the /relay WebSocket->TCP bridge, from ONE
# port (`lodestone-web-server`, web/server/src/main.rs) on http://127.0.0.1:8080/
# by default. Address, port and the two COOP/COEP headers are baked into that
# binary's own defaults (LODESTONE_WEB_LISTEN overrides), not read from
# web/Trunk.toml — trunk itself no longer serves anything for this recipe.
#
# Named `run-wasm` rather than `run:wasm` or `run --surface wasm` for two
# reasons. `:` is just's module-path separator, so it is not available in a
# recipe name at all. And a `--surface` flag on `run` would mean parsing an
# argument and branching on it inside this file — the one thing the header
# forbids; two recipes is the shape that keeps "one name per raw invocation"
# true. It sits beside `run` so `just --list` shows both ways to launch the game
# together, while the `wasm-*` recipes below stay grouped as what they are:
# guards, not launchers.
#
# --release is NOT a preference here, the same way it is not for `run`, but for a
# different reason: a debug build makes single-threaded worldgen ~10x slower,
# which blows the singleplayer probe's own 30 s deadline and therefore *presents
# as a failure* rather than as slowness. See web/README.md → "Run it". Applies to
# BOTH halves now — `trunk watch --release` for the wasm bundle, and a --release
# build of `lodestone-web-server` itself.
#
# No {{jflag}} and no --target-dir {{tdir}}, and their absence is deliberate
# rather than an oversight, for BOTH the wasm build and lodestone-web-server's own
# build: trunk drives cargo itself and exposes neither flag (its output knob is
# --dist), and both `web/` and `web/server` are members of web/'s own workspace
# root, with its own Cargo.lock and its own web/target/, so neither ever contends
# for the shared target/ lock that {{tdir}} exists to avoid.
#
# It links the relay in rather than starting a separate one, which is why there
# is no LODESTONE_NO_RELAY any more: `/relay` is just a route on the one listener,
# idle until something dials it, so there is no second process whose absence needs
# a flag. Without a real server behind --target, the multiplayer server-list ping
# still fails *visibly* (a row reads `Failed`, naming the reason) rather than
# hanging or looking broken — same guarantee the old two-process shape gave.
#
# **A real multiplayer join is not wired to the relay yet.** Only the
# server-list ping is, as of this recipe's current doc. `net.rs`'s browser join
# path still refuses outright ("a browser cannot open a TCP socket … must go
# through the WebSocket relay") rather than actually dialling one — that
# refusal names the right fix but nothing currently performs it. Do not extend
# this comment to claim joining works; check `net.rs`'s `run_async` before
# trusting any future version of this line that does.
#
# Two long-lived processes in one command is why the body is a script and not
# inline here: it needs a trap, so neither process can outlive the run and keep
# its port bound (or its watch running) for the *next* one. Per this file's
# header that body belongs in scripts/, exactly as `wasm-size` delegates.
# LODESTONE_WEB_LISTEN / LODESTONE_RELAY_TARGET are LODESTONE_* names, not
# CARGO_* ones, for the sccache reason at the top of this file, and are meant to
# be set inline on the command rather than via `set export`.
#
# Port 0 (`LODESTONE_WEB_LISTEN=127.0.0.1:0`) asks the OS for a free port
# instead of the fixed default, for exactly the conflict case a fixed port
# risks; the script reads the port lodestone-web-server actually bound back
# from a file it writes (--port-file), never from a pipeline.
#
# Prerequisites are trunk 0.21.x and the wasm32-unknown-unknown target; the
# script verifies both up front and fails with the install command, rather than
# letting a missing one surface as a confusing build error.
#
# The [doc] attribute is here because `just --list` otherwise shows the LAST
# comment line before a recipe, which for any recipe carrying real rationale is a
# mid-sentence fragment. Prefer it over reordering the prose so the summary lands
# last — the rationale should read top-to-bottom for someone in the file.
[doc("watch + serve (page + /relay, one port) the browser build; :8080 by default")]
run-wasm *args:
    ./scripts/run-wasm.sh {{args}}

# cargo run -p lodestone-relay — the STANDALONE WebSocket→TCP bridge, its own
# process on its own port (25580 by default). **`run-wasm` above does NOT use
# this any more** — it links `lodestone-relay` in as a library inside
# `lodestone-web-server` instead, so there is no second port or process to keep
# in sync with. This recipe is for pairing a bare `trunk serve` (page-only dev
# loop, see web/Trunk.toml's own comment) with a relay by hand, or for anything
# else that wants a standalone protocol-blind WS↔TCP bridge. Override by
# passing your own flags, e.g.
# `just run-relay --listen 127.0.0.1:25580 --target 127.0.0.1:25570`.
[doc("cargo run -p lodestone-relay — the STANDALONE relay (run-wasm no longer needs this)")]
run-relay *args=relay_defaults:
    cargo run --release -p lodestone-relay {{jflag}} --target-dir {{tdir}} -- {{args}}

# --- xtask ------------------------------------------------------------------

# cargo xtask <args>, pre-expanded. The `cargo xtask` alias in
# .cargo/config.toml ("run --quiet --package xtask --") cannot carry
# --target-dir (docs/repo-tooling.md), so agents were hand-expanding this
# every time; this recipe bakes that expansion instead.
xtask *args:
    cargo run -q -p xtask {{jflag}} --target-dir {{tdir}} -- {{args}}

# --- LODESTONE_REGEN regeneration recipes -----------------------------------
# Each of these mirrors a committed-table drift gate that is #[ignore]d by
# default and only makes sense to run explicitly after a data bump.

# Regenerate docs/README.md from every doc's own H1 + "## What it is"
# summary. docs/README.md is GENERATED — never hand-edit it; `cargo test -p
# xtask` fails loudly if the committed file drifts from this output.
regen-docs-index:
    cargo run -q -p xtask {{jflag}} --target-dir {{tdir}} -- docs-index

# Regenerate crates/lodestone-data's collision-shape table
# (src/generated/collision_shapes.rs) from the committed physics oracle
# dump. Test: crates/lodestone-data/tests/collision_shapes.rs ::
# committed_table_matches_dump (#[ignore]d).
regen-collision:
    LODESTONE_REGEN=1 cargo test -p lodestone-data --test collision_shapes {{jflag}} --target-dir {{tdir}} committed_table_matches_dump -- --ignored --nocapture

# Regenerate crates/lodestone-data's hardness/correct-tool table
# (src/generated/hardness.rs) from the committed JVM dump. Test:
# crates/lodestone-data/tests/hardness.rs :: committed_table_matches_dump
# (#[ignore]d).
regen-hardness:
    LODESTONE_REGEN=1 cargo test -p lodestone-data --test hardness {{jflag}} --target-dir {{tdir}} committed_table_matches_dump -- --ignored --nocapture

# Regenerate crates/lodestone-data's damage-type + tag table
# (src/generated/damage_types.rs) from vanilla's own datapack JSON. Unlike the
# two above, this needs NO JVM and no container: damage types ship as data
# files, so step 1 re-extracts them straight out of the jar. Note the OUTER
# .cache/mc/26.2/server.jar is a bundler and contains none of them. Test:
# crates/lodestone-data/tests/damage_types.rs :: committed_table_matches_dump
# (#[ignore]d).
regen-damage-types:
    python3 scripts/extract-damage-types.py .cache/mc/26.2/versions/26.2/server-26.2.jar crates/lodestone-data/tests/support/damage_types_jar.txt
    LODESTONE_REGEN=1 cargo test -p lodestone-data --test damage_types {{jflag}} --target-dir {{tdir}} committed_table_matches_dump -- --ignored --nocapture

# Re-extract the bundled 26.2 structure corpus (1606 files: 34 structures, 20
# structure sets, 188 template pools, 40 processor lists, 7 world presets, 9 flat
# presets, 4 noise settings, 92 worldgen tags, 1212 NBT templates) VERBATIM from
# the server jar, together with the jar-derived SHA-256 manifest that is the
# drift gate's anchor. Needs no JVM and no container — this is all datapack data,
# so unzipping it is strictly more authoritative than asking a program to
# describe it. Note the OUTER .cache/mc/26.2/server.jar is a bundler and holds
# none of these paths. Test: crates/lodestone-server/tests/
# worldgen_structure_corpus.rs :: manifest_matches_a_fresh_jar_extraction
# (#[ignore]d).
regen-worldgen-structures:
    python3 scripts/extract-worldgen-structures.py
    cargo test -p lodestone-server --test worldgen_structure_corpus {{jflag}} --target-dir {{tdir}} -- --nocapture

# Re-extract crates/lodestone-server/assets/loot_table/ VERBATIM from the
# decompiled client's datapack data: every one of Mojang's 1355 26.2 loot tables
# whose features src/loot.rs fully evaluates (1230 of them, 823 KB). Needs no JVM
# and no container -- loot tables are datapack data, so copying them is strictly
# more authoritative than asking a program to describe them. It DOES need
# .cache/mc/26.2/client-src. Deletes and rewrites the tree, so a table that
# stopped being clean is removed rather than left to trip load_bundled's
# zero-unsupported assertion. Test: crates/lodestone-server/tests/loot_corpus.rs
# :: the_bundle_is_exactly_the_clean_subset_of_the_vanilla_corpus (#[ignore]d),
# which is also the drift gate -- it compares the bundle against the CACHE, not
# against itself, so a table falling in or out of scope fails loudly.
regen-loot-corpus:
    LODESTONE_REGEN=1 cargo test -p lodestone-server --test loot_corpus {{jflag}} --target-dir {{tdir}} the_bundle_is_exactly -- --ignored --nocapture
    cargo test -p lodestone-server --test loot_corpus {{jflag}} --target-dir {{tdir}} -- --ignored --nocapture

# Regenerate crates/lodestone-data's freeze_top_layer support table
# (src/generated/snow_support.rs) from the committed JVM dump. Test:
# crates/lodestone-data/tests/snow_support.rs :: committed_table_matches_dump
# (#[ignore]d). Re-dump first with `just oracle-snow-support` after a data bump.
regen-snow-support:
    LODESTONE_REGEN=1 cargo test -p lodestone-data --test snow_support {{jflag}} --target-dir {{tdir}} committed_table_matches_dump -- --ignored --nocapture

# Re-dump the five per-block-state freeze_top_layer facts from the real 26.2
# server, over the committed anchor. Needs Apple `container` (see
# docs/oracles-and-benchmarks.md). Follow with `just regen-snow-support`.
oracle-snow-support:
    #!/usr/bin/env bash
    set -euo pipefail
    CACHE="$(cd .cache/mc/26.2 && pwd)"
    HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
    container system start >/dev/null 2>&1 || true
    container run --rm --memory 3g -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work \
      eclipse-temurin:25-jdk bash -c '
        set -e
        CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
        mkdir -p /work && cp /oracle/SnowSupportOracle.java /work/
        javac -cp "$CP" -d /work /work/SnowSupportOracle.java
        java -cp "/work:$CP" SnowSupportOracle
      ' > crates/lodestone-data/tests/support/snow_support_jvm.txt

# Re-dump the four freeze_top_layer whole-chunk parity fixtures (issue #404's
# U2) from the real 26.2 server. Each is one container run of a few minutes.
# The gate reading them is crates/lodestone-server/src/worldgen_data.rs ::
# top_layer_parity. See docs/worldgen-biomes.md for why these four
# biomes and not others — windswept_hills is the one that discriminates the
# height-adjusted temperature from the flat biome field.
oracle-top-layer:
    #!/usr/bin/env bash
    set -euo pipefail
    out=crates/lodestone-worldgen/tests/support
    ./scripts/worldgen-oracle/run.sh TopLayerOracle minecraft:snowy_plains -1200 -2400 > $out/top_layer_snowy_plains_jvm.txt
    ./scripts/worldgen-oracle/run.sh TopLayerOracle minecraft:frozen_ocean -600 0 > $out/top_layer_frozen_ocean_jvm.txt
    ./scripts/worldgen-oracle/run.sh TopLayerOracle minecraft:windswept_hills 0 240 > $out/top_layer_windswept_hills_jvm.txt
    ./scripts/worldgen-oracle/run.sh TopLayerOracle minecraft:desert -160 -240 > $out/top_layer_desert_jvm.txt

# --- Delegating wrappers (scripts/* keep their bodies and paths) ------------

# wasm32 compile + confinement-guard tripwire (debug build, fast). Does NOT
# prove the browser runs — see xtask's wasm-check section (the tested port of
# scripts/wasm-check.sh, which remains as the reference original).
#
# CI runs this on every push and PR (the `wasm` job in
# .github/workflows/ci.yml), which is the *only* thing that makes it a tripwire.
# For a long time nothing ran it — not this file's `health`, not CI — and the
# first `just run-wasm` duly found three E0433s in lodestone-server, each a
# `cfg(not(target_arch = "wasm32"))` module named from ungated code. Deliberately
# still NOT in `health`: that is four full workspace builds already, and the
# trade-off is written out at the CI job. Nothing here checks `web/` either way —
# it is its own workspace with its own Cargo.lock, outside the root `members`
# glob, so `check`/`check-all` structurally cannot reach it and the trunk build
# inside wasm-check is the only thing that does.
[doc("wasm32 compile + confinement-guard tripwire — does NOT prove the browser runs")]
wasm-check:
    cargo run -q -p xtask {{jflag}} --target-dir {{tdir}} -- wasm-check

# Release wasm bundle-size ceiling (gzip-enforced; brotli reported when
# available). Separate from wasm-check because a --release + lto=fat build
# is slow enough that folding it in would slow the command everyone runs.
[doc("release wasm bundle-size ceiling, gzip-enforced (slow: --release + lto=fat)")]
wasm-size:
    ./scripts/wasm-size.sh

# Gate for scripts/profile-cost-table.py, the samply join the whole worldgen
# perf record was measured through (docs/roadmap/benchmarks.md). It rotted
# undetected across a samply upgrade because nothing ran it: the script had no
# test, and `cargo test --workspace` cannot see a Python file. 20 checks over
# three committed fixtures, stdlib only, no capture needed.
#
# NOT part of `just health`, and that is the remaining gap rather than a
# decision — a gate you must choose to run is the shape of the original
# problem. Folding it in wants an xtask test shelling out to python3, and the
# trap there is that "python3 missing => skip" is the *precondition* species of
# vacuous test (CLAUDE.md): it must fail loudly, not skip. Tracked separately.
test-profile-table:
    python3 scripts/test-profile-cost-table.py

# Server-only heavyweight scene handoff and finite runtime capture. The scene
# file is the immutable client-runner input; the runtime record stays in the
# caller-selected local output path.
heavy-server-emit scenario="mixed" seed="1" scale="1":
    cargo build --release -p lodestone-server --example heavy-scene-server
    target/release/examples/heavy-scene-server --emit-scene /tmp/lodestone-heavy-scene.json --scenario {{scenario}} --seed {{seed}} --scale {{scale}}

samply-heavy-server:
    cargo build --release -p lodestone-server --example heavy-scene-server
    python3 scripts/samply-heavy-server.py

samply-heavy-server-smoke:
    cargo build --release -p lodestone-server --example heavy-scene-server
    python3 scripts/samply-heavy-server.py --smoke --wall-deadline-secs 12

validate-heavy-server-profile capture:
    python3 scripts/samply-heavy-server.py --validate-capture {{capture}}

profile-heavy-server capture:
    python3 scripts/profile-cost-table.py {{capture}}

# A finite Samply input for the chunk-owner hand-off architecture. The example
# drives a paused 128-tick scene and exits; it is an investigation, never CI.
bench-chunk-owner-tick:
    cargo bench {{jflag}} -p lodestone-server --features profile-harness --bench chunk_owner_tick -- --quick

samply-chunk-owner-tick *args:
    cargo build --release {{jflag}} --target-dir {{tdir}} -p lodestone-server --features profile-harness --example chunk-owner-tick-profile
    python3 scripts/samply-chunk-owner-tick.py --server {{tdir}}/release/examples/chunk-owner-tick-profile {{args}}

# A finite, adapter-free profiling input for the 256-chunk coarse horizon.
# It requests 256 reduced far columns plus a fixed tile budget per recenter and
# fails if any far request returns a full column, so Samply has explicit path witnesses.
profile-distant-horizon:
    cargo run --release {{jflag}} --target-dir {{tdir}} -p lodestone-shell --bin horizon-profile

samply-distant-horizon capture:
    cargo build --release {{jflag}} --target-dir {{tdir}} -p lodestone-shell --bin horizon-profile
    samply record --save-only --unstable-presymbolicate -o {{capture}} -- {{tdir}}/release/horizon-profile

# --- Benchmark baselines and regression detection -------------------------
#
# Three recipes, in the order you use them. `bench-record` runs the subset of
# benches that produce deterministic COUNTS on any machine — no GPU adapter,
# no vanilla jar, no wall-clock number in the baseline — writing them to the
# gitignored bench-results/. `bench-gate` then compares those counts against
# the committed bench-baselines/ and fails on drift in either direction.
# `bench-baseline-update` is the sanctioned way to move a baseline when a
# change moved a number on purpose: run it and commit the diff alongside the
# change. See docs/benchmark-regression-gate.md.

# Run the hermetic, count-producing benches once each (criterion --test mode:
# one iteration per benchmark, since the recorded counts do not need samples).
bench-record:
    cargo bench {{jflag}} --target-dir {{tdir}} -p lodestone-render -p lodestone-world \
      --bench meshing --bench render_submit --bench memory_footprint -- --test

# Compare the recorded counts against bench-baselines/. --min-compared makes
# "the benches did not write anything" red rather than a silent green: an
# audit that checks nothing is unrun, not passing.
bench-gate:
    python3 scripts/bench-gate.py --min-compared 40

# Move the committed baseline to what the last run recorded. Tolerances and
# required-flags survive; only the values move. Commit the diff with the
# change that moved them, so an improvement is recorded rather than absorbed.
bench-baseline-update:
    python3 scripts/bench-gate.py --update

# Executable control for the gate itself: 25 checks over synthetic fixtures,
# including a planted per-section-uniform regression the gate must catch and
# a healthy control it must pass. Stdlib python3, no pytest.
test-bench-gate:
    python3 scripts/test-bench-gate.py

# Region-level worldgen throughput/peak-RSS sweep. No args: the script's own
# courteous default radii (8 16) apply. Pass radii to override, e.g.
# `just worldgen-sweep 3 32` for the full RD-32 sweep — only on an otherwise
# idle machine, per CLAUDE.md.
worldgen-sweep *args:
    ./scripts/worldgen-region-sweep.sh {{args}}

# Where a frame goes, CPU *and* GPU, over a fixed camera path on a fixed demo
# world. Prints a per-waypoint CPU-vs-GPU verdict from real TIMESTAMP_QUERY
# pass timings, with the section/draw-call counts beside every duration, and
# records medians to bench-results/frame_profile.jsonl for a same-machine
# comparison against the previous run. Needs a GPU adapter; skips loudly
# without one. See docs/render-benchmarks.md.
#
# Run it on an otherwise IDLE machine: a duration gathered while other agents
# build gets attributed to the wrong cause (CLAUDE.md). The bench states its
# own noise estimate (slowest frame / median) per waypoint so a run taken
# under load says so rather than being quietly believed.
[doc("where a frame goes, CPU and GPU, over a fixed camera path (needs a GPU adapter)")]
bench-frame:
    cargo bench {{jflag}} --target-dir {{tdir}} -p lodestone-shell --bench frame_profile

# Three Java-backed normal-terrain trials at physical 2560x1440, RD24,
# unlimited/no-VSync. The foreground runner owns the child until it exits.
bench-client-terrain:
    python3 scripts/client-frame-benchmark.py --workload terrain

# Three trials of the dense signs/heads/banners/maps/entities/particles scene.
bench-client-showcase:
    python3 scripts/client-frame-benchmark.py --workload showcase

# One 2s/2s/3s showcase run: the end-to-end gate before expensive trials.
bench-client-smoke:
    python3 scripts/client-frame-benchmark.py --workload showcase --smoke

# Hermetic controls for the live-client runner's completion, provenance, and
# production-render witness checks. Does not launch a server, client, or GPU.
test-client-frame-benchmark:
    python3 scripts/test-client-frame-benchmark.py

# Hermitcraft S10 at RD24, once with F3 closed and once open. Install the
# pinned world first with `python3 scripts/install-client-benchmark-world.py`.
bench-client-megaworld:
    python3 scripts/client-frame-benchmark.py --workload megaworld

# One 2s/2s/3s run per F3 arm: setup/compatibility gate before full trials.
bench-client-megaworld-smoke:
    python3 scripts/client-frame-benchmark.py --workload megaworld --smoke

# Stampy's Lovelier World from an open-air waypoint, with a climbing orbit.
bench-client-lovelier:
    python3 scripts/client-frame-benchmark.py --workload lovelier

bench-client-lovelier-smoke:
    python3 scripts/client-frame-benchmark.py --workload lovelier --smoke

# One low-density production-client run. It still requires every selected
# render witness and writes only local scene evidence.
bench-client-heavy-smoke:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario mixed --smoke

# Full local scene; this is profiler input, never a cross-machine timing gate.
bench-client-heavy:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario mixed

# One bounded release-client Samply capture with a scene-record sidecar.
profile-client-heavy:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario mixed --samply

# One dense, fixed-envelope Samply capture: thousands of entities plus dense
# sign, light, liquid, palette, transparent, block-entity, and scheduled work.
profile-client-heavy-dense:
    python3 scripts/client-frame-benchmark.py --workload heavyweight --heavy-scenario dense-mixed --heavy-scale 1 --samply

# Validate a saved heavyweight capture, Samply sidecar, and emitted scene record
# without launching the client, server, GPU, or Samply.
validate-client-heavy-profile capture:
    python3 scripts/client-frame-benchmark.py --validate-heavy-profile {{capture}}

# Turn a saved capture into CPU-delta-weighted inclusive and self-time tables.
profile-cost-table capture:
    python3 scripts/profile-cost-table.py {{capture}}

# Live-oracle launchers — one recipe per canonical oracle. Each script
# creates a fresh container and tears it down when it exits. See
# docs/oracles-and-benchmarks.md and CLAUDE.md for the spawn contracts.
oracle-creative:
    ./scripts/live-oracles/creative.sh

oracle-terrain:
    ./scripts/live-oracles/terrain.sh

oracle-megaworld:
    ./scripts/live-oracles/megaworld.sh

oracle-lovelier:
    ./scripts/live-oracles/lovelier.sh

oracle-survival:
    ./scripts/live-oracles/survival.sh

# Bounded, opt-in release-client acceptance matrix. The supplied driver must
# operate an installed release client and write the external evidence contract;
# this command never substitutes Lodestone's own client for that artifact.
# Example: just external-client-acceptance --protocol 766 --output /private/tmp/lodestone-v766
external-client-acceptance *args:
    python3 scripts/live-oracles/external-client-acceptance.py {{args}}

# Re-capture the README's in-game screenshots into docs/images/, by joining the
# flat creative oracle with the real client and rendering one frame per scene.
# Needs `just oracle-creative` up first, plus a GPU adapter and the vanilla
# assets under .cache/mc/26.2. Scenes are data — scripts/screenshot-scenes/*.txt
# — so editing one costs no recompile; LODESTONE_SCENES=stem1,stem2 restricts a
# run to those files. See docs/screenshots.md.
#
# --test-threads=1 is not tidiness: every scene shares one world, one session
# and one GPU context, and the harness rebuilds the stage between shots.
[doc("re-capture docs/images/*.png from a live session (needs `just oracle-creative`)")]
screenshots:
    cargo test {{jflag}} --target-dir {{tdir}} -p lodestone-shell --features live --test capture_screenshots -- --ignored --nocapture --test-threads=1

# Re-dump the per-block blast-resistance + flammability facts (#312/#313) from
# the real 26.2 server, over the committed anchor
# (crates/lodestone-data/tests/support/blast_fire_jvm.txt). Needs Apple
# `container` (see docs/oracles-and-benchmarks.md). Follow with `just regen-blast-fire`.
oracle-blast-fire:
    #!/usr/bin/env bash
    set -euo pipefail
    CACHE="$(cd .cache/mc/26.2 && pwd)"
    HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
    container system start >/dev/null 2>&1 || true
    container run --rm --memory 3g -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work \
      eclipse-temurin:25-jdk bash -c '
        set -e
        CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
        mkdir -p /work && cp /oracle/BlastFireOracle.java /work/
        javac -cp "$CP" -d /work /work/BlastFireOracle.java
        java -cp "/work:$CP" BlastFireOracle
      '

# Regenerate crates/lodestone-data's blast/flammability table
# (src/generated/block_blast.rs) from the committed JVM dump. Test:
# crates/lodestone-data/tests/block_blast.rs :: committed_table_matches_dump
# (#[ignore]d). Re-dump first with `just oracle-blast-fire` after a data bump.
regen-blast-fire:
    LODESTONE_REGEN=1 cargo test -p lodestone-data --test block_blast {{jflag}} --target-dir {{tdir}} committed_table_matches_dump -- --ignored --nocapture

# Reproduces docs/oracles-and-benchmarks.md's baseline-vs-PGO instructions-retired
# comparison on demand (issue #556: opt-in, NOT a default build-config
# change -- see that doc before reading anything into the number this
# prints). Three full `--release` builds in a private CARGO_TARGET_DIR
# (RUSTFLAGS changes between them, so a shared target dir would cost every
# other live agent a cold-rebuild wave); expect several minutes per build on
# a loaded machine, not the doc's original "a few minutes total" figure.
# macOS only (the counter is proc_pid_rusage). No {{jflag}}/{{tdir}}: this
# recipe deliberately does not touch the shared target dir at all.
pgo-probe:
    ./scripts/pgo-probe.sh

# --- PGO for the game binary (opt-in, three steps) ------------------------
#
# PGO is deliberately NOT a default profile setting: the owner's call is that
# it stays off until a real-workload measurement justifies it, and the only
# number we have is 14.6% fewer instructions retired on a worldgen probe --
# a proxy, not frame time or tick time. See docs/oracles-and-benchmarks.md.
#
# These are three recipes rather than a `--pgo` flag on `run` for the reason
# this file's header gives: a flag would mean parsing an argument and
# branching on it inside a recipe body, which is the one thing that header
# forbids -- the same reason `run-wasm` is its own name rather than
# `run --surface wasm`. Three names, three raw invocations.
#
# The cycle is inherently interactive: an instrumented binary only learns a
# useful profile from a representative workload, and for a game that means
# PLAYING it. So `pgo-instrument` builds and launches; you play for a few
# minutes doing whatever you care about being fast; you quit; `pgo-merge`
# folds the .profraw files into one .profdata; `run-pgo` rebuilds against it.
#
# All three use a private target dir (LODESTONE_PGO_DIR, default
# target/pgo) because RUSTFLAGS differs from every other recipe here and a
# shared target dir would cost every concurrent build a cold rebuild.
# `pgo-merge` needs llvm-profdata: `xcrun llvm-profdata` on macOS, or the
# one in ~/.rustup/toolchains/*/lib/rustlib/*/bin/.

# cargo run --release with -Cprofile-generate -- play a representative session, then quit
pgo-instrument:
    RUSTFLAGS="-Cprofile-generate={{pgo_dir}}/raw" cargo run --release -p lodestone-shell --bin lodestone {{jflag}} --target-dir {{pgo_dir}}/build

# xcrun llvm-profdata merge -- fold the recorded .profraw files into one .profdata
pgo-merge:
    xcrun llvm-profdata merge -o {{pgo_dir}}/merged.profdata {{pgo_dir}}/raw

# cargo build --release with -Cprofile-use -- the optimised binary, not installed anywhere
build-pgo:
    RUSTFLAGS="-Cprofile-use={{pgo_dir}}/merged.profdata -Cllvm-args=-pgo-warn-missing-function" cargo build --release -p lodestone-shell --bin lodestone {{jflag}} --target-dir {{pgo_dir}}/build

# cargo run --release with -Cprofile-use -- play the PGO-optimised build
run-pgo *args:
    RUSTFLAGS="-Cprofile-use={{pgo_dir}}/merged.profdata -Cllvm-args=-pgo-warn-missing-function" cargo run --release -p lodestone-shell --bin lodestone {{jflag}} --target-dir {{pgo_dir}}/build -- {{args}}

# --- Git hooks --------------------------------------------------------------

# Point git at .githooks/, which carries this repo's pre-commit hook. Hooks are
# per-clone configuration rather than tracked state, so this has to be run once
# in each checkout; it is idempotent. The pre-commit hook enforces a token
# budget on the files that are auto-loaded into an AI agent's context
# (CLAUDE.md, AGENTS.md) -- see the header of .githooks/pre-commit for the cap
# and how it was chosen.
[doc("point git at .githooks/ -- one-off per clone; enforces the CLAUDE.md token budget")]
install-hooks:
    git config core.hooksPath .githooks
    @echo "core.hooksPath -> $(git config core.hooksPath)"

# --- Fuzzing (docs/fuzzing.md) ----------------------------------------------
#
# `fuzz/` is its own cargo-fuzz workspace (own Cargo.lock, own empty
# `[workspace]` table), the same reason `web/` and the wasm guests under
# `crates/lodestone-wasm-host` are — libFuzzer/ASan instrumentation flags must
# not leak into a plain `cargo build --workspace`. Both recipes below `cd` into
# it rather than using `--manifest-path`, because `cargo fuzz` resolves its
# corpus/artifact directories relative to the current directory, not the
# manifest. Two recipes, not one with a `--check`-only flag, for the same
# reason `run-wasm`/`run-pgo` are their own names: this file's header forbids
# a recipe that parses an argument and branches on it. Needs the nightly
# toolchain `rust-toolchain.toml` already pins (no separate `+channel` needed:
# a fuzz/ invocation picks it up the same way any other cargo command here
# does) and the `cargo-fuzz` binary (`cargo install cargo-fuzz`).

# cargo fuzz build -- compiles every fuzz/fuzz_targets/*.rs under ASan, no run
[doc("cargo-fuzz ASan build of every fuzz/fuzz_targets/*.rs -- no run")]
fuzz-build:
    cd fuzz && cargo fuzz build

# cargo fuzz run <target> -- bounded run of one target; pass libFuzzer flags after the name,
# e.g. `just fuzz-run nbt_decode -max_total_time=60 -rss_limit_mb=1024`. ALWAYS pass
# -max_total_time on a shared machine -- an unbounded run never terminates on its own,
# and CLAUDE.md's disk/memory hazards apply to a fuzz corpus exactly as they do to target/.
[doc("cargo fuzz run <target> [libfuzzer-flags...] -- ALWAYS pass -max_total_time=N")]
fuzz-run target *args:
    cd fuzz && cargo fuzz run {{target}} -- {{args}}

# cargo fuzz run <target> starting from the COMMITTED seed corpus rather than
# from whatever `fuzz/corpus/` happens to hold. Prefer this over `fuzz-run` for
# any real campaign: a decoder started from an empty corpus spends its first
# minutes rediscovering that a packet begins with a varint, while
# `fuzz/seeds/<target>/` already carries bytes a real vanilla server wrote.
# libFuzzer writes new units only into the FIRST corpus directory, so the
# committed seeds stay read-only. Same "ALWAYS pass -max_total_time" rule as
# `fuzz-run`.
[doc("cargo fuzz run <target> from the committed seeds -- ALWAYS pass -max_total_time=N")]
fuzz-run-seeded target *args:
    cd fuzz && cargo fuzz run {{target}} corpus/{{target}} seeds/{{target}} -- {{args}}

# Re-run one saved crash/timeout artifact under the target that produced it --
# the first thing to do with a `fuzz/artifacts/...` file, and what CI's failure
# message points at. Deterministic: one input, one execution, no mutation.
[doc("cargo fuzz run <target> <artifact-file> -- reproduce one saved crash")]
fuzz-repro target artifact:
    cd fuzz && cargo fuzz run {{target}} {{artifact}}

# Bounded run of EVERY fuzz target from the committed seeds -- the recipe CI's
# `fuzz` job calls. Gates on panics, OOM and hangs; exclusions (with reasons)
# live in fuzz/smoke-exclusions.txt. The default 30s per target is a tripwire,
# not a campaign: it proves each target still builds, loads its seeds and
# reaches real code, and it replays the whole committed corpus.
[doc("bounded cargo-fuzz run of every target from committed seeds (CI's fuzz job)")]
fuzz-smoke seconds="30":
    ./fuzz/smoke.sh {{seconds}}

# Rebuild fuzz/seeds/** from data this repo did not author: the vanilla
# packet-id and block-state reports, captured 26.2 wire payloads, the vanilla
# data pack, and a world save a real vanilla server wrote. Needs a populated
# `.cache/mc` (not repo state -- see docs/fuzzing.md); a missing source is a
# hard error rather than a quietly smaller corpus. The seeds it writes ARE
# committed, so this is only needed when a new capture or a new target lands.
[doc("regenerate fuzz/seeds/** from .cache/mc vanilla data (needs a populated .cache)")]
fuzz-seeds-regen:
    python3 fuzz/seeds/generate-seeds.py

# Restore one historical fluid scheduling defect in a disposable detached
# worktree, then require the generated live differential search to find,
# shrink and replay the resulting divergence. The local vanilla oracle must
# already be running; see docs/fuzzing.md for the command and cleanup scope.
[doc("verify the generated live fluid search rediscovers its historical delay-one seed defect")]
fuzz-historical-fluid-reversion:
    ./scripts/historical-fluid-reversion.sh

# Reclaim disk from `target/` without disturbing a build in progress.
#
# Two independently safe reclaims, in increasing order of cost to redo:
#
#  1. `target/debug/incremental` -- pure cache, and the single largest sink
#     here: measured at 22 GB, 8.6 GB and 8.0 GB on three separate days.
#     sccache refuses to cache an incremental compilation at all ("incremental
#     compilation is prohibited"), so nothing downstream depends on these
#     files. Skipped while any `rustc` is live, because that is when deleting
#     them can fail a compile rather than merely slow the next one.
#  2. Per-crate build directories under `target/debug/build/*/` untouched for
#     more than a day. Cargo never garbage-collects these, so they accumulate
#     one directory per crate per configuration: `lodestone-server` alone held
#     2,112. A directory nothing has written to in 24h cannot belong to a
#     running compile; the worst case is that cargo re-runs a build script.
#
# What this deliberately does NOT do is `rm -rf target/debug`, which is the
# reclaim that works but takes every concurrent agent's in-flight compile with
# it -- its signature is a flood of `E0463 can't find crate` hitting every
# crate uniformly.
#
# Measured expectation on this workspace: reclaim 2 freed 5.6 GB with 456 of
# 6,317 build directories stale, so on a busy day most of `target/debug/build`
# is genuinely live and reclaim 1 is where the space is. If both leave you
# short, the fleet is simply too large for one target directory -- see the
# sccache/incremental measurements on the infrastructure issue.
[doc("reclaim target/ disk that no running build depends on")]
reclaim:
    #!/usr/bin/env bash
    set -euo pipefail
    df -h . | tail -1
    live=$(ps -Ao command | grep -c "[r]ustc" || true)
    if [ "$live" -eq 0 ]; then
        rm -rf target/debug/incremental
        echo "removed target/debug/incremental"
    else
        echo "kept target/debug/incremental ($live rustc live; re-run when idle)"
    fi
    stale=$(find target/debug/build -mindepth 2 -maxdepth 2 -type d -mtime +1 2>/dev/null | wc -l | tr -d ' ')
    find target/debug/build -mindepth 2 -maxdepth 2 -type d -mtime +1 -print0 2>/dev/null \
        | xargs -0 rm -rf 2>/dev/null || true
    echo "removed $stale build directories untouched for over a day"
    df -h . | tail -1
