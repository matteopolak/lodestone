import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const source = fs.readFileSync(new URL("../src/browser-yield.js", import.meta.url), "utf8");
const { createHostYielder } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

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
