import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { validateThreadedWorkerArtifacts } from "../scripts/verify_threaded_worker.mjs";

const source = fs.readFileSync(new URL("./worker_bootstrap.js", import.meta.url), "utf8");
const bootstrap = () => {
  const context = vm.createContext({});
  vm.runInContext(source, context, { filename: "worker_bootstrap.js" });
  return context;
};
const context = bootstrap();
const { invalidLaunch } = context.LodestoneWorkerBootstrap;

class ProbeMessageChannel {
  constructor() {
    this.port1 = { postMessage() {}, close() {} };
    this.port2 = { close() {} };
  }
}

const launchRequest = Object.freeze({
  kind: "start",
  protocol: 776,
  seed: "-42",
  preset: 0,
  epoch: 1,
  logLevel: "debug",
});

// Messages originate in the isolated `vm` realm, whereas the expected values
// belong to Node's test realm. Round-trip only the JSON-shaped control payload
// before comparison so this asserts values rather than prototype identity.
const controlMessages = (messages) => JSON.parse(JSON.stringify(messages));

test("validates the complete worker launch envelope before importing wasm", () => {
  assert.equal(invalidLaunch(launchRequest, [{}, {}, {}]), null);
  assert.equal(invalidLaunch({ ...launchRequest, protocol: 0 }, [{}, {}, {}]), "invalid server worker protocol");
  assert.equal(invalidLaunch({ ...launchRequest, seed: "42.5" }, [{}, {}, {}]), "invalid server worker seed");
  assert.equal(invalidLaunch({ ...launchRequest, preset: 7 }, [{}, {}, {}]), "invalid server worker world preset");
  assert.equal(invalidLaunch({ ...launchRequest, logLevel: "verbose" }, [{}, {}, {}]), "invalid server worker log level");
  assert.equal(invalidLaunch(launchRequest, [{}, {}]), "invalid server worker launch");
  assert.equal(invalidLaunch(launchRequest, []), "invalid server worker launch");
});

test("selects a bounded pool only when isolation and Wasm atomics are real", () => {
  const supportedHost = {
    crossOriginIsolated: true,
    SharedArrayBuffer,
    Atomics,
    WebAssembly,
    MessageChannel: ProbeMessageChannel,
    navigator: { hardwareConcurrency: 12 },
  };
  assert.deepEqual(
    controlMessages([context.LodestoneWorkerBootstrap.threadSupport(supportedHost)])[0],
    { available: true, reason: "available" },
  );
  assert.equal(context.LodestoneWorkerBootstrap.poolWidth(supportedHost), 4);
  assert.equal(
    context.LodestoneWorkerBootstrap.poolWidth({ ...supportedHost, navigator: { hardwareConcurrency: 2 } }),
    1,
  );
  assert.deepEqual(
    controlMessages([context.LodestoneWorkerBootstrap.threadSupport({ ...supportedHost, crossOriginIsolated: false })])[0],
    { available: false, reason: "cross-origin-isolation-required" },
  );
  class RejectingMessageChannel extends ProbeMessageChannel {
    constructor() {
      super();
      this.port1.postMessage = () => { throw new Error("memory cannot be cloned"); };
    }
  }
  assert.deepEqual(
    controlMessages([context.LodestoneWorkerBootstrap.threadSupport({
      ...supportedHost,
      MessageChannel: RejectingMessageChannel,
    })])[0],
    { available: false, reason: "shared-memory-clone-unavailable" },
  );
});

test("reports ordered startup milestones and passes all three supplied ports", async () => {
  const serverPort = { name: "server-port" };
  const progressPort = { name: "progress-port" };
  const horizonPort = { name: "horizon-port" };
  const { launch } = bootstrap().LodestoneWorkerBootstrap;
  const received = [];
  const started = [];
  let initialized = 0;
  await launch(
    { data: launchRequest, ports: [serverPort, progressPort, horizonPort] },
    async () => ({
      default: async () => { initialized += 1; },
      start_worker: (...args) => started.push(args),
    }),
    (message) => received.push(message),
  );

  assert.equal(initialized, 1);
  assert.deepEqual(controlMessages(received), [
    { kind: "progress", stage: "loading-module" },
    {
      kind: "progress",
      stage: "selecting-serial-fallback",
      workers: 1,
      reason: "cross-origin-isolation-required",
    },
    { kind: "progress", stage: "starting-server" },
    { kind: "progress", stage: "preparing-world", executor: "serial", workers: 1 },
    { kind: "ready" },
  ]);
  assert.deepEqual(started, [[serverPort, progressPort, horizonPort, 776, -42n, 0, 1, "debug"]]);
});

test("does not import wasm after a malformed launch and makes failure observable", async () => {
  const { launch } = bootstrap().LodestoneWorkerBootstrap;
  const received = [];
  let imported = false;
  await launch(
    { data: { ...launchRequest, seed: "not-a-seed" }, ports: [{}, {}, {}] },
    async () => { imported = true; },
    (message) => received.push(message),
  );
  assert.equal(imported, false);
  assert.deepEqual(controlMessages(received), [{ kind: "error", message: "invalid server worker seed" }]);
});

test("reports a wasm startup failure instead of claiming a ready server", async () => {
  const { launch } = bootstrap().LodestoneWorkerBootstrap;
  const received = [];
  let horizonClosed = 0;
  const horizonPort = { close: () => { horizonClosed += 1; } };
  await launch(
    { data: launchRequest, ports: [{}, {}, horizonPort] },
    async () => ({
      default: async () => {},
      start_worker: () => { throw new Error("world source failed"); },
    }),
    (message) => received.push(message),
  );
  assert.deepEqual(controlMessages(received), [
    { kind: "progress", stage: "loading-module" },
    {
      kind: "progress",
      stage: "selecting-serial-fallback",
      workers: 1,
      reason: "cross-origin-isolation-required",
    },
    { kind: "progress", stage: "starting-server" },
    { kind: "progress", stage: "preparing-world", executor: "serial", workers: 1 },
    { kind: "error", message: "Error: world source failed" },
  ]);
  assert.equal(horizonClosed, 1);
});

test("initializes the threaded artifact before entering the authoritative server", async () => {
  const port = { name: "server-port" };
  const progressPort = { name: "progress-port" };
  const horizonPort = { name: "horizon-port" };
  const { launch } = bootstrap().LodestoneWorkerBootstrap;
  const received = [];
  const loaded = [];
  let initialized = 0;
  const supportedHost = {
    crossOriginIsolated: true,
    SharedArrayBuffer,
    Atomics,
    WebAssembly,
    MessageChannel: ProbeMessageChannel,
    navigator: { hardwareConcurrency: 6 },
  };
  await launch(
    { data: launchRequest, ports: [port, progressPort, horizonPort] },
    async (mode) => {
      loaded.push(mode);
      return {
        default: async () => { initialized += 1; },
        initThreadPool: async (workers) => {
          assert.equal(workers, 4);
        },
        start_worker: () => {},
      };
    },
    (message) => received.push(message),
    supportedHost,
  );
  assert.deepEqual(loaded, ["threaded"]);
  assert.equal(initialized, 1);
  assert.deepEqual(controlMessages(received), [
    { kind: "progress", stage: "loading-module" },
    { kind: "progress", stage: "selecting-threaded-pool", workers: 4 },
    { kind: "progress", stage: "starting-compute-pool", workers: 4 },
    { kind: "progress", stage: "compute-pool-ready", workers: 4 },
    { kind: "progress", stage: "starting-server" },
    { kind: "progress", stage: "preparing-world", executor: "threaded", workers: 4 },
    { kind: "ready" },
  ]);
});

test("fails when threaded pool startup is rejected after capability detection", async () => {
  const { launch } = bootstrap().LodestoneWorkerBootstrap;
  const received = [];
  const loaded = [];
  const supportedHost = {
    crossOriginIsolated: true,
    SharedArrayBuffer,
    Atomics,
    WebAssembly,
    MessageChannel: ProbeMessageChannel,
    navigator: { hardwareConcurrency: 4 },
  };
  await launch(
    { data: launchRequest, ports: [{}, {}, {}] },
    async (mode) => {
      loaded.push(mode);
      return {
        default: async () => {},
        initThreadPool: async () => { throw new Error("worker blocked by policy"); },
        start_worker: () => {},
      };
    },
    (message) => received.push(message),
    supportedHost,
  );
  assert.deepEqual(loaded, ["threaded"]);
  assert.deepEqual(controlMessages(received), [
    { kind: "progress", stage: "loading-module" },
    { kind: "progress", stage: "selecting-threaded-pool", workers: 3 },
    { kind: "progress", stage: "starting-compute-pool", workers: 3 },
    {
      kind: "progress",
      stage: "compute-pool-error",
      reason: "Error: worker blocked by policy",
      workers: 3,
    },
    { kind: "error", message: "Error: worker blocked by policy" },
  ]);
});

test("routes cancellation and worldgen counters through the progress port", async () => {
  const context = bootstrap();
  const { launch, cancel } = context.LodestoneWorkerBootstrap;
  const progress = [];
  const progressPort = { postMessage: (message) => progress.push(message) };
  const received = [];
  await launch(
    { data: launchRequest, ports: [{}, progressPort, {}] },
    async () => ({
      default: async () => {},
      cancel_worker: (epoch) => epoch === 1,
      start_worker: () => {},
    }),
    (message) => received.push(message),
  );
  cancel({ data: { kind: "cancel", epoch: 1 } }, (message) => received.push(message));
  assert.equal(controlMessages(received).at(-1).kind, "ready");
  assert.equal(progress.at(-1).stage, "cancelled");
  assert.equal(progress.at(-1).epoch, 1);
});

test("staging keeps serial and threaded artifacts separate", () => {
  const staging = fs.readFileSync(new URL("../scripts/stage_worker.sh", import.meta.url), "utf8");
  assert.match(staging, /lodestone-server-worker-wasm-serial/);
  assert.match(staging, /lodestone-server-worker-wasm-threaded/);
  assert.match(staging, /--features wasm-threads/);
  assert.match(staging, /target-feature=\+atomics,\+bulk-memory/);
  for (const flag of [
    "--shared-memory",
    "--max-memory=1073741824",
    "--import-memory",
    "--export=__wasm_init_tls",
    "--export=__tls_size",
    "--export=__tls_align",
    "--export=__tls_base",
  ]) {
    assert.match(staging, new RegExp(`link-arg=${flag.replaceAll("+", "\\\\+")}`));
  }
  assert.match(staging, /workerHelpers\.no-bundler\.js/);
  assert.match(staging, /patch_threaded_worker_helper\.mjs/);
  assert.doesNotMatch(fs.readFileSync(new URL("./worker.js", import.meta.url), "utf8"), /new Worker/);
});

test("patches the rayon child-worker initialization to the object API", () => {
  const patcher = fs.readFileSync(new URL("../scripts/patch_threaded_worker_helper.mjs", import.meta.url), "utf8");
  assert.match(patcher, /data\.module, data\.memory/);
  assert.match(patcher, /module_or_path: data\.module, memory: data\.memory/);
});

function memoryModule({ defined = false, imports = [], exports = [] } = {}) {
  const header = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
  const section = (id, payload) => [id, payload.length, ...payload];
  const importEntry = (shared) => [3, 101, 110, 118, 6, 109, 101, 109, 111, 114, 121, 2, shared ? 3 : 1, 1, 1];
  const exportEntry = (name = "memory", index = 0) => [name.length, ...name.split("").map((char) => char.charCodeAt(0)), 2, index];
  const importPayload = [imports.length, ...imports.flatMap((shared) => importEntry(shared))];
  const exportPayload = [exports.length, ...exports.flatMap(({ name, index }) => exportEntry(name, index))];
  return Uint8Array.from([
    ...header,
    ...(defined ? section(5, [1, 1, 1, 1]) : []),
    ...(imports.length ? section(2, importPayload) : []),
    ...(exports.length ? section(7, exportPayload) : []),
  ]);
}

test("accepts the shared imported and exported memory contract", () => {
  const glue = "function __wbg_get_imports(memory) { return { memory: memory || new WebAssembly.Memory({initial:1,maximum:1,shared:true}) }; } async function __wbg_init(input, memory) {}";
  assert.equal(validateThreadedWorkerArtifacts(glue, memoryModule({ imports: [true], exports: [{}] })).name, "memory");
});

test("rejects unshared, multiple, or non-imported memories", () => {
  const glue = "function __wbg_get_imports(memory) { return { memory: memory || new WebAssembly.Memory({initial:1,maximum:1,shared:true}) }; } async function __wbg_init(input, memory) {}";
  assert.throws(() => validateThreadedWorkerArtifacts(glue, memoryModule({ imports: [false], exports: [{}] })), /memory import must be shared/);
  assert.throws(() => validateThreadedWorkerArtifacts(glue, memoryModule({ imports: [true, true], exports: [{}] })), /import exactly one memory/);
  assert.throws(() => validateThreadedWorkerArtifacts(glue, memoryModule({ defined: true, exports: [{}] })), /import exactly one memory/);
});

test("rejects multiple or non-memory exports", () => {
  const glue = "function __wbg_get_imports(memory) { return { memory: memory || new WebAssembly.Memory({initial:1,maximum:1,shared:true}) }; } async function __wbg_init(input, memory) {}";
  assert.throws(() => validateThreadedWorkerArtifacts(glue, memoryModule({ imports: [true], exports: [{}, { name: "other" }] })), /export its imported memory/);
  assert.throws(() => validateThreadedWorkerArtifacts(glue, memoryModule({ imports: [true], exports: [{ name: "other" }] })), /export its imported memory/);
});
