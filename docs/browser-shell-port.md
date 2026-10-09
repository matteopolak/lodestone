# Browser shell port

## What it is

The wasm32 target: `web/` runs the real `lodestone-shell` (same menu, `Sim` and renderer as native), fetching `lodestone-resources.zip` and `blocks.json` at startup instead of reading a filesystem. This is the hazard census the port is driven from: for each OS dependency, what was measured and the disposition (gate, replace with a seam, or delete the need), plus the confinement guards that keep a fixed hazard fixed. Read `scripts/wasm-check.sh`'s header first for why "compiles for wasm" differs from "works on wasm".

## How it works

### Large browser assets

The boot loader fetches a page-relative `lodestone-resources.zip` and passes the bytes to `lodestone::platform::assets::install`. Hosts with a per-file cap instead serve `lodestone-resources.zip.parts.json` plus ordered content-addressed `lodestone-resources.zip.part-NNN-<sha256>` files. `web::resource_pack::ResourcePackParts` validates version, names, order, digests and size bounds before reconstructing the archive. The manifest is fetched `cache: "no-store"` (part names change with content, so a deploy cannot mix a fresh manifest with a stale part). A malformed present manifest is fatal; only a 404 falls back to the direct archive.

`web/scripts/stage_resource_pack_parts.py` emits deterministic 20 MiB parts; `web/Trunk.toml` runs it when `LODESTONE_WEB_CLIENT_JAR_PARTS=1`, and packaging must then omit the direct archive. URLs stay relative so output works under a subpath.

### Dedicated integrated-server Worker

Browser singleplayer is two wasm modules. The page module owns client, renderer, input and page ECS; `web/worker/` builds the server module and `web/scripts/stage_worker.sh` stages it beside the page bundle. On Play, `net::launch_browser_worker` creates a `Worker` and a `MessageChannel`; the Worker gets one port plus launch settings, builds the one world source and integrated server, and replies `ready`. The control channel reports milestones `loading-module`, `starting-server`, `preparing-world` (emitted just before the long synchronous source construction). Terrain progress stays the client-observed count of applied chunk packets. Only the centre column precedes the play loop; deferred generation races packet and timer service; each column is framed independently.

- `lodestone_net::MessagePortTransport` turns the two ports into an async byte stream. Binary messages carry framed bytes; a private `{kind: "credit", bytes}` envelope carries flow control and never reaches the codec. Payloads may be `ArrayBuffer` or `Uint8Array`. The receive window is replenished only after `ByteInbox` hands bytes to the reader, so `poll_write` writes partially or waits for credit instead of growing the port queue; `ByteInbox` reassembles split or coalesced packets. Startup/error messages stay on the control channel. A malformed credit or an over-window payload is a terminal transport error that wakes parked reads and writes.
- The page keeps the worker for the session. A post-ready failure closes the port and reaches the normal disconnect path and must not fall back (a second authoritative world). A pre-`ready` failure is a launch error; the page never builds an integrated server, so singleplayer cannot silently return to main-thread worldgen. Dropping the page endpoint terminates the Worker (a `MessagePort` has no peer-close event).
- `web/worker/worker_bootstrap.test.mjs` drives the control module with synthetic events (malformed envelopes do not import wasm; a valid launch transfers one port and reports milestones in order; a failing bootstrap cannot claim ready). `cargo xtask wasm-check` builds the server wasm, runs that test and scans the worker entry for crash-class host calls before building the bundle. It complements the browser smoke test.
- The Worker is the only world owner. When the page is cross-origin isolated with shared memory, the threaded artifact creates bounded Rayon workers sharing the owner's module and memory for immutable production-session products only; mutable state, ordered commits, ticks and packet encoding stay in the Worker. Otherwise the serial-yielding artifact is used. See [`browser-worldgen-worker.md`](./browser-worldgen-worker.md).
- Native and browser share the portal contact controller and dimension commit/reset helpers. Destination preparation owns its source and races transport/timer service. The browser relight future owns an active-source snapshot, and a dimension reset drops it with old encodes, unsent batches and queues. Fresh `PlayerLoaded` gates vitals after the destination anchor is published. See `docs/nether-portals.md`. The resident portal site scan still runs synchronously after admission (so no measured portal latency bound), and the loading cover begins at the dimension commit.
- Page ECS commands cannot cross the boundary; the worker installs a sink that visibly refuses page-plugin commands. Bridging needs an explicit request/reply protocol and page-side authorization.

### Measurement that reorders the census

Compiling each call into a `cdylib` with `panic = "abort"` and running it in a wasm VM:

| call | behaviour on `wasm32-unknown-unknown` |
|---|---|
| `std::fs::read` | `Err(ErrorKind::Unsupported)`, does not trap |
| `std::time::Instant::now()` | traps (`unreachable`) |
| `std::time::SystemTime::now()` | traps |
| `std::thread::spawn` | traps |

So crash-class is the clock pair and threads (one reached call kills the tab); degradation-class is `std::fs` (call sites already discard errors, giving honest absence: no options, saves or packs). `SystemTime::now()` had several production sites (clock-derived seeds, UI blink timers, a recipe-toast clock) and appeared in no hazard list; a green `cargo check` says nothing since referencing a symbol compiles until called.

### Crash-class hazards

| hazard | disposition |
|---|---|
| `Instant::now()` and `Instant` fields | seam: `crate::platform::Instant` |
| `SystemTime::now()` | seam: `crate::platform::epoch_duration` |
| `std::thread::spawn` | gated per site (mesher pool, network) |
| `tokio::time::{sleep,timeout}` | native only; browser uses the shared host-timer future |
| blocking `Runtime::new` + `block_on` | gated (main thread cannot block) |

The clock seam is `lodestone-time` (absorbing three copies in shell, net, particle). `crate::platform` is a re-export with no `cfg` fork: native is `std::time::*` (provably no behaviour change); the browser arm is `web_time`, since `winit` types `ControlFlow::WaitUntil` as `web_time::Instant` and a private newtype would not type-check. Check whether a crate already in the graph is the type the platform layer speaks before writing a shim.

Browser deadlines use `lodestone_time::browser_sleep`, a cancel-safe `setTimeout` future working in page and worker globals (client read timeout, relay probes, integrated-server interval). Elapsed time uses the `performance.now()`-backed `Instant`. Dropping a losing future clears its timer.

### Dependency-class hazards

| dependency | problem | disposition |
|---|---|---|
| `tokio` with `net` | `mio` has a wasm `compile_error!` | wasm gets `io-util, rt, macros, sync, time` only |
| `tracing-subscriber`, `tracing-chrome` | stderr/file | gated; browser installs `console_log` |
| `pollster` | blocks main thread | gated; `spawn_local` |
| `memory-stats` | `/proc`/`task_info` | `core::arch::wasm32::memory_size(0)` as RSS high-water proxy |
| `lodestone-anvil` | `std::fs` codecs | gated |
| `reqwest` | `.blocking` is native-only | browser keeps async `fetch` arm for unauthenticated services |
| `lodestone-auth` | keychain and network services | account, entitlement, token paths unreachable; session-local ownership acknowledgement |

### Degradation-class (`std::fs`)

| subsystem | disposition |
|---|---|
| Assets (jar, `blocks.json`) | seam: `platform::assets` |
| Options (`options.json`) | not done |
| Saves (`level.dat`) | gated, explicit refusal |
| Resource packs | need deleted (a local pack needs directory listing; a browser pack would come via a file input and the byte-source seam) |
| Server list, offline identity, social | degrade (an `Err` read gives empty list / fresh id) |
| Screenshots, sound object store | unreached |

Assets are the seam that matters: only byte acquisition differs, since parsers, atlas builders and baker are synchronous and byte-based (`ResourceSource`, `BlocksJsonRegistry::from_slice`). `platform::assets` is a process-wide `OnceLock<Bundle>` that `web/` fills before the app starts.

### Subsystems with no browser implementation

Each is gated with an explicit self-describing refusal, not a silent no-op: audio (needs an `AudioWorklet` sink; the mixer is wasm-clean), server-list ping (needs an async relay probe), remote skins (needs a browser-safe trusted-host fetch), screenshots (would be a download). Browser account sign-in is absent; the local gate stores only its checkbox for the active handle. Audio's gate is an uninhabited type (`pub enum ShellAudio {}`), making a do-nothing stub that looks wired a compile-time impossibility. `open_in_browser` is `window.open`, called inside a user gesture so popup blockers allow it.

### Confinement guards

`cfg(target_arch = "wasm32")` only removes native entry points, so a new ungated `Instant::now()` compiles. `wasm-check.sh`'s confinement guards confine a hazard to one gated file and grep everywhere else in the crate for the banned symbol, failing with the site. `lodestone-shell` bans `std::time::Instant` and `std::time::SystemTime::now` outside `platform.rs` by full path (a bare `Instant::now(` rule could never go green since call sites read `crate::platform::Instant::now()`); comment lines are skipped. `cargo xtask wasm-check` parses the script's rule tables and diffs them against its own (a hand-copied list once drifted to eight of seventeen rules). A guard whose detector cannot fail is decorative: `every_confinement_rule_fires_under_a_planted_violation` plants a real violation per rule on every `cargo test -p xtask`.

## How to change it, and the gotchas

- A guard covers only the crate it names, and the browser links about fifteen. A seed calling `SystemTime::now()` three layers down killed the tab with every shell rule green. Every browser-linked crate needs the clock rules; verify the crate list is complete.
- A green wasm32 compile or `wasm-check` proves nothing about running. The evidence is loading the page to a title screen, then a world.
- `cargo check` stopping at a failing dependency reports zero errors for later crates; in a shared checkout attribute errors before believing silence.
- `web/` is its own Cargo workspace (own lockfile), so `cargo check --workspace` and `just check` never cover it; `just wasm-check` (via `trunk`) does and catches wasm-bindgen-level breaks.
- `cargo check` cannot see a doctest: a `///` example naming a native-only crate rather than `crate::platform` fails only `cargo test -p lodestone-shell --doc`.
- A title screen does not prove colour. The WebGPU surface capability list never includes an sRGB format, so a swapchain from `get_default_config`'s first entry renders linear output with no EOTF (uniformly dark). Fixed by reinterpreting the swapchain texture through an explicit sRGB view format (`lodestone-render`'s `target.rs`).
- Bundle size is about three quarters generated tables (`lodestone-data` censuses, a trig table, the pre-Flattening bridge) compiled in. `opt-level`/`lto` do not touch it; the fix is moving them behind the fetch seam `client.jar` uses (they inflate the native binary unnoticed too).

## Configuration

| knob | effect |
|---|---|
| `web/Trunk.toml` `[serve] headers` | COOP/COEP cross-origin isolation under `trunk serve` |
| `LODESTONE_WEB_LISTEN` | `just run-wasm` listen address for the page and `/relay` |
| `LODESTONE_RELAY_TARGET` | the Minecraft server `/relay` bridges to |
| `LODESTONE_WEB_CLIENT_JAR_PARTS=1` | stage split resource-pack parts |
| `just wasm-size` | fails above a fixed gzip ceiling |
| `web/[profile.release]` | `opt-level = "z"`, fat LTO, one codegen unit, `panic = "abort"` (why a trap is fatal), strip |

## Dependencies

`web-time` (via `winit`), `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys` (`Window`, `Document`, `HtmlCanvasElement`, `Performance`, `Storage`), confined to `lodestone-shell`'s wasm target section; `lodestone-time` for the clock seam.

## Open work

The server's per-connection periodic driver (keep-alive, air, border damage, burning, effects, hunger) and the integrated-world driver both use `crate::browser_timer::BrowserInterval` (`window.setTimeout`, `Delay` missed-tick semantics). Singleplayer runs the same simulation body sharing the source, scheduled-tick registries, entity snapshots and block-change feeds with the duplex connection, so item falling, fluids, random ticks, weather and block entities reach the wire with no catch-up burst after a delayed tab. A Nether or End source first created by portal travel starts its own task via that body, follows the per-dimension anchor set and the shared shutdown signal, and shares no queues with the overworld. Still open: the options file has no wasm persistence seam, and one world-list error path swallows a failure without surfacing it.
