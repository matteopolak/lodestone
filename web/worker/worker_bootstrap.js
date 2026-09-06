// The worker control-plane protocol, kept separately from `worker.js` so its
// launch/error behaviour can run under Node without a browser or a wasm build.
// Protocol bytes deliberately do not pass through this module: they use the
// transferred MessagePort after `ready`.
(function installWorkerBootstrap(global) {
  "use strict";

  function invalidLaunch(request, ports) {
    if (request?.kind !== "start" || ports.length !== 1) {
      return "invalid server worker launch";
    }
    if (!Number.isInteger(request.protocol) || request.protocol <= 0) {
      return "invalid server worker protocol";
    }
    if (typeof request.seed !== "string" || !/^-?\d+$/.test(request.seed)) {
      return "invalid server worker seed";
    }
    if (!Number.isInteger(request.preset) || request.preset < 0 || request.preset > 6) {
      return "invalid server worker world preset";
    }
    return null;
  }

  async function launch(event, loadWasm, postMessage) {
    const request = event.data;
    const ports = event.ports ?? [];
    const error = invalidLaunch(request, ports);
    if (error !== null) {
      postMessage({ kind: "error", message: error });
      return;
    }

    // These are truthful control-plane milestones: module loading is separate
    // from creating the world, and `ready` is sent only after the server has
    // accepted its MessagePort. The page keeps its existing chunk-progress UI
    // sourced from received chunk packets; no synthetic generation percentage
    // crosses this channel.
    postMessage({ kind: "progress", stage: "loading-module" });
    try {
      const wasm = await loadWasm();
      await wasm.default();
      postMessage({ kind: "progress", stage: "starting-server" });
      wasm.start_worker(ports[0], request.protocol, BigInt(request.seed), request.preset);
      postMessage({ kind: "ready" });
    } catch (caught) {
      postMessage({ kind: "error", message: String(caught) });
    }
  }

  global.LodestoneWorkerBootstrap = Object.freeze({ invalidLaunch, launch });
})(globalThis);
