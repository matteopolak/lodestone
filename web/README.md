# lodestone-web — browser (WebAssembly) build

The browser build uses the same Lodestone shell as native. `src/main.rs` is
the standalone adapter; `src/embed.rs` provides the host API. `web/Cargo.toml`
defines a separate workspace containing the page, native page server, and
server Worker. Parent-workspace commands do not build these packages.

## Browser identity and capabilities

The Wasm client is always singleplayer-only. Its startup gate is a local,
session-memory attestation (`I confirm that I own Minecraft: Java Edition.`
followed by `Continue`); it has no Microsoft sign-in, account switcher, OAuth
or device-code flow, credential storage, or identity provider callback. The
attestation is cleared when the embedded handle is destroyed.

The separate native page server may still be built with its optional relay
feature, but the browser client does not expose that transport or a remote
multiplayer screen.

## Page-server relay configuration

Both the browser package and its native page server expose a `multiplayer`
feature, enabled by default. The Wasm shell disables remote multiplayer
regardless of that feature (`MULTIPLAYER_ENABLED` also requires a native
target). The title button remains visible but disabled with an explanation.

The native server's feature independently controls whether it links
`lodestone-relay` and registers `/relay`. Build a static-only page server with:

```sh
cd web
cargo build -p lodestone-web-server --release --no-default-features
```

Run that binary with `--listen` and `--dist`, without `--target`; the static-only
build rejects `--target`. `just run-wasm` instead builds the default server
with its relay enabled. Neither choice enables browser remote joins.

The standalone page and embedding adapter share the same shell, asset contract,
and browser event-loop lifetime. The embedding API is described in
`docs/wasm-embedding.md`. Historical experiments below are not current API
instructions or performance baselines.

## What it runs

- **Rendering:** integrated-server protocol bytes → client-owned world → mesh
  → wgpu under **WebGPU**. Compilation and HUD counters alone do not prove
  terrain reaches the canvas; the historical pixel control below illustrates
  that distinction.
- **Assets:** the sync, byte-based `lodestone-assets` `ResourceSource` pipeline
  runs unchanged once bytes are `fetch`ed (zip + PNG decoded in-browser).
- **Singleplayer:** the page owns DOM and the input bridge; a render Worker
  owns client/render state, and a separate server Worker owns the real server
  and world generator. A `MessageChannel` carries raw framed protocol bytes
  between the Workers — no relay, no socket, and no duplicate world state.
- **Relay tooling:** the native page server can still expose an optional relay
  for diagnostics, but the Wasm client does not expose server-list ping or
  remote joins.
- **Audio:** a real `web_sys::AudioContext` + `ScriptProcessorNode` drives the
  same device-free `lodestone_audio::Mixer` native uses, fed a curated `.ogg`
  subset `scripts/stage_sounds.py` stages at build time — see
  `docs/sound-playback.md`'s "Configuration" section for the byte counts and
  `crates/lodestone-shell/src/audio.rs`'s module doc for the autoplay gesture
  gate (`Sim::resume_audio_on_gesture`, wired from every real mouse/key press
  in `app/lifecycle.rs`).

## Toolchain

| tool | version | notes |
|---|---|---|
| `trunk` | 0.21.14 | The version named by the launcher's install hint; not a claim about the latest release. |
| `wasm-bindgen-cli` | 0.2.126 | Matches `web/Cargo.lock`; required on `PATH` by the server-worker staging hook. |
| target | `wasm32-unknown-unknown` | `rustup target add wasm32-unknown-unknown` |
| component | `rust-src` | `rustup component add rust-src` (required by the threaded worker's `-Z build-std`) |

The threaded Worker build also requires a nightly toolchain for `-Z build-std`.
These are repository requirements, not a check of locally installed tools.

Example Trunk installation for macOS arm64:

```sh
curl -sSL https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-aarch64-apple-darwin.tar.gz \
  | tar xz -C ~/.cargo/bin trunk
trunk --version   # => trunk 0.21.14
```

(or `cargo install trunk --version 0.21.14`, which compiles from source.)

The server-worker staging hook also invokes `wasm-bindgen` directly. Install
the matching CLI with `cargo install wasm-bindgen-cli --version 0.2.126 --locked`
if it is not already on `PATH`.

## Run it

```sh
just run-wasm
# open http://127.0.0.1:8080/
```

This runs `scripts/run-wasm.sh`: `trunk watch --release` rebuilds `web/dist/`
on source changes, while the native `lodestone-web-server` serves that directory
and its default `/relay` route from one listener. The script manages both
processes and cleans them up on exit. The browser singleplayer session does
not use `/relay`.

For page-only serving and rebuilding, without the native page server:

```sh
cd web && trunk serve --release --address 127.0.0.1 --port 8080
# open http://127.0.0.1:8080/
```

The launcher and server-Worker staging use release builds. Use the same build
profile when comparing timings. Older single-threaded probe speed ratios,
per-column timings, and deadlines are not measurements of the current Worker
path; this README makes no current throughput or startup-time guarantee.

### Assets: the build succeeds without them, the page does not

The page needs two files served beside it — `lodestone-resources.zip` (Whimscape
art over non-image game definitions) and `blocks.json` (the block-state id table).
The archive is built from `.cache/mc/<version>/client.jar` and the vendored Whimscape
pack by the `post_build` hook in `Trunk.toml`; base images and unused data are
excluded after both source archives pass CRC checks. A digest manifest is staged beside the archive
and the browser verifies it before installing the pack. Both files are staged
**only if their sources exist**. They arrive by two different routes:

Embedded hosts can bypass page-relative fetching by importing the wasm module's
`mount` export in a worker. Pass the `OffscreenCanvas` received after the page
transfers control, plus either both byte blobs or an `assetProvider(name)` callback
returning a blob or promise of one. The worker-local entrypoint renders directly to
that surface and uses worker timers, so no DOM window is required. The page owns the
source HTML canvas, transfer, UI, input bridge, and worker lifecycle. Pass
`onHostAction` when the page needs to perform user-gesture-gated actions such as
Pointer Lock, and forward the resulting state through the handle's typed input
methods. Release consumers read `worker_entrypoint` from the SDK manifest and send
that same manifest in the worker's mount message, keeping the worker, glue module,
and Wasm binary on one content-versioned release. See `docs/wasm-embedding.md` for
the lifecycle and progress events.

`onProgress` also receives one bounded sequence for each singleplayer join:
`world-create-started`, `joining`, `loading-terrain`, `loading-overlay-ready`,
`first-terrain-presented`, `full-view-presented`, and `full-view-quiescent`. These records expose
`elapsedMs`, `loadedColumns`, `expectedColumns`, `settledColumns`, and
`pendingMeshes` and `pendingLightRemeshes`; they measure
from the in-game world action rather than SDK mount. `first-frame` remains the
mount readiness signal and must not be interpreted as terrain readiness.
Opening an existing world emits `world-open-started` at the same boundary.
`full-view-presented` can retain earlier geometry while replacements are queued.
`full-view-quiescent` requires the requested view's latest mesh handoffs and
empty-section classifications to be settled after presentation, with scheduler,
column-repair, light-intent and removal queues drained. It is a sampled completion
milestone, not the gameplay unlock condition. Halo-only waiting columns outside
the view do not prevent it. `pendingMeshes` counts scheduler work and ready results;
`pendingLightRemeshes` counts light intents not yet admitted to that scheduler.
The standalone adapter prints these transition-only records as `lodestone join:`
console messages, including elapsed milliseconds and loaded, expected, settled,
and pending counts. Asset and frame-heartbeat progress remains in the page status
rather than producing per-frame console output.

Pass `logLevel: "debug"` to the embedding API, or open the standalone page with
`?log=debug`, to expose render-worker and integrated-server tracing in the JavaScript
console. Supported levels are `off`, `error`, `warn`, `info`, `debug`, and `trace`;
the default is `warn` so diagnostics do not affect ordinary play.

Block-breaking latency has a separate opt-in: `traceBlockActions: true` at mount
or `handle.setBlockActionTrace(bool)` afterward. Reports use the existing
`onProgress` callback with phase `block-action-trace`. The standalone page accepts
`?trace-block-actions=1`; its optional `?probe=1` panel can toggle tracing and retain
bounded reports. See [block-action latency](../docs/block-action-latency.md) for
milestones and measurement limits.

The standalone adapter fetches `lodestone-resources.zip` and `blocks.json` concurrently. When a
multipart jar manifest is present, all authenticated parts are fetched concurrently,
then copied into their declared order before the whole-archive digest is checked. The
completed required blobs stay as transferable `ArrayBuffer`s across the page-to-render-
worker boundary; the worker's mount boundary performs the single Wasm-owned byte copy.
The panorama is part of the same resource archive.

```sh
cargo xtask fetch-assets --version "$(cat mc-version)"   # -> .cache/mc/<version>/client.jar
# blocks.json is a Mojang *generated report*, not a download: it comes from the
# vanilla server jar's own data generator, which needs a JVM.
java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
#   -> generated/reports/blocks.json, placed under .cache/mc/<version>/
```

**`trunk build` deliberately does NOT fail when they are absent.** It prints one
named line per unstaged file and exits 0; the page then reports `ASSET LOAD FAILED`
and draws nothing. So a blank page with that message means "populate `.cache/`",
not "the browser build is broken" — and the two are worth telling apart, which is
the whole reason the failure moved out of the build.

#### Hosts with a per-file cap

For a host with a per-file cap, stage an ordered manifest and 20 MiB-or-smaller
siblings of the filtered archive instead of the direct jar:

```sh
python3 web/scripts/stage_resource_pack.py \
  --jar .cache/mc/<version>/client.jar \
  --visual-pack assets/resource-packs/whimscape-26.1-26.3-r2.zip --out web/dist
python3 web/scripts/stage_resource_pack_parts.py \
  --jar web/dist/lodestone-resources.zip --out web/dist
rm web/dist/lodestone-resources.zip  # package only the parts for capped hosts
```

The output is `lodestone-resources.zip.parts.json` plus content-addressed names such as
`lodestone-resources.zip.part-000-<sha256>`. The manifest records exact byte sizes and SHA-256
digests for every part and the reconstructed archive; the browser rejects bad
order, path, size, or hash rather than starting with corrupt assets. Part names
change with their content and the browser fetches the mutable manifest with
`cache: "no-store"`, so a new deployment cannot combine a fresh manifest with a
previous deployment's cached part. Names are plain relative URLs, so a deployment
under `/lodestone/` fetches its own sibling assets, not a domain-root archive.
`just run-wasm` and ordinary `trunk` use the direct merged archive: when the parts
manifest returns 404, the browser validates the direct archive manifest instead.
To make Trunk emit parts directly, run
`LODESTONE_WEB_CLIENT_JAR_PARTS=1 trunk build --release`.

They used to be `data-trunk rel="copy-file"` links in `index.html`, i.e. a
build-time hard dependency on 46 MB of gitignored files. That made `trunk build`
fail outright on every CI runner and on every contributor's first build, with the
real cause buried (see `docs/ci.md`).

The sound corpus is optional for development. A missing sound corpus leaves
`ShellAudio` disabled with a logged reason, just as a native checkout with no
`.ogg` corpus fetched degrades. The panorama is required artwork inside the
built-in resource archive. Sound staging is in `scripts/stage_sounds.py`.

### Relay tooling (optional)

A native page server can expose `lodestone-relay`, a protocol-blind
WebSocket-to-TCP bridge, for diagnostics. The Wasm client deliberately has no
caller path to it: there is no browser account or online identity behind a
remote join, and the Multiplayer title button is disabled.

The relay's origin-derived WebSocket URL and server-list diagnostics remain
available to the separate native page-server tooling described below. They are
not part of the browser client's public mount API or startup flow.

## Serving the page and the relay from one process

`lodestone-web-server` (`web/server/`, a plain native binary, crate
`lodestone-web-server`) links `lodestone-relay` in as a **library dependency**
rather than running it as a spawned child. It serves the built page out of
`--dist` (default `./dist`, what `trunk build`/`trunk watch` write) **and**
answers `/relay` as a WebSocket upgrade bridged to `--target` — one listener,
one process, so there is no second port and nothing to keep in sync by hand.
Static misses return HTTP 404 rather than the page shell. This is part of the
asset-loader contract: `lodestone-resources.zip.parts.json` is optional, and its 404 selects
the direct archive; serving `index.html` with status 200 at that path makes
the missing manifest look present and fail later as invalid JSON.

This relay exists only with the server's default `multiplayer` feature. A
`--no-default-features` server is intentionally static-only and has no `/relay`
route or `lodestone-relay` dependency. It can serve the same singleplayer-only
Wasm bundle; no matching browser feature change is needed. The command below
is for the default relay-enabled server.

```sh
target_dir="$(cargo metadata --manifest-path web/Cargo.toml --format-version 1 --no-deps \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
"$target_dir/release/lodestone-web-server" \
  --listen 127.0.0.1:8080 --dist web/dist --target 127.0.0.1:25565
```

`--listen 127.0.0.1:0` asks the OS for a free port instead of the fixed
default — the conflict case a hardcoded port risks. Pass `--port-file <path>`
to have the actually-bound port written there as a bare decimal, for a script
to read without a pipeline; `scripts/run-wasm.sh` does exactly this.

**This is also the deployable artifact `trunk serve`'s dev-only proxy could
never be.** `trunk build` produces a plain static `dist/` and `trunk serve`'s
proxy is a dev-server feature with nothing behind a `dist/` served any other
way — that used to mean a deployed build had **no relay path at all**, an
accepted, documented gap. Running `lodestone-web-server` in front of `dist/`
closes it: point `--target` at a reachable Minecraft server and `/relay`
works from any deployment that can run this binary, not only `trunk serve`.

**What is still unsolved:** TLS. `lodestone-web-server` speaks plain HTTP/WS
only, so an `https://` deployment (required the moment the page is served
over anything but `localhost`, since an `https` page cannot open `ws://`)
needs a reverse proxy in front of it (nginx, Caddy, a CDN edge function, a
load balancer doing TLS termination) forwarding to this binary's `--listen`
address — ordinary practice for any plain-HTTP origin server, and nothing
here builds or runs that reverse proxy. Nothing about the relay path itself
is loopback-specific (`--target`/`--listen` take any address), but going from
"binds `127.0.0.1:8080`" to "reachable over `wss://` from the public
internet" is genuinely separate work that has not been attempted.

`web/Trunk.toml`'s `[[proxies]]` entry that used to forward `/relay` to a
separately-run `lodestone-relay` process is **gone** — it would just be a
second, redundant way to reach a relay that this binary already serves on the
one port `trunk serve` itself is not used for by `just run-wasm` any more.
`trunk serve` used standalone (page-only iteration, no relay) still works;
see "Run it" above.

## ⚠ COOP/COEP: why both servers set the same two headers

Both `web/Trunk.toml`'s `[serve]` block and `lodestone-web-server` set two
headers on every response:

```
Cross-Origin-Opener-Policy:   same-origin
Cross-Origin-Embedder-Policy: require-corp
```

These make the page **cross-origin isolated**, which enables the optional
shared-memory WebAssembly compute pool used by browser world generation. The
integrated server always remains in its dedicated Worker; only immutable shaped
admissions may fan out to child Web Workers through `wasm-bindgen-rayon`.

A host that omits these headers does not provide cross-origin isolation, so
the Worker capability probe selects the serial artifact rather than the shared-
memory variant. `lodestone-web-server` sets both unconditionally
(`tower_http::set_header`); replicate them when serving `dist/` elsewhere if
the threaded Worker is required.

## Integrated-server Worker

`web/worker/` is a separate wasm package staged by
`web/scripts/stage_worker.sh` during Trunk's post-build hook. The hook emits
serial and atomics-enabled bindgen modules; the bootstrap checks isolation,
shared-memory construction, Atomics, and Wasm validation before choosing the
threaded artifact. Its launch envelope transfers one protocol port and one
structured worldgen-progress port, builds the world and server before reporting
ready, then bridges framed protocol bytes with a bounded private credit
envelope. The client/render Worker keeps the protocol endpoint as `MessagePortTransport` for
the normal client driver; writes wait or complete partially when the peer's
receive window is full. A negative capability probe selects the serial module;
threaded initialization failures are reported as startup errors. After ready, a
worker crash is a disconnect rather than a hidden second world. Dropping or
shutting down the page endpoint also terminates the dedicated
Worker because a `MessagePort` has no peer-close event.

For a browser measurement, load the staged
`lodestone-worldgen-long-task-harness.js` and call
`LodestoneWorldgenMeasurement.measure({ seed: "42", runtimeMs: 5000 })`. The
report records startup mode/timing and page Long Tasks API entries; it reports
`under100ms: false` when that API is unavailable. `web/scripts/measure_worker_size.sh`
reports post-bindgen raw/gzip/Brotli Wasm sizes plus generated glue sizes for
both worker variants.

Page-side plugin commands are explicitly refused in worker singleplayer until
there is a request/reply command bridge with an authorization policy.

## Guarding the browser build — `just wasm-check`

`cargo test --workspace` is **structurally blind** to wasm breakage: it builds
for the host, so any crate that gains a native-only dependency (threads, filesystem,
OS sockets, OS audio like `cpal`) still passes there while the browser build is
broken, and nothing tells the author. `just wasm-check` closes that gap through
the tested `xtask` implementation; `scripts/wasm-check.sh` remains its reference.

Run it whenever a dependency is added or bumped **anywhere** in the workspace:

```sh
just wasm-check
```

It does two things a host build cannot:

1. **Compiles the wasm crate subset** for `wasm32-unknown-unknown`, one crate at a
   time, and on failure prints the offending crate and the fix (the captured
   cargo error usually names the actual native-only dependency).
2. **Runs confinement greps** for the "compiles on wasm, panics at runtime" family
   (`std::fs`, `Instant::now`, `std::thread::spawn`, `tokio::time`, `cpal`) that
   the compile pass is blind to — each owning crate confines its hazard to one
   `cfg(not(target_arch = "wasm32"))`-gated file, and the grep fails (naming
   file:line) if the symbol reappears anywhere else.

The final step builds the browser app **through trunk** (cargo → wasm →
wasm-bindgen), so a wasm-bindgen-level break is caught too.

**Prerequisites are verified, not assumed.** If `wasm32-unknown-unknown` or `trunk`
is missing, the check exits non-zero with the install command rather than passing
quietly — a check that cannot run must fail, not skip.

Build duration depends on cache state and the shared Cargo queue. No current
timing baseline is recorded here.

## Historical browser rendering evidence

The following measurements came from a retired fixture launcher, not the current
shell or embedding API. Its frame-driving export is no longer available. They
illustrate why a compile check and HUD counters need an independent pixel control,
not a current frame-rate or terrain-readiness baseline.

`lodestone-camera-bgl` binding 1 (the section origin) is declared
`has_dynamic_offset: true` in the group-0 split, so `set_bind_group` must
supply exactly one dynamic offset. The former fixture launcher passed `&[]`.
WebGPU's response:

```
The number of dynamic offsets (0) does not match the number of dynamic buffers (1)
in [BindGroupLayoutInternal "lodestone-camera-bgl"].
[Invalid CommandBuffer] is invalid due to a previous error.
```

Every command buffer was invalid, so **the clear still landed and every draw was
discarded**. The page therefore showed a clean sky, the HUD reported
`250 greedy quads`, and wgpu logged it as a **warning** — not a panic, not an
error, nothing a `cargo` command can observe. Fixed by passing `&[0]`.

The fixture's controls were:

- **Explicit frames:** in one hidden headless pane,
  `document.visibilityState == "hidden"` gave **0** `requestAnimationFrame`
  callbacks in 600 ms. The fixture used a synchronous frame-driving export
  rather than waiting for those callbacks.
- **Withheld geometry:** `draw_geometry = false` ran the identical pass, clear,
  and depth attachment without submitting draws. The result had to be exactly
  the clear colour, distinguishing a clear from actual scene rendering.

Recorded with that fixture (900×640, 16 sections, 250 quads):

| arm | distinct colours | non-clear pixels | bbox |
|---|---|---|---|
| control (`draw_geometry=false`) | **1** (`140,173,217`) | **0** | none |
| subject (`draw_geometry=true`) | **2299** | **41588** (7.22%) | `[298,263,634,483]` |

`140,173,217` is exactly `round(255 × {0.55, 0.68, 0.85})`, the `LoadOp::Clear`
colour in the fixture — its surface was a **non-sRGB** format and the clear value
was written raw. The bbox being a strict sub-rectangle of 900×640 is the
load-bearing part: a full-canvas result would mean something is painting
everything, which is what a premise-false control looks like.

For the current SDK, use the singleplayer progress events documented above to
select the observation boundary, then verify terrain pixels independently.
`first-frame` alone is not terrain evidence.

## Browser bind-group and adapter limits

The following is a historical Chrome/M5 adapter sample (`navigator.gpu` →
`requestAdapter().limits`), not a guarantee about the current browser or GPU:

| limit | browser | note |
|---|---|---|
| `maxBindGroups` | **4** | the sampled native Metal path reported **8** |
| `maxStorageBuffersPerShaderStage` | 10 | |
| `maxStorageBuffersInVertexStage` | 10 | WebGL2 has none — see below |
| `maxUniformBufferBindingSize` | 65536 | |
| `maxUniformBuffersPerShaderStage` | 12 | |

The renderer's four-bind-group budget avoids depending on the higher native
limit in this sample. A fifth group would exceed the sampled browser limit.

WebGPU is required; there is no WebGL2 fallback. The retired WebGL2 experiment
added 537 KB Brotli and rendered no frame: the atlas layout required a
vertex-stage storage buffer unavailable on that path.

## Historical multiplayer transport experiments

These results belong to the retired browser-join probe, not the current page.
The Wasm shell is always singleplayer-only; the old join boxes and `?join=1`
controls are no longer available.

| leg | historical evidence |
|---|---|
| `WsWebTransport` (browser `WebSocket` as a `Transport`) | exercised in-browser |
| browser → `lodestone-relay` → live TCP server | returned status JSON with `version.name = "26.2"` and protocol 776 |
| browser play join, rendered | reached Play and rendered streamed terrain in the retired probe |

The probe opened `WsWebTransport` through a local relay and used the real client
adapter. It rebuilt its drawn scene by querying the client-owned chunk store
rather than depending on when its event loop began relative to the chunk stream.

Recorded in Chrome against the survival oracle (normal terrain, offline mode):

```
join: Play reached (entity id 2101) — streaming world…
LIVE world from 127.0.0.1:25565 — 81 of 150 columns, 584 sections,
  97447 greedy quads | player chunk (-3, -24) | atlas: 62 blocks → 47 sprites,
  256×1024 px | 69 block(s) skipped — no assets in the trimmed pack
```

Only localhost was exercised; this record does not validate a remote deployment
and has no current browser-join reproduction recipe.

The probe exposed two native clock calls that compiled for Wasm but aborted at
runtime. A stand-in singleplayer protocol did not exercise the same ingestion
path, so its success could not validate the real adapter. These are test-design
lessons, not a list of current unpatched call sites.

Its 88 KB, 73-blockstate `assets/blocks_pack.zip` deliberately omitted assets
from a corpus measured at roughly 21 MB uncompressed. The old atlas skipped
missing blocks, explaining the holes reported above. That pack and its coverage
numbers do not describe the current staged `lodestone-resources.zip` archive.

## Saving worlds in the browser — the storage options, unbuilt

World persistence is `#[cfg(not(target_arch = "wasm32"))]` today
(`lodestone-shell/src/app/session.rs` gates `world_dir` that way), so a browser
session has no save directory. **Nothing below is implemented**; it is recorded so
the decision keeps its constraints.

`localStorage` is not merely too small (5–10 MB) — it is **the wrong shape**. It is
string-keyed and string-valued with no seek, while Anvil region files are
random-access binary with a 1024-entry sector table. A key-value string store
cannot express `.mca` access at all.

Two viable targets, and the choice is a product decision, not an implementation
detail:

| | OPFS | File System Access API |
|---|---|---|
| entry point | storage's get directory | `showDirectoryPicker()` |
| random access | `createSyncAccessHandle()` — **synchronous** read/write at byte offsets, in a Web Worker | async read/write via `FileSystemFileHandle` |
| quota | orders of magnitude above `localStorage` | the user's real disk |
| prompt | none | one permission prompt per session |
| visible in Finder | **no** | **yes** |
| Safari | supported | **not supported** |

OPFS's `createSyncAccessHandle()` is the one browser API actually shaped like
`.mca` access — synchronous, seekable, binary — which is why it is the right
default rather than a workaround. The File System Access API is what the owner's
"access their files directly" describes, and its advantage is real: the world lands
somewhere the user can see and back up. Browser storage also stays **evictable**
unless `navigator.storage.persist()` is granted, which matters for a save file.

## Browser tick timing

The worker uses the server's browser timer seam, not `tokio::time`, for the
integrated world's periodic work. Browser timer clamping and background-tab
throttling still apply, so the timer uses delay semantics rather than replaying
a catch-up burst when a tab resumes. A remote browser client never runs this
server loop.

Related trap, and the reason the whole wasm build was red: `tokio::time::Instant`
is **not** a wasm-safe substitute for `std::time::Instant`. It bottoms out in
`std::time::Instant::now()` (tokio 1.53.1, `src/time/clock.rs:16`), which panics on
`wasm32-unknown-unknown`. Swapping one import for the other converts a compile
error into a runtime crash, which is strictly worse and invisible to `cargo check`.

```
web/
  index.html          data-trunk links: rust app; resource pack/blocks.json are
                       fetched at runtime, not linked here — see "Assets" above
  Trunk.toml           dev-server config for standalone `trunk serve` (page-only,
                       no relay) — COOP/COEP headers, post_build asset hooks
  scripts/
    stage_resource_pack.py stages Whimscape art over non-image game definitions
    test_stage_resource_pack.py controls the staging filter and corruption paths
    stage_sounds.py    post_build hook: stages a curated .ogg sound subset
                        plus the full sounds.json registry, if present — see
                        its own module doc for the curated event list and the
                        measured byte counts, and docs/sound-playback.md
    stage_worker.sh     builds/stages serial + atomics server-worker artifacts
    measure_worker_size.sh reports post-bindgen worker Wasm/glue sizes
  dist/                 Trunk output, including post_build-hook staged assets
  src/
    main.rs             standalone boot adapter and asset fetch
    embed.rs            host-facing mount/destroy API — see
                         docs/wasm-embedding.md
  server/               native `lodestone-web-server` — serves dist/; the default
                         multiplayer feature also exposes /relay. Shares the
                         web workspace lockfile; built by scripts/run-wasm.sh,
                         not by Trunk's root-package build.
```
