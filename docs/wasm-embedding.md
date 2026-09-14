# Browser embedding

## What it is

The browser target exposes a small `mount(options)` / `LodestoneHandle.destroy()` API for hosts that already own a canvas and have downloaded the required resource bytes. The standalone page remains a thin adapter that uses the same runtime and asset contract.

## How it works

`mount` accepts an `HTMLCanvasElement`, `Uint8Array` or `ArrayBuffer` values for `clientJar` and `blocksJson`, and an optional `assetProvider(name)` callback for resolving either required blob. The provider may return bytes directly or a promise of bytes. The optional `onProgress` callback receives objects with `type`, `fraction`, and `message`; the lifecycle emits asset, startup, started, first-frame, and destroyed events. The renderer is scheduled through the existing browser event loop, and the returned handle sends an ordered quit event so the owned application, GPU state, animation work, and simulation tasks can be dropped by the event-loop owner.

The host canvas is passed directly into the windowing layer; its id and DOM ownership are untouched for the entire session. `first-frame` is emitted only after the renderer reports a submitted frame, with a bounded browser-frame timeout if WebGPU never becomes ready. Fullscreen remains entirely caller-owned. `destroyed` is emitted after the browser event loop has dropped the renderer and GPU state. A mount started while that teardown is in progress waits for it, so callers may safely reuse the initialized module and asset bundle without an arbitrary delay.

The browser startup gate is local only. It shows `Confirm ownership`, the checkbox
`I confirm that I own Minecraft: Java Edition.`, and a `Continue` button that stays disabled
until checked. Wasm has no account switcher, Microsoft sign-in, OAuth/device-code flow, token or
identity storage, or auth callback in the mount API. The checkbox state lives in the menu for the
current handle and is dropped by `destroy`; it is an acknowledgement, not a security boundary.

OffscreenCanvas is not enabled. The current windowing layer binds a DOM canvas during its synchronous resume callback and the renderer creates its WebGPU surface from that window; moving it to a worker would require a worker-owned windowing backend and a separate input/event bridge. The compute worker remains a separate concern, so this limitation does not move world generation onto the renderer thread.

## How to change it

Change `web/src/embed.rs` when adding host-facing lifecycle events or options. Keep `web/src/main.rs` limited to the standalone bootstrap. Changes to shutdown semantics belong in the browser control exported by `lodestone-shell`, because only that layer owns the event-loop proxy and can guarantee application teardown. Keep the API idempotent: hosts may call `destroy` from both an explicit unmount and a page teardown path.

The public Wasm mount shape is intentionally limited to `canvas`, `clientJar`, `blocksJson`,
`assetProvider`, and `onProgress`; do not add an auth provider or credential-bearing option.

## Configuration

Example:

```js
const game = await lodestone.mount({
  canvas,
  assetProvider: name => fetch(name).then(response => response.arrayBuffer()),
  onProgress: event => console.log(event.type, event.fraction),
});

game.destroy();
```

The host must serve the page with WebGPU support and the same cross-origin isolation headers required by the optional compute-worker pool. The standalone page marks its canvas with `data-lodestone-standalone`; embedded hosts omit that marker so importing the module does not auto-start a second session.

One wasm module instance owns one installed asset bundle. A different bundle requires a fresh module instance after `destroy`; the shell intentionally keeps its immutable resource caches for the lifetime of the module.

## SDK package

`just wasm-sdk` builds a release Trunk bundle and writes `target/wasm-sdk/lodestone-web-sdk.tar.gz` plus its sidecar manifest. The archive contains the ESM page module and Wasm, both server-worker variants and their bootstrap/snippet files, and the filtered `client.jar` plus `blocks.json` required by `mount`. Optional panorama and sound files stay out of the SDK archive because the host controls optional media loading. It intentionally excludes the standalone `index.html`, consumer CSS, service workers, and diagnostic harnesses; hosts own those concerns.

The archive preserves the emitted filenames. Read `entrypoint` from the manifest before importing the ESM module from the unpacked directory:

```js
const manifest = await fetch("./lodestone-web-sdk.manifest.json").then(response => response.json());
const { default: init, mount } = await import(`./${manifest.entrypoint}`);

await init();
const session = await mount({ canvas, assetProvider });
```

`lodestone-web-sdk.manifest.json` has schema version `1`, the source commit, archive digest and size, and a sorted digest/size record for every archive member. The packager requires a clean checkout, filtered `client.jar`, and `blocks.json`; use `--allow-dirty` only for local diagnostics. Set `LODESTONE_WEB_SDK_DIR`, `LODESTONE_WEB_SDK_VERSION`, or `LODESTONE_TRUNK` to change the output directory, package label, or Trunk executable without changing the archive layout.

## Dependencies

The API uses `wasm-bindgen`, `web-sys`, and the shell's browser event-loop control. Asset bytes are installed through `lodestone-shell::platform::assets`; rendering, input, simulation, and cleanup remain in the shared shell rather than being duplicated in the web adapter.
