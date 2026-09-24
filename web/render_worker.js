// The standalone page owns the source HTMLCanvasElement and transfers its
// OffscreenCanvas here. This worker owns the Wasm renderer for its whole life.

let session = null;
let loading = false;
const PACKAGE_ASSET_PATHS = Object.freeze({
  clientJar: "client.jar",
  blocksJson: "blocks.json",
});

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
    const { sdk, wasmUrl } = await loadSdk(request);
    await sdk.default({
      module_or_path: wasmUrl,
    });
    session = await sdk.mount({
      canvas: request.canvas,
      clientJar: request.clientJar,
      blocksJson: request.blocksJson,
      logLevel: request.logLevel,
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

async function loadSdk(request) {
  const workerName = new URL(self.location.href).pathname.split("/").pop();
  if (workerName === "lodestone-render-worker.js" && !request.manifest && !request.manifestUrl) {
    return {
      sdk: await import("./lodestone-web-entry.js"),
      wasmUrl: new URL("./lodestone-web-entry_bg.wasm", self.location.href),
    };
  }

  const manifestUrl = new URL(
    request.manifestUrl ?? "./lodestone-web-sdk.manifest.json",
    self.location.href,
  );
  const manifest = request.manifest ?? await fetch(manifestUrl, { cache: "no-store" }).then(response => {
    if (!response.ok) throw new Error(`failed to fetch SDK manifest: HTTP ${response.status}`);
    return response.json();
  });
  if (manifest.worker_entrypoint !== workerName) {
    throw new Error(`SDK worker mismatch: expected ${manifest.worker_entrypoint}, running ${workerName}`);
  }
  if (typeof manifest.entrypoint !== "string" || !manifest.entrypoint.endsWith(".js")) {
    throw new Error("SDK manifest has no valid module entrypoint");
  }
  const moduleUrl = new URL(manifest.entrypoint, manifestUrl);
  const wasmUrl = new URL(`${manifest.entrypoint.slice(0, -3)}_bg.wasm`, manifestUrl);
  return { sdk: await import(moduleUrl.href), wasmUrl };
}

async function packageAsset(name) {
  const path = PACKAGE_ASSET_PATHS[name] ?? name;
  const response = await fetch(new URL(path, self.location.href));
  if (!response.ok) {
    throw new Error(`failed to fetch ${name} from ${path}: HTTP ${response.status}`);
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
