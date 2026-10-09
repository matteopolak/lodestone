# Browser embedding

## What it is

The browser target exposes `mount(options)` and `LodestoneHandle.destroy()` for hosts that own a transferred `OffscreenCanvas` and have downloaded the required resource bytes. The standalone page is a thin adapter over the same runtime and asset contract.

## How it works

### Mount options

`mount` takes the `OffscreenCanvas` transferred into the calling worker, `Uint8Array`/`ArrayBuffer` values for `resourcePack` and `blocksJson`, and an optional `assetProvider(name)` supplying either blob (it must resolve any blob omitted directly; the panorama is already in `resourcePack`). `onProgress` receives `{type, phase, fraction, message}` for asset, startup, started, first-frame and destroyed events. `onHostAction(action)` receives actions such as `{ type: "pointer-lock", locked: true }` that the page performs under a user gesture and reports back. `logLevel` (`off` to `trace`, default `warn`) applies to the render worker and its integrated-server worker. Other options: `traceBlockActions` and `benchmark`. There is no auth provider or credential option.

The worker creates a WebGPU surface directly and drives the same `WindowApp` renderer from a worker timer. The handle owns renderer shutdown and `destroy` is idempotent (hosts may call it from both unmount and worker teardown).

### Lifecycle

The canvas is worker-owned for the session; Lodestone never touches the DOM or calls `transferControlToOffscreen()`. `first-frame` fires only after the render loop hands a frame to the presentation queue (including the ownership-menu path), through a per-mount readiness latch set at `WindowApp`'s present boundary (not a process-global that a threaded build could miss); the worker poll is a bounded failure diagnostic only. `destroyed` fires after the worker drops the renderer and GPU state, and a mount started during teardown waits for it, so the initialised module and bundle can be reused. Shutdown request and teardown completion are separate (presentation resources drop before the mount lease releases), and waiting uses host timers, not animation frames, so a hidden page finishes cleanup. Singleplayer joins emit elapsed-time progress. See [browser frame pacing](browser-frame-pacing.md): animation opportunities pace presentation while independent timers service input, network and lifecycle.

`full-view-presented` means the requested terrain is shown (possibly with older meshes). `full-view-quiescent` samples the first presented frame after mesh handoffs settle and the scheduler, column-repair, light-intent and removal queues drain. They are measurement milestones, not input unlocks; `pendingMeshes` counts scheduler work and `pendingLightRemeshes` light intents awaiting admission ([browser worker diagnostics](browser-worldgen-worker.md)).

The startup gate is local only: `Confirm ownership`, the checkbox `I confirm that I own Minecraft: Java Edition.` and a `Continue` button disabled until checked. State lives in the menu for the handle and is dropped by `destroy`; it is an acknowledgement, not a security boundary. Wasm has no account switcher, sign-in, token storage or auth callback.

Required asset installation copies host bytes into the shell's bundle and renderer bring-up builds pipelines and atlases synchronously after async adapter selection; both run on the canvas-owning worker, not the page.

### Input bridge

The handle exposes `pointerMove`, `mouseMotion`, `mouseButton`, `wheel`, `key`, `focus`, `visibility`, `resize` and `pointerLock`. Forward keyboard, pointer, wheel, focus, backing-size/DPR and pointer-lock events. Send `visibility(!document.hidden)` initially and on `visibilitychange`: visibility suppresses surface acquisition without stopping client servicing, while focus controls input and the background framerate cap. `pointerMove` takes backing-store pixels (scale CSS `offsetX`/`offsetY` by the DPR used for `resize`; mouse-motion deltas are unscaled). After transfer, the page must not set the element's `width` or `height`; send DPR-scaled dimensions through `resize`, which updates canvas and surface together. Pointer lock is two-way: the shell asks via `onHostAction`, the page calls `requestPointerLock()` from its gesture handler (or `exitPointerLock()`) and reports `pointerLock(actualState)` on `pointerlockchange`.

`key` takes DOM `KeyboardEvent.code` values, including `MetaLeft`/`MetaRight` (mapped to Super; `SuperLeft`/`SuperRight` still accepted). Forward physical codes and modifiers, with printable text as a separate argument.

### Benchmark declaration

`benchmark` selects the terrain workload, joins the declared external fixture through normal multiplayer and auto-captures its stationary phase. It needs `onProgress` and a multiplayer-enabled build with an adapter for the declared protocol. Validation precedes downloads and startup and is strict (unknown or missing fields, wrong types, fractional or nonfinite numbers and unsupported values fail the mount). `username` is an offline fixture identity of 1 to 16 ASCII letters, digits or underscores.

The eleven graphics values become an in-memory `Options::default()` snapshot used by the menu, simulation and renderer (persisted options do not seed it; the preset identity does not overwrite declared values; `Config::render_distance` uses it). Browser benchmarks use `windowed` policy and `options` pacing with real focus and visibility input. `width`/`height` must match the transferred canvas backing size, the host owns CSS layout and later `resize`, and fullscreen is not requested.

Bounds: `width` 320..8192, `height` 240..8192, port 1..65535, warmup 0..3600 s, stationary 1..10 s (integers; movement and mutation durations are zero, debug overlay closed). FPS 10..260 (260 unlimited), FOV 30..110, render distance 2..256, biome blend 0..7; `enable_vsync`, `cutout_leaves`, `entity_shadows` are booleans. Names: `minimized`/`afk` inactivity; `fast`/`fancy`/`fabulous`/`custom` preset; `off`/`fast`/`fancy` clouds (legacy booleans rejected); `all`/`decreased`/`minimal` particles.

The packaged worker forwards `onProgress` as `{ kind: "progress", event }`. Phase `benchmark-configured` carries a JSON witness of options, dimensions, presentation mode and effective render distance (it proves configuration, not terrain settlement or cadence). Completion is `presentation-capture-complete` with the JSON report in `event.message`; failure is `presentation-capture-error`. `startPresentationCapture`/`stopPresentationCapture` stay available for ordinary mounts. Short durations reduce retained-row pressure but do not guarantee zero dropped rows; acceptance must check the report's rows and observed settings, dimensions, camera, readiness and foreground witnesses.

### Server radius

The integrated-server worker takes the shell's `integrated_stream_radius` for the render distance, including mesh-neighbour and lookahead halo: render distance nine requests server radius eleven, as native does. Initial playable loading stays capped at radius six and the rest streams. Benchmark mounts seed configuration from their declaration; direct legacy server-worker callers omitting `viewRadius` keep radius eight.

The standalone page accepts `?log=debug` (or another level), which forwards sampled worldgen stage counters and server tick health to the page console alongside join timings, with session and target coordinates. Verbose levels are opt-in because traces cost console and scheduling time.

One module instance owns one asset bundle; a different bundle needs a fresh module after `destroy` because immutable resource caches live for the module. An `OffscreenCanvas` cannot be remounted after its owner is torn down.

### SDK package

`just wasm-sdk` runs `just fetch-assets-ci` (verifies the source client and index), builds a release Trunk bundle and writes `target/wasm-sdk/lodestone-web-sdk.tar.gz` plus a manifest. The archive holds a content-versioned ESM and Wasm pair, a content-versioned render worker, both server-worker variants with bootstrap and snippets, `lodestone-resources.zip` and `blocks.json`. Curated sounds (optional, host-controlled), the source client archive, separate panorama objects, stable aliases, `index.html`, CSS, service workers and diagnostics are excluded.

`lodestone-web-sdk.manifest.json` (schema version `2`) holds the source commit, archive digest and size, content-versioned `entrypoint` and `worker_entrypoint`, and a sorted digest and size per member. Create the worker from `worker_entrypoint` and send the manifest in the mount message; the worker rejects a manifest naming another worker, imports `entrypoint` and derives the matching `_bg.wasm` URL from the same name, so glue and Wasm never mismatch. The worker's provider maps logical `resourcePack`/`blocksJson` to the packaged filenames; custom providers receive logical names and must map themselves. The packager requires a clean checkout (`--allow-dirty` for local diagnostics) and the two asset files; `LODESTONE_WEB_SDK_DIR`, `LODESTONE_WEB_SDK_VERSION` and `LODESTONE_TRUNK` change output directory, label and Trunk executable. `just test-wasm-sdk` covers inventory, a two-release worker cache-key swap and a missing-archive control without compiling Wasm.

## How to change it

Change `web/src/embed.rs` for host-facing lifecycle events or options; keep `web/src/main.rs` to the standalone bootstrap and its render worker. Shutdown semantics belong in the worker-local browser control exported by `lodestone-shell`, the only owner of teardown. Keep `destroy` idempotent.

## Configuration

Inside the caller's render worker:

```js
const assetPaths = { resourcePack: "./lodestone-resources.zip", blocksJson: "./blocks.json" };
const game = await lodestone.mount({
  canvas: receivedOffscreenCanvas,
  assetProvider: name => fetch(assetPaths[name]).then(r => {
    if (!r.ok) throw new Error(`Asset download failed: ${r.status}`);
    return r.arrayBuffer();
  }),
  logLevel: "warn",
  onProgress: e => console.log(e.type, e.fraction),
  onHostAction: a => { /* call requestPointerLock from a gesture handler */ },
});
game.destroy();
```

In the page: `const offscreen = canvas.transferControlToOffscreen(); worker.postMessage({ kind: "mount", canvas: offscreen }, [offscreen]);`. For a benchmark, set `canvas.width`/`height` before transfer and add `benchmark: { host, port, protocol, width, height, warmupSeconds, stationarySeconds, username, settings: { framerate_limit, enable_vsync, inactivity_fps_limit, graphics_preset, cloud_status, cutout_leaves, entity_shadows, particles, fov, render_distance, biome_blend_radius } }` to the message.

SDK use: read `entrypoint` from the manifest, `const { default: init, mount } = await import("./" + manifest.entrypoint); await init(); await mount({ canvas, assetProvider })`, or start the worker as `new Worker(new URL(manifest.worker_entrypoint, manifestUrl))` and post `{ kind: "mount", canvas, manifest }`.

The host must serve WebGPU support and the cross-origin isolation headers the optional compute-worker pool needs. The standalone page marks its canvas `data-lodestone-standalone`; embedded hosts omit it so importing does not auto-start a second session.

## Dependencies

`wasm-bindgen`, `web-sys`, the shell's browser event-loop control, and `lodestone-shell::platform::assets` for asset bytes; rendering, input, simulation and cleanup stay in the shared shell.
