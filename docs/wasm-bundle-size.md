# WebAssembly bundle size

## What it is

The browser delivery consists of a page WebAssembly module, a dedicated server-worker module, their `wasm-bindgen` JavaScript glue, and runtime-fetched game assets. This document defines the size gates and the trade-offs that keep the browser build small without changing rendering or world-generation behavior.

## How it works

`web/Trunk.toml` builds the page package and then stages the Whimscape-based resource archive, block report, curated sound files, and the worker bundle. The panorama art is inside the archive. `just wasm-sdk` verifies the source game assets first. `web/src/main.rs` fetches the page assets relative to the current page before installing them into the shared asset loader. Starting singleplayer loads `lodestone-server-worker-bootstrap.js`, which selects the serial or threaded worker glue and its matching Wasm module; the worker owns the integrated world and sends framed bytes over a `MessageChannel`.

The page's `release` profile uses `opt-level = "z"`, `lto = "fat"`, one codegen unit, `panic = "abort"`, and `strip = true`, with optimization level `3` only for `lodestone-worldgen-core`. The worker-only `worker-release` profile inherits those settings and also optimizes `lodestone-worldgen` and `lodestone-server` at level `3`. Both serial and threaded workers use this profile; gameplay and generation code remain shared with the page and native client. Keeping decoration and settlement overrides worker-local avoids increasing the page's download cost. The `strip` setting removes symbol and debug sections from the release artifact; `wasm-bindgen` adds only the exports and glue metadata needed by the browser.

In the release browser seed-4242, render-distance-eight control, overriding only
the core reduced full-view presentation from 44.403 to 35.440 seconds. Gzip cost
rose by 11,631 bytes for the page and 19,149 bytes for the threaded worker;
the final page remained below the existing ceiling at 5,745,662 bytes. These are
single-run end-to-end samples, not isolated kernel CPU measurements. Compare
both runtime and post-bindgen compressed size before changing the hot-crate overrides.

## How to change it

Use `just wasm-size` for the page release ceiling and `just wasm-check` for the compile, confinement, worker-control, and Trunk gates. The size gate also rejects browser artifacts that retain Microsoft, Xbox, or device-code authentication endpoints and copy. A size investigation should measure the post-`wasm-bindgen` module that the browser fetches, not only Cargo's intermediate `.wasm`. Use `twiggy top` on an unoptimized release artifact to attribute code and data; generated block/state tables currently dominate the data section, so changing LTO or codegen units cannot remove that cost.

Run `python3 web/scripts/test_stage_resource_pack.py` when changing the staging
filter. Its controls cover loader-surface retention, deterministic bytes,
compatible overlay precedence, format rejection, archive part reconstruction,
and missing/corrupt source rejection. Keep the filter aligned with production
resource consumers; adding a loader that reads another namespace requires either
including that namespace or proving it is supplied by a separate runtime bundle.
Run `just test-wasm-sdk` when changing SDK collection or archive inventory; its
missing-archive control must continue to fail closed.

Keep multiplayer enabled in the default browser build unless the product explicitly accepts a singleplayer-only deployment. `web/Cargo.toml --no-default-features` is a supported alternative, but saves only about 9 KiB gzip in the measured build. Do not enable a required `wasm-opt` dependency casually: it is not installed by the repository's pinned Trunk workflow and its measured gain is expected to be mostly raw bytes, while compression and startup still need independent checks.

The browser archive is staged by `web/scripts/stage_resource_pack.py`. It combines Whimscape images and overrides with non-image base definitions and the recipe/item-tag records consumed by the browser recipe loader. It excludes base images, classes, signatures, reports, and unused data namespaces. The
block report is 6.8 MiB raw but about 244 KiB gzip, so host compression remains
effective. Content-addressed archive parts are for per-file host limits; they do
not reduce total bytes. Ogg files remain curated rather than embedded in the Wasm module.

## Configuration

- `web/Cargo.toml`: page features, `release` page profile, and `worker-release` worker profile.
- `web/scripts/stage_worker.sh`: selects `worker-release` for both worker variants; uses the existing shared Cargo target directory.
- `web/worker/Cargo.toml`: worker crate type and feature set; its profile is ignored when Cargo is invoked from the parent web workspace, so the root `web/Cargo.toml` profile is authoritative for that build.
- `web/index.html`: `data-wasm-opt="0"` keeps Trunk independent of an unpinned Binaryen installation.
- `CEILING_BYTES`: optional gzip ceiling override for `scripts/wasm-size.sh` (default `5_800_000`). The default is based on the measured 5,678,198 B post-bindgen page baseline and leaves a small, reviewable allowance for toolchain drift.
- `LODESTONE_WEB_CLIENT_JAR_PARTS=1`: stages content-addressed archive parts instead of one direct archive.
- `fetch-assets-ci`: verifies `.cache/mc/<version>/client.jar` and its asset index, which are staging inputs rather than distributed artwork.
- `lodestone-resources.zip.manifest.json`: digest and entry-count manifest for the direct merged archive.
- `web/scripts/stage_resource_pack.py`: deterministic resource-pack staging and CRC validation.

## Dependencies

The build uses Cargo, the `wasm32-unknown-unknown` target, Trunk 0.21.14, and its matching `wasm-bindgen` CLI. `brotli`, when installed, supplies the reported Brotli wire estimate; gzip is the enforced portable comparison. The page links `lodestone-shell` with the live protocol family, runtime presentation, and window features. The worker links the same shell with the live family and executes in a dedicated browser Worker.

## Measurements

These post-bindgen artifacts use the worker-only speed profile above. Gzip is the portable enforced comparison; no additional Wasm optimization pass was applied.

| artifact | raw bytes | gzip -n -9 | JavaScript gzip |
| --- | ---: | ---: | ---: |
| page | 16,687,148 | 5,752,525 | 18,451 |
| serial server worker | 18,534,786 | 5,878,659 | 5,807 |
| threaded server worker | 18,457,288 | 5,836,979 | 7,085 |

The page remains within the unchanged 5,800,000-byte gzip ceiling. Compared with the preceding core-only speed override, the worker modules cost approximately 18–20% more compressed bytes; that tradeoff must be checked against end-to-end generation and presentation latency. The merged resource archive, regenerated from the 26.2 definitions and Whimscape 26.1–26.3 r2, is 7,344,932 bytes with 16,166 entries and SHA-256 `faed8bee10b68692beae3d5bd51d4fecd88a3cce4a762f836178675fbff58163`. These are local staging measurements, not a compressed-transfer benchmark or a texture-coverage result.

`scripts/wasm-size.sh` fails closed unless `wasm-bindgen` emits the browser module and measures that exact output without an environment-dependent optimization pass. Run `web/scripts/measure_worker_size.sh` separately for both worker modules, their glue, and bindgen snippets.
