# WebAssembly bundle size

## What it is

The browser delivery is a page Wasm module, a dedicated server-worker module, their `wasm-bindgen` glue, and runtime-fetched game assets. This doc defines the size gates and the trade-offs that keep it small without changing rendering or world generation.

## How it works

`web/Trunk.toml` builds the page and stages the Whimscape-based resource archive (panorama included), block report, curated sounds and the worker bundle. `web/src/main.rs` fetches assets relative to the page. Starting singleplayer loads `lodestone-server-worker-bootstrap.js`, which picks the serial or threaded worker glue and Wasm; the worker owns the integrated world and talks over a `MessageChannel`.

Profiles (in `web/Cargo.toml`):
- `release` (page): `opt-level = "z"`, fat LTO, one codegen unit, `panic = "abort"`, `strip = true`; level 3 only for `lodestone-worldgen-core`.
- `worker-release` (both worker variants): inherits `release` and also builds `lodestone-worldgen` and `lodestone-server` at level 3. Keeping those overrides worker-local avoids page download cost.

Overriding only the core to level 3 cut a seed-4242, render-distance-8 full-view presentation from 44.4 s to 35.4 s for +11.6 KB (page) and +19.1 KB (threaded worker) gzip. These are single end-to-end runs, not kernel measurements; compare runtime and post-bindgen compressed size before changing the overrides.

The resource archive is built by `web/scripts/stage_resource_pack.py`: Whimscape images and overrides, non-image base definitions, and the recipe/item-tag records the browser loader reads. It excludes base images, classes, signatures, reports and unused namespaces. The block report is 6.8 MiB raw but about 244 KiB gzip. Content-addressed archive parts exist for per-file host limits and do not reduce total bytes. Ogg files stay curated, not embedded.

## How to change it

- `just wasm-size` enforces the page ceiling; `just wasm-check` runs compile, confinement, worker-control and Trunk gates. The size gate also rejects artifacts containing Microsoft, Xbox or device-code authentication endpoints or copy.
- Measure the post-`wasm-bindgen` module the browser fetches, not Cargo's intermediate `.wasm`. Use `twiggy top` on an unoptimized release build; generated block/state tables dominate the data section, so LTO or codegen-unit changes cannot remove them.
- Changing the staging filter: run `python3 web/scripts/test_stage_resource_pack.py`. A new loader reading another namespace must include it or get it from a separate runtime bundle.
- Changing SDK collection or archive inventory: run `just test-wasm-sdk`; its missing-archive control must keep failing closed.
- Keep multiplayer in the default browser build unless singleplayer-only is accepted; `--no-default-features` saves only about 9 KiB gzip.
- Do not make `wasm-opt` a hard dependency: the pinned Trunk workflow does not install it.

## Configuration

- `web/Cargo.toml`: page features and both profiles. It is authoritative; `web/worker/Cargo.toml` profiles are ignored when built from the parent workspace.
- `web/scripts/stage_worker.sh`: builds both worker variants with `worker-release`.
- `web/index.html`: `data-wasm-opt="0"` keeps Trunk independent of Binaryen.
- `CEILING_BYTES`: gzip ceiling override for `scripts/wasm-size.sh`, default `5_800_000` (measured page baseline 5,678,198 B plus a small allowance).
- `LODESTONE_WEB_CLIENT_JAR_PARTS=1`: stage content-addressed archive parts instead of one archive.
- `fetch-assets-ci` / `just wasm-sdk`: verify `.cache/mc/<version>/client.jar` and its asset index, which are staging inputs.
- `lodestone-resources.zip.manifest.json`: digest and entry-count manifest of the direct archive.

## Dependencies

Cargo with `wasm32-unknown-unknown`, Trunk 0.21.14 and its matching `wasm-bindgen` CLI; `brotli` (optional) for the Brotli estimate, with gzip as the enforced comparison. Page and worker link `lodestone-shell` with the live family.

## Measurements

Post-bindgen, no extra optimization pass; gzip is `gzip -n -9`.

| artifact | raw bytes | gzip | JS gzip |
| --- | ---: | ---: | ---: |
| page | 16,687,148 | 5,752,525 | 18,451 |
| serial server worker | 18,534,786 | 5,878,659 | 5,807 |
| threaded server worker | 18,457,288 | 5,836,979 | 7,085 |

Speed-profiled workers cost about 18-20% more compressed bytes than a core-only override. The merged resource archive is 7,344,932 bytes, 16,166 entries, SHA-256 `faed8bee10b68692beae3d5bd51d4fecd88a3cce4a762f836178675fbff58163`. `web/scripts/measure_worker_size.sh` measures both workers, glue and snippets.
