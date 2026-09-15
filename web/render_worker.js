// The standalone page owns the source HTMLCanvasElement and transfers its
// OffscreenCanvas here. This worker owns the Wasm renderer for its whole life.

let session = null;
let loading = false;

self.onmessage = event => {
  const request = event.data;
  if (!request || typeof request !== "object") return;

  if (request.kind === "destroy") {
    if (session) {
      session.destroy();
      session = null;
    }
    return;
  }
  if (request.kind === "input") {
    if (session) dispatchInput(request.input);
    return;
  }
  if (request.kind !== "mount") return;
  if (loading || session) {
    self.postMessage({ kind: "error", message: "render worker is already mounted" });
    return;
  }

  loading = true;
  void mount(request);
};

async function mount(request) {
  try {
    const sdk = await import("./lodestone-web-entry.js");
    await sdk.default({
      module_or_path: new URL("./lodestone-web-entry_bg.wasm", self.location.href),
    });
    session = await sdk.mount({
      canvas: request.canvas,
      clientJar: request.clientJar,
      blocksJson: request.blocksJson,
      assetProvider: packageAsset,
      onHostAction: action => self.postMessage({ kind: "host-action", action }),
      onProgress: event => self.postMessage({ kind: "progress", event }),
    });
    self.postMessage({ kind: "ready" });
  } catch (error) {
    self.postMessage({ kind: "error", message: String(error) });
  } finally {
    loading = false;
  }
}

async function packageAsset(name) {
  const response = await fetch(new URL(name, self.location.href));
  if (!response.ok) {
    throw new Error(`failed to fetch ${name}: HTTP ${response.status}`);
  }
  return response.arrayBuffer();
}

function dispatchInput(input) {
  if (!input || typeof input !== "object") return;
  try {
    switch (input.type) {
      case "pointerMove":
        session.pointerMove(input.x, input.y);
        break;
      case "mouseMotion":
        session.mouseMotion(input.dx, input.dy);
        break;
      case "mouseButton":
        session.mouseButton(input.button, input.pressed);
        break;
      case "wheel":
        session.wheel(input.dx, input.dy);
        break;
      case "key":
        session.key(input.code, input.pressed, input.text ?? null, input.modifiers ?? 0);
        break;
      case "focus":
        session.focus(input.focused);
        break;
      case "resize":
        session.resize(input.width, input.height);
        break;
      case "pointerLock":
        session.pointerLock(input.locked);
        break;
      default:
        break;
    }
  } catch (error) {
    self.postMessage({ kind: "error", message: `input dispatch failed: ${String(error)}` });
  }
}
