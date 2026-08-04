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

# --- Delegating wrappers (scripts/* keep their bodies and paths) ------------

# wasm32 compile + confinement-guard tripwire (debug build, fast). Does NOT
# prove the browser runs — see the script's own header.
wasm-check:
    ./scripts/wasm-check.sh

# Release wasm bundle-size ceiling (gzip-enforced; brotli reported when
# available). Separate from wasm-check because a --release + lto=fat build
# is slow enough that folding it in would slow the command everyone runs.
wasm-size:
    ./scripts/wasm-size.sh

# Region-level worldgen throughput/peak-RSS sweep. No args: the script's own
# courteous default radii (8 16) apply. Pass radii to override, e.g.
# `just worldgen-sweep 3 32` for the full RD-32 sweep — only on an otherwise
# idle machine, per CLAUDE.md.
worldgen-sweep *args:
    ./scripts/worldgen-region-sweep.sh {{args}}
