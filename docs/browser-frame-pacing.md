# Browser frame pacing

## What it is

The browser runner presents through the existing `WindowApp` pipeline on host animation opportunities, while a separate service watchdog keeps client simulation, input, network polling and diagnostic controls running when animation callbacks are suspended.

## How it works

- `app::browser_pacing::BrowserFrameHost` belongs to one mount and owns two reusable callback closures, at most one pending animation request, one pending timer and one waiting-task waker. Callbacks only coalesce ready bits; the async runner is the sole consumer and sole owner of updates and rendering, so there is no second simulation or renderer. Weak callback references avoid an ownership cycle.
- It picks Window or dedicated-worker `requestAnimationFrame` on the actual canvas owner. Request and cancel APIs must both exist and registration is fallible; missing APIs or a throwing registration select deadline fallback once. A suspended but supported animation clock does not. Mode and reason go through the browser diagnostic bridge.
- A service wake keeps the pending animation request (cancel-and-restart every 8 ms would starve a 16.7 ms callback). After an animation callback the runner rearms before expensive drawing; opportunities coalesce and never replay as a catch-up queue. When the cap is not due, an animation wake can rearm without a full client update.
- `FramePacer::begin_frame_with_opportunity` always advances the client clock but checks the opportunity before moving the presentation deadline, so service-only updates do not spend the deadline, acquire a surface or initialise deferred renderer assets. Menu and world presentation share the redraw body and the successful-submission capture markers.
- `FramePacer::background_deadline` requests servicing at most 8 ms after the last update; input and control queue additions wake the runner immediately. Hidden presentation is suppressed through the separate visibility input; a visible unfocused canvas presents at no more than 30 FPS. Browser timer throttling can delay the whole worker, so these are requests, and bounded simulation catch-up applies on resumption.
- Without animation support one timer waits for the earlier of client service and the shared absolute presentation deadline. Positive fractional milliseconds round up, an already-due deadline yields asynchronously for at least 1 ms, and missed deadlines rebase without a burst. Explicit caps are not clamped to 60; only the focused uncapped fallback uses a conservative 60 Hz yield cadence, which is not a refresh measurement.
- Shutdown closes the ready state, cancels registrations and wakes the runner even with a suspended callback; the runner tears down GPU presentation before the lifecycle signal releases the mount. Closed-state callbacks make no work for that mount or its replacement, and the last owner drops the closures. Cancellation errors are reported, and a failed cancel blocks replacement registrations during ordinary servicing.

## How to change it

- FPS, focus and AFK policy belong in `app::pacing`, not the host; native `begin_frame` stays the true-opportunity wrapper. Keep simulation and control work on the single runner; never run it in the animation callback or make animation availability a precondition.
- Host registration and cancellation live in `app::browser_pacing`; preserve the persistent pending handle across watchdog wakes, pre-work rearming, coalescing and the async due-deadline yield. Visibility rides the SDK/render-worker input bridge separately from focus. Mount waiting uses host timers, not the animation clock.
- Injected-clock tests separate a service update from a post-hoc render mask, check fractional rounding and stalled-fallback rebasing, and reject ready sources after shutdown. They do not prove live callback timing. Live acceptance needs suspended-animation input and network service, shutdown and remount, missing and throwing animation fallback, and no cancel-the-loser starvation.
- For performance comparisons match scene, cap, viewport and DPR, seed, view distance, actions, focus and visibility, starting at the same 60 FPS cap. [presentation-capture](presentation-capture.md) measures surface handoff, not compositor delivery; reject truncated captures for full-trial percentiles. Watch service-only attempt counts and CPU as well as submission intervals.

## Configuration

Framerate, inactivity and focus policies are shared with native pacing. `BACKGROUND_POLL` is the 8 ms service request; `UNLIMITED_FALLBACK_INTERVAL` only sets uncapped fallback cadence. Once-per-second diagnostics report animation, timer and control callback counts and maximum timer lateness (counts are not FPS). WebGPU currently advertises Fifo, and the persisted vsync flag does not make browser display unsynchronised.

## Dependencies

Portable `Instant`, `wasm-bindgen-futures`, `js-sys`, `web-sys` (0.3.103 gives caught dedicated-worker animation request and cancel bindings; the shell enables `DedicatedWorkerGlobalScope`). No new crate or unstable flag.
