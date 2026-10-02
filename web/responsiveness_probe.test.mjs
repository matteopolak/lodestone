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

test("Creative readiness uses presented gameplay rather than a nonexistent Survival cover", () => {
  const probe = new ResponsivenessProbe(() => {});
  const progress = phase => probe.observe({ kind: "progress", event: { phase } });
  progress("world-create-started");
  progress("full-view-quiescent");
  assert.equal(probe.playable, false);
  progress("gameplay-ready");
  assert.equal(probe.playable, false);
  progress("first-terrain-presented");
  assert.equal(probe.playable, true);
  assert.equal(probe.overlayReady, false);
  progress("world-open-started");
  progress("first-terrain-presented");
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
  assert.equal(probe.joinReport.runtime, null);
  probe.observe({ kind: "diagnostic", message:
    "server startup: phase=preparing-world executor=threaded workers=4" });
  assert.deepEqual(probe.joinReport.runtime, { executor: "threaded", workers: 4 });
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
  assert.equal(probe.joinReport.runtime, null);
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

test("quiescent is retained once after full presentation with pending work and resets on new join", () => {
  const probe = new ResponsivenessProbe(() => {}, () => 100);
  const progress = (phase, elapsedMs, pendingMeshes, pendingLightRemeshes) => probe.observe({
    kind: "progress", event: {
      phase, elapsedMs, loadedColumns: 25, expectedColumns: 25,
      settledColumns: 25, pendingMeshes, pendingLightRemeshes,
    },
  });
  progress("world-create-started", 0, 0, 0);
  progress("loading-overlay-ready", 13_950, 910, 11);
  progress("first-terrain-presented", 13_980, 909, 10);
  progress("full-view-presented", 14_170, 905, 7);
  assert.equal(probe.playable, true);
  assert.equal(probe.joinReport.milestones.some(row => row.phase === "full-view-quiescent"), false);
  const presented = probe.joinReport.milestones.at(-1);
  assert.equal(presented.pendingMeshes, 905);
  assert.equal(presented.pendingLightRemeshes, 7);
  progress("full-view-quiescent", 21_234, 0, 0);
  progress("full-view-quiescent", 23_000, 0, 0);
  const settled = probe.joinReport.milestones.filter(row => row.phase === "full-view-quiescent");
  assert.equal(settled.length, 1);
  assert.equal(settled[0].elapsedMs, 21_234);
  assert.equal(settled[0].pendingMeshes, 0);
  assert.equal(settled[0].pendingLightRemeshes, 0);
  progress("world-open-started", 0, 0, 0);
  assert.equal(probe.playable, false);
  assert.deepEqual(probe.joinReport.milestones.map(row => row.phase), ["world-open-started"]);
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
  const sent = [];
  const worker = {
    postMessage(message) { sent.push(message); },
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
  const trace = nodes.find(node => node.textContent === "Trace blocks: off");
  trace.onclick();
  assert.deepEqual(sent.at(-1), {
    kind: "input", input: { type: "setBlockActionTrace", enabled: true },
  });
  onMessage({ data: { kind: "progress", event: {
    phase: "block-action-trace", message: "id=1 outcome=completed",
  } } });
  assert.equal(JSON.parse(reportNode.textContent).blockActions.rows[0].message,
    "id=1 outcome=completed");
  walk.onclick();
  stop.onclick();
  const completed = JSON.parse(reportNode.textContent);
  assert.equal(completed.mode, "walk");
  assert.equal(completed.reason, "stopped");

  sendProgress("full-view-presented");
  sendProgress("full-view-quiescent");
  sendProgress("joining");
  const sameJoin = JSON.parse(reportNode.textContent);
  assert.equal(sameJoin.mode, "walk");
  assert.equal(sameJoin.blockActions.rows.length, 1);
  assert.deepEqual(sameJoin.join.milestones.map(row => row.phase), [
    "world-create-started", "loading-overlay-ready", "first-terrain-presented", "full-view-presented",
    "full-view-quiescent",
  ]);

  sendProgress("world-open-started");
  const reset = JSON.parse(reportNode.textContent);
  assert.equal(reset.mode, undefined);
  assert.deepEqual(reset.blockActions, { rows: [], droppedRows: 0 });
  assert.deepEqual(reset.join.milestones.map(row => row.phase), ["world-open-started"]);
  sendProgress("full-view-presented");
  assert.equal(JSON.parse(reportNode.textContent).mode, undefined);
  onMessage({ data: { kind: "error", message: "invalid key input" } });
  assert.equal(nodes.some(node => node.textContent === "Worker error: invalid key input"), true);
  trace.onclick();
  assert.deepEqual(sent.at(-1), {
    kind: "input", input: { type: "setBlockActionTrace", enabled: false },
  });
});

test("block action rows survive console churn with bounded retention", () => {
  let now = 11;
  const probe = new ResponsivenessProbe(() => {}, () => now++);
  for (let id = 0; id < 35; id++) {
    probe.observe({ kind: "progress", event: {
      phase: "block-action-trace", message: `id=${id}`,
    } });
    probe.observe({ kind: "diagnostic", message: `server health: ticks=${id}` });
  }
  const report = probe.blockActionReport;
  assert.equal(report.rows.length, 32);
  assert.equal(report.droppedRows, 3);
  assert.equal(report.rows[0].message, "id=3");
  report.rows.length = 0;
  assert.equal(probe.blockActionReport.rows.length, 32);
  probe.observe({ kind: "progress", event: { phase: "world-open-started" } });
  assert.deepEqual(probe.blockActionReport, { rows: [], droppedRows: 0 });
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
  assert.deepEqual(sent.slice(-4), [
    { type: "key", code: "KeyW", pressed: false, modifiers: 0 },
    { type: "key", code: "ControlLeft", pressed: false, modifiers: 0 },
    { type: "key", code: "Space", pressed: false, modifiers: 0 },
    { type: "mouseButton", button: 0, pressed: false },
  ]);
  assert.equal(sent.some(input => input.type === "pointerLock" && !input.locked), false);
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
  probe.aimDown();
  assert.deepEqual(sent.at(-1), { type: "mouseMotion", dx: 0, dy: 600 });
  assert.equal(sent.some(input => input.type === "mouseButton"), false);
  probe.start("mine");
  assert.deepEqual(sent.at(-1), { type: "mouseButton", button: 0, pressed: true });
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

test("action diagnostic maxima and counts continue beyond the raw sample cap", () => {
  const probe = new ResponsivenessProbe(() => {}, () => 0);
  const observe = message => probe.observe({ kind: "diagnostic", message });
  probe.start("walk");
  for (let i = 0; i < 260; i++) {
    observe("view presentation: resident=97 presented=90 expected=100 missing_resident=3 resident_unpresented=7 submitted_meshes=5 pending_light_remeshes=2 pending_columns=1 pending_removals=0");
  }
  observe("view presentation: resident=53 presented=24 expected=100 missing_resident=47 resident_unpresented=29 submitted_meshes=31 pending_light_remeshes=11 pending_columns=13 pending_removals=17");
  observe("view presentation: resident=98 presented=98 expected=100 missing_resident=2 resident_unpresented=0 submitted_meshes=0 pending_light_remeshes=0 pending_columns=0 pending_removals=0");
  observe("server health: ticks=200 overruns=7 mspt_ms=13.25 budget_tps=20.0 observed_tps=Some(18.0) callback_gap_ms=1203.5 wake_p95_ms=Some(17.25) wake_max_ms=Some(89.5) deadline_max_ms=Some(105.75) catch_up_ticks=Some(2.0) recovery_yields=Some(1.0) yield_max_ms=Some(3.0) shed_ticks=Some(0.0)");
  observe("server health: ticks=220 overruns=7 mspt_ms=2.0 callback_gap_ms=1000.0 wake_p95_ms=None wake_max_ms=Some(3.5) deadline_max_ms=None");
  observe("wasm mesh drain and upload profile: frames=60 uploads=12 frame_gap_p95/max_ms=18.25/57.5 drain_p95/max_ms=3.25/9.75 upload_p95/max_ms=2.5/7.25 backlog_ready/waiting/forced/sections_max=11/23/5/61");
  observe("wasm mesh queue: totals_insert/replace/cancel/pop=100/30/4/90 queued_keys=19 high_water_keys_lifetime=200 oldest_wait_ms=123.75 max_pop_wait_lifetime_ms=999.0");
  const report = probe.stop();
  assert.equal(report.samples.length, 256);
  assert.equal(report.samplesSeen, 266);
  assert.equal(report.samplesTruncated, true);
  const metric = (samples, maximum) => ({ samples, maximum });
  assert.deepEqual(report.diagnosticSummary, {
    view: { samples: 262, metrics: {
      missingResident: metric(262, 47), residentUnpresented: metric(262, 29),
      pendingMeshes: metric(262, 31), pendingLightRemeshes: metric(262, 11),
      pendingColumns: metric(262, 13), pendingRemovals: metric(262, 17),
    } },
    server: { samples: 2, metrics: {
      msptMs: metric(2, 13.25), overruns: metric(2, 7), callbackGapMs: metric(2, 1203.5),
      wakeP95Ms: metric(1, 17.25), wakeMaxMs: metric(2, 89.5), deadlineMaxMs: metric(1, 105.75),
    } },
    mesh: { samples: 1, metrics: {
      frameGapP95Ms: metric(1, 18.25), frameGapMaxMs: metric(1, 57.5),
      drainP95Ms: metric(1, 3.25), drainMaxMs: metric(1, 9.75),
      uploadP95Ms: metric(1, 2.5), uploadMaxMs: metric(1, 7.25),
      readyColumns: metric(1, 11), waitingColumns: metric(1, 23),
      forcedColumns: metric(1, 5), pendingSections: metric(1, 61),
    } },
    meshQueue: { samples: 1, metrics: {
      queuedKeys: metric(1, 19), oldestWaitMs: metric(1, 123.75),
    } },
  });
});

test("action summaries exclude old maxima and unavailable or unknown metrics", () => {
  const probe = new ResponsivenessProbe(() => {}, () => 0);
  const observe = message => probe.observe({ kind: "diagnostic", message });
  observe("server health: mspt_ms=999");
  probe.start("walk");
  observe("server health: mspt_ms=87 wake_max_ms=Some(91.0)");
  const previous = probe.stop();
  observe("server health: mspt_ms=1001");
  probe.start("mine");
  observe("server health: mspt_ms=NaN overruns=Infinity wake_p95_ms=None wake_max_ms=Some(-1.0) deadline_max_ms=Some(1e309) unknown=1000");
  observe("view unknown: missing_resident=999");
  observe("server health: mspt_ms=2.75 wake_max_ms=Some(4.25)");
  const report = probe.stop();
  assert.deepEqual(report.diagnosticSummary, { server: { samples: 1, metrics: {
    msptMs: { samples: 1, maximum: 2.75 }, wakeMaxMs: { samples: 1, maximum: 4.25 },
  } } });
  assert.equal(previous.diagnosticSummary.server.metrics.msptMs.maximum, 87);
});
