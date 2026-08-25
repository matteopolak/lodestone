# Lodestone task runner — the canonical-COMMAND layer, not a script host.
#
# Division of labour (see docs/task-runner.md for the full writeup):
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

# Per-agent private target dir (docs/build-caching.md). Deliberately NOT a
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
# LODESTONE_JOBS=4 yourself for local multi-agent courtesy (docs/build-caching.md).
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
check-seam:
    cargo check -p lodestone-shell --no-default-features {{jflag}} --target-dir {{tdir}}

# cargo test --workspace --no-fail-fast. Plain `cargo test` stops at the
# first failing test BINARY, hiding every alphabetically-later crate's
# failures — --no-fail-fast is not optional here.
test:
    cargo test --workspace --no-fail-fast {{jflag}} --target-dir {{tdir}}

# All four checks above, in order.
health: check check-all check-seam test

# --- Running the game -------------------------------------------------------

# The [[bin]] is `lodestone`, not `lodestone-shell`, and `default-members`
# makes a bare `cargo run` target the shell anyway; -p is spelled out so this
# recipe does not depend on that. Release is not a preference here — a debug
# build is unplayable. The `live` feature (which turns on v770, and nothing
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
# --target-dir (docs/build-caching.md), so agents were hand-expanding this
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
# docs/oracle-runtimes.md). Follow with `just regen-snow-support`.
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
# top_layer_parity. See docs/worldgen-freeze-top-layer.md for why these four
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
# without one. See docs/frame-profiling.md.
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

# Hermitcraft S10 at RD24, once with F3 closed and once open. Install the
# pinned world first with `python3 scripts/install-client-benchmark-world.py`.
bench-client-megaworld:
    python3 scripts/client-frame-benchmark.py --workload megaworld

# One 2s/2s/3s run per F3 arm: setup/compatibility gate before full trials.
bench-client-megaworld-smoke:
    python3 scripts/client-frame-benchmark.py --workload megaworld --smoke

# Live-oracle launchers — one recipe per canonical oracle. Each script
# creates a fresh container and tears it down when it exits. See
# docs/oracle-runtimes.md and CLAUDE.md for the spawn contracts.
oracle-creative:
    ./scripts/live-oracles/creative.sh

oracle-terrain:
    ./scripts/live-oracles/terrain.sh

oracle-megaworld:
    ./scripts/live-oracles/megaworld.sh

oracle-survival:
    ./scripts/live-oracles/survival.sh

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
# `container` (see docs/oracle-runtimes.md). Follow with `just regen-blast-fire`.
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

# Reproduces docs/pgo-experiment.md's baseline-vs-PGO instructions-retired
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
# a proxy, not frame time or tick time. See docs/pgo-experiment.md.
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
