import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./worker_bootstrap.js", import.meta.url), "utf8");
const context = vm.createContext({});
vm.runInContext(source, context, { filename: "worker_bootstrap.js" });
const { invalidLaunch, launch } = context.LodestoneWorkerBootstrap;

const launchRequest = Object.freeze({
  kind: "start",
  protocol: 776,
  seed: "-42",
  preset: 0,
});

// Messages originate in the isolated `vm` realm, whereas the expected values
// belong to Node's test realm. Round-trip only the JSON-shaped control payload
// before comparison so this asserts values rather than prototype identity.
const controlMessages = (messages) => JSON.parse(JSON.stringify(messages));

test("validates the complete worker launch envelope before importing wasm", () => {
  assert.equal(invalidLaunch(launchRequest, [{}]), null);
  assert.equal(invalidLaunch({ ...launchRequest, protocol: 0 }, [{}]), "invalid server worker protocol");
  assert.equal(invalidLaunch({ ...launchRequest, seed: "42.5" }, [{}]), "invalid server worker seed");
  assert.equal(invalidLaunch({ ...launchRequest, preset: 7 }, [{}]), "invalid server worker world preset");
  assert.equal(invalidLaunch(launchRequest, []), "invalid server worker launch");
});

test("reports ordered, real startup milestones and transfers only the supplied port", async () => {
  const port = { name: "server-port" };
  const received = [];
  const started = [];
  let initialized = 0;
  await launch(
    { data: launchRequest, ports: [port] },
    async () => ({
      default: async () => { initialized += 1; },
      start_worker: (...args) => started.push(args),
    }),
    (message) => received.push(message),
  );

  assert.equal(initialized, 1);
  assert.deepEqual(controlMessages(received), [
    { kind: "progress", stage: "loading-module" },
    { kind: "progress", stage: "starting-server" },
    { kind: "ready" },
  ]);
  assert.deepEqual(started, [[port, 776, -42n, 0]]);
});

test("does not import wasm after a malformed launch and makes failure observable", async () => {
  const received = [];
  let imported = false;
  await launch(
    { data: { kind: "start", protocol: 776, seed: "not-a-seed", preset: 0 }, ports: [{}] },
    async () => { imported = true; },
    (message) => received.push(message),
  );
  assert.equal(imported, false);
  assert.deepEqual(controlMessages(received), [{ kind: "error", message: "invalid server worker seed" }]);
});

test("reports a wasm startup failure instead of claiming a ready server", async () => {
  const received = [];
  await launch(
    { data: launchRequest, ports: [{}] },
    async () => ({
      default: async () => {},
      start_worker: () => { throw new Error("world source failed"); },
    }),
    (message) => received.push(message),
  );
  assert.deepEqual(controlMessages(received), [
    { kind: "progress", stage: "loading-module" },
    { kind: "progress", stage: "starting-server" },
    { kind: "error", message: "Error: world source failed" },
  ]);
});
