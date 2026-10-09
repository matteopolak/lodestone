# Presentation submission capture

## What it is

An opt-in, bounded per-attempt trace of successful surface presentation submissions and GPU queue-completion callbacks, shared by the native shell and browser SDK. Submission rate is not compositor FPS, and callback latency includes host delivery delay, so it is not pure GPU execution time.

## How it works

The `FrameProfiler` owns one `PresentationCapture`. Its `begin_frame` opens an attempt and finalises a preceding unsubmitted one as `not-submitted`. Menu and world paths mark a successful submission right after their `present` returns; headless targets, failed acquisition, missing GPU state and pacing skips cannot raise the count, and a repeated submission without a fresh attempt increments `rejectedSubmissions`. With mailbox presentation (macOS, VSync off; [camera and view](camera-and-view.md)) a submission is a finished frame handed to the presenter thread, which shows only the newest per drawable. The debug FPS counter uses the same successful boundaries whether capture is on or not.

The portable monotonic clock gives capture-relative integer microseconds. Intervals run from one successful submission to the next (gaps with skipped attempts included; the first has none). A skipped row's `finishedUs` is when the missing submission was finalised at the next attempt or stop, not a CPU-work measurement.

Each trial retains at most 32,768 attempt rows as a prefix (about 6.3 MB compact JSON). Whole-capture counts and interval min/max/sum continue past the limit and `droppedRows` reports truncation. While disabled there is no buffer, formatting, callback registration, polling or file I/O; rows and the three-slot completion channel are allocated at start and JSON is serialised at stop.

After a successful submission, a non-blocking device poll delivers ready callbacks and the recorder registers `Queue::on_submitted_work_done`; at most three stay outstanding (further submissions count but skip completion sampling). The callback records a portable-clock timestamp through a non-blocking channel. It covers preceding queue work (uploads, UI passes) but not compositor display, and is an upper bound on queue completion; use the render-pass timestamp timer for GPU execution. Stop harvests delivered callbacks without waiting; `completionPendingAtStop` counts unresolved samples (row timestamp null) and late callbacks go to the stopped capture's disconnected channel. `completionRequests`, `completionObserved` and `completionSkipped` continue past row retention; latency aggregates cover only observed callbacks, so reject incomplete or backpressured samples before whole-trial conclusions.

JSON schema 2 has `rowScope: retained-prefix`, `aggregateScope: whole-capture`, attempts/submissions/skips, separate menu/world counts, interval count/sum/min/max, elapsed duration, completion counters and rows with:

| Column | Meaning |
| --- | --- |
| `attempt` | one-based redraw attempt sequence |
| `startedUs` | attempt start relative to the capture origin |
| `finishedUs` | successful submission or skipped-attempt finalisation time |
| `outcome` | `0` not submitted, `1` menu, `2` world |
| `submission` | one-based successful submission sequence; null for a skip |
| `intervalUs` | time since the preceding successful submission; null for the first or a skip |
| `targetFps` | effective option/AFK cap given to the pacer; null is uncapped |
| `vsync` | requested persisted vsync flag `0`/`1`, not an observed display mode |
| `gpuCompletionCallbackUs` | queue-completion callback time; null if unsampled or unresolved |

Submission rate is `submissions * 1_000_000 / elapsedUs` for a whole capture; for a subwindow use its actual timestamp span and only successful submission rows (not attempts or mesh profile samples). Row percentiles describe the retained prefix only. Focus, visibility, host wait mode, actual surface mode, scene and display refresh must be established by the surrounding controlled trial. Reject `droppedRows != 0` for exact percentile comparisons: attempts include skips, so a 30-second trial at 1,200 attempts/s already exceeds the budget; use a shorter matched trial with a complete trace. Streamed chunks are not supported.

## How to change it

Keep the recorder in `app::presentation_capture` and consume it via `FrameProfiler::record_surface_submission`. Preserve both post-present markers in `app::menus` and `app::redraw`; any new surface path needs the same marker with its device and queue, and headless targets whose `present` is a no-op must be guarded. Never add a blocking GPU wait to live capture (the ignored headless queue control may wait only to verify delivery). When adding row fields or strings update the export-size arithmetic and its worst-row control; a larger row budget needs a new explicit bound. Keep intervals tied to successful submissions and aggregates whole-capture. Scheduling and worker animation-frame cancellation belong to the pacing/host runner, not this recorder.

## Configuration

Native: `LODESTONE_PRESENTATION_CAPTURE=/private/tmp/native-trial.json just run` (works with the native Surface benchmark). Capture starts at `FrameProfiler` construction and ends on orderly shutdown (a killed process produces no report), covering startup and menus; the file is overwritten and invalid paths warn. It is independent of debug logging and the phase CSV. `LODESTONE_PRESENTATION_CAPTURE_SEGMENT=<benchmark label>` (e.g. `singleplayer.walking_mining` with `--benchmark singleplayer --benchmark-walk-mine`) captures only that phase, starting before its first attempt and exporting when the label changes, so other phases cannot consume the row budget. An interrupted phase is a partial trial even if shutdown exports it (require the next phase and normal completion in the log); a misspelled or unreached label yields no capture.

Browser SDK: callers supply `onProgress` and call `handle.startPresentationCapture()` / `stopPresentationCapture()`. These queue requests; the render loop applies each between frames and emits `presentation-capture-started`, `presentation-capture-complete` (`event.message` holds the compact JSON) or `presentation-capture-error` (e.g. a capture already active). Wait for the start acknowledgement before the controlled action and for complete before destroying the session. At most four requests/responses are outstanding (explicit error when full); lifecycle reports are separate from the bounded diagnostic rows. The render-worker bridge forwards `startPresentationCapture`/`stopPresentationCapture` messages. No frame scheduling change or cap override.

The `?probe=1` panel has `Capture frames 10s`, timed from the start acknowledgement, retaining the report in `#lodestone-presentation-report` separately from join and action diagnostics and able to run alongside the walking probe. It changes no focus, FPS cap or visibility; a hidden page may delay its stop timer, so use the report's actual duration. `Save metrics` downloads `lodestone-metrics.json` (`presentation` complete trace, `responsiveness` latest action/join report); export is explicit, and since the next completed action replaces the action report, save after each controlled action.

## Dependencies

`lodestone_time` via the shell's portable `Instant`, `FrameProfiler`, `serde_json`; browser lifecycle through `BrowserControl`, the render runner and the SDK progress polling callback; native export uses file writing only when opted in.
