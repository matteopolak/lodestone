import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const source = fs.readFileSync(new URL("./responsiveness_probe.js", import.meta.url), "utf8");
const { ResponsivenessProbe, install } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

test("terrain behind the loading overlay is not a playable readiness signal", () => {
  const probe = new ResponsivenessProbe(() => {});
  for (const phases of [["first-terrain-presented", "loading-overlay-ready"],
    ["loading-overlay-ready", "first-terrain-presented"]]) {
    probe.observe({ kind: "progress", event: { phase: "world-create-started" } });
    assert.equal(probe.playable, false);
    probe.observe({ kind: "progress", event: { phase: phases[0] } });
    assert.equal(probe.playable, false);
    probe.observe({ kind: "progress", event: { phase: phases[1] } });
    assert.equal(probe.playable, true);
  }
  probe.observe({ kind: "progress", event: { phase: "world-open-started" } });
  assert.equal(probe.playable, false);
});

test("join milestones survive diagnostic churn and reset without invented phases", () => {
  let now = 0;
  const probe = new ResponsivenessProbe(() => {}, () => now++);
  probe.observe({ kind: "progress", event: {
    phase: "first-terrain-presented", elapsedMs: 20,
  } });
  assert.equal(probe.joinReport, null);
  assert.equal(probe.playable, false);

  const progress = (phase, elapsedMs) => probe.observe({ kind: "progress", event: {
    phase, elapsedMs, loadedColumns: 12, expectedColumns: 16,
    settledColumns: 9, pendingMeshes: 3,
  } });
  progress("world-create-started", 0);
  progress("loading-overlay-ready", 25);
  progress("first-terrain-presented", 31);
  progress("full-view-presented", 44);
  for (let i = 0; i < 400; i++) progress("full-view-presented", 100 + i);
  for (let i = 0; i < 400; i++) {
    probe.observe({ kind: "diagnostic", message: `server health: ticks=${i}` });
  }

  const firstJoin = probe.joinReport;
  assert.equal(firstJoin.milestones.length, 4);
  assert.deepEqual(firstJoin.milestones.map(row => row.phase), [
    "world-create-started", "loading-overlay-ready", "first-terrain-presented", "full-view-presented",
  ]);
  assert.equal(firstJoin.milestones[2].elapsedMs, 31);
  assert.equal(firstJoin.milestones[3].elapsedMs, 44);
  assert.equal(firstJoin.milestones[2].loadedColumns, 12);
  assert.equal(firstJoin.milestones.some(row => row.phase === "world-open-started"), false);
  assert.equal(probe.playable, true);

  progress("world-open-started", 0);
  assert.equal(probe.playable, false);
  assert.equal(probe.joinReport.sequence, firstJoin.sequence + 1);
  assert.deepEqual(probe.joinReport.milestones.map(row => row.phase), ["world-open-started"]);
  progress("full-view-presented", 55);
  progress("first-terrain-presented", 51);
  assert.equal(probe.playable, false);
  assert.deepEqual(probe.joinReport.milestones.map(row => row.phase), [
    "world-open-started", "full-view-presented", "first-terrain-presented",
  ]);
  assert.equal(probe.joinReport.milestones.some(row => row.phase === "loading-overlay-ready"), false);

  probe.start("walk");
  const report = probe.stop();
  assert.deepEqual(report.join, probe.joinReport);
  assert.equal(report.join.milestones.length, 3);
});

test("the opt-in DOM report keeps completed probe data through same-join progress", t => {
  const previousGlobals = ["location", "document", "window"].map(name => [
    name, Object.getOwnPropertyDescriptor(globalThis, name),
  ]);
  const nodes = [];
  const document = {
    body: { append() {} },
    createElement(tagName) {
      const node = {
        tagName,
        style: {},
        children: [],
        append(...children) { this.children.push(...children); },
      };
      nodes.push(node);
      return node;
    },
  };
  Object.defineProperties(globalThis, {
    location: { configurable: true, value: { search: "?probe=1" } },
    document: { configurable: true, value: document },
    window: { configurable: true, value: { addEventListener() {} } },
  });
  t.after(() => {
    for (const [name, descriptor] of previousGlobals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete globalThis[name];
    }
  });

  let onMessage;
  const worker = {
    postMessage() {},
    addEventListener(type, listener) { if (type === "message") onMessage = listener; },
  };
  install(worker, { focus() {} });
  const sendProgress = phase => onMessage({ data: { kind: "progress", event: {
    phase, elapsedMs: 20, loadedColumns: 4, expectedColumns: 8,
    settledColumns: 3, pendingMeshes: 1,
  } } });
  sendProgress("world-create-started");
  sendProgress("loading-overlay-ready");
  sendProgress("first-terrain-presented");

  const walk = nodes.find(node => node.tagName === "button" && node.textContent === "Walk 20s");
  const stop = nodes.find(node => node.tagName === "button" && node.textContent === "Stop probe");
  const reportNode = nodes.find(node => node.id === "lodestone-responsiveness-report");
  walk.onclick();
  stop.onclick();
  const completed = JSON.parse(reportNode.textContent);
  assert.equal(completed.mode, "walk");
  assert.equal(completed.reason, "stopped");

  sendProgress("full-view-presented");
  sendProgress("joining");
  const sameJoin = JSON.parse(reportNode.textContent);
  assert.equal(sameJoin.mode, "walk");
  assert.deepEqual(sameJoin.join.milestones.map(row => row.phase), [
    "world-create-started", "loading-overlay-ready", "first-terrain-presented", "full-view-presented",
  ]);

  sendProgress("world-open-started");
  const reset = JSON.parse(reportNode.textContent);
  assert.equal(reset.mode, undefined);
  assert.deepEqual(reset.join.milestones.map(row => row.phase), ["world-open-started"]);
  sendProgress("full-view-presented");
  assert.equal(JSON.parse(reportNode.textContent).mode, undefined);
});

test("held normal inputs are released and diagnostics are bounded", () => {
  const sent = [];
  let now = 17;
  const probe = new ResponsivenessProbe(input => sent.push(input), () => now);
  probe.observe({ kind: "diagnostic", message: "connection: center=0,0" });
  probe.start("walk");
  assert.deepEqual(sent.filter(input => input.type === "key").map(input => [input.code, input.pressed]),
    [["ControlLeft", true], ["Space", true], ["KeyW", true]]);
  assert.throws(() => probe.start("mine"), /already running/);
  for (let i = 0; i < 260; i++) {
    now++;
    probe.observe({ kind: "diagnostic", message: `server health: ticks=${i}` });
  }
  now = 20017;
  const report = probe.stop();
  assert.equal(report.durationMs, 20000);
  assert.equal(report.baseline.length, 1);
  assert.equal(report.samplesSeen, 260);
  assert.equal(report.samples.length, 256);
  assert.equal(report.samplesTruncated, true);
  assert.equal(report.final.at(-1).message, "server health: ticks=259");
  assert.equal(report.final.length, 2);
  assert.deepEqual(sent.slice(-5), [
    { type: "key", code: "KeyW", pressed: false, modifiers: 0 },
    { type: "key", code: "ControlLeft", pressed: false, modifiers: 0 },
    { type: "key", code: "Space", pressed: false, modifiers: 0 },
    { type: "mouseButton", button: 0, pressed: false },
    { type: "pointerLock", locked: false },
  ]);
  assert.equal(probe.stop(), null);
});

test("mining uses the host mouse-button path rather than modifying a world", () => {
  const sent = [];
  const probe = new ResponsivenessProbe(input => sent.push(input), () => 0);
  probe.start("mine");
  assert.deepEqual(sent.at(-1), { type: "mouseButton", button: 0, pressed: true });
  assert.equal(probe.stop("worker-error").reason, "worker-error");
  assert.throws(() => probe.start("teleport"), /unknown probe mode/);
  sent.length = 0;
  probe.start("mine", true);
  assert.deepEqual(sent.slice(-2), [
    { type: "mouseMotion", dx: 0, dy: 600 },
    { type: "mouseButton", button: 0, pressed: true },
  ]);
  probe.stop();
});

test("phase totals and latest rows survive raw sample truncation", () => {
  const probe = new ResponsivenessProbe(() => {}, () => 0);
  probe.start("walk");
  for (let i = 0; i < 260; i++) {
    probe.observe({ kind: "diagnostic", message:
      "worldgen timing: phase=browser-yield calls=2 items=3 sum_ms=7.125 max_ms=3.500" });
  }
  probe.observe({ kind: "diagnostic", message:
    "worldgen timing: phase=mutable-settlement calls=1 items=9 sum_ms=2.750 max_ms=2.750" });
  const report = probe.stop();
  assert.equal(report.samplesTruncated, true);
  assert.equal(report.samplesSeen, 261);
  assert.equal(report.final.length, 2);
  assert.deepEqual(report.generationPhases, [
    { phase: "browser-yield", calls: 520, items: 780, elapsedMs: 1852.5, maxMs: 3.5 },
    { phase: "mutable-settlement", calls: 1, items: 9, elapsedMs: 2.75, maxMs: 2.75 },
  ]);
});
