# Browser shell port

## What it is

The wasm32 target: `web/` runs the real `lodestone-shell` — the same menu, `Sim`, and renderer
the native binary uses — fetching `client.jar` and `blocks.json` at startup instead of reading
them off a filesystem. This document is the hazard census the port is driven from: for each way
the shell depends on an operating system, what was measured about the hazard and the chosen
disposition (**gate**, **replace with a seam**, or **delete the need**), plus the confinement
guards that keep a fixed hazard from creeping back in.

Read `scripts/wasm-check.sh`'s header first — it explains why "compiles for wasm" and "works on
wasm" are different questions; this doc corrects two of its claims (see "What the record got
wrong" below).

## How it works

### Large browser assets

The browser boot loader normally fetches a direct, page-relative `client.jar` and
passes its bytes through `lodestone::platform::assets::install`. Static hosts with
a per-file cap instead serve `client.jar.parts.json` alongside ordered,
content-addressed `client.jar.part-NNN-<sha256>` files.
`web::client_jar::ClientJarParts` validates the manifest's version, exact
names/order/digests, total and per-part size bounds, and every SHA-256 digest before
reconstructing the archive. The mutable manifest is fetched with `cache: "no-store"`;
part names change with their content, so a deploy cannot combine a fresh manifest
with a stale cached part. A malformed present manifest is fatal; only a 404 falls
back to the direct jar, which keeps local development simple without making a broken
production deployment look healthy.

`web/scripts/stage_client_jar_parts.py` emits the deterministic 20 MiB parts and
manifest. `web/Trunk.toml` runs it when `LODESTONE_WEB_CLIENT_JAR_PARTS=1`; package
workflows must omit the direct jar afterward for hosts that reject it. URLs are
intentionally relative, so the same output works below a deployment subpath.

### Dedicated integrated-server Worker

Browser singleplayer has two wasm modules. The page module owns the client,
renderer, input, and page ECS. `web/worker/` builds the server module, and
`web/scripts/stage_worker.sh` places its bootstrap and wasm output beside the
page bundle. On Play, `net::launch_browser_worker` creates a `Worker` and a
`MessageChannel`; the Worker receives one port together with launch settings,
constructs the one world source and integrated server, then replies `ready`.
The control channel also reports the two observed startup milestones
(`loading-module` and `starting-server`) to the page. These are not a made-up
percentage: terrain progress remains the client-observed count of chunk packets
already applied to its world, so the existing loading grid and bar continue to
paint while the Worker prepares and streams the initial view.

`lodestone_net::MessagePortTransport` turns the two ports into an async byte
stream. It intentionally transports raw framed bytes only: message boundaries
can split or coalesce protocol packets, and `ByteInbox` reassembles them before
the codec reads. Startup/error messages remain on the Worker control channel,
so they can never be decoded as packets. The page retains the worker object for
the session; a post-ready worker failure closes the port and reaches the normal
client disconnect path. It must not fall back then, because that would create a
second authoritative mutable world. A failure before `ready` is the one safe
fallback point and uses the existing in-page duplex server.

`web/worker/worker_bootstrap.test.mjs` drives the control module with synthetic
Worker events. It proves malformed launch envelopes do not import wasm, valid
launches transfer exactly one port and report milestones in order, and a failing
server bootstrap cannot claim readiness. `cargo xtask wasm-check` runs that
Node test before building the browser bundle; it complements the real browser
smoke test, rather than pretending a host-side control-plane test can prove
painting or browser scheduling.

The page's synchronous ECS command dispatch cannot cross this boundary. The
worker installs a command sink that visibly refuses page-plugin commands rather
than silently accepting or losing them. Bridging those commands needs an
explicit request/reply protocol and page-side authorization policy; it is not
implicit byte transport work.

### The measurement that reorders the whole census

Hazard calls that compile for `wasm32-unknown-unknown` and "die at runtime" are usually
described as one group. **They are not.** Measured by compiling each call into a `cdylib` with
`panic = "abort"` and executing it in a real wasm VM:

| call | `wasm32-unknown-unknown` behaviour |
|---|---|
| `std::fs::read` | returns `Err(ErrorKind::Unsupported)` — **does not trap** |
| `std::time::Instant::now()` | **traps** — `RuntimeError: unreachable` |
| `std::time::SystemTime::now()` | **traps** |
| `std::thread::spawn` | **traps** |

This splits the census in two:

- **Crash-class**: the clock pair and threads. One reached call kills the tab; these had to be
  fixed before anything could run at all.
- **Degradation-class**: `std::fs`. Nearly every filesystem call site already discards its
  error, so on wasm it resolves to "no options file", "no saves", "no pack" — honest absence,
  the same path a native machine with no `HOME` would take. A correctness/UX problem, not a
  crash, fixable incrementally.

`SystemTime::now()` is crash-class and, before this census, appeared in no hazard list anywhere
in the repo — it had several production call sites (clock-derived seeds, UI blink timers, a
recipe-toast clock), each of which would abort the tab, and a green wasm32 `cargo check` gave
zero evidence about any of them: referencing an existing symbol compiles fine on wasm right up
until it is called.

### Crash-class hazards and their seam

| hazard | disposition |
|---|---|
| `Instant::now()` / `Instant` in struct fields | replaced with a seam: `crate::platform::Instant` |
| `SystemTime::now()` | replaced with a seam: `crate::platform::epoch_duration` |
| `std::thread::spawn` | gated per call site (sign-in workers, mesher worker pool, network) |
| `tokio::time::{sleep,timeout}` | gated with the native-only workers that use them |
| blocking `Runtime::new` + `block_on` | gated — a browser main thread cannot block |

The clock seam now lives in `lodestone-time`, a shared crate absorbing what used to be three
independently-grown copies of the same idea in `lodestone-shell`, `lodestone-net`, and
`lodestone-particle`. `crate::platform` is a **re-export, not a wrapper, with no `cfg` fork
inside the shell** — the non-wasm arm is `pub use std::time::*`, so `platform::Instant` *is*
`std::time::Instant` on native, provably no behaviour change. The browser arm is `web_time`, not
a hand-rolled `performance.now()` newtype: `winit`'s own wasm arm already types
`ControlFlow::WaitUntil` as `web_time::Instant`, so a private newtype would not type-check
against it, and `web_time` was already in the dependency graph via `winit`. Before reaching for
a portability shim, check whether a crate already in the graph is the type the platform layer
above you already speaks.

### Dependency-class hazards

These do not compile at all, so a plain `cargo check --target wasm32-unknown-unknown` sees them:

| dependency | problem | disposition |
|---|---|---|
| `tokio` with `net` | pulls `mio`, whose wasm32 arm is a hard `compile_error!` | gated: wasm gets `io-util, rt, macros, sync, time` only |
| `tracing-subscriber`, `tracing-chrome` | write to stderr/a file | gated; the browser installs `console_log` |
| `pollster` | blocks the browser main thread | gated; `spawn_local` is the wasm arm |
| `memory-stats` | reads `/proc`/`task_info` | replaced: `core::arch::wasm32::memory_size(0)` as a high-water-mark proxy for RSS |
| `lodestone-anvil` | `std::fs`-based region/`level.dat` codecs | gated |
| `reqwest` | blocking client (`.blocking` feature) is native-only | reqwest itself is **not** gated any more: its `wasm32-unknown-unknown` arm is `fetch`-backed and async, so the sign-in workers use the same `reqwest::Client` type on both targets |
| `lodestone-auth` | looked native-only, is not | **not gated** — `metadata`/`paths`/`entitlement` were always plain `serde`/`uuid`/`PathBuf`; `flow`/`login`/`store` joined them once the device-code flow (needs no listener) got a wasm32 HTTP client and `store::LocalStorageStore` replaced the OS keychain (see `docs/accounts-and-join.md`). Only `browser_login` (the loopback flow — a real listener and an OS browser process), `chat_session`, `texture` and `tls` stay native-only. |

### Degradation-class (`std::fs`), by subsystem

| subsystem | disposition |
|---|---|
| Assets (jar, `blocks.json`) | replaced with a seam: `platform::assets` |
| Options (`options.json`) | not yet done |
| Saves (`level.dat`) | gated, refusing explicitly |
| Resource packs | the need was deleted, not met — see below |
| Server list, offline identity, social | left to degrade (an `Err` read yields an empty list / fresh offline id) |
| Screenshots, sound object store | unreached — the subsystem that calls them cannot exist in a browser |

**Assets are the seam that matters**, because only the byte *acquisition* differs — every
parser, atlas builder, and model baker downstream is already synchronous and byte-based
(`ResourceSource`, `BlocksJsonRegistry::from_slice`). `platform::assets` is a process-wide
`OnceLock<Bundle>` holding the fetched jar/report bytes; `web/` fetches them and installs the
bundle before starting the app. **Resource packs are the one place the need was deleted rather
than met**: a user-selected pack is a file off the user's own disk, and enumerating one needs a
directory listing this target cannot do at all — a browser pack would have to arrive through a
file input and the existing byte-source seam, not through the native open-pack path, which is
simply unreachable here.

### Subsystems with no browser implementation

Each is gated with an **explicit, self-describing refusal** rather than a silent no-op, because
a UI row that silently shows nothing is indistinguishable from a subsystem that is broken:
audio (needs an `AudioWorklet` sink; the mixer itself is already wasm-clean), server-list ping
(needs an async probe over a relay, since a page cannot open a raw `TcpStream`), remote player
skins (blocked on porting the authlib host-allowlist that guards the fetch, not merely swapping
the HTTP client — a signed-in browser account plays with the default skin rig until this lands),
and screenshots (would need to trigger a download instead of a disk write).
**Microsoft sign-in is no longer in this list**: it used to need a real HTTP client, an OS
keychain, and (via the loopback flow) a listener, so browser accounts were offline-identity
only. The device-code flow needs none of those — see `docs/accounts-and-join.md` for the
`flow`/`login`/`store::LocalStorageStore` wiring and `crate::platform::relay::sleep` as the
`tokio::time::sleep` replacement in its poll loop. Audio's gate is an **uninhabited type**
(`pub enum ShellAudio {}`) rather than a stub with do-nothing methods — a stub is a reachable
value that silently produces nothing, which is exactly the shape that makes a subsystem look
wired while doing nothing; an uninhabited type makes that a compile-time impossibility.
`open_in_browser` is the one capability a browser does *better*: handing a URL to the platform
browser is `window.open`, a real implementation, called from inside a user gesture so a popup
blocker does not eat it.

### Confinement guards: turning a fixed hazard into a permanent one

`cfg(target_arch = "wasm32")` does not turn a hazard into a compile error — it only removes
existing native entry points, so a brand-new ungated `Instant::now()` sails straight through.
What actually catches this class is `wasm-check.sh`'s **confinement guards**: the owning crate
confines a hazard to one gated file, and the script greps for the banned symbol everywhere else
in that crate, failing and naming the offending site. `lodestone-shell` carries confinement
rules banning `std::time::Instant`/`std::time::SystemTime::now` outside `platform.rs` — banning
the full `std::time::` *path* rather than a bare `Instant::now(` spelling, since every real call
site now reads `crate::platform::Instant::now()` and a bare-spelling rule could never go green.
The guard skips comment lines, since a call site explaining *why* it avoids the trapping form
would otherwise itself trip the rule it is documenting. `cargo xtask wasm-check`'s own parity
test parses `wasm-check.sh`'s rule tables directly and diffs them field by field, rather than
comparing against a hand-copied list — a hard-coded label list previously let the two tools
drift silently until the xtask version was missing eight of the script's seventeen rules.
**A guard whose detector cannot fail is decorative**: run the control on any new rule by
planting a real (non-comment) violation in a non-allowlisted file and confirming the rule
reports it, before trusting a green run — `xtask`'s
`every_confinement_rule_fires_under_a_planted_violation` does this mechanically for every rule
on every `cargo test -p xtask` run.

## How to change it, and the gotchas

- **A confinement guard only covers the crate it names, and the browser links roughly fifteen.**
  A hazard three dependency layers down (a RNG seed calling `SystemTime::now()` inside an
  unrelated engine crate) killed the tab with every `lodestone-shell` rule green, because that
  crate was not in `wasm-check.sh`'s list at all. Every crate the browser links wants the clock
  rules; check the crate list itself is complete, not only that its rules pass.
- **A green wasm32 compile or a green `wasm-check` proves nothing about the browser actually
  running.** Both are a compile pass plus static greps; the only evidence that counts is loading
  the page and watching it reach a title screen, then a world.
- **`cargo check` stopping at a failing dependency reports zero errors for the crate after it**,
  which reads exactly like that crate being clean when it was never actually compiled — in a
  shared checkout where a sibling crate is mid-edit, attribute the errors before believing the
  silence.
- **`web/` is its own Cargo workspace** (its own lockfile, outside the root members glob), so
  neither `cargo check --workspace` nor `just check` ever covers it; `just wasm-check` (via
  `trunk`) is the only thing that does, and it catches wasm-bindgen-level breaks plain `rustc`
  would not.
- **`cargo check` cannot see a doctest.** A `///` example naming a native-only backend crate
  directly, rather than the portable `crate::platform` wrapper, fails only `cargo test -p
  lodestone-shell --doc` while every other check stays green.
- **A green title screen is not evidence the colour is right.** The WebGPU backend's surface
  capability list never includes an sRGB format at all (unlike native, where one is sorted
  first), so a swapchain configured off `get_default_config`'s first entry renders every linear
  shader output with no EOTF applied — uniformly darker, world and menus alike, since they share
  one swapchain. Fixed by reinterpreting the swapchain texture through an explicit sRGB *view*
  format rather than trusting the physical format `get_default_config` picks.
- **Bundle size is dominated by generated data, not code.** Roughly three quarters of the
  shipped binary is jar-derived static tables (`lodestone-data`'s generated block/path/outline
  censuses, a trig lookup table, the pre-Flattening bridge table) compiled directly into the
  binary rather than fetched at runtime. `opt-level`/`lto` act on code, which is not where the
  size is; the durable fix is moving those tables behind the same fetch seam `client.jar`
  already uses, and the same tables inflate the native binary too, unnoticed only because
  nobody has had a reason to measure it there.

## Configuration

| knob | effect |
|---|---|
| `web/Trunk.toml` `[serve] headers` | COOP/COEP, cross-origin isolation under `trunk serve` |
| `LODESTONE_WEB_LISTEN` | `just run-wasm`'s listen address for the page and `/relay` |
| `LODESTONE_RELAY_TARGET` | the real Minecraft server `/relay` bridges to |
| `just wasm-size` | fails above a fixed gzip byte ceiling |
| `web/[profile.release]` | `opt-level = "z"`, fat LTO, one codegen unit, `panic = "abort"`, strip — `panic = "abort"` is why a trap is fatal rather than recoverable |

## Dependencies

`web-time` (via `winit`), `wasm-bindgen`, `wasm-bindgen-futures` (`spawn_local`), `js-sys`,
`web-sys` (`Window`/`Document`/`HtmlCanvasElement`/`Performance`/`Storage`), all confined to
`lodestone-shell`'s `cfg(target_arch = "wasm32")` target section. `lodestone-time` supplies the
clock seam; `lodestone-render`'s `target.rs` owns the swapchain sRGB-view decision.

## Open work

The server's per-connection periodic driver (keep-alive, air supply, world-border damage,
burning, status effects, hunger) and the integrated-world driver both use
`crate::browser_timer::BrowserInterval`, built on `window.setTimeout` with `Delay` missed-tick
semantics. The timer is the only target-specific part: browser singleplayer runs the same world
simulation body and shares its source, scheduled-tick registries, entity snapshots, and block
change feeds with the duplex connection. This makes item falling, scheduled fluid and block work,
random ticks, weather, and block-entity updates reach the normal wire path without a catch-up
burst after a delayed tab. A Nether or End source first created by portal travel starts its own
task through that same tick body, follows the shared per-dimension anchor set, and races the same
shutdown signal; it does not share the overworld's queues or tick task. Also open: the options
file has no wasm persistence seam yet, and one world-list error path swallows a failure without
surfacing it to the UI.

## What the record got wrong

Kept because the corrections cost more to rediscover than to write down: `std::fs::*` does not
trap (only the clock pair and thread spawn do, and grouping them together hid which one was the
real emergency); `SystemTime::now()` appeared in no hazard list anywhere in the repo despite
being crash-class; the shell already had some wasm gating (`net.rs`, `app/session.rs`,
`app/launch.rs`, `app/menus.rs`, `audio.rs`) before this port began; and `lodestone-auth` looked
native-only by analogy with `lodestone-client` and was not.
