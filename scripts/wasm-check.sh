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
#   "compiles" and "works" are different, and a family of std/tokio calls COMPILE
#   for wasm32 and only misbehave at RUNTIME.
#
#   BUT THEY DO NOT ALL MISBEHAVE THE SAME WAY, and an earlier version of this
#   header grouped them as though they did. Measured by compiling each into a
#   cdylib with `panic = "abort"` and EXECUTING it in a real wasm VM:
#
#       std::fs::*             returns Err(ErrorKind::Unsupported) -- DOES NOT TRAP
#       std::time::Instant     ::now() TRAPS (RuntimeError: unreachable)
#       std::time::SystemTime  ::now() TRAPS  <-- was named nowhere before
#       std::thread::spawn     TRAPS
#       tokio::time::*         timeout()/sleep() panic without a wasm timer
#
#   The split matters for triage. The trapping calls are EMERGENCIES: one reached
#   call kills the tab, and with `panic = "abort"` it is not recoverable. `std::fs`
#   is a DEGRADATION: every caller that already discards its error (`.ok()?`,
#   `let Ok(..) else`) resolves to honest absence -- "no options file", "no saves"
#   -- which is a correctness/UX problem to fix incrementally, not a crash. Do not
#   spend the emergency budget on `fs`.
#
#   `SystemTime::now()` is the one this file used to miss entirely. lodestone-shell
#   had 8 production sites (clock-derived seeds, the chat caret blink, glint phase,
#   the recipe-toast clock) and every one would have aborted the tab. See
#   docs/browser-shell-port.md.
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
  # The canonical 26.2 game-data censuses (issue #361), extracted out of
  # lodestone-v770. Depends on nothing but lodestone-model, so it was already
  # wasm-safe transitively via lodestone-v770's own row below; listed
  # separately so a future regression here is unambiguous rather than only
  # surfacing through v770.
  "lodestone-data"
  "lodestone-v770"
  "lodestone-v47"
  "lodestone-net|--features ws-web"
  # docs/bevy-migration.md Stage 0's single biggest go/no-go: bevy_ecs must be
  # wasm32-clean, or the whole migration stops here. Placed after
  # lodestone-client (which now depends on it via EcsHandle/WorldTime) so a
  # failure here is unambiguous rather than only surfacing transitively.
  "lodestone-ecs"
  "lodestone-client"
  "lodestone-controller"
  # Added once the tokio target-split landed (impl-worldgen) and the crate was
  # verified to compile to wasm32: the integrated server runs in the browser
  # under the `spawn_local` seam, and browser singleplayer (web/ → client ↔
  # server over an in-memory duplex) now depends on it. Guarding it here catches
  # a wasm regression at the crate, not only transitively via lodestone-web.
  "lodestone-server"
  "lodestone-worldgen"
  # The playable game shell -- the menu, `Sim`, the renderer, all of it. This is
  # the whole point of `web/` becoming the real client rather than a spike: the
  # browser consumes this crate's LIB target. Placed last because it sits on top of
  # everything above, so a failure here is unambiguous rather than transitive.
  #
  # It is ~166k lines and it is the crate most likely to regress, because almost
  # nobody working in it is building for wasm. That is exactly why it is guarded
  # here and why the confinement rules below name it three times.
  "lodestone-shell"
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
    echo "      └─ two common causes: (a) a dependency pulled '$pkg' onto native-only"
    echo "         code (threads / std::fs / OS sockets / OS audio like cpal) — fix by gating"
    echo "         that dep or call behind cfg(not(target_arch = \"wasm32\")) or an"
    echo "         off-by-default feature; or (b) a plain compile error in '$pkg' or a crate"
    echo "         it depends on — which, in this shared workspace, is often a sibling crate"
    echo "         mid-edit (see the named crate in the error above): wait and re-run."
    echo "         Reproduce: cargo build -p $pkg --target $TARGET $extra"
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
  # --- lodestone-shell ---
  # The shell confines both trapping clocks to `crate::platform`, which re-exports
  # `web_time` (std's own types on native, `performance.now()`/`Date.now()` in a
  # browser). These rules ban the `std::time::` PATHS rather than the bare
  # `Instant::now(` spelling, deliberately: the shell's call sites now read
  # `crate::platform::Instant::now()`, so a `Instant::now(` pattern would match all
  # 30 of them and the rule could never go green. The path is what distinguishes a
  # trapping call from a portable one.
  #
  # `platform.rs` is allowlisted because it is the one file that may name the real
  # `std::time` items. Both rules found LIVE TRAPS when first added, on a tree whose
  # `cargo check --target wasm32-unknown-unknown` was already exit 0 -- which is the
  # entire argument for having them.
  # The allowlist is `platform.rs` ALONE — the strongest form, matching the
  # `lodestone-audio time-confinement` rule's empty one. `tests.rs` was in it
  # briefly, and taking it out was the right call: a test that names the trapping
  # clock cannot crash a browser (test code is never in a wasm `--lib` build), but
  # `crate::platform::Instant` *is* `std::time::Instant` on native, so converting
  # those sites cost nothing and turned "the shell never names the trapping clock"
  # from a promise into something one grep can check. 19 test sites in `net.rs`,
  # `menu/render/tests.rs` and `sim/tests.rs` were converted for exactly that.
  "lodestone-shell instant-confinement|crates/lodestone-shell/src|std::time::Instant|platform.rs"
  "lodestone-shell systemtime-confinement|crates/lodestone-shell/src|std::time::SystemTime::now|platform.rs"
)

for rule in "${CONFINEMENT_RULES[@]}"; do
  IFS='|' read -r c_label c_dir c_banned c_allow <<< "$rule"
  printf '  %-34s ' "$c_label"
  leak="$(grep -rn "$c_banned" "$ROOT/$c_dir" 2>/dev/null || true)"
  # Drop COMMENT lines before judging. The banned symbol legitimately appears in
  # prose — every one of these confinements is worth a sentence at its call site
  # saying "`crate::platform::epoch_duration`, not `SystemTime::now()`, because the
  # latter traps" — and a guard that fires on its own documentation trains people to
  # delete the documentation. `grep -rn` output is `path:line:content`, so this
  # strips the two leading fields and drops the line when what remains begins with
  # `//` (a Rust line or doc comment), `*` (a `/* … */` continuation) or `#` (a
  # comment in a manifest or script, for rules pointed at one).
  #
  # This does NOT weaken any rule: a hazard inside a comment cannot execute. It is
  # the same reasoning that made a `"` legal inside a `.wgsl` comment.
  leak="$(printf '%s\n' "$leak" | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|\*|#)' || true)"
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
