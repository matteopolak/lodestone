# WebAssembly bundle size

## What it is

The browser delivery consists of a page WebAssembly module, a dedicated server-worker module, their `wasm-bindgen` JavaScript glue, and runtime-fetched game assets. This document defines the size gates and the trade-offs that keep the browser build small without changing rendering or world-generation behavior.

## How it works

`web/Trunk.toml` builds the page package and then stages the client archive, block report, panorama faces, curated sound files, and the worker bundle. `just wasm-sdk` runs `fetch-assets-ci` first; its content-addressed verification means a preceding explicit fetch does not redownload the faces. The SDK package requires all six panorama files and records them in its manifest, while the development `trunk serve`/`just run-wasm` path remains fail-open. `web/src/main.rs` fetches the page assets relative to the current page before installing them into the shared asset loader. Starting singleplayer loads `lodestone-server-worker-bootstrap.js`, which dynamically imports the worker glue and its sibling `lodestone-server-worker-wasm_bg.wasm`; the worker owns the integrated world and sends framed bytes over a `MessageChannel`.

The page and worker Cargo profiles use `opt-level = "z"`, `lto = "fat"`, one codegen unit, `panic = "abort"`, and `strip = true`. `opt-level = "z"` is intentional: measured compressed output is smaller than both `"s"` and `3`. The `strip` setting removes symbol and debug sections from the release artifact; the `wasm-bindgen` step adds only the exports and glue metadata needed by the browser.

## How to change it

Use `just wasm-size` for the page release ceiling and `just wasm-check` for the compile, confinement, worker-control, and Trunk gates. The size gate also rejects browser artifacts that retain Microsoft, Xbox, or device-code authentication endpoints and copy. A size investigation should measure the post-`wasm-bindgen` module that the browser fetches, not only Cargo's intermediate `.wasm`. Use `twiggy top` on an unoptimized release artifact to attribute code and data; generated block/state tables currently dominate the data section, so changing LTO or codegen units cannot remove that cost.

Run `python3 web/scripts/test_stage_resource_pack.py` when changing the staging
filter. Its controls cover loader-surface retention, deterministic bytes, and
missing/corrupt source rejection. Keep the filter aligned with production
resource consumers; adding a loader that reads another namespace requires either
including that namespace or proving it is supplied by a separate runtime bundle.
Run `just test-wasm-sdk` when changing SDK collection or archive inventory; its
missing-face control must continue to fail closed.

Keep multiplayer enabled in the default browser build unless the product explicitly accepts a singleplayer-only deployment. `web/Cargo.toml --no-default-features` is a supported alternative, but saves only about 9 KiB gzip in the measured build. Do not enable a required `wasm-opt` dependency casually: it is not installed by the repository's pinned Trunk workflow and its measured gain is expected to be mostly raw bytes, while compression and startup still need independent checks.

The browser archive is staged by `web/scripts/stage_resource_pack.py`. It keeps
every `assets/` entry, pack metadata, and the `data/*/recipe/**` and
`data/*/tags/item/**` records consumed by the browser recipe loader. It excludes
classes, signatures, reports, and data namespaces no browser loader reads. The
block report is 6.8 MiB raw but about 244 KiB gzip, so host compression remains
effective. Content-addressed archive parts are for per-file host limits; they do
not reduce total bytes. Panorama PNGs and Ogg files are already compressed and
stay optional or curated rather than embedded in the Wasm module.

## Configuration

- `web/Cargo.toml`: page features and release profile.
- `web/worker/Cargo.toml`: worker crate type and feature set; its profile is ignored when Cargo is invoked from the parent web workspace, so the root `web/Cargo.toml` profile is authoritative for that build.
- `web/index.html`: `data-wasm-opt="0"` keeps Trunk independent of an unpinned Binaryen installation.
- `CEILING_BYTES`: optional gzip ceiling override for `scripts/wasm-size.sh` (default `5_800_000`). The default is based on the measured 5,678,198 B post-bindgen page baseline and leaves a small, reviewable allowance for toolchain drift.
- `LODESTONE_WEB_CLIENT_JAR_PARTS=1`: stages content-addressed archive parts instead of one direct archive.
- `fetch-assets-ci`: verifies `.cache/mc/26.2/client.jar` and its asset index, including the six panorama objects used by `wasm-sdk`.
- `client.jar.manifest.json`: digest and entry-count manifest for the direct filtered archive.
- `web/scripts/stage_resource_pack.py`: deterministic resource-pack staging and CRC validation.

## Dependencies

The build uses Cargo, the `wasm32-unknown-unknown` target, Trunk 0.21.14, and its matching `wasm-bindgen` CLI. `brotli`, when installed, supplies the reported Brotli wire estimate; gzip is the enforced portable comparison. The page links `lodestone-shell` with the live protocol family, runtime presentation, and window features. The worker links the same shell with the live family and executes in a dedicated browser Worker.

## Measurements

The current browser-fetched release artifacts use Brotli quality 11 for the wire estimate. Gzip is the portable enforced comparison.

| artifact | raw bytes | gzip -9 | Brotli-q11 | JavaScript gzip |
| --- | ---: | ---: | ---: | ---: |
| page | 16,415,799 | 5,638,555 | 4,604,171 | 19,838 |
| serial server worker | 14,576,818 | 4,596,880 | 3,811,558 | 5,526 |
| threaded server worker | 14,913,728 | 4,592,649 | 3,812,654 | 6,787 |

The page remains within the 5,800,000-byte gzip ceiling. The filtered resource archive is 5,697,099 bytes raw, 3,285,761 bytes gzip, and 3,120,121 bytes Brotli-q11, down from 39,193,383 bytes raw and 33,289,633 bytes gzip. It retains 12,778 entries and has digest `abba0ee70d999de903e3be8e6029faf418e4ee8d4604d5bc0d4c824e3d68c43d`.

`scripts/wasm-size.sh` fails closed unless `wasm-bindgen` emits the browser module and measures that exact output without an environment-dependent optimization pass. Run `web/scripts/measure_worker_size.sh` separately for both worker modules, their glue, and bindgen snippets.
