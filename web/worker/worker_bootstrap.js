// The worker control-plane protocol, kept separately from `worker.js` so its
// launch/error behaviour can run under Node without a browser or a wasm build.
// Protocol bytes deliberately do not pass through this module: they use the
// transferred MessagePort after `ready`.
(function installWorkerBootstrap(global) {
  "use strict";

  // Probe actual shared-memory Wasm support; SharedArrayBuffer alone is insufficient.
  const SHARED_MEMORY_PROBE = Object.freeze([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    0x05, 0x04, 0x01, 0x03, 0x01, 0x01,
  ]);
  let activeLaunch = null;

  function threadSupport(host = global) {
    if (host?.crossOriginIsolated !== true) {
      return { available: false, reason: "cross-origin-isolation-required" };
    }
    if (typeof host?.SharedArrayBuffer !== "function") {
      return { available: false, reason: "shared-array-buffer-unavailable" };
    }
    if (typeof host?.MessageChannel !== "function") {
      return { available: false, reason: "message-channel-unavailable" };
    }
    if (
      typeof host?.Atomics !== "object" ||
      typeof host?.Atomics?.wait !== "function" ||
      typeof host?.Atomics?.notify !== "function"
    ) {
      return { available: false, reason: "atomics-wait-unavailable" };
    }
    if (
      typeof host?.WebAssembly?.validate !== "function" ||
      typeof host?.WebAssembly?.Memory !== "function"
    ) {
      return { available: false, reason: "webassembly-validation-unavailable" };
    }
    let memory;
    try {
      memory = new host.WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
      if (!(memory.buffer instanceof host.SharedArrayBuffer) ||
          !host.WebAssembly.validate(new Uint8Array(SHARED_MEMORY_PROBE))) {
        return { available: false, reason: "webassembly-threads-unavailable" };
      }
    } catch (_error) {
      return { available: false, reason: "webassembly-threads-unavailable" };
    }
    try {
      const channel = new host.MessageChannel();
      channel.port1.postMessage(memory);
      channel.port1.close?.();
      channel.port2.close?.();
    } catch (_error) {
      return { available: false, reason: "shared-memory-clone-unavailable" };
    }
    return { available: true, reason: "available" };
  }

  // Leave one hardware lane for the authoritative server and cap pool startup cost.
  function poolWidth(host = global, maximum = 4) {
    const cap = Number.isInteger(maximum) ? Math.max(1, maximum) : 4;
    const hardware = Number(host?.navigator?.hardwareConcurrency);
    if (!Number.isFinite(hardware) || hardware < 2) {
      return 1;
    }
    return Math.max(1, Math.min(cap, Math.floor(hardware) - 1));
  }

  function invalidLaunch(request, ports) {
    if (request?.kind !== "start" || ports.length !== 3) {
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
    if (!Number.isSafeInteger(request.epoch) || request.epoch < 1) {
      return "invalid server worker cancellation epoch";
    }
    return null;
  }

  async function launch(event, loadWasm, postMessage, host = global) {
    const request = event.data;
    const ports = event.ports ?? [];
    if (activeLaunch !== null) {
      postMessage({ kind: "error", message: "server worker launch already completed" });
      return;
    }
    const error = invalidLaunch(request, ports);
    if (error !== null) {
      postMessage({ kind: "error", message: error });
      return;
    }

    activeLaunch = {
      epoch: request.epoch,
      progress: ports[1],
      horizon: ports[2],
      wasm: null,
    };
    postMessage({ kind: "progress", stage: "loading-module" });
    const support = threadSupport(host);
    const threaded = support.available;
    const width = poolWidth(host);
    const selection = {
      kind: "progress",
      stage: threaded ? "selecting-threaded-pool" : "selecting-serial-fallback",
      workers: threaded ? width : 1,
    };
    if (!threaded) {
      selection.reason = support.reason;
    }
    postMessage(selection);
    postWorldgenProgress(
      ports[1],
      request.epoch,
      threaded ? "threaded-selected" : "serial-fallback",
      { queue: 0 },
    );
    try {
      const mode = threaded ? "threaded" : "serial";
      let wasm = await loadWasm(mode);
      activeLaunch.wasm = wasm;
      await wasm.default();
      if (threaded) {
        postMessage({ kind: "progress", stage: "starting-compute-pool", workers: width });
        try {
          if (typeof wasm.initThreadPool !== "function") {
            throw new Error("threaded server worker has no initThreadPool export");
          }
          await wasm.initThreadPool(width);
          postMessage({ kind: "progress", stage: "compute-pool-ready", workers: width });
          postWorldgenProgress(ports[1], request.epoch, "compute-ready", { queue: 0 });
        } catch (caught) {
          postMessage({
            kind: "progress",
            stage: "compute-pool-error",
            reason: String(caught),
            workers: width,
          });
          postWorldgenProgress(ports[1], request.epoch, "compute-pool-error", { queue: 0 });
          throw caught;
        }
      }
      postMessage({ kind: "progress", stage: "starting-server" });
      postMessage({
        kind: "progress",
        stage: "preparing-world",
        executor: mode,
        workers: mode === "threaded" ? width : 1,
      });
      postWorldgenProgress(ports[1], request.epoch, "starting-server", { queue: 0 });
      wasm.start_worker(
        ports[0],
        ports[1],
        ports[2],
        request.protocol,
        BigInt(request.seed),
        request.preset,
        request.epoch,
      );
      postMessage({ kind: "ready" });
      postWorldgenProgress(ports[1], request.epoch, "server-ready", { queue: 0 });
    } catch (caught) {
      postMessage({ kind: "error", message: String(caught) });
      postWorldgenProgress(ports[1], request.epoch, "startup-error", { queue: 0 });
      activeLaunch.horizon.close?.();
    }
  }

  function postWorldgenProgress(port, epoch, stage, counters = {}) {
    if (!port || typeof port.postMessage !== "function") return;
    port.postMessage({
      kind: "worldgen-progress",
      epoch,
      admitted: Number.isSafeInteger(counters.admitted) ? counters.admitted : 0,
      completed: Number.isSafeInteger(counters.completed) ? counters.completed : 0,
      committed: Number.isSafeInteger(counters.committed) ? counters.committed : 0,
      queue: Number.isSafeInteger(counters.queue) ? counters.queue : 0,
      bytes: Number.isSafeInteger(counters.bytes) ? counters.bytes : 0,
      stage,
    });
  }

  function cancel(event, postMessage) {
    const request = event.data;
    if (request?.kind !== "cancel" || !Number.isSafeInteger(request.epoch)) {
      postMessage({ kind: "error", message: "invalid server worker cancellation" });
      return;
    }
    if (activeLaunch === null || request.epoch !== activeLaunch.epoch) {
      postMessage({ kind: "error", message: "stale server worker cancellation epoch" });
      return;
    }
    if (typeof activeLaunch.wasm?.cancel_worker !== "function") {
      postMessage({ kind: "error", message: "server worker cancellation is unavailable" });
      return;
    }
    if (activeLaunch.wasm.cancel_worker(request.epoch) !== true) {
      postMessage({ kind: "error", message: "server worker rejected cancellation epoch" });
      return;
    }
    postWorldgenProgress(activeLaunch.progress, request.epoch, "cancelled");
  }

  function reset() {
    activeLaunch?.horizon?.close?.();
    activeLaunch = null;
  }

  global.LodestoneWorkerBootstrap = Object.freeze({
    invalidLaunch,
    launch,
    cancel,
    poolWidth,
    postWorldgenProgress,
    reset,
    threadSupport,
  });
})(globalThis);
