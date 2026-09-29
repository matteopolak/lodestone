import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./worldgen_long_task_harness.js", import.meta.url), "utf8");

class FakePort {
  constructor() {
    this.closed = false;
    this.listeners = new Map();
  }

  addEventListener(kind, callback) {
    this.listeners.set(kind, callback);
  }

  removeEventListener(kind) {
    this.listeners.delete(kind);
  }

  dispatchMessage(data) {
    this.listeners.get("message")?.({ data });
  }

  start() {}

  close() {
    this.closed = true;
  }
}

class FakeMessageChannel {
  static channels = [];

  constructor() {
    this.port1 = new FakePort();
    this.port2 = new FakePort();
    FakeMessageChannel.channels.push(this);
  }
}

class FakeWorker {
  static healthEvents = [];

  constructor() {
    this.listeners = new Map();
    this.terminated = false;
  }

  addEventListener(kind, callback) {
    this.listeners.set(kind, callback);
  }

  removeEventListener(kind) {
    this.listeners.delete(kind);
  }

  postMessage(message, transfer) {
    assert.equal(message.kind, "start");
    assert.equal(transfer.length, 3);
    queueMicrotask(() => {
      this.listeners.get("message")?.({ data: { kind: "progress", stage: "compute-pool-ready", workers: 3 } });
      this.listeners.get("message")?.({ data: { kind: "progress", stage: "preparing-world", executor: "threaded", workers: 3 } });
      this.listeners.get("message")?.({ data: { kind: "ready" } });
      for (const event of FakeWorker.healthEvents) {
        FakeMessageChannel.channels[1].port1.dispatchMessage(event);
      }
    });
  }

  terminate() {
    this.terminated = true;
  }
}

class ThrowingWorker extends FakeWorker {
  postMessage() {
    throw new Error("port transfer failed");
  }
}

function resetChannels() {
  FakeMessageChannel.channels = [];
}

function assertAllLocalPortsClosed() {
  assert.equal(FakeMessageChannel.channels.length, 3);
  assert.ok(FakeMessageChannel.channels.every(({ port1 }) => port1.closed));
}

test("treats a two-port launch as an invalid control", () => {
  assert.throws(
    () => new FakeWorker().postMessage({ kind: "start" }, [{}, {}]),
    assert.AssertionError,
  );
});

test("closes every local channel when launch transfer fails", async () => {
  resetChannels();
  let clock = 0;
  const host = vm.createContext({
    Worker: ThrowingWorker,
    MessageChannel: FakeMessageChannel,
    performance: { now: () => { clock += 1; return clock; } },
    setTimeout,
    clearTimeout,
  });
  vm.runInContext(source, host, { filename: "worldgen_long_task_harness.js" });
  const report = await host.LodestoneWorldgenMeasurement.measure({}, host).done;
  assert.equal(report.error, "port transfer failed");
  assert.equal(report.finishReason, "worker-error");
  assertAllLocalPortsClosed();
});

test("records worker milestones and reports page long-task evidence", async () => {
  resetChannels();
  FakeWorker.healthEvents = [
    { kind: "worker-health", epoch: 0, tickCount: 2, callbackGapMs: 4 },
    { kind: "worker-health", epoch: 1, tickCount: 10, callbackGapMs: 1_108 },
    { kind: "worker-health", epoch: 1, tickCount: 17, callbackGapMs: 1_003 },
  ];
  const observed = [];
  class FakePerformanceObserver {
    static supportedEntryTypes = ["longtask"];

    constructor(callback) {
      this.callback = callback;
    }

    observe(options) {
      observed.push(options);
      queueMicrotask(() => this.callback({
        getEntries: () => [{ startTime: 12, duration: 17 }],
      }));
    }

    disconnect() {}
  }

  let clock = 0;
  const host = vm.createContext({
    Worker: FakeWorker,
    MessageChannel: FakeMessageChannel,
    PerformanceObserver: FakePerformanceObserver,
    performance: { now: () => { clock += 1; return clock; } },
    setTimeout,
    clearTimeout,
  });
  vm.runInContext(source, host, { filename: "worldgen_long_task_harness.js" });
  const measurement = host.LodestoneWorldgenMeasurement.measure({ runtimeMs: 0 }, host);
  const report = await measurement.done;
  assert.deepEqual(JSON.parse(JSON.stringify(observed)), [{ type: "longtask", buffered: true }]);
  assert.equal(report.error, null);
  assert.equal(report.executor, "threaded");
  assert.equal(report.workers, 3);
  assert.equal(report.longTaskApi, true);
  assert.equal(report.maxLongTaskMs, 17);
  assert.equal(report.under100ms, true);
  assert.equal(report.memoryApi, false);
  assert.equal(report.startupMemoryBytes, null);
  assert.equal(report.workerHealthSampleCount, 2);
  assert.equal(report.maxCallbackGapMs, 1_108);
  assert.equal(report.tickAdvancement, 7);
  assert.deepEqual(JSON.parse(JSON.stringify(report.workerHealth.map((event) => event.epoch))), [1, 1]);
  assert.equal(report.finishReason, "runtime-complete");
  assertAllLocalPortsClosed();
  FakeWorker.healthEvents = [];
});

test("does not call an empty long-task log a passing result", async () => {
  resetChannels();
  FakeWorker.healthEvents = [];
  class NoLongTaskObserver {
    static supportedEntryTypes = [];
  }
  let clock = 0;
  const host = vm.createContext({
    Worker: FakeWorker,
    MessageChannel: FakeMessageChannel,
    PerformanceObserver: NoLongTaskObserver,
    performance: { now: () => { clock += 1; return clock; } },
    setTimeout,
    clearTimeout,
  });
  vm.runInContext(source, host, { filename: "worldgen_long_task_harness.js" });
  const report = await host.LodestoneWorldgenMeasurement.measure({ runtimeMs: 0 }, host).done;
  assert.equal(report.longTaskApi, false);
  assert.equal(report.under100ms, false);
  assert.equal(report.memoryApi, false);
  assert.equal(report.startupMemoryBytes, null);
  assert.deepEqual(JSON.parse(JSON.stringify(report.longTasks)), []);
  assert.equal(report.workerHealthSampleCount, 0);
  assert.equal(report.maxCallbackGapMs, null);
  assert.equal(report.tickAdvancement, null);
  assertAllLocalPortsClosed();
});

test("reports startup memory delta when the browser exposes it", async () => {
  resetChannels();
  let clock = 0;
  const samples = [100, 160];
  const host = vm.createContext({
    Worker: FakeWorker,
    MessageChannel: FakeMessageChannel,
    performance: {
      now: () => { clock += 1; return clock; },
      measureUserAgentSpecificMemory: () => Promise.resolve({ bytes: samples.shift() }),
    },
    setTimeout,
    clearTimeout,
  });
  vm.runInContext(source, host, { filename: "worldgen_long_task_harness.js" });
  const report = await host.LodestoneWorldgenMeasurement.measure({ runtimeMs: 0 }, host).done;
  assert.equal(report.memoryApi, true);
  assert.equal(report.startupMemoryBytes, 60);
  assertAllLocalPortsClosed();
});
