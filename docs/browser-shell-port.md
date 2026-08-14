# Browser shell port

## What it is

The work of making `web/` run the **real `lodestone-shell`** — the same menu, the
same `Sim`, the same renderer the native binary uses — instead of a separate
feasibility-spike application with its own `main`. This document is the hazard
census that the port is driven from: for each way the shell depends on an operating
system, which files are involved, what was actually *measured* about the hazard, and
the chosen disposition (**gate**, **replace with a seam**, or **delete the need**).

Read [`scripts/wasm-check.sh`](../scripts/wasm-check.sh)'s header first — it is the
best writeup in the repo on why "compiles for wasm" and "works on wasm" are
different questions. This document corrects two claims in it (see
[What the record got wrong](#what-the-record-got-wrong)).

## Status

**The browser reaches the real title screen.** `web/` fetches `client.jar` (37.4 MiB)
and `blocks.json` (6.5 MiB), installs them through `platform::assets`, starts
`lodestone-shell`, and draws the actual menu — Singleplayer / Multiplayer / Minecraft
Realms / Options / Quit Game, the Accounts button, and
`Minecraft 26.2 (Lodestone 0.1.0)` — in the real vanilla font out of the fetched jar.
Verified by loading the page: a screenshot, plus a console with no errors after startup.
Real glyphs rather than the fixed-width stand-in is itself the evidence that collapsing
the four duplicate jar lookups into `resources::vanilla_manager` was necessary and not
tidiness.

`web/` is now a thin launcher: the spike application — its own `main`, camera, HUD,
chunk fixture and trimmed pack, ~1,200 lines — is deleted.

Every menu screen works, input works, and **Create New World reaches the Play state
against an in-memory integrated server** — measured in the page:

```
creating an in-memory browser world (nothing is written to disk; lost when the tab closes)
starting the integrated server (singleplayer) seed=1638668955722967429 view_radius=9
join chunks: 9 columns inline, 352 deferred to the play loop, 10 rings, window 4
Configuration -> Play: 0ms total
```

Not yet **playable**: the page sits on "Joining world…". The blocker is precise and is
the next unit — the server's play and tick loops are built on `tokio::time`
(`Instant`/`sleep`/`interval`, 40 sites across `server.rs` and `tick.rs`). Our wasm
`tokio` enables the `time` feature, which **compiles and then traps**: tokio has no
`wasm32-unknown-unknown` clock. So the 352 deferred columns never arrive and the loading
screen never dismisses. That needs a timer seam — the old `web/` spike used
`gloo-timers` for exactly this — which is a port rather than a swap.

## Where the port started

`web/` was, by its own README, "an isolated feasibility spike": its own Cargo
workspace, its own `main`, **no `lodestone-shell` dependency at all**, and zero
references to the menu or the title screen. It rendered a committed fixture of real
`level_chunk_with_light` bytes through the real `v770` parser plus an in-process
singleplayer probe. That is why the browser showed a "demo world" and no main menu:
not staleness, but the shell never having been wired in. The engine half was already
proven on wasm32 — `lodestone-{render,assets,net,world,data,core,controller,client,server,worldgen,model}`
and `lodestone-v770` all build and run in a browser.

`web/` is deliberately its own workspace, outside the root `crates/lodestone-*`
members glob, so `cargo check --workspace` has never covered it. Keep that.

Two parts of the shell were **already** wasm-aware before this port began, which
the brief for the work did not know: `net.rs`, `app/session.rs`, `app/launch.rs` and
`app/menus.rs` carried `cfg(not(target_arch = "wasm32"))` gates, and `audio.rs`
carried a whole-module `#![cfg(not(target_arch = "wasm32"))]`.

## The measurement that reorders the whole census

The hazard families are usually described as one group: calls that compile for
`wasm32-unknown-unknown` and "die at runtime". **They are not one group.** Measured
by compiling each call into a `cdylib` with `panic = "abort"` and executing it in a
real wasm VM (`node`, `WebAssembly.instantiate`):

| call | `wasm32-unknown-unknown` behaviour |
|---|---|
| `std::fs::read` | returns `Err(ErrorKind::Unsupported)` — **does not trap** |
| `std::time::Instant::now()` | **traps** — `RuntimeError: unreachable` |
| `std::time::SystemTime::now()` | **traps** |
| `std::thread::spawn` | **traps** |

The `Err` is real, not a coincidence of optimisation: the compiled module contains
the `unsupported` platform layer's literal string `operation not supported on this
platform` and no panic path, and the executed probe returned the `Unsupported`
discriminant.

This splits the census in two, and the split is the useful part:

* **Crash-class**: the clock pair and threads. One reached call kills the tab. These
  had to be fixed before anything could run at all.
* **Degradation-class**: `std::fs`. Every one of the shell's ~51 production `fs`
  call sites already discards its error (`.ok()?`, `let Ok(..) else`,
  `map_err(...)`), so on wasm they resolve to "no options file", "no saves", "no
  pack" — *honest absence*, arrived at through the same code path a native machine
  with no `HOME` would take. That is a correctness and UX problem, not a crash, and
  it can be fixed incrementally.

**`SystemTime::now()` is crash-class and appears in no hazard list anywhere in the
repo.** The shell had 8 production sites (clock-derived seeds, the chat caret blink,
glint phase, the recipe-toast clock); each would have aborted the tab.

## Census

Counts are production call sites — `#[cfg(test)] mod tests` bodies excluded, since
they are not in a wasm `--lib` build. Cite symbols, not lines.

### Crash-class

| hazard | production sites | disposition |
|---|---|---|
| `Instant::now()` / `Instant` in struct fields | 30 sites, 16 files | **replaced with a seam**: `crate::platform::Instant` |
| `SystemTime::now()` | 8 sites | **replaced with a seam**: `crate::platform::epoch_duration` |
| `std::thread::spawn` | `mesher.rs` (worker pool), `net.rs`, `menu/status.rs`, `menu/accounts.rs`, `remote_skins.rs`, `worldgen.rs` (legacy, unreached) | **gated** so far; `mesher.rs` is the one that still needs a single-threaded arm — see [Open work](#open-work) |
| `tokio::time::{sleep,timeout}` | `menu/accounts.rs`, `net.rs` | **gated** with the sign-in workers |
| blocking `Runtime::new` + `block_on` | `menu/accounts.rs`, `menu/status.rs`, `net.rs`, `remote_skins.rs` | **gated**: a browser main thread cannot block |

**Update: the seam now lives in its own crate.** `crate::platform::Instant`
and `crate::platform::epoch_duration` are unchanged in name and behaviour, but
`crate::platform` is now a two-line re-export of `lodestone-time`, which absorbed this
module's clock content plus two improvised copies of the identical seam that had grown
independently in `lodestone-net` and `lodestone-particle`. The reasoning below is kept
as the historical record of *why* the seam looks the way it does — see
`docs/portable-clock.md` and `crates/lodestone-time/src/lib.rs`'s crate docs for the
current, crate-level source of truth.

**A green wasm32 compile hid five of those `SystemTime::now()` sites, and that is the
single most useful thing in this document.** `cargo check -p lodestone-shell
--target wasm32-unknown-unknown` reached exit 0 while the chat-caret blink (which
runs every frame chat is open), both glint-phase clocks, the audio seed in `Sim`
construction and the screenshot timestamp were all still calling it. Each would have
killed the tab. They were found by the confinement guards below, *not* by the
compiler, on a tree that was already green — so treat "it compiles for wasm" as
carrying no information at all about this hazard family.

`crate::platform` is the seam module. It is a **re-export, not a wrapper, and with no
`cfg` fork at all** — `web_time`'s non-wasm arm is `pub use std::time::*`, so
`crate::platform::Instant` *is* `std::time::Instant` on native: the same type, not a
newtype over it, and provably no behaviour change. The practical consequence is what
made the port tractable: **any crate with an `Instant` in a public signature can
switch to `web_time::Instant` as a no-op on native, and no call site needs a `cfg`.**
That is how `Sim::tick_music`/`tick_ambience` and the shell's `music`/`ambient`
`advance` came to take one clock type instead of two behind a gate.

The browser clock is **`web_time`, not a hand-rolled `performance.now()` newtype**,
and that is worth knowing before you reach for one: `winit`'s wasm arm types
`ControlFlow::WaitUntil` as `web_time::Instant`, so `app::pacing` does not
type-check against any other clock type. The first attempt here *was* a private
`f64`-millisecond newtype and the compiler rejected it —
*"`browser::Instant` and `web_time::time::instant::Instant` have similar names, but
are actually distinct types"*. `web-time` was already in the graph
(`winit 0.30.13 -> web-time 1.1.0`), so it also costs the bundle nothing. **Generalise
this: before writing a portability shim, check whether a crate already in the graph
is the one the platform layer above you already speaks.** A shim that is merely
*equivalent* to the neighbouring crate's type is not interchangeable with it.

### Dependency-class

These do not compile at all, so `cargo check --target wasm32-unknown-unknown` sees
them. One was a hard blocker for the entire graph:

| dependency | problem | disposition |
|---|---|---|
| `tokio` with `net` | pulls `mio`, whose wasm32 arm is a **`compile_error!`** — 36 errors, and *nothing else in the graph* failed before it was split | **gated**: wasm gets `io-util, rt, macros, sync, time`, mirroring `lodestone-net`/`-server`/`-client` |
| `tracing-subscriber`, `tracing-chrome` | write to stderr and to a file | **gated**; the browser installs `console_log` in `web/` |
| `pollster` | `block_on` on the browser main thread | **gated**; `spawn_local` is the wasm arm |
| `memory-stats` | `task_info`/`/proc` | **replaced**: see below |
| `lodestone-anvil` | `std::fs`-based region/`level.dat` codecs | **gated** |
| `reqwest` | no blocking client in a browser | **gated** with the sign-in workers |
| `lodestone-auth` | *appeared* native-only | **not gated** — see below |

Two of these are worth their own note because the obvious call was wrong.

**`lodestone-auth` must stay an unconditional dependency.** The first attempt gated
it by analogy with `lodestone-client`'s edge. That was wrong: the crate is already
target-split internally and compiles clean for wasm32, exposing its device-free
half. Its `metadata` (the `profiles.json` roster) and `paths` modules were gated
only because the whole native block had been written as one unit — they are `serde`
+ `uuid` + `PathBuf` with no HTTP client, no keychain and no runtime. The account
switcher's *UI state* is built out of `AccountProfile`/`AccountsMetadata`
throughout, so gating the **types** rather than the **sign-in flow that needs a
network** cost 27 errors across five shell files for want of two plain structs.
Those two modules are now ungated at `lodestone-auth`; `login`/`flow`/`store`/
`texture` stay native-only.

**`memory-stats` was replaced, not stubbed, and the reason is specific to this
function.** `hud::process_rss_bytes` exists because it *used* to return a flat 0 on
macOS, and a zero-reading gauge is worse than none (§12) — so a browser arm
returning 0 would have reintroduced exactly the defect the function was written to
fix. wasm has a real analogue: `core::arch::wasm32::memory_size(0)` × 64 KiB is the
module's linear memory, which is the whole of its heap. Read it as a high-water
mark — linear memory never shrinks after a `memory.grow`, whereas RSS can fall.

### Degradation-class (`std::fs`), by subsystem

| subsystem | files | disposition |
|---|---|---|
| **Assets** (jar, `blocks.json`) | `resources.rs` | **replaced with a seam**: `platform::assets` |
| **Options** (`options.json`) | `config.rs` | **not yet done** — see [Open work](#open-work) |
| **Saves** (`level.dat`) | `saves.rs` | **gated**, refusing explicitly |
| **Resource packs** | `resources.rs` | **deleted the need** |
| **Server list**, **offline identity**, **social** | `menu/servers.rs`, `offline_identity.rs`, `menu/social.rs` | **left to degrade** (reads yield `Err`, so: empty list, fresh offline id) |
| **Screenshots** | `screenshot.rs` | native-only path, unreached in a browser |
| **Sound object store** | `asset_objects.rs` | unreached — the audio engine that calls it cannot exist |

**Assets are the seam that matters**, and the observation that makes it cheap is
that *only the byte acquisition differs*. `lodestone-assets`' `ResourceSource` is a
**synchronous, byte-based** trait and `ZipSource::from_bytes` builds a fully
in-memory pack; `lodestone-render`'s `BlocksJsonRegistry::from_slice` is likewise
ungated (only the *path*-taking `blocks_json_registry` wrapper is native-only, and
it is confined to its own gated file). So the browser crosses the filesystem wall
**once**, asynchronously, at the byte source, and every parser, atlas builder and
model baker downstream runs unchanged. `platform::assets` is a `OnceLock<Bundle>`
holding `client_jar` and `blocks_report` bytes; `web/` `fetch`es them and calls
`install` before starting the app, and `resources::Assets::try_vanilla` reads them
back. It is process-wide rather than threaded through `Config` because the consumers
are ~20 lazily-called `load_*` functions that each independently re-resolve the pack
root today — matching `SELECTED_PACKS`, which is a process-wide `RwLock` for the
same reason. `install` reports a second call as an error rather than ignoring it,
because the symptom of ignoring it is a world rendered from the wrong pack with
nothing in the log.

**Resource packs are the one place the need was deleted rather than met.** A
user-selected pack is a file the user picked off their disk; `DirectorySource` and
`ZipSource::open` are both native-only, and `scan_resource_packs` cannot enumerate
one either. Browser packs would arrive as bytes through a file input, i.e. through
`platform::assets`, not through `open_pack_source`. That path is therefore
*unreachable* in a browser, not merely unsupported.

### Subsystems with no browser implementation

Each is gated with an **explicit, self-describing refusal** rather than a silent
no-op, because a row that silently shows nothing is indistinguishable from a
subsystem that is broken.

| subsystem | why, and what a real browser version needs |
|---|---|
| **Audio** | `lodestone_sound::AudioEngine` wraps a `cpal` sink. `lodestone-audio`'s mixer is already wasm-clean; what is missing is an `AudioWorklet` sink to drive it. |
| **Microsoft sign-in** | Device-code and loopback flows need a real HTTP client, an OS keychain, a loopback listener and a blocking runtime. Browser accounts are offline-identity only. |
| **Server-list ping** | `lodestone_net::server_status` opens a raw `TcpStream`, which a page cannot do. Needs an `async` probe over the `ws-web` relay — a different function, not a shim over this one. |
| **Remote player skins** | `lodestone_auth::texture::fetch_texture` carries authlib's `TextureUrlChecker` host allow list and is `reqwest`-based. Reimplementing the GET over `web_sys::fetch` without porting that allow list would drop the only security check in the path, **so the allow list has to move first**. |
| **Screenshots** | `key.screenshot` writes a PNG to disk; a browser would trigger a download. |

**Audio's gate is an uninhabited type, and that shape is worth reusing.**
`crate::audio` was `#![cfg(not(target_arch = "wasm32"))]` wholesale, which deleted
the module on wasm and took thirteen call sites with it — most of them naming
nothing device-backed at all, just `subtitles::SubtitleCaption` and the pure
`music`/`ambient` selection arithmetic. Those submodules now stay, and `ShellAudio`
is a cfg fork whose browser arm is an **empty enum** (`pub enum ShellAudio {}`) with
every method present and every body `match *self {}`. `from_env()` returns `None` —
the exact path native takes with no asset store or no output device — and every
consumer already reads `Option<ShellAudio>`. The point is that this is **not a
stub**: a stub is a reachable value whose methods do nothing, which is precisely how
a subsystem comes to look wired while producing nothing. An uninhabited type makes
"browser audio silently pretends to work" a compile-time impossibility rather than
something to remember.

**`open_in_browser` is the one capability a browser does *better*.** Handing a URL
to the platform's browser is the platform's whole job, so the wasm arm is
`window.open(url, "_blank")` — a real implementation. It is only called from a key
handler, i.e. inside a user gesture, which is what stops a popup blocker eating it.

## How the guards work, and how to add one

`cfg(target_arch = "wasm32")` **does not turn a hazard into a compile error.** It
only removes the existing native entry points, so *referencing* a removed symbol
fails to compile while a brand-new ungated `fs::read` or `Instant::now()` sails
straight through. A Cargo feature is weaker still, because unification lets any
consumer re-enable it.

What catches this class is `wasm-check.sh`'s **confinement guards**: the owning
crate confines a hazard to one gated file, and the script greps for the banned
symbol everywhere *else* in that crate and fails, naming `file:line`. Extend that
table; do not invent a parallel mechanism. A rule for a crate that still calls the
symbol in ungated code goes red for everyone, so **confine first, then add the
guard**.

`lodestone-shell` is in both the script's and xtask's compile lists, and has three
rules (the `thread::spawn` one is described further down):

| rule | bans | allowlist |
|---|---|---|
| `lodestone-shell instant-confinement` | `std::time::Instant` | `platform.rs` |
| `lodestone-shell systemtime-confinement` | `std::time::SystemTime::now` | `platform.rs` |

Three things about their shape are deliberate, and each was arrived at the hard way:

* **They ban the `std::time::` *paths*, not the bare `Instant::now(` spelling.** The
  shell's 30 call sites now read `crate::platform::Instant::now()`, so a
  `Instant::now(` pattern matches every one of them and the rule could never go
  green. The path is what separates a trapping call from a portable one.
* **The allowlist is `platform.rs` alone** — the strongest form, matching
  `lodestone-audio time-confinement`'s empty one. `tests.rs` was in it briefly.
  Removing it meant converting 19 test-only sites in `net.rs`,
  `menu/render/tests.rs` and `sim/tests.rs`; test code cannot crash a browser, but
  `platform::Instant` *is* `std::time::Instant` on native, so the conversion cost
  nothing and turned "the shell never names the trapping clock" from a promise into
  something one grep decides.
* **The `SystemTime` rule names `::now` and the `Instant` one does not**, and that
  asymmetry is deliberate rather than an oversight. `screenshot.rs` takes a
  `now: SystemTime` *parameter* and reads `SystemTime::UNIX_EPOCH` — the type and the
  constant, never the trapping call — so banning the bare `std::time::SystemTime`
  path here would go red on a file that cannot trap. The clock rules in the other five
  crates *do* ban the bare path, because none of them has a legitimate reason to name
  the type at all. Pattern width follows what the crate legitimately needs, not a
  house style.
* **The guard mechanism now skips comment lines.** Every one of these confinements
  deserves a sentence at its call site saying *"not `SystemTime::now()`, because it
  traps"*, and a guard that fires on its own documentation trains people to delete
  the documentation. A hazard inside a comment cannot execute, so no rule is
  weakened — the same reasoning that made a `"` legal inside a `.wgsl` comment.

**Run the control on any rule you add.** Plant a real (non-comment) call in a
non-allowlisted file, confirm the rule reports `FAIL` naming `file:line`, then
restore by `cp` from an md5-checked backup. A confinement rule is an assertion of an
absence, and it is worth exactly as much as the evidence that it would have fired.

That control is now **mechanical and permanent** rather than a habit:
`xtask`'s `every_confinement_rule_fires_under_a_planted_violation` plants a probe file
in the directory each rule scans, requires the scan to report it *by path*, and removes
it. It runs in `cargo test -p xtask`, so a rule that cannot fail is a red test. Use
`scripts/wasm-check.sh --confinement-only` to run just the greps in seconds, with no
cargo build and no `trunk`.

### Two ways a guard reported PASS without running, both now mechanically impossible

Both were measured, and neither was visible by reading the rule table:

* **Five rules' greps never executed.** The script's rule rows are `|`-separated, and
  the five clock rules spelled a BRE alternation `std::time::\(Instant\|SystemTime\)` —
  whose `\|` *is* the field separator. `IFS='|' read` truncated the pattern to
  `std::time::\(Instant\`, grep exited 2 (*"trailing backslash"* on BSD grep,
  *"parentheses not balanced"* elsewhere), and the `|| true` that swallows grep's
  no-match exit 1 swallowed the **error** too. An empty result reads as "nothing
  leaked". The other twelve rules were correct only because no pattern happened to
  contain a `|`.

  Fixed three ways, in increasing generality: grep's exit status is read and `>= 2` is
  a hard FAIL printing grep's own stderr; every row is validated to split into exactly
  four fields before use; and the five alternation rules are split into one rule per
  hazard, so **every pattern in the table is a literal substring** — which also makes
  it dialect-independent, since BSD, GNU and ugrep disagree about BRE alternation.
* **`cargo xtask wasm-check`, the implementation CI actually runs, enforced eight
  fewer rules than the script it claimed parity with** — all three `lodestone-shell`
  rules and all five clock rules were absent, and `lodestone-shell` was missing from
  its compile list. Its parity test hard-coded a list of nine labels, so it kept
  passing as the script grew to seventeen. A gate that compares a table against a copy
  of itself cannot tell you a third table exists.

  The parity test now **parses** `scripts/wasm-check.sh`'s `CRATES` and
  `CONFINEMENT_RULES` arrays and compares field by field, so drift in either direction
  is red, and a `|` in any pattern fails it as well.

The reusable shape: **a check whose detector errored has measured nothing, and must
say so.** Wherever a guard maps "no findings" and "could not look" onto the same value,
that guard is one typo away from being decorative.

## Configuration

| knob | effect |
|---|---|
| `web/Trunk.toml` `[serve] headers` | COOP/COEP, so the page is cross-origin isolated. Already set — `SharedArrayBuffer` is available and threads are not automatically fatal. |
| `LODESTONE_NO_RELAY=1` | `just run-wasm` serves the page without the WebSocket→TCP relay. What you want for most iterations. |
| `just wasm-size` | fails above **1,600,000 B** gzip. |
| `web/[profile.release]` | `opt-level = "z"`, fat LTO, one codegen unit, `panic = "abort"`, `strip`. **`panic = "abort"` is why a trap is fatal rather than recoverable.** |

## Dependencies

`web-time` (the clock, already present via `winit`), `wasm-bindgen`,
`wasm-bindgen-futures` (the `spawn_local` executor), `js-sys`, `web-sys`
(`Window`/`Document`/`HtmlCanvasElement`/`Performance`/`Storage`) — all in
`lodestone-shell`'s `cfg(target_arch = "wasm32")` target section.

## Open work

In rough dependency order.

1. **The server's tokio timers.** The one thing between Play and a playable world; see
   [Status](#status). `tokio::time` compiles on wasm and traps, so `server.rs` and
   `tick.rs` need a sleep/interval/Instant seam. Note `lodestone-client` already
   discovered this independently — it logs *"read_timeout is unsupported on wasm32 (no
   runtime timer); ignoring"*.
2. **Bundle size.** Measured and attributed; see [Bundle size](#bundle-size). Its cause
   is generated static tables, which is a whole-project question rather than a wasm one,
   so it is deliberately not being acted on here.
3. **The panorama** does not draw. Cosmetic next to a playable world.
4. **Silent refusals.** `saves::create_world`'s browser refusal is now unreachable from
   the menu (the in-memory path replaced it), but the world-list screen still swallows a
   `set_error` without showing it on the path that produced one.

## Bundle size

Measured with the whole shell linked in, and **the ceiling has not been moved**:

| | bytes | |
|---|---|---|
| raw | 10,476,414 | |
| **gzip** | **3,766,970** | **enforced; ceiling 1,600,000 → FAIL at 2.35×** |
| brotli | 3,222,830 | real wire cost |
| *baseline before the shell* | *882,220 gzip* | *so the shell added +2,884,750 B gzip* |

Attribution, from `twiggy top` on a build made once with
`CARGO_PROFILE_RELEASE_STRIP=false`:

**`.rodata` is 8,004,211 B — 61.4% of the unstripped binary and ~76% of the 10.47 MB
shipped one.** It is not code, and it is *not* `include_str!` — lib-only `include_`
sites total 1.2 MB raw, and the multi-megabyte ones that show up in a naive grep are
all `tests/support/**` JVM oracle fixtures that never link into `lodestone-web`. It is
**generated static tables**:

| source | size |
|---|---|
| `lodestone-data/src/generated/` (whole directory) | ~4.9 MB of Rust |
| — `block_states.rs` | 1,426,724 B |
| — `path_types.rs` | 713,210 B |
| — `outline_shapes.rs` | 428,382 B |
| — `block_entity_types.rs` | 305,194 B |
| — `item_prototypes.rs` | 256,749 B |
| `lodestone-physics/src/sin_table.rs` | 819,881 B |
| `lodestone-canonical/src/generated/flattening.rs` | 369,419 B |

So the browser bundle is roughly **three quarters jar-derived game data compiled into
the binary**. The next unit is to move those corpora behind the fetch seam
`client.jar` already uses — `platform::assets` exists precisely for "this data is not
code, acquire it at runtime" — not to raise a number. Note what that implies for
*native* too: the same tables are in the desktop binary, where nobody has had a reason
to notice.

Do not reach for `opt-level` or `lto` first. They act on code, and code is the quarter
that is not the problem.

## Gotchas

* **`cargo check` stopping at a failing dependency reports zero errors for your
  crate, and that is not a pass.** This bit during the port: a run showed "5 errors,
  all in `lodestone-server/src/mobs.rs`" and *nothing* in `lodestone-shell`, which
  reads exactly like the shell being clean. It was not checked at all — cargo never
  got to it. In a shared checkout where siblings are mid-edit, **attribute the
  errors before believing the silence**, and confirm your crate was actually
  compiled.
* **`web/` has its own `Cargo.lock` and its own workspace.** `just check` and
  `cargo check --workspace` will never cover it. `just wasm-check` builds it through
  `trunk` — which is deliberate, because that catches a wasm-bindgen-level break
  that `rustc` alone would not.
* **`cargo check` cannot see a doctest, so add `cargo test -p lodestone-shell --doc`
  to your loop for this work specifically.** A target-split is the same shape of
  change as a crate rename: it moves which dependency resolves on which target, and a
  `///` fenced example naming the backend crate directly (`web_time`, say) rather than
  the portable wrapper will fail on the host while all three `check` recipes stay
  green. Prefer referring to `crate::platform` in examples — if callers should not
  name the backend, neither should the docs.
* **A confinement guard only covers the crate it names, and the browser reaches
  about fifteen.** This cost the last hour of the port. `cargo check --target
  wasm32-unknown-unknown` was exit 0, all three `lodestone-shell` rules PASSed, and the
  tab still died on `time not implemented on this platform` — from three crates down:
  `Sim::build` → `Particles::new` → `ParticleEngine::new()` →
  `JavaRandom::from_entropy()` → `SystemTime::now()`. `lodestone-particle` is not even in
  `wasm-check.sh`'s crate list. **Every crate in that list wants the clock rules, and the
  ones not in the list want to be.** Until then: run the page.
* **A green `wasm-check` does not prove the browser runs.** It is a compile pass
  plus greps. The only evidence that counts is the page reaching the title screen
  and then a world.
* **`just check-seam`** (`cargo check -p lodestone-shell --no-default-features`) is
  the only thing proving the shell compiles with *no* protocol family, and a large
  `cfg` refactor is exactly what breaks it. Run it often.

## What the record got wrong

Kept because the corrections cost more to rediscover than to write down.

* **`scripts/wasm-check.sh`'s header lists `std::fs::*` first among calls that
  "compile for wasm32 and only die at RUNTIME".** Measured and executed: `std::fs`
  returns `Err(Unsupported)` and does not trap. The three that do trap are
  `Instant::now`, `SystemTime::now` and `std::thread::spawn`. Grouping them hid the
  fact that the clock was the emergency and `fs` was not.
* **`SystemTime::now()` appears in no hazard list in the repo**, and is crash-class.
* **The shell was described as having no wasm gating.** `net.rs`, `app/session.rs`,
  `app/launch.rs`, `app/menus.rs` and `audio.rs` already had it.
* **"20 files use `std::fs`" overstates the work by ~4x.** Most of those counts are
  inside `#[cfg(test)] mod tests`, which a wasm `--lib` build never compiles: 51
  production call sites, not 200+.
* **`lodestone-auth` looked native-only and is not.** See above.
