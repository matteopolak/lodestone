# Browser frame pacing

## What it is

The browser runner uses host animation opportunities to present through the existing
`WindowApp` pipeline. A separate service watchdog keeps client simulation, input,
network polling and diagnostic controls active when animation callbacks are suspended.

## How it works

`app::browser_pacing::BrowserFrameHost` belongs to one mount. It owns two reusable
callback closures, at most one pending animation request, one pending timer and one
waiting-task waker. Callbacks only coalesce ready bits; the async runner is the sole
consumer and the sole owner of client updates and rendering. Weak callback references
avoid an ownership cycle. There is no second simulation or renderer.

The host selects Window or dedicated-worker `requestAnimationFrame` on the actual
canvas owner. Both request and cancellation APIs must exist, and registration is
fallible. Missing APIs or a throwing animation registration select deadline fallback
once. A suspended supported animation clock does not select fallback. The host reports
its mode and reason through the existing browser diagnostic bridge.

A service wake retains the pending animation request. Canceling and restarting it
every 8 ms would starve a 16.7 ms animation callback. After an animation callback the
runner rearms before expensive drawing, not after it; ready opportunities are
coalesced, never replayed as a catch-up queue. When the cap is not due, an animation
wake can rearm without running the full client update.

`FramePacer::begin_frame_with_opportunity` always advances the client clock but checks
the opportunity before changing the presentation deadline. Service-only updates do
not spend that deadline, acquire a surface or initialize deferred renderer assets.
Both menu and world presentation continue through the ordinary redraw body and the
same successful-submission capture markers.

`FramePacer::background_deadline` requests client servicing at most 8 ms after its
last update. Input and control queue additions wake the runner immediately. Hidden
presentation is suppressed through the distinct visibility input; focus still
permits a visible unfocused canvas to present at no more than 30 FPS. Browser timer
throttling can delay the entire worker: these are requested deadlines, not a promise
of 20 Hz delivery while the browser withholds execution. The existing bounded
simulation catch-up applies on resumption.

Without animation support, the one timer waits for the earlier of client service
and the shared absolute presentation deadline. Positive fractional milliseconds
round up, and an already-due deadline yields asynchronously for at least 1 ms.
Missed presentation deadlines rebase without a burst. Explicit configured caps are
not clamped to 60. Only the focused uncapped fallback has a conservative 60 Hz host
yield cadence; it is not a measurement of display refresh.

Shutdown closes the ready state, cancels registrations and wakes the runner even
with a suspended animation callback. The runner tears down GPU presentation before
the existing lifecycle completion signal releases the mount. Closed-state callbacks
cannot generate work for that mount or its replacement. Final owner release drops
the closures. Cancellation errors are reported rather than ignored; a failed
cancellation does not allow replacement registrations during ordinary servicing.

## How to change it

Change FPS, focus and AFK policy in `app::pacing`, not the browser host. Keep native
`begin_frame` as the true-opportunity wrapper. Keep browser simulation/control work
on the single existing runner; never move it into the animation callback or make
animation availability a simulation precondition.

Change host registration and cancellation in `app::browser_pacing`. Preserve the
persistent pending animation handle across watchdog wakes, pre-work rearming,
coalesced opportunities and asynchronous due-deadline yield. Visibility belongs to
the existing SDK/render-worker input bridge, separately from focus. Mount waiting
and completion use host timers, not a presentation animation clock.

The injected-clock tests discriminate a service update from a post-hoc render mask,
check fractional timer rounding and stalled fallback rebasing, and reject queued
ready sources after shutdown. These controls do not prove live host callback timing
or browser cleanup. Live acceptance must include suspended-animation input/network
service, shutdown/remount, missing/throwing animation fallback and the absence of
cancel-the-loser starvation.

For a performance comparison use matched scene, cap, viewport/DPR, seed, view
distance, camera/actions, focus and visibility. Start with the same 60 FPS cap to
separate wait jitter from simply raising a cap. Higher rates require observed host
opportunities and adequate scene throughput. The existing
[presentation capture](presentation-capture.md) measures successful surface handoff,
not compositor delivery; reject truncated captures for exact full-trial percentiles.
Watch the service-only attempt count and CPU cost as well as submission intervals.

## Configuration

Persisted framerate, inactivity and focus policies remain shared with native pacing.
`BACKGROUND_POLL` defines the 8 ms service request; `UNLIMITED_FALLBACK_INTERVAL`
defines only uncapped deadline-fallback cadence. Once-per-second bounded diagnostics
report animation/timer/control callback counts and maximum timer-deadline lateness.
The diagnostic counts are not FPS.

The WebGPU backend currently advertises Fifo presentation. The persisted vsync flag
does not imply that browser display can become unsynchronized; animation pacing
provides opportunities, not GPU completion or compositor acknowledgements.

## Dependencies

The host uses the existing portable `Instant`, `wasm-bindgen-futures`, `js-sys` and
`web-sys`. The locked `web-sys` 0.3.103 provides stable caught dedicated-worker
animation request/cancel bindings; the shell enables `DedicatedWorkerGlobalScope`.
No new crate, unstable API flag, host worker or scheduling framework is required.
