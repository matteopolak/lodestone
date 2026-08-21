# Frame profiling

## What it is

A live, in-process instrument that answers "where does a frame's time go":
a per-phase CPU breakdown of `WindowApp::redraw` (`crates/lodestone-shell/src/app/frame_profile.rs`)
and coarse per-pass GPU timing via `wgpu`'s `TIMESTAMP_QUERY` feature
(`crates/lodestone-shell/src/gpu/gpu_timing.rs`). It exists to make an
optimisation *measured* rather than guessed — this repo's evidence standard
forbids fixing what has not been profiled — and it ships live in every build,
not behind a feature flag, so it is available on whatever machine and server
the owner is actually playing on.

## What to run, and what to look at

Join a real server with the debug overlay open and let the tracing line
accumulate for a minute or two of ordinary play (moving through unloaded
terrain is the interesting case, not standing still):

```bash
RUST_LOG=frame_profile=info,warn cargo run --release -p lodestone-shell --bin lodestone
```

Then press **F3** in-game. Two new blocks appear in the right column, below
the existing engine-internals lines:

- One line per CPU phase: `name: mean/p95/p99 ms (samples/window, N skip)`.
- One line per GPU segment (`gpu world: … ms`, `gpu first_person: … ms`), or
  a single `gpu timing: unavailable (device lacks Features::TIMESTAMP_QUERY)`
  line if the adapter/device does not support it — see "GPU timing" below.

The same two blocks are also emitted once a second as a `tracing` line on the
`frame_profile` target (the `RUST_LOG` filter above), so a session run
without a window focused on the overlay (or piped to a log file) still
records the numbers. Read `mean` for the steady-state cost and `p95`/`p99`
for the stutter a player actually notices — a phase whose mean looks cheap
but whose p99 is 10x the mean is exactly the "occasional freeze" complaint,
and a mean alone cannot show it.

For a full session you can pull into a spreadsheet or a script afterwards,
set the dump env var before launching:

```bash
LODESTONE_FRAME_PROFILE_DUMP=/tmp/lodestone-frames.csv \
  RUST_LOG=frame_profile=info,warn cargo run --release -p lodestone-shell --bin lodestone
```

That writes one CSV row per frame (`frame,setup,sim_tick,mesh_upload,acquire,prepare,world_encode_submit,hud_ui_encode_submit,present`,
milliseconds, one column per [`FramePhase`](../crates/lodestone-shell/src/app/frame_profile.rs)),
with an empty field for a phase that was skipped that frame — never a `0`,
which would read as free rather than "did not run".

**Always run `--release`.** Cranelift is this repo's debug-build backend and
a debug binary's numbers say almost nothing about what the owner will
actually experience; see `CLAUDE.md`'s own note on this. Every number this
instrument reports should be quoted alongside which profile produced it.

## How it works

### CPU phases

`FrameProfiler` (owned by `WindowApp`, `crates/lodestone-shell/src/app.rs`)
is a set of fixed-size ring buffers (240 samples each, roughly 4 s at 60 fps),
one per [`FramePhase`](../crates/lodestone-shell/src/app/frame_profile.rs):
`Setup`, `SimTick`, `MeshUpload`, `Acquire`, `Prepare`, `WorldEncodeSubmit`,
`HudUiEncodeSubmit`, `Present`. `redraw` calls `mark(phase, Instant::now())`
at each phase boundary; each `mark` records the elapsed time since the
*previous* mark (or since `begin_frame`) as that phase's cost.

These are **not** a clean textbook "input / tick / mesh / prepare / record /
submit / present" split, because `redraw` itself does not have seams there —
reporting seams that do not exist would be worse than reporting the real
ones:

- **Input is genuinely absent.** Raw key/mouse events are handled by winit
  callbacks outside `redraw` entirely (`app::lifecycle`/`app::input`), so
  there is nothing to time on this side. `Setup` starts *after* that, at the
  pacer/option-sync work `redraw` does first.
- **Record and submit are fused.** `RenderState::render_with_crack_and_effects`
  builds its command encoder *and* calls `queue.submit` internally, so there
  is no CPU-side seam between "recorded" and "submitted" to time separately.
  `WorldEncodeSubmit` is that whole call. (`queue.submit` itself only
  *enqueues* work for the GPU; the GPU-side cost is what the timestamp
  queries below measure, on a different clock entirely.)
- **HUD, status effects and the container/menu overlay share one bucket**
  (`HudUiEncodeSubmit`) because each issues its own encoder/submit pair in
  sequence and none is individually worth a separate checkpoint — the CPU
  cost there is dominated by per-frame state gather (colour-stream
  building), not by the `queue.submit` calls.

A frame that returns early (unfocused/occluded window, a failed `acquire()`,
no GPU state yet — `redraw` has several of these) simply never calls `mark`
for the phases past the return. Rather than requiring every return site to
know about the profiler, `begin_frame` finalises whatever the *previous*
call left pending: every phase that was marked gets a real sample, and every
phase that was not gets its `skipped` counter incremented — visible in both
the F3 line's `(samples/window, N skip)` suffix and the CSV dump's empty
field, never a fabricated `0.0`.

### GPU timing

`gpu::gpu_timing::GpuQueryTimer` writes `wgpu::Timestamp` queries through
`RenderPassDescriptor::timestamp_writes`, **not**
`CommandEncoder::write_timestamp`. That is a deliberate choice, not a style
preference: the per-pass descriptor field needs only `Features::TIMESTAMP_QUERY`,
which Metal supports, while the encoder-level method needs
`Features::TIMESTAMP_QUERY_INSIDE_ENCODERS` — whose own `wgpu` doc excludes
Apple GPUs by name ("Metal (AMD & Intel, not Apple GPUs)"), as does
`TIMESTAMP_QUERY_INSIDE_PASSES` (which would be needed to time *inside* one
pass rather than around it). So on Apple-silicon Metal, whole-pass timing is
the finest grain available, and this instrument does not pretend otherwise.

`RenderState` owns one `GpuQueryTimer` with two segments:

- **`world`** — the terrain/entities/block-entities/particles/weather/
  outline/debug-line/nametag pass. This is genuinely **one** `wgpu` render
  pass (`gpu::frame::render_inner`'s "block pass"), not several stages that
  happen to be reported together — so its cost cannot be decomposed further
  without `TIMESTAMP_QUERY_INSIDE_PASSES`, which this adapter does not have.
- **`first_person`** — the first-person hand/held-item pass.

**Not** separately timed, on purpose: the sky pass, the seven individual
screen-overlay passes (underwater/fire/pumpkin/spyglass/freeze/confusion/
portal) and the HUD's own, separately-submitted encoder. `world` is
overwhelmingly the dominant GPU cost in this renderer, and adding query
plumbing to a fourth-plus struct for passes that are typically near-zero cost
was judged not worth the risk to files under concurrent edit. Their absence
is documented here rather than silent — if you need to know whether the sky
or an overlay pass is expensive, that is unmeasured and would need extending
this module, not something already folded invisibly into `world`.

Feature gating happens **twice**. The adapter advertising `TIMESTAMP_QUERY`
is not sufficient — the device must have requested it at creation
(`lodestone_render::device::GpuContext::from_instance` now does, whenever
`GpuCapabilities::probe` finds the adapter supports it), or every
`timestamp_writes` write is invalid. `GpuQueryTimer::new` checks
`device.features()` — the *granted* set — and returns `None` when the
device does not carry the feature. Every caller (the F3 lines, the tracing
line) reports GPU timing as **unavailable** in that case, never as `0.0 ms`,
which would read as "free" rather than "not measured".

Readback is asynchronous and lagged by design: a query result is only valid
once the command buffer containing its resolve has actually executed on the
GPU, which is never the same tick as submission. `GpuQueryTimer` uses a
3-frame ring of resolve/readback buffers and reports the last **completed**
frame's numbers, a few frames behind — never a synchronous stall (a
profiling tool must not itself become the frame-time cost it exists to
measure). If a ring slot's previous readback has not completed by the time
it comes back around, that is counted in `stalled_frames` (shown in the F3
overlay and the tracing line whenever non-zero) rather than silently dropped
or overwritten — real GPU/driver backpressure, not a bug in the timer.

## How to change it

- **Add a CPU phase**: add a variant to `FramePhase`, add it to `FramePhase::ALL`
  and `FramePhase::name`, bump `PHASE_COUNT`, and call `self.frame_profile.mark(...)`
  at the new checkpoint in `redraw`. The compiler will not catch a missed
  `ALL`/`name` arm — there is no exhaustive match tying them together — so
  double check both after adding a variant.
- **Add a GPU segment**: pass its name into the `&["world", "first_person"]`
  slice `RenderState::new` builds the timer with (`gpu/state.rs`), then at
  that pass's `RenderPassDescriptor`, set
  `timestamp_writes: self.gpu_timer.borrow().as_ref().and_then(|t| t.writes("your_segment"))`.
  The `.borrow()` there is a **statement-scoped temporary** — it must be
  dropped before `resolve`/`after_submit` (which need `borrow_mut()`) run
  later in the same function, which is why every existing call site is a
  single inline expression rather than a `let`-bound `Ref` carried across
  statements. If you bind it to a variable instead, make sure it goes out of
  scope (or `drop()` it) before the next `borrow_mut()`, or you get a runtime
  `RefCell` panic, not a compile error.
- **Change the CPU window size**: `frame_profile::WINDOW`. Larger costs more
  memory and a slower `percentile()` sort (only run from the F3/tracing path,
  never per-frame, so this is cheap even at a few thousand); smaller settles
  faster after a regime change (e.g. joining a server) at the cost of a
  noisier percentile.
- **Change the CSV dump's columns**: `frame_profile_dump::DumpWriter::write_header`/
  `write_row` — both read `FramePhase::ALL`, so a new phase appears in the
  header automatically; you do not need to touch this file for that case.

### Gotchas

- `RenderState::gpu_timer` is a `RefCell`, because `render`/`render_inner`
  take `&self` (the whole crate's convention for per-frame installable
  state) but the timer's ring-buffer bookkeeping is real mutation. See the
  "add a GPU segment" note above for the borrow-scoping trap this causes.
- The GPU timer's constructor lives in `RenderState::new`
  (`gpu/state.rs`), which is a different file from the pass call sites
  (`gpu/frame.rs`, `gpu/first_person.rs`). Adding a segment name to one
  without the other compiles fine and silently reports nothing for it —
  `writes(name)` returns `None` for an unknown name exactly like it does for
  "timer unavailable", so a typo'd segment name is indistinguishable from a
  missing feature at the call site. There is no test pinning the two lists
  against each other; if you add one, consider adding a debug assertion in
  `RenderState::new` that every name a call site might ask for is present.
- Do not attach a GPU timer segment to the HUD's renderer without giving
  `HudRenderer::new` a `&wgpu::Queue` parameter first — the constructor
  today only takes `&wgpu::Device`, and `GpuQueryTimer::new` needs the queue
  for `get_timestamp_period()`. This is exactly why HUD GPU timing is out of
  scope today rather than half-wired.

## Configuration

- `LODESTONE_FRAME_PROFILE_DUMP` — path to a CSV file for raw per-frame
  samples. Unset (the default) records nothing extra; set to a path that
  cannot be opened (bad directory, read-only mount) logs a `tracing::warn!`
  once on the `frame_profile` target and then behaves exactly as if unset —
  it never panics and never retries per-frame.
- `RUST_LOG=frame_profile=info` (or `=debug`/broader) — enables the
  once-a-second CPU+GPU tracing line. The line is built only when
  `tracing::enabled!` for that target is true, so leaving it off costs
  nothing beyond the check itself.
- F3 (in-game) — shows the same two blocks live, rebuilt only while the
  overlay is open (building the summary sorts each phase's ring buffer,
  which is wasted work for a screen nobody is looking at).

## Dependencies

- `wgpu`'s `TIMESTAMP_QUERY` feature (native only where the adapter
  advertises it — this instrument degrades to "unavailable", never to
  fabricated zeros, everywhere else, including the browser build).
- `crate::platform::Instant` (`lodestone-time`) for the CPU-side clock —
  never `std::time::Instant::now()` directly, which traps on `wasm32`.
- `tracing` for the periodic line; `std::fs`/`std::env` for the CSV dump and
  its env var, both of which degrade (`Err`, never a trap) rather than
  panicking on `wasm32`.

## Evidence and traps for the next reader

This instrument exists because a wrong number here does not just mislead
about magnitude — it can **invert the conclusion**, exactly as this repo's
own record already shows for a residency counter computed from a drawn-quad
count (`DESIGN.md` §12): a value that moved on pure camera rotation was read
as "we barely use any VRAM", when true residency was flat and the *counter*
was the bug. Before trusting any number this instrument reports:

- **A camera rotation alone must not move `world`'s GPU time noticeably**,
  assuming the same geometry stays in the frustum. If it does, suspect the
  counter before suspecting the renderer.
- **A mean that looks cheap with a p99 far above it is the interesting
  reading**, not the mean — that gap *is* the stutter a player reports, and
  averaging over a whole session would hide it.
- **`gpu world` and the CPU `world_encode_submit` phase are different
  clocks measuring different things.** `world_encode_submit` is wall-clock
  CPU time to record commands and call `queue.submit` (which returns almost
  immediately — it only enqueues work); `gpu world` is actual GPU execution
  time for that pass, reported several frames later. A slow `world_encode_submit`
  with a fast `gpu world` points at CPU-side command recording (likely too
  many draw calls or state changes); the reverse points at the GPU itself
  (shader cost, overdraw, memory bandwidth). Reading them as one number
  loses exactly the distinction the two instruments exist to make.
- **A `None`/"no reading yet" GPU segment is not a zero-cost pass** — it
  means the 3-frame readback ring has not completed once yet (the first
  handful of frames of a session) or, if it persists, that the pass is
  reachable but the query pool never resolved (check `stalled_frames`
  first).
- **Do not predict a plausible round number and call it validated.** The
  test `mark_reports_the_real_elapsed_time_not_a_placeholder`
  (`app/frame_profile.rs`) sleeps a *non-round* 18 ms inside a marked phase
  and asserts the reported figure lands near it — the magnitude-species
  control this repo's evidence standard asks for. A percentile computed
  before the ring buffer has filled (`samples < window` in the F3 line) is
  real data, just over fewer than 240 frames; do not treat it with the same
  confidence as a settled window, and the line says so on every read.
