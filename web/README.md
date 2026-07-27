# lodestone-web — browser (WebAssembly) spike

An isolated feasibility spike proving Lodestone's stack runs in a browser via
WebAssembly + WebGPU. It is its **own** Cargo workspace (empty `[workspace]` in
`Cargo.toml`), deliberately outside the parent `crates/lodestone-*` glob, so it
never affects other crates' `cargo build --workspace`.

## What it demonstrates

- **Rendering:** real `level_chunk_with_light` fixture bytes → `lodestone-world`
  → greedy mesh → wgpu, drawn under **WebGPU** at ~120 fps.
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

## Layout

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
