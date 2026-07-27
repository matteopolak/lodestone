#!/usr/bin/env bash
#
# wasm-check.sh — repeatable wasm32 compile guard for Lodestone.
#
# WHY THIS EXISTS
#   The `uuid` breakage that blocked the entire browser path was invisible for
#   months because *nobody had ever built for wasm*. This script makes that class
#   of regression impossible to reintroduce silently: it compiles the
#   wasm-relevant crate subset for `wasm32-unknown-unknown` and fails loudly if
#   any of them stops compiling.
#
# WHAT IT PROVES  (read this before trusting a green run)
#   Two separate things: (1) a COMPILE pass for the wasm crate subset, and
#   (2) a set of grep-based CONFINEMENT guards. You need both because on wasm
#   "compiles" and "works" are different, and a whole family of std/tokio calls
#   COMPILE for wasm32 and only die at RUNTIME (verified: each of the first three
#   builds green in a throwaway wasm32 crate):
#       std::fs::*            filesystem — there is no fs in a browser
#       std::time::Instant    ::now() panics
#       std::thread::spawn    no threads without COOP/COEP + shared memory
#       tokio::time::*        timeout()/sleep() panic without a wasm timer
#
#   The COMPILE step below is STRUCTURALLY BLIND to every one of these: a freshly
#   added `std::fs::read(...)` compiles green and dies only when a browser runs
#   it. `cfg(target_arch = "wasm32")` does NOT turn such a call into a compile
#   error either — it only removes the *existing* native entry points, so
#   *referencing* a removed symbol (e.g. `DirectorySource`, `ZipSource::open`)
#   fails to compile, while a brand-new ungated `fs::read` sails straight through.
#   (A Cargo feature is weaker still: feature unification lets any consumer, e.g.
#   lodestone-render, re-enable a default-on feature across the whole graph.)
#
#   What actually catches this class is the CONFINEMENT GUARD below. The pattern:
#   when the type system can't express a constraint ("no fs on wasm"), make it
#   CHECKABLE and check it — the owning crate confines the hazard to a single
#   `cfg(not(target_arch = "wasm32"))`-gated file, and we grep for the banned
#   symbol everywhere ELSE and FAIL (naming file:line) if it reappears.
#
#   A green run therefore means: nothing regressed the wasm *compile*, AND no
#   confined hazard leaked out of its gated file. It still does NOT prove the
#   browser runs. Treat it as a tripwire, not a functional test.
#
# WHERE THIS SHOULD EVENTUALLY LIVE
#   This is intentionally a standalone script because `xtask` is owned by another
#   agent right now. Once that settles it should become `cargo xtask wasm-check`
#   (same crate list, same "compilation only" caveat printed) so it can join CI.
#
# USAGE
#   scripts/wasm-check.sh
#
# PREREQUISITES (the script verifies both and FAILS with the install command if
# either is missing — a check that cannot run must fail, not pass quietly):
#   * rustup target: wasm32-unknown-unknown
#   * trunk (0.21.x) — the browser app is built through it as the final step.
#
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TARGET="wasm32-unknown-unknown"

if ! rustup target list --installed 2>/dev/null | grep -q "$TARGET"; then
  echo "error: rust target '$TARGET' is not installed."
  echo "       this check CANNOT RUN without it — failing rather than passing quietly."
  echo "       run: rustup target add $TARGET"
  exit 2
fi

# trunk is required: it is how the browser app is built and served, and the final
# step below builds the app THROUGH it (cargo -> wasm -> wasm-bindgen bundle), so
# a wasm-bindgen-level break is caught, not just a rustc one. A check that cannot
# run must FAIL, not skip — so a missing trunk is exit 2 with the install command,
# never a quiet green.
if ! command -v trunk >/dev/null 2>&1; then
  echo "error: 'trunk' is not installed (required to build/serve the browser app)."
  echo "       this check CANNOT RUN without it — failing rather than passing quietly."
  echo "       run: cargo install trunk --version 0.21.14"
  echo "       or (prebuilt, faster): curl -sSL \\"
  echo "         https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-\$(uname -m)-apple-darwin.tar.gz \\"
  echo "         | tar xz -C ~/.cargo/bin trunk"
  exit 2
fi

# Workspace crates that are expected to compile to wasm, with any features that
# the browser configuration requires. Format: "<pkg>[|<extra cargo args>]".
CRATES=(
  "lodestone-core"
  "lodestone-model"
  "lodestone-world"
  "lodestone-physics"
  "lodestone-assets"
  "lodestone-registry"
  "lodestone-render"
  "lodestone-audio"
  # The event→sound bridge. Its default build is device-free and version-free
  # (deps: lodestone-audio, lodestone-assets, lodestone-model, glam, thiserror —
  # all wasm-safe); the live gate's client/tokio/registry deps are gated behind
  # the off-by-default `live-v770` feature, so the wasm build never pulls them.
  "lodestone-sound"
  "lodestone-v770"
  "lodestone-v47"
  "lodestone-net|--features ws-web"
  "lodestone-client"
  "lodestone-controller"
  # Added once the tokio target-split landed (impl-worldgen) and the crate was
  # verified to compile to wasm32: the integrated server runs in the browser
  # under the `spawn_local` seam, and browser singleplayer (web/ → client ↔
  # server over an in-memory duplex) now depends on it. Guarding it here catches
  # a wasm regression at the crate, not only transitively via lodestone-web.
  "lodestone-server"
  "lodestone-worldgen"
)

fails=()

echo "== Lodestone wasm32 compile guard =="
echo "target: $TARGET"
echo

for entry in "${CRATES[@]}"; do
  pkg="${entry%%|*}"
  extra=""
  [[ "$entry" == *"|"* ]] && extra="${entry#*|}"
  printf '  %-34s ' "$pkg ${extra}"
  # shellcheck disable=SC2086
  if out="$(cargo build -p "$pkg" --target "$TARGET" $extra 2>&1)"; then
    echo "PASS"
  else
    echo "FAIL"
    fails+=("$pkg $extra")
    # Name the offender and the fix, inline. The captured cargo error usually
    # names the actual native-only crate (e.g. `could not compile \`cpal\``),
    # which is the single most useful line for whoever just broke the build.
    printf '%s\n' "$out" \
      | grep -E "^error|could not compile|is not supported|cannot find (function|type|crate)|unresolved import|native" \
      | head -6 | sed 's/^/      │ /'
    echo "      └─ likely cause: a dependency pulled '$pkg' onto native-only code"
    echo "         (threads / std::fs / OS sockets / OS audio like cpal). Fix: gate that"
    echo "         dep or call behind cfg(not(target_arch = \"wasm32\")) or an off-by-default"
    echo "         feature. Reproduce: cargo build -p $pkg --target $TARGET $extra"
  fi
done

# --- Confinement guards ------------------------------------------------------
# A family of calls compile to wasm32 and then die at runtime (see header). The
# type system can't forbid them and the compile pass above is blind to them, so
# each owning crate confines its hazard to a single
# cfg(not(target_arch = "wasm32"))-gated file, and we enforce that by grepping for
# the banned symbol everywhere ELSE in that crate. This is the general form of the
# original one-off lodestone-assets fs guard.
#
# Each rule (fields separated by '|'):
#   <label> | <src dir under repo root> | <banned grep regex> | <comma-sep allowlisted file basenames>
#
# ADD A ROW ONLY AFTER YOUR CRATE ACTUALLY CONFINES THE HAZARD. A rule for a crate
# that still calls the symbol in ungated code will (correctly) go red for everyone
# who runs this script, so confine first, then add the guard.
# (lodestone-audio has NO time source at all — its clock is sample-driven,
# deterministic from frames rendered — so the strongest form applies: the
# Instant::now() call is banned across the WHOLE crate with an empty allowlist,
# making "audio never touches wall-clock time" a checked invariant rather than a
# promise. If a future latency/xrun measurement ever needs Instant, it must live
# in one cfg(not(wasm32))-gated file added to that rule's allowlist, same shape
# as the fs and device rules.)
# (lodestone-client confines its runtime-timer hazard — tokio::time::timeout/sleep,
# which compiles to wasm and panics for want of a timer-enabled runtime — to the
# single cfg(not(wasm32))-gated src/native_time.rs, and additionally bans the
# whole Instant::now()/std::fs/std::thread family across the crate with empty
# allowlists: the driver is event-driven and never reads a wall clock, so those
# are checked invariants. tokio::spawn is confined to the spawn.rs seam, whose
# wasm arm uses wasm_bindgen_futures::spawn_local instead.)
CONFINEMENT_RULES=(
  "lodestone-assets fs-confinement|crates/lodestone-assets/src|std::fs::|source_native.rs"
  "lodestone-audio device-confinement|crates/lodestone-audio/src|cpal::|sink.rs"
  "lodestone-audio time-confinement|crates/lodestone-audio/src|Instant::now(|"
  "lodestone-sound time-confinement|crates/lodestone-sound/src|Instant::now(|"
  "lodestone-client time-confinement|crates/lodestone-client/src|tokio::time::|native_time.rs"
  "lodestone-client instant-ban|crates/lodestone-client/src|Instant::now(|"
  "lodestone-client fs-ban|crates/lodestone-client/src|std::fs::|"
  "lodestone-client thread-ban|crates/lodestone-client/src|std::thread|"
  "lodestone-client spawn-confinement|crates/lodestone-client/src|tokio::spawn|spawn.rs"
)

for rule in "${CONFINEMENT_RULES[@]}"; do
  IFS='|' read -r c_label c_dir c_banned c_allow <<< "$rule"
  printf '  %-34s ' "$c_label"
  leak="$(grep -rn "$c_banned" "$ROOT/$c_dir" 2>/dev/null || true)"
  if [[ -n "$c_allow" ]]; then
    IFS=',' read -ra c_allow_files <<< "$c_allow"
    for f in "${c_allow_files[@]}"; do
      [[ -z "$f" ]] && continue
      leak="$(printf '%s\n' "$leak" | grep -v "/$f:" || true)"
    done
  fi
  leak="$(printf '%s' "$leak" | sed '/^[[:space:]]*$/d')"
  if [[ -z "$leak" ]]; then
    echo "PASS"
  else
    echo "FAIL"
    echo "$leak" | sed 's/^/      /'
    fails+=("$c_label: '$c_banned' used outside {${c_allow:-none}}")
  fi
done

# The browser app is its own workspace (outside the crates/ glob), so it is built
# from its own directory. This is the end-to-end integration of the subset above,
# and it is built THROUGH trunk on purpose: trunk runs cargo for wasm32 and then
# wasm-bindgen, so this step catches a wasm-bindgen-level break (a signature rustc
# accepts but wasm-bindgen rejects), not only a rustc one. Cheap because the crate
# graph above is already warm in the shared target dir.
if [[ -f "$ROOT/web/Cargo.toml" ]]; then
  printf '  %-34s ' "lodestone-web (trunk build)"
  if out="$(cd "$ROOT/web" && trunk build 2>&1)"; then
    echo "PASS"
  else
    echo "FAIL"
    fails+=("lodestone-web (trunk build)")
    printf '%s\n' "$out" \
      | grep -E "^error|could not compile|is not supported|unresolved import|wasm-bindgen|error from" \
      | head -8 | sed 's/^/      │ /'
    echo "      └─ the browser app failed to build. If the per-crate rows above are all"
    echo "         PASS, this is a wasm-bindgen/trunk-level break in web/ itself."
    echo "         Reproduce: (cd web && trunk build)"
  fi
fi

echo
if (( ${#fails[@]} > 0 )); then
  echo "RESULT: FAIL — ${#fails[@]} crate(s) no longer compile to $TARGET:"
  for f in "${fails[@]}"; do echo "  - $f"; done
  exit 1
fi

echo "RESULT: PASS — all listed crates COMPILE to $TARGET."
echo
echo "NOTE: the COMPILE pass proves compilation, NOT runtime, and is blind to the"
echo "      'compiles on wasm, panics at runtime' family: std::fs, Instant::now,"
echo "      std::thread::spawn, tokio::time all build green here. cfg(target_arch)"
echo "      does NOT turn a fresh ungated call into a compile error (it only removes"
echo "      existing native entry points), and a Cargo feature is weaker still"
echo "      (unification re-enables it). The CONFINEMENT guards above are what"
echo "      actually catch a leaked hazard, by grepping it back to file:line."
