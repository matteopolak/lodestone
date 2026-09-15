# Browser embedding

## What it is

The browser target exposes a small `mount(options)` / `LodestoneHandle.destroy()` API for hosts that own a transferred `OffscreenCanvas` and have downloaded the required resource bytes. The standalone page remains a thin adapter that uses the same runtime and asset contract.

## How it works

`mount` accepts an `OffscreenCanvas` transferred into the calling worker. It also accepts `Uint8Array` or `ArrayBuffer` values for `clientJar` and `blocksJson`, and an optional `assetProvider(name)` callback for resolving either required blob. The provider may return bytes directly or a promise of bytes. The optional `onProgress` callback receives objects with `type`, `phase`, `fraction`, and `message`; the lifecycle emits asset, startup, started, first-frame, and destroyed events. `onHostAction(action)` receives browser actions such as `{ type: "pointer-lock", locked: true }`; the page must perform the corresponding user-gesture-gated DOM operation and forward the resulting state back to the handle. The worker-local path creates a WebGPU surface directly and drives the same `WindowApp` renderer from a worker timer. The returned handle owns renderer shutdown and can be destroyed idempotently from the host.

The transferred canvas is owned by the worker for the entire session; Lodestone never accesses the page DOM or performs the transfer itself. `first-frame` is emitted only after the render loop has handed a frame to the browser presentation queue, including the full-screen ownership menu path. The readiness latch is created per mount and is set by the same `WindowApp` present boundary that draws the frame, so it is not a process-global or thread-local observation that can miss a canonical threaded build. The worker-facing poll remains bounded only as a failure diagnostic if WebGPU never becomes ready. Fullscreen, input bridging, and worker lifetime remain caller-owned. `destroyed` is emitted after the worker has dropped the renderer and GPU state. A mount started while that teardown is in progress waits for it, so callers may safely reuse the initialized module and asset bundle without an arbitrary delay.

The browser startup gate is local only. It shows `Confirm ownership`, the checkbox
`I confirm that I own Minecraft: Java Edition.`, and a `Continue` button that stays disabled
until checked. Wasm has no account switcher, Microsoft sign-in, OAuth/device-code flow, token or
identity storage, or auth callback in the mount API. The checkbox state lives in the menu for the
current handle and is dropped by `destroy`; it is an acknowledgement, not a security boundary.

The presentation seam is intentionally caller-owned: the page creates an HTML canvas, calls `transferControlToOffscreen()`, transfers that object to its worker, owns worker lifetime and input/UI bridging, and invokes `mount` from the worker. Lodestone never calls `transferControlToOffscreen()` and never touches the DOM. Its worker loop creates the WebGPU surface directly from the received `OffscreenCanvas`, runs the existing simulation/render passes, and marks the per-mount readiness latch immediately after `present`. Worker timers replace page `requestAnimationFrame` for both render pacing and bounded lifecycle polling. Required asset installation still materializes host-provided bytes into the shell's owned bundle, and renderer bring-up still performs synchronous pipeline/atlas construction after asynchronous adapter selection; those costs occur on the worker that owns the canvas rather than blocking the page.

The handle exposes `pointerMove`, `mouseMotion`, `mouseButton`, `wheel`, `key`, `focus`, `resize`, and `pointerLock` for a caller-owned input bridge. Forward keyboard, pointer, wheel, focus, backing-size/DPR, and browser pointer-lock events from the page to the worker. Pointer lock is a two-way protocol: the shell requests or releases it through `onHostAction`; the page performs `requestPointerLock()` from its gesture handler (and `exitPointerLock()` for release), then calls `pointerLock(actualState)` when `pointerlockchange` fires.

## How to change it

Change `web/src/embed.rs` when adding host-facing lifecycle events or options. Keep `web/src/main.rs` limited to the standalone bootstrap and its caller-owned render worker. Changes to shutdown semantics belong in the worker-local browser control exported by `lodestone-shell`, because only that layer owns renderer teardown. Keep the API idempotent: hosts may call `destroy` from both an explicit unmount and a worker teardown path.

The public Wasm mount shape is intentionally limited to `canvas`, `clientJar`, `blocksJson`,
`assetProvider`, `onProgress`, and `onHostAction`; do not add an auth provider or
credential-bearing option.

## Configuration

Example:

```js
const offscreen = canvas.transferControlToOffscreen();
const game = await lodestone.mount({
  canvas: offscreen,
  assetProvider: name => fetch(name).then(response => response.arrayBuffer()),
  onProgress: event => console.log(event.type, event.fraction),
  onHostAction: action => {
    if (action.type === "pointer-lock" && action.locked) {
      // Call requestPointerLock from the page's pointer gesture handler.
    }
  },
});

game.destroy();
```

For worker presentation, transfer the canvas in the caller and run the same
module entrypoint in that worker:

```js
const offscreen = canvas.transferControlToOffscreen();
worker.postMessage({ kind: "mount", canvas: offscreen }, [offscreen]);
```

The worker calls `mount({ canvas: event.data.canvas, ... })`; the page remains
responsible for the visible DOM overlay, worker termination, and any input
messages. An `OffscreenCanvas` cannot be remounted after its owner is torn down;
create a fresh transferred canvas for a new worker session.

The host must serve the page with WebGPU support and the same cross-origin isolation headers required by the optional compute-worker pool. The standalone page marks its canvas with `data-lodestone-standalone`; embedded hosts omit that marker so importing the module does not auto-start a second session.

One wasm module instance owns one installed asset bundle. A different bundle requires a fresh module instance after `destroy`; the shell intentionally keeps its immutable resource caches for the lifetime of the module.

## SDK package

`just wasm-sdk` first runs `just fetch-assets-ci`, which verifies the cached client/index and fetches the six content-addressed panorama faces when needed, then builds a release Trunk bundle and writes `target/wasm-sdk/lodestone-web-sdk.tar.gz` plus its sidecar manifest. The archive contains the ESM page module and Wasm, both server-worker variants and their bootstrap/snippet files, the filtered `client.jar` plus `blocks.json` required by `mount`, and `panorama_0.png` through `panorama_5.png`. Curated sound files remain outside the SDK archive because audio is optional and host-controlled. It intentionally excludes the standalone `index.html`, consumer CSS, service workers, and diagnostic harnesses; hosts own those concerns.

The archive preserves the emitted filenames. Read `entrypoint` from the manifest before importing the ESM module from the unpacked directory:

```js
const manifest = await fetch("./lodestone-web-sdk.manifest.json").then(response => response.json());
const { default: init, mount } = await import(`./${manifest.entrypoint}`);

await init();
const session = await mount({ canvas, assetProvider });
```

`lodestone-web-sdk.manifest.json` has schema version `1`, the source commit, archive digest and size, and a sorted digest/size record for every archive member. The packager requires a clean checkout, filtered `client.jar`, `blocks.json`, and all six panorama faces; use `--allow-dirty` only for local diagnostics. Set `LODESTONE_WEB_SDK_DIR`, `LODESTONE_WEB_SDK_VERSION`, or `LODESTONE_TRUNK` to change the output directory, package label, or Trunk executable without changing the archive layout. `just test-wasm-sdk` exercises the inventory and a missing-face negative control without compiling Wasm.

## Dependencies

The API uses `wasm-bindgen`, `web-sys`, and the shell's browser event-loop control. Asset bytes are installed through `lodestone-shell::platform::assets`; rendering, input, simulation, and cleanup remain in the shared shell rather than being duplicated in the web adapter.
