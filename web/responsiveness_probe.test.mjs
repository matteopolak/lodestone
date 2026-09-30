import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const source = fs.readFileSync(new URL("./responsiveness_probe.js", import.meta.url), "utf8");
const { ResponsivenessProbe } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

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
});
