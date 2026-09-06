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
# WHERE THIS LIVES NOW  (this section used to say "eventually" — it has happened)
#   `cargo xtask wasm-check` is the LIVE implementation: `just wasm-check` calls it
#   and the CI `wasm` job calls that recipe. This script is the reference original,
#   kept because xtask's `wasm_crates()` and `confinement_rules()` are asserted
#   against the tables below by name. Fix a mechanism in BOTH — the port is a
#   parity target, not a fork.
#
# READING A FAILURE FROM THIS SCRIPT
#   Every captured build is written to `target/wasm-check/<pkg>.log` in full, and
#   what is printed to the console is a summary of that file. If the summary is
#   not enough, read the file. This is not decoration: an earlier version piped
#   each build through `grep -E "^error…" | head -8` and nothing else, and the one
#   CI failure it ever caught reached the log as two lines naming neither a file
#   nor a cause. See report_build_failure below for the three properties that
#   fixed it.
#
# A RULE THAT CANNOT RUN IS A FAILURE, NEVER A PASS
#   Measured, and the reason the loop below looks paranoid: five of these rules
#   printed PASS for their whole life without their grep ever executing. The rule
#   table is `|`-separated and five patterns spelled a BRE alternation
#   `\(Instant\|SystemTime\)`, whose `\|` IS the field separator — so
#   `IFS='|' read` truncated the pattern to `std::time::\(Instant\`, grep exited 2
#   ("trailing backslash" on BSD grep, "parentheses not balanced" on others), and
#   the `|| true` that swallows grep's "no match" exit 1 swallowed the ERROR too.
#   An empty result reads as "nothing leaked".
#
#   Three mechanism fixes, in order of how much they generalise:
#
#     1. grep's exit status is now read, and >=2 (an ERROR, as distinct from 1 =
#        no match) is a hard FAIL naming the rule and grep's own stderr. This is
#        the general form: a guard whose detector errored has measured nothing.
#     2. every rule row is validated to split into EXACTLY four fields before it
#        is used — a `|` anywhere inside a pattern is now a hard FAIL saying so,
#        rather than a silently truncated regex. The other twelve rules were
#        correct only because no pattern happened to contain a `|`; nothing in the
#        mechanism required it.
#     3. the five alternation rules are split into one rule per hazard
#        (`std::time::Instant` and `std::time::SystemTime` separately), so every
#        pattern in the table is now a LITERAL substring. That keeps the table
#        dialect-independent — BSD, GNU and ugrep disagree about BRE alternation —
#        and it is what lets `cargo xtask wasm-check` match these patterns with a
#        plain `str::contains`.
#
#   Each rule also has a POSITIVE CONTROL: `xtask`'s
#   `every_confinement_rule_fires_under_a_planted_violation` plants a violating
#   line in the crate each rule names, asserts the rule reports it by path, and
#   removes it. A confinement rule with no control is a rule you hope works.
#
# USAGE
#   scripts/wasm-check.sh                      # compile pass + guards + trunk build
#   scripts/wasm-check.sh --confinement-only   # just the greps (seconds, no cargo)
#
# PREREQUISITES (the script verifies both and FAILS with the install command if
# either is missing — a check that cannot run must fail, not pass quietly):
#   * rustup target: wasm32-unknown-unknown
#   * trunk (0.21.x) — the browser app is built through it as the final step.
# `--confinement-only` needs neither, and skips both checks for that reason.
#
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
TARGET="wasm32-unknown-unknown"

# `.cargo/config.toml` makes Cranelift the dev profile's codegen backend, and
# **Cranelift cannot emit wasm32**: every `cargo build --target
# wasm32-unknown-unknown` under it dies with "can't compile for
# wasm32-unknown-unknown: Support for this target has not been implemented yet",
# on the *proc-macro and dependency* crates, before a single line of ours is
# looked at. Measured: all 21 compile checks here failed that way, with error
# text naming `unicode-ident`, `once_cell` and `cfg-if` — which reads exactly
# like a real wasm confinement break in a sibling crate, and is not one.
#
# So this line is what makes the guard able to run at all, and it is worth
# stating why it is not merely a speed knob. `cargo check` is unaffected
# (Cranelift never runs, since nothing is codegen'd), so a `check`-based
# reproduction of this failure comes back green and sends the reader looking
# for a difference that is not there. The release profile already pins
# `codegen-backend = "llvm"` per-crate in the root `Cargo.toml`; the dev
# profile is the gap, and this is the one command in the repo that needs the
# dev profile to target a non-native architecture.
#
# `cargo xtask wasm-check` — the tested port, and the one CI runs — has carried
# this same env pair as `WASM_CODEGEN_BACKEND_ENV` all along. Only this script
# lacked it, which is the parity drift going the *other* way from the one this
# file's own history records (the xtask once carried 9 of 17 rules). Neither
# implementation is the master; they have to be diffed in both directions.
export CARGO_PROFILE_DEV_CODEGEN_BACKEND=llvm

CONFINEMENT_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --confinement-only) CONFINEMENT_ONLY=1 ;;
    -h|--help)
      echo "usage: scripts/wasm-check.sh [--confinement-only]"
      exit 0
      ;;
    *)
      echo "error: unknown argument '$arg'"
      echo "usage: scripts/wasm-check.sh [--confinement-only]"
      exit 2
      ;;
  esac
done

if (( CONFINEMENT_ONLY == 0 )) && ! rustup target list --installed 2>/dev/null | grep -q "$TARGET"; then
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
if (( CONFINEMENT_ONLY == 0 )) && ! command -v trunk >/dev/null 2>&1; then
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
  # The portable clock seam (issue #552): nearly every crate below depends on
  # this one, so a regression here would otherwise surface only transitively,
  # attributed to whichever dependent happened to fail first. Listed first for
  # the same reason lodestone-data is listed separately from v26-2.
  "lodestone-time"
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
  # the off-by-default `live-v26-2` feature, so the wasm build never pulls them.
  "lodestone-sound"
  # The canonical 26.2 game-data censuses (issue #361), extracted out of
  # lodestone-v26-2. Depends on nothing but lodestone-model, so it was already
  # wasm-safe transitively via lodestone-v26-2's own row below; listed
  # separately so a future regression here is unambiguous rather than only
  # surfacing through v26-2.
  "lodestone-data"
  "lodestone-v26-2"
  "lodestone-v1-8"
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

# Where every captured build goes. The console summary below is explicitly a
# summary OF THESE FILES, never the only copy — see report_build_failure.
LOGDIR="$ROOT/target/wasm-check"
mkdir -p "$LOGDIR"

# Substrings marking a line worth showing from a failed build, matched
# case-insensitively against ANSI-STRIPPED text.
#
# Deliberately NOT anchored. The anchored form this replaces (`grep -E "^error…"`)
# destroyed the evidence in the only CI failure this check has ever caught:
# `trunk` prefixes every line with an RFC-3339 timestamp and a level, so nothing
# it writes starts with `error`, and `cargo` under CARGO_TERM_COLOR=always (which
# CI sets for every job) starts its error lines with an escape sequence rather
# than a letter. The whole failure reached the log as two lines that named neither
# a file nor a cause, and had to be re-diagnosed from scratch.
DIAG_MARKERS='error|caused by|could not compile|is not supported|unresolved import|cannot find|wasm-bindgen'
DIAG_MAX_LINES=40

# Prints a diagnosable summary of a failed build FROM ITS LOG FILE, and says
# where that file is.
#
# Three properties, each of which the anchored-grep-then-`head` version lacked:
#
#   * matching happens on ANSI-STRIPPED text, so a coloured `error:` is found;
#   * a matched line brings its INDENTED CONTINUATION lines with it — which is
#     where `Caused by:` chains and rustc's `--> file:line` frames live, so a
#     per-line filter drops precisely the payload and keeps only the headline;
#   * when NOTHING matches, the tail is printed VERBATIM rather than nothing at
#     all. That is the mechanism fix: a filter that can yield an empty summary
#     turns a failing build into a silent one, and CLAUDE.md's rule is that
#     output which prints nothing is a failure to run, never an absence of
#     findings.
#
# Nothing here decides pass/fail. The verdict is the build's own exit status,
# read directly by the `if cargo build … ; then` at each call site.
report_build_failure() {
  local log="$1"
  echo "      │ full output: $log"
  local stripped="${log%.log}.stripped.log"
  # CSI (ESC [ … final byte in @-~) and OSC (ESC ] … BEL) forms.
  LC_ALL=C sed -e $'s/\x1B\[[0-9;?]*[ -\/]*[@-~]//g' -e $'s/\x1B\][^\x07]*\x07//g' \
    "$log" > "$stripped"
  local summary="${log%.log}.summary.log"
  # tolower() rather than gawk's IGNORECASE, which BSD awk (macOS) lacks.
  awk -v max="$DIAG_MAX_LINES" -v markers="$DIAG_MARKERS" '
    n >= max { exit }
    { low = tolower($0) }
    low ~ markers { print; n++; cont = 1; next }
    cont == 1 && $0 ~ /^[ \t]+[^ \t]/ { print; n++; next }
    $0 ~ /[^ \t]/ { cont = 0 }
  ' "$stripped" > "$summary"
  # A count with a verdict that depends on the count, not an eyeball.
  if [[ -s "$summary" ]]; then
    sed 's/^/      │ /' "$summary"
  else
    echo "      │ (no line matched the diagnostic markers — last $DIAG_MAX_LINES non-blank lines verbatim)"
    grep -v '^[[:space:]]*$' "$stripped" | tail -"$DIAG_MAX_LINES" | sed 's/^/      │ /'
  fi
}

echo "== Lodestone wasm32 compile guard =="
echo "target: $TARGET"
echo

for entry in "${CRATES[@]}"; do
  (( CONFINEMENT_ONLY == 1 )) && break
  pkg="${entry%%|*}"
  extra=""
  [[ "$entry" == *"|"* ]] && extra="${entry#*|}"
  printf '  %-34s ' "$pkg ${extra}"
  # cargo writes its own output straight to a file and the `if` reads its REAL
  # exit status — no filter sits between a long build and the only view of it.
  # CARGO_TERM_COLOR keeps that file readable; report_build_failure strips ANSI
  # anyway, for any producer that colours regardless.
  log="$LOGDIR/$pkg.log"
  # shellcheck disable=SC2086
  if CARGO_TERM_COLOR=never \
    cargo build -p "$pkg" --target "$TARGET" $extra > "$log" 2>&1; then
    echo "PASS"
  else
    echo "FAIL"
    fails+=("$pkg $extra")
    # Name the offender and the fix, inline. The captured cargo error usually
    # names the actual native-only crate (e.g. `could not compile \`cpal\``),
    # which is the single most useful line for whoever just broke the build.
    report_build_failure "$log"
    echo "      └─ two common causes: (a) a dependency pulled '$pkg' onto native-only"
    echo "         code (threads / std::fs / OS sockets / OS audio like cpal) — fix by gating"
    echo "         that dep or call behind cfg(not(target_arch = \"wasm32\")) or an"
    echo "         off-by-default feature; or (b) a plain compile error in '$pkg' or a crate"
    echo "         it depends on — which, in this shared workspace, is often a sibling crate"
    echo "         mid-edit (see the named crate in the error above): wait and re-run."
    echo "         Reproduce: cargo build -p $pkg --target $TARGET $extra"
  fi
done

# The page bundle is not the only browser wasm artifact. Browser
# singleplayer's authoritative server is a dedicated Worker package, built by
# web/scripts/stage_worker.sh during Trunk's post-build staging. Check its Rust
# half explicitly here so a worker-only dependency break is named before the
# final page build obscures it behind a hook failure.
if (( CONFINEMENT_ONLY == 0 )); then
  worker_log="$LOGDIR/lodestone-server-worker.log"
  printf '  %-34s ' "lodestone-server-worker"
  if CARGO_TERM_COLOR=never \
    cargo build --manifest-path "$ROOT/web/worker/Cargo.toml" --target "$TARGET" > "$worker_log" 2>&1; then
    echo "PASS"
  else
    echo "FAIL"
    fails+=("lodestone-server-worker")
    report_build_failure "$worker_log"
    echo "      └─ reproduce: cargo build --manifest-path web/worker/Cargo.toml --target $TARGET"
  fi
fi

# --- Confinement guards ------------------------------------------------------
# A family of calls compile to wasm32 and then die at runtime (see header). The
# type system can't forbid them and the compile pass above is blind to them, so
# each owning crate confines its hazard to a single
# cfg(not(target_arch = "wasm32"))-gated file, and we enforce that by grepping for
# the banned symbol everywhere ELSE in that crate. This is the general form of the
# original one-off lodestone-assets fs guard.
#
# Each rule (fields separated by '|'):
#   <label> | <src dir under repo root> | <banned pattern> | <comma-sep allowlisted file basenames>
#
# THE PATTERN MUST NOT CONTAIN A '|'. It is the field separator, so a `\|`
# alternation truncates the pattern mid-escape and grep exits 2 — which is exactly
# how five of these rules spent their life printing PASS without running. Split the
# alternation into one rule per hazard instead; the loop below now hard-FAILs any
# row that does not have exactly four fields, so this cannot recur silently.
# Keeping every pattern a plain literal substring also keeps the table
# dialect-independent (BSD, GNU and ugrep disagree about BRE alternation) and lets
# `cargo xtask wasm-check` match it with `str::contains`.
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
  # `thread::spawn` TRAPS on wasm32. Measured, executed in a wasm VM, and the three
  # thread entry points do NOT behave alike -- which is why this rule names one of
  # them and not the family:
  #
  #     std::thread::spawn                TRAPS
  #     std::thread::sleep                TRAPS
  #     std::thread::Builder::new().spawn Err(Unsupported)  -- degrades
  #     std::thread::available_parallelism Err              -- degrades
  #
  # So `remote_skins.rs` and `net.rs`, which use the `Builder` form and handle its
  # `Err`, were never crash-class; `mesher.rs`'s pool and `menu/status.rs`'s probe
  # thread were. The status one was REACHABLE -- it fires when the player opens the
  # Multiplayer screen -- and no `cargo check` at any target could see it.
  #
  # Allowlist = the files that genuinely confine it behind
  # `cfg(not(target_arch = "wasm32"))`, each with a browser arm beside it. A new
  # ungated `thread::spawn` anywhere else in the crate is what this catches.
  #
  # SCOPE LIMIT, stated because a guard you trust further than it reaches is worse
  # than none: this does NOT cover `thread::sleep`, which traps just as hard. Its
  # production site (`app/runners.rs::run_connect`) is gated, but its other sites are
  # inside `#[cfg(test)] mod tests` in `net.rs` and `menu/status.rs` -- files whose
  # production halves must stay covered. A grep cannot tell a test module from a
  # production one, so allowlisting those two files to add a `sleep` rule would buy
  # one hazard and blind two files to it. If you add `thread::sleep` to production
  # code here, nothing will stop you: gate it yourself.
  # `terminal.rs` is itself declared only by `lib.rs`'s native target arm, so
  # its blocking stdin reader never enters the browser compilation graph.
  "lodestone-shell thread-spawn-confinement|crates/lodestone-shell/src|thread::spawn|mesher.rs,accounts.rs,status.rs,runners.rs,terminal.rs"
  # --- the clock, in every other crate the browser reaches ---
  #
  # **These exist because `lodestone-shell`'s three rules were not enough, and the way
  # they were not enough is the reusable lesson.** The browser build reached exit 0 with
  # all three PASSing and still died twice: once in `lodestone-particle`
  # (`from_entropy` -> `SystemTime::now()`, three crates below the shell) and once in
  # `lodestone-server`/`lodestone-worldgen` on the way into a world. A confinement guard
  # only covers the crate it names, and the browser reaches about fifteen.
  #
  # `lodestone-server` is the sharpest case: its `collect_nearby_items` already carried a
  # comment stating the rule — *"this crate must not call `std::time::Instant::now()`
  # anywhere in `lodestone-server`, because the crate links into a wasm32 bundle where
  # that compiles and then panics at runtime"* — and four sites violated it anyway
  # (`server.rs`'s import, `integrated.rs` x3). The rule was right and it was prose. This
  # is the same rule, checked.
  #
  # Empty allowlists: these crates have no business reading a wall clock through `std`.
  # Each now uses `web_time`, whose non-wasm arm is `pub use std::time::*` — so native is
  # byte-identical and the rule costs nothing to keep.
  #
  # ONE RULE PER HAZARD, not one `\(Instant\|SystemTime\)` rule per crate. The
  # alternation form is what broke: `\|` is this table's field separator, so all five
  # of these rules had their pattern truncated to `std::time::\(Instant\`, grep exited
  # 2, and the `|| true` reported PASS. Two literal rows per crate cost one extra line
  # and cannot do that.
  "lodestone-server instant-ban|crates/lodestone-server/src|std::time::Instant|"
  "lodestone-server systemtime-ban|crates/lodestone-server/src|std::time::SystemTime|"
  # `tokio::time::Instant::now()` is a DIFFERENT literal than `std::time::Instant`,
  # so the rule above cannot see it — and it traps identically:
  # `server.rs`'s own `JoinStopwatch` doc says so ("it bottoms out in
  # std::time::Instant::now() ... and panics identically"), which did not stop
  # `serve_play`'s keep-alive/time-sync/vitals/container-sync interval setup (six
  # unguarded `tokio::time::Instant::now()` calls) from shipping anyway. Measured
  # live in the browser build: joining a singleplayer world panics at
  # `library/std/src/sys/time/unsupported.rs:13:9` the instant the client reaches
  # Play, and "Joining world..." spins forever because the connection task that
  # died was the one about to send the rest of the view. `tick.rs` also names this
  # symbol, but only inside `run_tick_loop`, which wasm32's `open_in_memory`
  # deliberately never spawns (see `net.rs`'s own comment on that constructor) —
  # a real, documented gap rather than a live trap, so it is allowlisted rather
  # than making this rule impossible to turn green.
  "lodestone-server tokio-instant-ban|crates/lodestone-server/src|tokio::time::Instant|tick.rs"
  # This crate's clock now goes through `lodestone_time::` rather than a direct
  # `web_time::` dependency -- `Cargo.toml` no longer lists `web-time` at all, so a
  # reintroduced bare `web_time::` call would not even compile. That is not a reason
  # to skip a rule for it: the whole point of a confinement guard, per the two rules
  # above, is to catch a regression by name before anyone waits on a build to find
  # it. Empty allowlist: every legitimate call site in this crate (including
  # `browser_timer.rs`, migrated alongside the rest -- its `BrowserInstant` alias is
  # `lodestone_time::Instant`, the identical type on every target) reads
  # `lodestone_time::`, which this qualified `web_time::` pattern does not match.
  "lodestone-server web-time-ban|crates/lodestone-server/src|web_time::|"
  "lodestone-worldgen instant-ban|crates/lodestone-worldgen/src|std::time::Instant|"
  "lodestone-worldgen systemtime-ban|crates/lodestone-worldgen/src|std::time::SystemTime|"
  "lodestone-particle instant-ban|crates/lodestone-particle/src|std::time::Instant|"
  "lodestone-particle systemtime-ban|crates/lodestone-particle/src|std::time::SystemTime|"
  "lodestone-net instant-ban|crates/lodestone-net/src|std::time::Instant|"
  "lodestone-net systemtime-ban|crates/lodestone-net/src|std::time::SystemTime|"
  # `async_task.rs`'s only clock hits are inside a `#[cfg(test)] mod`, which never
  # reaches a browser; a grep cannot tell a test module from a production one, so it is
  # named. Its `use std::time::{Duration, Instant};` does not match the
  # `std::time::Instant` spelling anyway — the allowlist is here so that a future
  # `std::time::Instant::now()` in that same test module does not go red either.
  "lodestone-ecs instant-ban|crates/lodestone-ecs/src|std::time::Instant|async_task.rs"
  "lodestone-ecs systemtime-ban|crates/lodestone-ecs/src|std::time::SystemTime|async_task.rs"
  # `lodestone-auth` joined this qualified-pattern bucket once `flow.rs` (which
  # now compiles and runs on wasm32 — see that module's doc) took a
  # `lodestone-time` dependency for a real wall-clock deadline
  # (`PendingLogin::is_expired`). Empty allowlist: neither `browser_login.rs`
  # nor `migrate.rs` (both still native-only) spells the type fully qualified —
  # both reach it through a bare `use std::time::{..., Instant};` import, which
  # this qualified substring does not match at all. Its `systemtime-ban`
  # sibling stays in the bare-pattern bucket below: `lodestone-time` has no
  # `SystemTime` re-export at all, so there is still no legitimate qualified
  # `lodestone_time::SystemTime::now()` spelling in this crate to protect.
  "lodestone-auth instant-ban|crates/lodestone-auth/src|std::time::Instant|"
  # --- crates outside the wasm build, tightened toward "no crate but
  # lodestone-time may name std::time's clocks" ---
  #
  # None of these three crates appears in the CRATES compile subset above: testsupport
  # is mostly dev-dependency-only, while its optional normal edges from sound/fuzz are
  # off by default; its new bench-record feature is additionally native-only. The
  # other two are a native-only bin nothing depends on (lodestone-allocbench — a
  # standalone allocator benchmark, already excluded from the workspace-wide
  # --all-features sweep for its allocator-feature mutual-exclusion) and code reached
  # only from a `#[cfg(test)]` module (lodestone-world's
  # `fill_region_lock_hold_time_on_a_large_synthetic_fill` benchmark-style test).
  #
  # Rules for them still earn their keep: a rule here is what turns "this file
  # structurally cannot reach wasm" from a claim into something a grep re-checks
  # on every run, and it is what stops a NEW file in one of these crates from
  # growing an ungated clock call unnoticed.
  #
  # PATTERN CHOICE: none of these three crates depends on `lodestone-time`, so
  # there is no legitimate `lodestone_time::Instant::now()` call anywhere in them
  # to avoid catching — unlike lodestone-server/worldgen/particle/net/ecs/auth
  # above, which must use the qualified `std::time::` path specifically so a
  # legitimate `lodestone_time::Instant::now()` elsewhere in the same crate does
  # not false-positive. These three instead use the bare
  # `Instant::now(`/`SystemTime::now(` method-call spelling (as
  # lodestone-audio/lodestone-sound do for the same "no legitimate caller
  # exists" reason) because their actual call sites are a mix of qualified and
  # unqualified spellings and the bare form catches both.
  #
  # `lodestone-auth systemtime-ban` lives here too, for its own reason (see the
  # comment on `lodestone-auth instant-ban` above) rather than this bucket's:
  # it did not move buckets because `lodestone-time` has no `SystemTime`
  # re-export for it to false-positive against.
  "lodestone-auth systemtime-ban|crates/lodestone-auth/src|SystemTime::now(|browser_login.rs,migrate.rs"
  "lodestone-world instant-ban|crates/lodestone-world/src|Instant::now(|world.rs"
  "lodestone-world systemtime-ban|crates/lodestone-world/src|SystemTime::now(|world.rs"
  # `bench_record.rs` is the one additional native-only source file: the module
  # declaration in `src/lib.rs` requires both `bench-record` and not-wasm32. The
  # scanner is lexical, so it must be allowlisted even though wasm never compiles it.
  "lodestone-testsupport instant-ban|crates/lodestone-testsupport/src|Instant::now(|lib.rs"
  "lodestone-testsupport systemtime-ban|crates/lodestone-testsupport/src|SystemTime::now(|lib.rs,bench_record.rs"
  "lodestone-allocbench instant-ban|crates/lodestone-allocbench/src|Instant::now(|main.rs"
  "lodestone-allocbench systemtime-ban|crates/lodestone-allocbench/src|SystemTime::now(|main.rs"
  # --- lodestone-time itself ---
  #
  # The whole point of issue #552 (this crate) is that it is the ONE place
  # allowed to depend on `web-time`, so every other crate's confinement rule
  # above can ban `std::time::{Instant,SystemTime}` with an empty allowlist.
  # This crate is held to the identical rule, with an EMPTY allowlist too — it
  # has no special exemption to spell `std::time` directly, because everything
  # it re-exports comes from `web_time`, whose own non-wasm arm is `pub use
  # std::time::*`. That happens inside the `web-time` dependency, not in this
  # crate's source, so this crate's own `.rs` files never need to write the
  # `std::time::` path at all.
  "lodestone-time instant-ban|crates/lodestone-time/src|std::time::Instant|"
  "lodestone-time systemtime-ban|crates/lodestone-time/src|std::time::SystemTime|"
)

confinement_ran=0

for rule in "${CONFINEMENT_RULES[@]}"; do
  # Structural validation FIRST, because a malformed row silently disarms its own
  # rule. Exactly three separators = exactly four fields; a `|` inside the pattern
  # (a `\|` BRE alternation is the way it happens) makes this four or more, which
  # used to truncate the pattern and report PASS. A row that cannot be parsed is a
  # FAIL, never a skip.
  seps="$(printf '%s' "$rule" | tr -cd '|' | wc -c | tr -d ' ')"
  if [[ "$seps" != "3" ]]; then
    printf '  %-34s ' "${rule%%|*}"
    echo "FAIL"
    echo "      malformed rule row: expected 3 '|' separators, found $seps"
    echo "      row: $rule"
    echo "      a '|' inside the banned pattern truncates it — split the alternation"
    echo "      into one rule per hazard instead."
    fails+=("malformed confinement rule row: $rule")
    continue
  fi
  IFS='|' read -r c_label c_dir c_banned c_allow <<< "$rule"
  printf '  %-34s ' "$c_label"
  # A rule pointed at a missing directory measures nothing. The `2>/dev/null` this
  # replaces hid exactly that, forever.
  if [[ ! -d "$ROOT/$c_dir" ]]; then
    echo "FAIL"
    echo "      rule scans a missing directory: $ROOT/$c_dir"
    fails+=("$c_label: missing src dir $c_dir")
    continue
  fi
  # grep's exit status is now READ, and the three cases are distinguished:
  #   0 = matched, 1 = no match (the PASS case), >=2 = ERROR.
  # An error means the detector did not run, so it is a FAIL that prints grep's own
  # stderr. The `|| true` this replaces mapped 2 onto the same empty string as 1,
  # which is how a broken pattern printed PASS.
  c_err="$LOGDIR/confinement-stderr.log"
  leak="$(grep -rn "$c_banned" "$ROOT/$c_dir" 2>"$c_err")"
  c_status=$?
  if (( c_status >= 2 )); then
    echo "FAIL"
    echo "      grep ERRORED (exit $c_status) — this rule measured NOTHING."
    echo "      pattern: $c_banned"
    sed 's/^/      grep: /' "$c_err"
    fails+=("$c_label: grep errored (exit $c_status) on pattern '$c_banned'")
    continue
  fi
  confinement_ran=$(( confinement_ran + 1 ))
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
      # -F: an allowlist entry is a literal basename, so the `.` in `platform.rs`
      # must not match any character.
      leak="$(printf '%s\n' "$leak" | grep -vF "/$f:" || true)"
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

# A count with a verdict that depends on the count. Every row in the table must have
# reached its grep and had that grep exit 0 or 1; anything else already appended to
# `fails` above, so a shortfall here means a row was skipped by a route nobody
# anticipated. `confinement_ran` is printed unconditionally so a reader can compare it
# against the table size without trusting this arm.
echo
echo "  confinement rules that actually ran: $confinement_ran/${#CONFINEMENT_RULES[@]}"
if (( confinement_ran != ${#CONFINEMENT_RULES[@]} )); then
  fails+=("only $confinement_ran of ${#CONFINEMENT_RULES[@]} confinement rules ran")
fi
echo

# The browser app is its own workspace (outside the crates/ glob), so it is built
# from its own directory. This is the end-to-end integration of the subset above,
# and it is built THROUGH trunk on purpose: trunk runs cargo for wasm32 and then
# wasm-bindgen, so this step catches a wasm-bindgen-level break (a signature rustc
# accepts but wasm-bindgen rejects), not only a rustc one. Cheap because the crate
# graph above is already warm in the shared target dir.
if (( CONFINEMENT_ONLY == 0 )) && [[ -f "$ROOT/web/Cargo.toml" ]]; then
  printf '  %-34s ' "lodestone-web (trunk build)"
  web_log="$LOGDIR/lodestone-web-trunk.log"
  # Explicitly remove an inherited NO_COLOR: `trunk` exposes `--no-color` through
  # clap with a bool parser, so the conventional `NO_COLOR=1` value aborts before
  # the build starts. CARGO_TERM_COLOR covers the cargo it shells out to, and
  # report_build_failure strips ANSI regardless.
  if (cd "$ROOT/web" && env -u NO_COLOR CARGO_TERM_COLOR=never trunk build) > "$web_log" 2>&1; then
    echo "PASS"
  else
    echo "FAIL"
    fails+=("lodestone-web (trunk build)")
    report_build_failure "$web_log"
    echo "      └─ the browser app failed to build. If the per-crate rows above are all"
    echo "         PASS, this is a wasm-bindgen/trunk-level break in web/ itself."
    echo "         Reproduce: (cd web && trunk build)"
  fi
fi

echo
if (( ${#fails[@]} > 0 )); then
  # "crate(s)" was wrong: this list mixes failed compiles, leaked confinements,
  # malformed rule rows and errored greps. Naming it "check(s)" stops a reader
  # concluding a leak was a compile break.
  echo "RESULT: FAIL — ${#fails[@]} check(s) failed:"
  for f in "${fails[@]}"; do echo "  - $f"; done
  exit 1
fi

if (( CONFINEMENT_ONLY == 1 )); then
  echo "RESULT: PASS — ${#CONFINEMENT_RULES[@]} confinement rules ran clean."
  echo "        (--confinement-only: the wasm32 COMPILE pass and the trunk build were"
  echo "         SKIPPED. This is not a substitute for a full run.)"
  exit 0
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
