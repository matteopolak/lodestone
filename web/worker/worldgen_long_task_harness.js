// Browser-side measurement helper for the authoritative server Worker.
(function installWorldgenMeasurement(global) {
  "use strict";

  const DEFAULTS = Object.freeze({
    workerUrl: "lodestone-server-worker.js",
    protocol: 776,
    seed: "42",
    preset: 0,
    runtimeMs: 5_000,
  });

  function now(host) {
    return host.performance.now();
  }

  function memorySample(host) {
    const measure = host.performance.measureUserAgentSpecificMemory;
    if (typeof measure !== "function") return Promise.resolve(null);
    return Promise.resolve()
      .then(() => measure.call(host.performance))
      .then((sample) => Number.isFinite(sample?.bytes) ? sample.bytes : null)
      .catch(() => null);
  }

  function measure(options = {}, host = global) {
    const settings = { ...DEFAULTS, ...options };
    if (!Number.isInteger(settings.protocol) || settings.protocol <= 0) {
      throw new TypeError("protocol must be a positive integer");
    }
    if (typeof settings.seed !== "string" || !/^-?\d+$/.test(settings.seed)) {
      throw new TypeError("seed must be a decimal string");
    }
    if (!Number.isInteger(settings.preset) || settings.preset < 0 || settings.preset > 6) {
      throw new TypeError("preset must be an integer from 0 through 6");
    }
    if (!Number.isFinite(settings.runtimeMs) || settings.runtimeMs < 0) {
      throw new TypeError("runtimeMs must be non-negative");
    }

    const WorkerCtor = settings.Worker ?? host.Worker;
    const MessageChannelCtor = settings.MessageChannel ?? host.MessageChannel;
    if (typeof WorkerCtor !== "function" || typeof MessageChannelCtor !== "function") {
      throw new Error("browser Worker and MessageChannel APIs are required");
    }

    const startedAt = now(host);
    const worker = new WorkerCtor(settings.workerUrl);
    const protocolChannel = new MessageChannelCtor();
    const progressChannel = new MessageChannelCtor();
    const horizonChannel = new MessageChannelCtor();
    const progress = [];
    const longTasks = [];
    let observer;
    let timer;
    let resolveDone;
    let stopped = false;
    let readyAt = null;
    let error = null;
    const memoryBefore = memorySample(host);
    let memoryAfter = Promise.resolve(null);
    const done = new Promise((resolve) => { resolveDone = resolve; });

    // Missing longtask support is reported as an unavailable measurement.
    const supported = Array.isArray(host.PerformanceObserver?.supportedEntryTypes)
      && host.PerformanceObserver.supportedEntryTypes.includes("longtask");
    if (supported && typeof host.PerformanceObserver === "function") {
      observer = new host.PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          longTasks.push({ startTime: entry.startTime, duration: entry.duration });
        }
      });
      observer.observe({ type: "longtask", buffered: true });
    }

    async function finish(reason) {
      if (stopped) return;
      stopped = true;
      if (timer !== undefined) host.clearTimeout(timer);
      observer?.disconnect();
      worker.removeEventListener?.("message", onMessage);
      worker.removeEventListener?.("error", onError);
      progressChannel.port1.removeEventListener?.("message", onWorldgenProgress);
      protocolChannel.port1.close?.();
      progressChannel.port1.close?.();
      horizonChannel.port1.close?.();
      worker.terminate?.();
      const finishedAt = now(host);
      const [beforeBytes, afterBytes] = await Promise.all([memoryBefore, memoryAfter]);
      const maxLongTaskMs = longTasks.reduce(
        (maximum, entry) => Math.max(maximum, entry.duration),
        0,
      );
      resolveDone({
        error,
        finishReason: reason,
        startupMs: readyAt === null ? null : readyAt - startedAt,
        runtimeMs: finishedAt - startedAt,
        memoryApi: beforeBytes !== null || afterBytes !== null,
        startupMemoryBytes: readyAt === null || beforeBytes === null || afterBytes === null
          ? null
          : afterBytes - beforeBytes,
        executor: progress.find((event) => event.executor)?.executor ?? null,
        workers: progress.findLast?.((event) => Number.isInteger(event.workers))?.workers ?? null,
        progress,
        longTaskApi: supported,
        longTasks,
        maxLongTaskMs,
        under100ms: supported && maxLongTaskMs < 100,
      });
    }

    function onMessage(event) {
      const value = event.data;
      if (value?.kind === "progress") {
        progress.push({
          ...value,
          elapsedMs: now(host) - startedAt,
        });
      } else if (value?.kind === "ready") {
        readyAt = now(host);
        memoryAfter = memorySample(host);
        timer = host.setTimeout(() => finish("runtime-complete"), settings.runtimeMs);
      } else if (value?.kind === "error") {
        error = String(value.message ?? "server worker startup failed");
        finish("worker-error");
      }
    }

    function onWorldgenProgress(event) {
      const value = event.data;
      if (value?.kind === "worldgen-progress") {
        progress.push({
          ...value,
          elapsedMs: now(host) - startedAt,
        });
      }
    }

    function onError(event) {
      error = String(event?.message ?? "server worker stopped");
      finish("worker-error");
    }

    worker.addEventListener("message", onMessage);
    worker.addEventListener("error", onError);
    progressChannel.port1.addEventListener?.("message", onWorldgenProgress);
    progressChannel.port1.start?.();
    horizonChannel.port1.start?.();
    try {
      worker.postMessage(
        {
          kind: "start",
          protocol: settings.protocol,
          seed: settings.seed,
          preset: settings.preset,
          epoch: 1,
        },
        [protocolChannel.port2, progressChannel.port2, horizonChannel.port2],
      );
    } catch (caught) {
      error = String(caught?.message ?? caught);
      finish("worker-error");
    }

    return Object.freeze({
      done,
      stop: () => finish("manual-stop"),
    });
  }

  global.LodestoneWorldgenMeasurement = Object.freeze({ measure });
})(globalThis);
