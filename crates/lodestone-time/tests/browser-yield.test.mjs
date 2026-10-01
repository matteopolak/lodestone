import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { Worker } from "node:worker_threads";

const source = fs.readFileSync(new URL("../src/browser-yield.js", import.meta.url), "utf8");
const moduleUrl = `data:text/javascript;base64,${Buffer.from(source).toString("base64")}`;
const { createHostYielder, observedTimerCallback } = await import(moduleUrl);

test("cooperation lazily queues FIFO host messages before continuing", async () => {
  let complete;
  let channels = 0;
  let messages = 0;
  class MessageChannel {
    constructor() {
      channels++;
      this.port1 = {};
      this.port2 = { postMessage(value) { assert.equal(value, 0); messages++; } };
      complete = () => this.port1.onmessage();
    }
  }
  const host = {
    MessageChannel,
    setTimeout() { assert.fail("timer path selected"); },
    scheduler: { postTask() { assert.fail("scheduler queue selected"); } },
  };
  const yieldTask = createHostYielder(host);
  assert.equal(channels, 0);
  const continued = [];
  const pending = [1, 2, 3].map(index => yieldTask().then(() => continued.push(index)));
  assert.equal(channels, 1);
  assert.equal(messages, 3);
  await Promise.resolve();
  assert.deepEqual(continued, []);
  for (const index of [1, 2, 3]) {
    complete();
    await pending[index - 1];
    assert.deepEqual(continued, [1, 2, 3].slice(0, index));
  }
});

test("unsupported hosts retain a real timer task, not a microtask", async () => {
  let complete;
  const yieldTask = createHostYielder({
    setTimeout(callback, delay) { assert.equal(delay, 0); complete = callback; },
  });
  let continued = false;
  const pending = yieldTask().then(() => { continued = true; });
  await Promise.resolve();
  assert.equal(continued, false);
  complete();
  await pending;
  assert.equal(continued, true);
});

test("callback measurement ends before Promise resumption", async () => {
  let now = 1.75;
  let callback;
  let resumed = false;
  const pending = new Promise(resolve => {
    callback = observedTimerCallback(resolve, { now: () => now });
  }).then(value => { resumed = true; return value; });
  now = 17.625;
  callback();
  now = 113;
  assert.equal(resumed, false);
  assert.equal(await pending, 15.875);
});

test("worker controls separate timer delay from callback-to-resume delay", async () => {
  const worker = new Worker(`
    const { parentPort } = require('node:worker_threads');
    (async () => {
      const { observedTimerCallback } = await import(${JSON.stringify(moduleUrl)});
      const spin = ms => { const end = performance.now() + ms; while (performance.now() < end) {} };
      const measure = async (before, after) => {
        const start = performance.now();
        const callback = await new Promise(resolve => {
          const fire = observedTimerCallback(resolve);
          setTimeout(() => { fire(); spin(after); }, 8);
          spin(before);
        });
        return { callback, gap: performance.now() - start - callback };
      };
      parentPort.postMessage({ timer: await measure(60, 0), resume: await measure(0, 40) });
    })().catch(error => { throw error; });
  `, { eval: true });
  try {
    const result = await new Promise((resolve, reject) => {
      worker.once("message", resolve);
      worker.once("error", reject);
      worker.once("exit", code => { if (code) reject(new Error(`worker exited ${code}`)); });
    });
    assert.ok(result.timer.callback >= 55, JSON.stringify(result));
    assert.ok(result.resume.gap >= 35, JSON.stringify(result));
  } finally {
    await worker.terminate();
  }
});
