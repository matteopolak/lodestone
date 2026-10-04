# Presentation submission capture

## What it is

An opt-in, bounded per-attempt trace of successful surface presentation submissions and GPU queue-completion callbacks, shared by the native shell and browser SDK. Submission rate is not compositor FPS; callback latency includes host delivery delay and is not pure GPU execution time.

## How it works

The existing `FrameProfiler` owns one `PresentationCapture`. Its normal `begin_frame` opens an attempt and finalizes any preceding attempt that never submitted as `not-submitted`. The menu and world paths mark a successful submission immediately after their existing `present` operation returns. Headless targets, failed acquisition, missing GPU state, and pacing skips cannot increase the submission count. A repeated submission without a fresh attempt increments `rejectedSubmissions` instead.

With mailbox presentation (macOS, VSync off; see `docs/camera-and-view.md`), a submission is a finished frame handed to the presenter thread, which shows only the newest one per available drawable.

The debug FPS counter uses the same successful menu/world surface boundaries, independently of whether capture is enabled. Preparing a world frame or rendering into a headless target does not count as presenting it.

The portable monotonic clock supplies capture-relative integer microseconds. Submission intervals run from one successful submission to the next, including gaps containing skipped attempts. The first submission has no preceding interval. A skipped row's `finishedUs` is the time the missing submission was finalized at the next attempt or capture stop; it is not an early-return timestamp or a measurement of CPU work.

Each trial retains at most 32,768 attempt rows as a prefix. Whole-capture counts and interval minimum, maximum and sum continue after the row limit; `droppedRows` makes that truncation explicit. The compact JSON bound is about 6.3 MB per trial. No sample buffer, formatting, GPU callback registration, extra polling or file I/O exists while capture is disabled. Rows and the three-slot completion channel are allocated at capture start; JSON serialization happens only at stop.

After a successful surface submission, a non-blocking device poll delivers ready callbacks and the recorder registers `Queue::on_submitted_work_done` for that queue boundary. At most three callbacks may remain outstanding; subsequent submissions still count but skip completion sampling until a slot becomes available. The callback only records a portable-clock timestamp through a non-blocking channel. It covers preceding queue work, including uploads and UI passes, but not compositor display. Its latency is an upper bound on queue completion: host scheduling and callback delivery can make it later than physical GPU completion. Use the existing render-pass timestamp timer for GPU execution durations.

Stop harvests already delivered callbacks without waiting for the GPU. `completionPendingAtStop` reports unresolved samples; their row timestamp remains null. Late callbacks belong to the stopped capture's disconnected channel and cannot contaminate a later capture. `completionRequests`, `completionObserved` and `completionSkipped` continue beyond row retention. Completion latency aggregates cover only observed callbacks, not every frame; reject incomplete or backpressured samples before drawing whole-trial completion conclusions.

JSON schema 2 has `rowScope: retained-prefix`, `aggregateScope: whole-capture`, explicit attempts/submissions/skips, separate menu/world counts, interval count/sum/min/max, elapsed capture duration, completion counters and rows with these columns:

| Column | Meaning |
| --- | --- |
| `attempt` | One-based redraw attempt sequence |
| `startedUs` | Attempt start relative to the capture origin |
| `finishedUs` | Successful submission or skipped-attempt finalization time |
| `outcome` | `0` not submitted, `1` menu, `2` world |
| `submission` | One-based successful submission sequence; null for a skip |
| `intervalUs` | Time since the preceding successful submission; null for the first or a skip |
| `targetFps` | Effective option/AFK cap supplied to the pacer; null means uncapped |
| `vsync` | Requested persisted vsync flag, `0` or `1`; not an observed display mode |
| `gpuCompletionCallbackUs` | Queue-completion callback time relative to capture origin; null if unsampled or unresolved |

For an entire capture with nonzero `elapsedUs`, submission rate is `submissions * 1_000_000 / elapsedUs`. For a retained subwindow, use its actual timestamp span and only successful submission rows. Do not substitute attempt count or mesh profile samples. Percentiles computed from rows describe the retained prefix; there is no whole-capture percentile claim after rows are dropped. Focus, visibility, host wait mode, actual surface mode, scene settings and display refresh must be established by the surrounding controlled trial; they are not invented by this trace.

Reject a capture with `droppedRows != 0` for exact whole-trial percentile comparisons. The budget counts attempts, including skips: a 30-second trial at 1,200 attempts/s already exceeds 32,768 rows. Use a shorter matched trial with an observed complete trace, rather than comparing a retained prefix against another trial's full window. This first slice does not support streamed chunks. Whole-capture submission counts and interval extrema remain valid after retention overflows, but they cannot reconstruct missing percentile samples.

## Configuration

Native runs opt in with an explicit output path:

```sh
LODESTONE_PRESENTATION_CAPTURE=/private/tmp/native-trial.json just run
```

This works with the existing native Surface benchmark too. Capture starts at `FrameProfiler` construction and ends when it is dropped during orderly shutdown, covering startup and menus as well as gameplay. The requested file is overwritten on shutdown; invalid paths produce a warning. A forcibly terminated process does not produce a completed report. Recording is independent of debug logging and the older phase CSV dump.

Set `LODESTONE_PRESENTATION_CAPTURE_SEGMENT` to an exact benchmark label to
capture only that phase. The capture starts before its first attempt and is
exported when the label changes. For example, use `singleplayer.walking_mining`
with `--benchmark singleplayer --benchmark-walk-mine`. Startup and later phases
cannot consume its row budget. An interrupted phase remains a partial trial,
even if orderly shutdown exports its prefix; require the next phase and normal
benchmark completion in the surrounding log. A misspelled or unreached label
does not produce a successful capture. The same 4096-row limit still applies.

Browser SDK callers supply `onProgress` and call `handle.startPresentationCapture()` / `handle.stopPresentationCapture()`. Calls queue lifecycle requests rather than synchronously starting or returning results. The owning render loop applies each request between frames and emits these existing progress callbacks:

- `presentation-capture-started`: the start boundary was applied.
- `presentation-capture-complete`: `event.message` contains the compact JSON report.
- `presentation-capture-error`: the request failed, for example because a capture was already active.

Wait for the start acknowledgment before beginning a controlled action. Wait for the complete event before destroying the session. Start requires a progress callback; at most four requests/responses can be outstanding, with an explicit error if that queue is full. Lifecycle reports are separate from bounded raw diagnostic rows and are not discarded by the responsiveness probe's diagnostic sample cap. The render-worker input bridge accepts `startPresentationCapture` and `stopPresentationCapture` messages and forwards the same SDK operations. There is no frame scheduling change or automatic cap override.

The standalone `?probe=1` panel provides `Capture frames 10s`. Its timer starts
only after the render loop acknowledges capture start. The unchanged report is
retained in `#lodestone-presentation-report`, separately from join and action
diagnostics, with the same export-size bound. Capture can run alongside the
walking probe. It does not change focus, the FPS cap or visibility; establish
those separately before making comparisons. A hidden page may delay its stop
timer, so use the report's actual duration and reject row overflow.
`Save metrics` downloads both retained reports as `lodestone-metrics.json`;
the `presentation` field contains the complete bounded submission trace and
`responsiveness` contains the most recent action/join report. Export is explicit,
not per-frame I/O or a request to an external service. Save after each controlled
action because the next completed action replaces that action report.

## How to change it

Keep the recorder in `app::presentation_capture` and use `FrameProfiler::record_surface_submission` for production consumption. Preserve both successful surface call sites in `app::menus` and `app::redraw`; adding another surface presentation path requires the same post-present marker with its device and queue. Keep partial attempts separate from submissions, and guard headless targets where their `present` method is a no-op. Never add a blocking GPU wait to live capture; the ignored headless queue control may wait solely to verify callback delivery, not to measure presentation throughput.

If adding row fields or strings, update the export-size arithmetic and its worst-row control. A larger row budget requires a new explicit bound. Keep intervals tied to successful submissions and preserve whole-capture aggregates after retention fills. Scheduling and worker animation-frame cancellation belong to the existing pacing/host runner boundary and must not be inferred from this recorder.

## Dependencies

The recorder uses `lodestone_time` through the shell's portable `Instant`, the existing `FrameProfiler`, and `serde_json`. Browser lifecycle delivery uses `BrowserControl`, the render runner, and the existing SDK progress polling callback. Native export uses ordinary file writing only when its environment opt-in is present.
