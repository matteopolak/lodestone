# lodestone-web — browser (WebAssembly) build

The browser build of Lodestone — the real shell, not a spike; see
`src/main.rs`'s own doc for what changed and when. It is its **own** Cargo
workspace (empty `[workspace]` in `Cargo.toml`), deliberately outside the
parent `crates/lodestone-*` glob, so it never affects other crates' `cargo
build --workspace`.

## Singleplayer-only deployment

Both the browser package and its native page server expose a `multiplayer`
feature, enabled by default. To build a page that cannot join or probe public
servers, disable defaults on **both** packages:

```sh
cd web
cargo check --no-default-features --target wasm32-unknown-unknown
cargo check -p lodestone-web-server --no-default-features
```

The shell leaves the Multiplayer title button visible but disabled with an
explanation. More importantly, the server build neither links
`lodestone-relay` nor registers `/relay`; it is static-only, so it cannot be
used as a WebSocket-to-TCP proxy even if a client bypasses the button.

**Much of the section-level detail below (the "Multiplayer: a real browser
join" section especially) still describes an earlier version of this crate
that had its own `src/multiplayer.rs`/`src/singleplayer.rs`/`src/input.rs`/
`src/terrain.rs` — none of which exist any more; `src/main.rs` is now the
entire crate. That is a larger, separate cleanup than the serving-architecture
change this pass made; treat any claim below that names a `web/src/*.rs` file
other than `main.rs` as unverified until it is swept.

## What it demonstrates

- **Rendering:** real `level_chunk_with_light` fixture bytes → `lodestone-world`
  → greedy mesh → wgpu, drawn under **WebGPU**. Verified by pixel measurement,
  not by the HUD — see "Verifying that it actually draws" below, and read it
  before trusting a frame-rate number here. This line used to claim "~120 fps",
  which was **false**: no terrain pixel had ever reached the canvas.
- **Assets:** the sync, byte-based `lodestone-assets` `ResourceSource` pipeline
  runs unchanged once bytes are `fetch`ed (zip + PNG decoded in-browser).
- **Singleplayer:** the page owns client/render/input while a dedicated Worker
  owns the real server and world generator. A `MessageChannel` carries raw
  framed protocol bytes between them — no relay, no socket, and no duplicate
  world state.
- **Server-list ping, over the relay:** `lodestone-shell`'s multiplayer server
  list now really pings a server from the browser, through `lodestone-relay`
  linked into `lodestone-web-server` (`web/server/`) — see "Live multiplayer
  transport" below for what that needs and what it does not (yet) cover.
- **Audio:** a real `web_sys::AudioContext` + `ScriptProcessorNode` drives the
  same device-free `lodestone_audio::Mixer` native uses, fed a curated `.ogg`
  subset `scripts/stage_sounds.py` stages at build time — see
  `docs/sound-playback.md`'s "Configuration" section for the byte counts and
  `crates/lodestone-shell/src/audio.rs`'s module doc for the autoplay gesture
  gate (`Sim::resume_audio_on_gesture`, wired from every real mouse/key press
  in `app/lifecycle.rs`).

## Toolchain (verified versions)

| tool | version | notes |
|---|---|---|
| `trunk` | **0.21.14** | current stable. `0.22.0-beta.2` needs Rust 1.96.1 (> our 1.95.0). |
| `wasm-bindgen-cli` | 0.2.126 | trunk fetches a matching one automatically. |
| target | `wasm32-unknown-unknown` | `rustup target add wasm32-unknown-unknown` |

Install trunk (prebuilt binary, fastest):

```sh
curl -sSL https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-aarch64-apple-darwin.tar.gz \
  | tar xz -C ~/.cargo/bin trunk
trunk --version   # => trunk 0.21.14
```

(or `cargo install trunk --version 0.21.14`, which compiles from source.)

## Run it

```sh
just run-wasm
# open http://127.0.0.1:8080/
```

This is `scripts/run-wasm.sh`: `trunk watch --release` keeps rebuilding
`web/dist/` on every source change, paired with `lodestone-web-server`
(`web/server/`, a plain native binary) which serves that directory **and**
the `/relay` WebSocket→TCP bridge from the same listener — one port, one
process for the browser to talk to, and Ctrl-C stops both. See "Live
multiplayer transport" below for what `/relay` needs, and "Serving the page
and the relay from one process" for why this replaced two separate ones.

Prefer bare `trunk` for quick, page-only visual iteration with no relay or
multiplayer ping (same restriction it always had before the relay existed):

```sh
cd web && trunk serve --release --address 127.0.0.1 --port 8080
# open http://127.0.0.1:8080/
```

**Use `--release`** for both the wasm bundle and `lodestone-web-server`
itself. For the wasm bundle specifically: a debug build makes single-threaded
worldgen ~10× slower; in release, one column is ~1 s (see below), which the
singleplayer probe's 30 s deadline tolerates. A debug build can blow that
deadline and *look* like a failure.

### Assets: the build succeeds without them, the page does not

The page needs two files served beside it — `client.jar` (37.4 MiB, the renderable
corpus) and `blocks.json` (6.5 MiB, the block-state id table). Both are copied out
of `.cache/mc/26.2/` by the `post_build` hook in `Trunk.toml`, which stages them
**only if they exist**. They arrive by two different routes, which is worth knowing
because only the first is a single command:

```sh
cargo xtask fetch-assets --version 26.2   # -> .cache/mc/26.2/client.jar
# blocks.json is a Mojang *generated report*, not a download: it comes from the
# vanilla server jar's own data generator, which needs a JVM.
java -DbundlerMainClass=net.minecraft.data.Main -jar server.jar --reports
#   -> generated/reports/blocks.json, placed under .cache/mc/26.2/
```

**`trunk build` deliberately does NOT fail when they are absent.** It prints one
named line per unstaged file and exits 0; the page then reports `ASSET LOAD FAILED`
and draws nothing. So a blank page with that message means "populate `.cache/`",
not "the browser build is broken" — and the two are worth telling apart, which is
the whole reason the failure moved out of the build.

#### Hosts with a per-file cap

`client.jar` is larger than Cloudflare Pages' per-file limit. For that deployment,
stage an ordered manifest and 20 MiB-or-smaller siblings instead of the direct jar:

```sh
python3 web/scripts/stage_client_jar_parts.py \
  --jar .cache/mc/26.2/client.jar --out web/dist
rm web/dist/client.jar  # do not package the over-limit development fallback
```

The output is `client.jar.parts.json` plus content-addressed names such as
`client.jar.part-000-<sha256>`. The manifest records exact byte sizes and SHA-256
digests for every part and the reconstructed archive; the browser rejects bad
order, path, size, or hash rather than starting with corrupt assets. Part names
change with their content and the browser fetches the mutable manifest with
`cache: "no-store"`, so a new deployment cannot combine a fresh manifest with a
previous deployment's cached part. Names are plain relative URLs, so a deployment
under `/lodestone/` fetches its own sibling assets, not `/client.jar` at the domain
root. `just run-wasm` and ordinary `trunk` work
continue to use a direct `client.jar`: when the manifest returns 404, that is the
intentional fallback. To make Trunk emit parts directly, run
`LODESTONE_WEB_CLIENT_JAR_PARTS=1 trunk build --release`.

They used to be `data-trunk rel="copy-file"` links in `index.html`, i.e. a
build-time hard dependency on 46 MB of gitignored files. That made `trunk build`
fail outright on every CI runner and on every contributor's first build, with the
real cause buried (see `docs/ci.md`).

**The panorama faces and the sound corpus are optional in the other
direction**: unlike `client.jar`/`blocks.json`, their absence does not stop the
page from working, only from looking/sounding as intended — a missing
panorama face falls back to `client.jar`'s flat grey stub, and a missing sound
corpus leaves the browser's `ShellAudio` disabled with a logged reason,
exactly as a native checkout with no `.ogg` corpus fetched degrades. Both are
staged by the same conditional `post_build` hook shape as `client.jar`/
`blocks.json` — see `scripts/stage_panorama.py`/`scripts/stage_sounds.py`.

### Live multiplayer transport (optional)

A browser page has no raw TCP socket, so both the multiplayer server-list
**ping** and (once wired — see the warning below) a real **join** go through
`lodestone-relay`, a protocol-blind WebSocket→TCP bridge. `just run-wasm`
already runs it — see "Serving the page and the relay from one process" below
— so nothing extra needs starting; this section explains what dialing it
actually does.

**The browser only ever needs to know its own origin.**
`crate::platform::relay::relay_ws_url()` (`lodestone-shell`) derives the
WebSocket URL from `window.location` plus the fixed path `/relay` — no port
baked in anywhere in Rust. Under `just run-wasm` that resolves to
`ws://127.0.0.1:8080/relay`, answered by `lodestone-web-server`'s own `/relay`
route on the same listener that served the page.

**Server-list ping:** `lodestone-shell`'s multiplayer screen (`menu/status.rs`)
pings a saved server entry by dialing the relay and running the ordinary
status exchange over it, asynchronously, with a 5 s deadline. With no server
reachable at the relay's `--target` (or with `/relay` unanswered entirely —
see "Serving the page and the relay from one process" below), a row resolves
to `Failed` with a reason naming the obstacle rather than hanging on
`Pending` forever. With a real server behind `--target`, a row shows that
server's real MOTD/ping/player count — verified live against
`scripts/live-oracles/creative.sh`, byte-matching its `server.properties`
(`motd=lodestone creative oracle`, `max-players=8`). **One relay forwards to
exactly one fixed backend** (`--target`), so every row in the list reaches the
*same* server when pinged through a relay, regardless of which row's
host/port triggered the probe — those fields still travel in the handshake, a
real server may virtual-host on them, but the relay itself does not route on
them.

**Browser multiplayer join:** `lodestone-shell/src/net.rs`'s `run_async` uses
the wasm `Origin::Remote` path to build a destination-specific relay URL with
`crate::platform::relay::relay_ws_url_for`. It opens that URL with
`lodestone_net::WsWebTransport::connect`, races the dial against the
browser-safe `crate::platform::relay::sleep` deadline, and passes the connected
transport to `ClientBuilder::connect_with`. The normal protocol handshake,
login, and event driver then run through the same version-adapter path as a
native connection; only the transport dial differs. The `ws-web` feature is
enabled on the shell's dependency edge, while its implementation remains
target-gated inside `lodestone-net`, so native builds retain their TCP path.

## Serving the page and the relay from one process

`lodestone-web-server` (`web/server/`, a plain native binary, crate
`lodestone-web-server`) links `lodestone-relay` in as a **library dependency**
rather than running it as a spawned child. It serves the built page out of
`--dist` (default `./dist`, what `trunk build`/`trunk watch` write) **and**
answers `/relay` as a WebSocket upgrade bridged to `--target` — one listener,
one process, so there is no second port and nothing to keep in sync by hand.
Static misses return HTTP 404 rather than the page shell. This is part of the
asset-loader contract: `client.jar.parts.json` is optional, and its 404 selects
the direct `client.jar`; serving `index.html` with status 200 at that path makes
the missing manifest look present and fail later as invalid JSON.

This relay exists only with the server's default `multiplayer` feature. A
`--no-default-features` server is intentionally static-only and has no `/relay`
route or `lodestone-relay` dependency; use that build with the matching
singleplayer-only WASM bundle above.

```sh
web/target/release/lodestone-web-server \
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

These make the page **cross-origin isolated**, which remains useful for future
shared-memory work. The integrated server does not need it: it runs in a
dedicated Worker through a transferable `MessagePort`, not a shared-memory or
rayon worker pool.

**The trap:** a plain static file server (`python -m http.server`, most CDNs by
default, etc.) does **not** send these headers. The build renders fine without
them today, so serving `dist/` statically *appears* to work — but anything that
later depends on cross-origin isolation will **silently fail** with
`crossOriginIsolated === false` and no error under a server that omits them.
`lodestone-web-server` sets both unconditionally (`tower_http::set_header`);
if you serve `dist/` with something else entirely, replicate both headers.

## Integrated-server Worker

`web/worker/` is a separate wasm package staged by
`web/scripts/stage_worker.sh` during Trunk's post-build hook. Its bootstrap
receives launch settings and one endpoint of a `MessageChannel`, builds the
world and server before reporting ready, then bridges only raw protocol bytes.
The page keeps the other endpoint as `MessagePortTransport` for the normal
client driver. Worker startup failures fall back to the legacy in-page server;
after ready, a worker crash is a disconnect rather than a hidden second world.

Page-side plugin commands are explicitly refused in worker singleplayer until
there is a request/reply command bridge with an authorization policy.

## Guarding the browser build — `scripts/wasm-check.sh`

`cargo test --workspace` is **structurally blind** to wasm breakage: it builds
for the host, so any crate that gains a native-only dependency (threads, filesystem,
OS sockets, OS audio like `cpal`) still passes there while the browser build is
broken, and nothing tells the author. `scripts/wasm-check.sh` closes that gap.

Run it whenever a dependency is added or bumped **anywhere** in the workspace:

```sh
scripts/wasm-check.sh
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
is missing, the script exits non-zero with the install command rather than passing
quietly — a check that cannot run must fail, not skip.

> Note on cost: the check is CPU-cheap (~20 s of actual work), but wall-time is
> dominated by contention on the shared `target/` build lock when many agents
> build at once. Uncontended it is ~1–2 min; warm and cached it is seconds.

## Verifying that it actually draws

**`wasm-check` passing does not mean the browser renders, and neither does the
HUD.** Both were green while the canvas showed nothing but sky, for a long time.
The failure is worth understanding because it is the repo's dominant defect class
wearing browser clothes:

`lodestone-camera-bgl` binding 1 (the section origin) is declared
`has_dynamic_offset: true` in the group-0 split, so `set_bind_group` must
supply exactly one dynamic offset. `src/main.rs` passed `&[]`. WebGPU's response:

```
The number of dynamic offsets (0) does not match the number of dynamic buffers (1)
in [BindGroupLayoutInternal "lodestone-camera-bgl"].
[Invalid CommandBuffer] is invalid due to a previous error.
```

Every command buffer was invalid, so **the clear still landed and every draw was
discarded**. The page therefore showed a clean sky, the HUD reported
`250 greedy quads`, and wgpu logged it as a **warning** — not a panic, not an
error, nothing a `cargo` command can observe. Fixed by passing `&[0]`.

Two lessons encoded in the code:

- **Drive frames explicitly; do not rely on `requestAnimationFrame`.** A hidden
  or backgrounded tab does not run rAF at all — measured in a headless pane,
  `document.visibilityState == "hidden"` gave **0** rAF callbacks in 600 ms. A
  harness that waits for rAF sees a transparent canvas and no error.
  `lodestone_render_frames(frames, draw_geometry)` (exported via `wasm_bindgen`)
  renders synchronously and returns the frame count, or `u32::MAX` if init has
  not finished — never a silent `0`.
- **`draw_geometry = false` is the negative control.** It runs the identical pass,
  clear and depth attachment and submits no draws, so the canvas must come back
  as *exactly* the clear colour. Without it, "the canvas is sky blue" is
  ambiguous between "nothing drew" and "the sky drew".

Measured with this harness (900×640, 16 sections, 250 quads), and the numbers a
regression should be compared against:

| arm | distinct colours | non-clear pixels | bbox |
|---|---|---|---|
| control (`draw_geometry=false`) | **1** (`140,173,217`) | **0** | none |
| subject (`draw_geometry=true`) | **2299** | **41588** (7.22%) | `[298,263,634,483]` |

`140,173,217` is exactly `round(255 × {0.55, 0.68, 0.85})`, the `LoadOp::Clear`
colour in `main.rs` — so the surface is a **non-sRGB** format and the clear value
is written raw. The bbox being a strict sub-rectangle of 900×640 is the
load-bearing part: a full-canvas result would mean something is painting
everything, which is what a premise-false control looks like.

Drive it from the devtools console:

```js
const B = window.wasmBindings;          // trunk exposes the module here
B.lodestone_render_frames(2, false);    // control: expect a uniform clear
B.lodestone_render_frames(2, true);     // subject: expect terrain
```

Use `window.wasmBindings`, **not** a fresh `import()` — a second import is a
second wasm instance with its own empty `RENDER_STATE`, and the hook will
correctly report `u32::MAX` for it.

## Browser bind-group and adapter limits

The browser is where the low-limit adapter actually lives, so limits matter more
here than natively. Measured in Chrome on this M5 (`navigator.gpu` →
`requestAdapter().limits`):

| limit | browser | note |
|---|---|---|
| `maxBindGroups` | **4** | the native Metal path reports **8** |
| `maxStorageBuffersPerShaderStage` | 10 | |
| `maxStorageBuffersInVertexStage` | 10 | WebGL2 has none — see below |
| `maxUniformBufferBindingSize` | 65536 | |
| `maxUniformBuffersPerShaderStage` | 12 | |

**`maxBindGroups == 4` in the browser confirms CLAUDE.md's rule empirically.** The
model shader already spends all four groups (camera / atlas / palette / anim), so a
fifth group validates on this Mac natively (8) and **fails in every browser**.
`BlockPipeline`, which this spike draws with, uses only groups 0 and 1, so it was
not the cause of the blank canvas — but a five-group shader would be, and the
symptom would look identical.

WebGPU is required; there is no WebGL2 fallback. It was measured and removed: it
cost 537 KB brotli *and* never rendered a frame, because the atlas bind group
layout needs a vertex-stage storage buffer WebGL2 categorically lacks.

## Multiplayer: a real browser join, and what it needs

Stated precisely, because "the relay has tests" is not the same claim as "the
browser joins a server":

| leg | state |
|---|---|
| `WsWebTransport` (browser `WebSocket` as a `Transport`) | **green**, exercised in-browser |
| browser → `lodestone-relay` → live TCP server | **green** — verified in-browser against a real vanilla 26.2 server, which returned its own status JSON (`version.name = "26.2"`, protocol 776) |
| a browser **play join**, rendered | **green** — `src/multiplayer.rs`, measured below. **Needs two brokered clock patches**; see "The clock wall" |

`wasm32` cannot open a raw TCP socket (`lodestone-net/src/connection.rs` documents
this), so a browser must go through the relay. `src/multiplayer.rs` is the producer
that had been missing: it opens `WsWebTransport`, hands it to
`ClientBuilder::connect_with` with the real `lodestone_v26_2::adapter()`, and then
rebuilds the drawn scene by **querying** the client-owned chunk store
(`ClientHandle::sections_at`) rather than folding `ChunkLoaded` events — which is
idempotent, so it converges no matter when the loop starts relative to the stream.

Measured in Chrome against the live survival oracle (normal terrain, offline mode)
through a local relay:

```
join: Play reached (entity id 2101) — streaming world…
LIVE world from 127.0.0.1:25565 — 81 of 150 columns, 584 sections,
  97447 greedy quads | player chunk (-3, -24) | atlas: 62 blocks → 47 sprites,
  256×1024 px | 69 block(s) skipped — no assets in the trimmed pack
```

**This has only been driven against a relay and a server on `localhost`.** Nothing
about the path is loopback-specific — the relay takes any `--target` and the page
takes any `?relay=` — but a remote deployment additionally needs a `wss://` relay
(an `https` page cannot open `ws://`) and a relay reachable from the browser, and
neither has been tried.

### Running it

**Note:** these commands predate this pass's serving-architecture change (see
"Serving the page and the relay from one process" above) and are unverified
against the current `src/main.rs` per this file's own top-of-file disclaimer
— kept here only so a reader attempting to reproduce the join measurement
above starts from the current run command, not a doubly-stale one naming a
separately-run `lodestone-relay` process.

```sh
just run-wasm
# then: fill in the relay/host/port/name boxes and press Join,
# or load http://127.0.0.1:8080/?join=1 to join on page load.
```

`host`/`port` are only what the **handshake advertises**; where the bytes go is the
relay's `--target`. `?join=1` selects the remote-join path on page load; local
singleplayer world generation runs in its dedicated Worker and does not starve
the page's relay socket.

### The clock wall

Two `std::time::Instant::now()` calls sit on the join path. Both compile for wasm
and **panic at runtime** ("time not implemented on this platform"), and because
the release profile is `panic = "abort"` they kill the session with no unwind:

| site | when it fires | fix |
|---|---|---|
| `lodestone-ecs`'s `hold_read`/`hold_write` | the first ingested event, just after `Login` | **landed** — `hold_clock()` returns `None` on wasm and the hold goes unmeasured |
| `V770Adapter::new`'s `batch_start` | adapter construction, before the first byte | **brokered** (`crates/versions/` is owned elsewhere) — make `batch_start` an `Option<Instant>` |

Neither is findable by any `cargo` command, and the *first* is why
`src/singleplayer.rs` was never evidence of anything here: it reaches `Play`
against a **`StandInProtocol`** whose only event is `ChunkLoaded`, which routes to
the echo branch and never reaches `hold_write`. That is CLAUDE.md's *world*
species of vacuous test exactly — the source reads as a real integration test and
the flaw is in which implementation its transport resolves to.

The breadcrumb `log::info!`s in `multiplayer.rs` are deliberate: with `abort` and
no unwind, the last line logged is the only evidence of where it stopped.

### The trimmed pack is why a live world has holes

`assets/blocks_pack.zip` (88 KB, 73 blockstates) is a **subset** of vanilla's block
corpus — the full set is ~21 MB uncompressed. A live server sends whatever it
likes, so the live path builds its atlas with `skip_missing = true`: a block with no
assets becomes a non-occluding hole and is **counted and named on the HUD**, so
"patchy terrain" is never mistaken for a decode or transport fault. Regenerate with
a wider list via `scripts/wasm-blocks-pack.sh`. The *fixture* path stays strict —
a block in `fixtures/chunks.bin` that the pack lacks is a real defect.

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
| entry point | `navigator.storage.getDirectory()` | `showDirectoryPicker()` |
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
  index.html          data-trunk links: rust app; client.jar/blocks.json are
                       fetched at runtime, not linked here — see "Assets" above
  Trunk.toml           dev-server config for standalone `trunk serve` (page-only,
                       no relay) — COOP/COEP headers, post_build asset hooks
  scripts/
    stage_panorama.py post_build hook: stages real panorama faces if present
    stage_sounds.py    post_build hook: stages a curated .ogg sound subset
                        plus the full sounds.json registry, if present — see
                        its own module doc for the curated event list and the
                        measured byte counts, and docs/sound-playback.md
  assets/               post_build-hook staging target; empty in the repo
  src/
    main.rs             the entire wasm crate: boot, asset fetch, hands off to
                         lodestone-shell's `app::run` — see its own module doc
  server/               NATIVE crate `lodestone-web-server` — links
                         lodestone-relay as a library, serves dist/ and /relay
                         from one listener; see "Serving the page and the
                         relay from one process" above. A workspace member of
                         web/'s own Cargo.toml (own web/Cargo.lock), never
                         built by trunk (which builds only the root package).
```
