# lodestone-web — browser (WebAssembly) spike

An isolated feasibility spike proving Lodestone's stack runs in a browser via
WebAssembly + WebGPU. It is its **own** Cargo workspace (empty `[workspace]` in
`Cargo.toml`), deliberately outside the parent `crates/lodestone-*` glob, so it
never affects other crates' `cargo build --workspace`.

## What it demonstrates

- **Rendering:** real `level_chunk_with_light` fixture bytes → `lodestone-world`
  → greedy mesh → wgpu, drawn under **WebGPU**. Verified by pixel measurement,
  not by the HUD — see "Verifying that it actually draws" below, and read it
  before trusting a frame-rate number here. This line used to claim "~120 fps",
  which was **false**: no terrain pixel had ever reached the canvas.
- **Assets:** the sync, byte-based `lodestone-assets` `ResourceSource` pipeline
  runs unchanged once bytes are `fetch`ed (zip + PNG decoded in-browser).
- **Singleplayer (`src/singleplayer.rs`):** the real `lodestone-client` ↔
  `lodestone-server` ↔ `lodestone-worldgen` stack, connected in-process over an
  in-memory duplex under `spawn_local` — no relay, no socket, no Docker.
- **Multiplayer transport (`src/main.rs` relay probe):** a browser WebSocket →
  `lodestone-relay` → live TCP server round-trip (Server-List-Ping).

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
cd web
trunk serve --release --address 127.0.0.1 --port 8080
# open http://127.0.0.1:8080/
```

**Use `--release`.** A debug build makes single-threaded worldgen ~10× slower;
in release, one column is ~1 s (see below), which the singleplayer probe's 30 s
deadline tolerates. A debug build can blow that deadline and *look* like a
failure.

### Live multiplayer transport (optional)

The relay probe expects a WebSocket→TCP bridge on `ws://127.0.0.1:25580`. Start
`lodestone-relay` pointing at a real server, then reload:

```sh
cargo run -p lodestone-relay -- --listen 127.0.0.1:25580 --target 127.0.0.1:25565
```

With it down, the page shows **`⚠ relay UNREACHABLE …`** on the `net` HUD line
(render + singleplayer are unaffected — that path is isolated). With it up, the
line shows the live server's status JSON.

## ⚠ COOP/COEP: why the dev server matters

`Trunk.toml` sets two headers on every response:

```
Cross-Origin-Opener-Policy:   same-origin
Cross-Origin-Embedder-Policy: require-corp
```

These make the page **cross-origin isolated**, which is a hard requirement for
`SharedArrayBuffer` and therefore for any future threading
(`wasm-bindgen-rayon`, or moving worldgen off the main thread). They are set now,
while the spike is single-threaded and doesn't need them, precisely so the
substrate is already isolated when threading lands.

**The trap:** a plain static file server (`python -m http.server`, most CDNs by
default, etc.) does **not** send these headers. The spike renders fine without
them today, so serving `dist/` statically *appears* to work — but anything that
later depends on cross-origin isolation will work under `trunk serve` and
**silently fail** under a plain server, with `crossOriginIsolated === false` and
no error. If you serve `dist/` yourself, replicate both headers, or you are
building on a foundation the dev server has and production may not.

## Worldgen is the singleplayer gate, not the transport

In-browser worldgen measured at **~1 s per column, single-threaded (release)**.
It is synchronous, so it blocks the event loop while it runs — a 5×5 view is
~25 s of frozen tab. Browser singleplayer is therefore gated on getting worldgen
off the main thread (a Web Worker, or the `wasm-bindgen-rayon` path the COOP/COEP
headers above already enable), **not** on the transport, which is proven.

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
`has_dynamic_offset: true` by issue #76's group-0 split, so `set_bind_group` must
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

## Multiplayer: what is green and what is an island

Stated precisely, because "the relay has tests" is not the same claim as "the
browser joins a server":

| leg | state |
|---|---|
| `WsWebTransport` (browser `WebSocket` as a `Transport`) | **green**, exercised in-browser |
| browser → `lodestone-relay` → live TCP server | **green** — verified in-browser against a real vanilla 26.2 server, which returned its own status JSON (`version.name = "26.2"`, protocol 776) |
| a browser **play join** | **island** — no producer |

`wasm32` cannot open a raw TCP socket (`lodestone-net/src/connection.rs`
documents this), so a browser must go through the relay. The relay leg works. What
does not exist is anything in `web/` that drives a *client* over it:
`ClientBuilder::connect` exists in `lodestone-client`, and the only
`WsWebTransport::connect` call in `web/` is inside `ping_via_relay`, a
Server-List-Ping. So the transport is proven and the join is unwritten — an island
in the "no producer" direction (CLAUDE.md rule 1). Wiring it is the next step for
browser multiplayer, and it needs no new dependency.

Note also that `src/singleplayer.rs` reaches `Play` against a **`StandInProtocol`**
test double, not `v770`. That is the *world* species of vacuous test in CLAUDE.md's
table: the source reads as a real integration test, and the flaw is in which
implementation its transport resolves to. Do not cite it as evidence that a real
26.2 join works in a browser.

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

## Deferred: `tokio::time` on wasm

`mobs::run_mob_tick_loop` needs `tokio::time`, which wasm lacks, so the browser
takes the mob-free `IntegratedServer::open_in_memory` path. That loop is
**server-side**, so a browser acting as a pure client against a remote server never
runs it — which is why neither `tokio-with-wasm` nor a frame-driven tick was
adopted here. Both would be real work with a real dependency cost (browser timers
are not tokio's timers: clamping, background-tab throttling, no multi-threaded
runtime), and neither is on the critical path to a rendered frame or a
multiplayer join. Revisit only if in-browser *singleplayer* with mobs becomes the
goal, and read issue #284 first — it wants **fewer** timers, not a seventh.

Related trap, and the reason the whole wasm build was red: `tokio::time::Instant`
is **not** a wasm-safe substitute for `std::time::Instant`. It bottoms out in
`std::time::Instant::now()` (tokio 1.53.1, `src/time/clock.rs:16`), which panics on
`wasm32-unknown-unknown`. Swapping one import for the other converts a compile
error into a runtime crash, which is strictly worse and invisible to `cargo check`.

```
web/
  index.html        data-trunk links: rust app, copy blocks_pack.zip, worldgen.json, fixtures/
  Trunk.toml        dev-server config (COOP/COEP headers)
  assets/
    blocks_pack.zip trimmed real vanilla resource pack (fetched at runtime)
    worldgen.json   97 density/noise JSON files concatenated (fetched by the browser resolver)
  fixtures/
    chunks.bin      real level_chunk_with_light payloads captured from live 26.2
  src/
    main.rs         app entry, render loop, relay probe
    singleplayer.rs in-browser client↔server↔worldgen probe
    input.rs        platform input → shared lodestone-controller
    terrain.rs      chunk meshing
```
