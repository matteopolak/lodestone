# Lodestone time

## What it is

`lodestone-time` is the single clock and browser-timer boundary used by code
that can ship to WebAssembly. It keeps elapsed-time deadlines live in a browser
without making native callers pay for a wrapper or a second clock implementation.

## How it works

`Instant` comes from `web-time`: native builds use the standard monotonic clock,
while WebAssembly uses the host's monotonic `performance.now()` clock.
`epoch_duration` is reserved for values that genuinely need wall time and uses
the host's `Date.now()` on WebAssembly. Browser waits use a cancel-safe
`setTimeout` future that works from both a page and a worker global.

Client and server deadlines race their work against that future on WebAssembly;
native code keeps using the Tokio timer driver. This means a browser timeout is
an actual deadline rather than an ignored option or a poll of a runtime clock
that has no browser driver.

`browser_yield` cooperates without requesting a deadline. It lazily opens one
`MessageChannel` per realm and resolves pending continuations in message order;
older hosts use a zero-delay timer. This avoids repeated timer scheduling cost
while sharing the host's message-task queue with worker communication. A resolved
Promise is not a substitute: microtasks can starve timers and messages. The host
controls scheduling, so this is not a maximum-latency guarantee.

Use `browser_sleep` for actual deadlines and periodic capacity polling. Polling
with the faster cooperation primitive can repeatedly check an unavailable slot
instead of waiting for useful work. Clock and tick deadlines are unchanged.

## How to change it

Add clock or timer primitives here before adding another direct JavaScript clock
call elsewhere. Keep monotonic measurements on `Instant`; use
`epoch_duration` only for timestamps, expiry, or intentionally wall-clock UI.
Run `cargo check -p lodestone-time --tests` and the wasm target equivalent after
changing the public surface.
Run `node --test crates/lodestone-time/tests/browser-yield.test.mjs` for the
FIFO message-task and timer-fallback controls. The local JavaScript module
must be included in wasm-bindgen's staged snippets, including worker artifacts.

## Configuration

There is no runtime configuration. The browser host must provide
`performance.now`, `Date.now`, and `setTimeout` on its active global.
`MessageChannel` is optional and detected once per realm.

## Dependencies

The crate depends on `web-time` for portable clock types. Its WebAssembly timer
uses `js-sys`, `wasm-bindgen`, and `wasm-bindgen-futures`; no Tokio runtime is
required for browser deadlines.
