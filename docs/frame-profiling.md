# Frame profiling

## What it is

A live, in-process instrument that answers "where does a frame's time go",
on **both** sides of the CPU/GPU divide: a per-phase CPU breakdown of
`WindowApp::redraw` (`crates/lodestone-shell/src/app/frame_profile.rs`), each
of the two large phases split further into sub-phases with counts, alongside
real per-pass and whole-command-buffer GPU timings from `wgpu`'s
`TIMESTAMP_QUERY` feature (`crates/lodestone-shell/src/gpu/gpu_timing.rs`) —
so "am I CPU-bound or GPU-bound" is answerable rather than guessed at. It
ships live in every build, not behind a feature flag, so it is available on
whatever machine and server the owner is actually playing on, and
`just bench-frame` is its reproducible counterpart for comparing two builds
over a fixed camera path.

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
  The two largest phases, `world_encode_submit` and `hud_ui_encode_submit`,
  each carry a bracketed sub-phase breakdown with counts — see "World
  sub-phases (CPU)" and "HUD sub-phases (CPU)" below.
- One line per GPU segment (`gpu world_total: … ms`, `gpu world: … ms`,
  `gpu first_person: … ms`, `gpu hud_total: … ms`), or a single
  `gpu timing: unavailable (device lacks Features::TIMESTAMP_QUERY)` line if
  the adapter/device does not support it — see "GPU timing" below.

**Read the GPU lines against the CPU ones first, before optimising
anything.** Every CPU phase measures how long it took to *record* commands;
`queue.submit` only enqueues. A frame can be 5 ms of CPU recording and 20 ms
of GPU execution, and there is no way to tell from the CPU column alone.
`world_total + hud_total` against the frame's wall-clock time is the
CPU-bound-or-GPU-bound question, and it is the first one to answer.

Press **Shift+F3** (while F3 is held) for the pie chart described below —
vanilla's own visual, in the bottom-right corner rather than the text
columns.

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

That writes one CSV row per frame: the eight
[`FramePhase`](../crates/lodestone-shell/src/app/frame_profile.rs) columns,
then the four
[`WorldSubphase`](../crates/lodestone-shell/src/gpu/gpu_timing.rs) columns,
then the six
[`HudSubphase`](../crates/lodestone-shell/src/app/frame_profile.rs) columns,
all in milliseconds, with an empty field for a phase (or sub-phase) that was
skipped that frame — never a `0`, which would read as free rather than "did
not run".

That last property was **broken for as long as the dump existed** and is
worth knowing about if you are reading an older capture: `finalise` built
each row by reading the ring buffer back through an `f32 -> Option<f32>`
`.into()`, which is `Some(_)` unconditionally, so every skipped phase
silently inherited the last frame that *did* run it and a never-run phase
wrote a fabricated `0.0000`. No dump row on any session had ever had an
empty field, which is the tell nobody looked for. It is fixed, and
`a_skipped_phase_writes_an_empty_csv_field_not_a_stale_value` is the control
— observed failing against the old line before the fix landed.

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

### World sub-phases (CPU)

`world_encode_submit` is the single largest CPU phase on a real server
(measured by the owner running this instrument against a live session), and
until now it was one number covering all of `RenderState::render_inner`
(`gpu/frame.rs`) — command recording *and* `queue.submit`, fused, per the
"Record and submit are fused" note above. `gpu::gpu_timing::WorldSubphase`
breaks that one number into four, each timed with its own
`crate::platform::Instant` inside `render_inner`:

- **`world.prepare_buffers`** — every `prepare_*` call, the sky pass, and
  every camera/outline/debug-line/plugin-billboard/crack uniform write:
  everything that must run before `begin_render_pass`, because a render pass
  cannot itself create or write a buffer (see `gpu/frame.rs`'s own module
  doc, "Submission order is load-bearing").
- **`world.terrain_cull_draw`** — opaque terrain only: the packed-table
  loop's per-section `visible()` check plus its draw, and the model-arena
  loop's `TerrainCull::classify` plus its draw. This is the sub-phase the
  owner's "maybe cull more, or use better data structures" question is
  actually about, and it is reported alongside **sections visited**
  (`packed_sections_visited`/`model_sections_visited` — the packed loop has
  no cull counters of its own, so its own section count is its whole
  "visited" figure; compare the model side against
  `RenderStats::sections_drawn` plus its three `sections_culled_*` fields,
  already on the F3 overlay) — count and timing together, because "3 ms
  across 60 draws" and "3 ms across 6000" are different problems.
- **`world.other_draws`** — everything else this pass records: entities,
  block entities, particles, weather, water, translucent geometry, the
  outline, debug lines, nametags, the first-person hand's own pass, and the
  seven screen overlays. Not split further, for the same reason
  `app::frame_profile` itself gives for folding HUD/effects/container into
  one `hud_ui_encode_submit` bucket: none of these individually costs enough
  to be worth its own checkpoint on a healthy frame, and each lives in a
  different subsystem's file.
- **`world.submit`** — `CommandEncoder::finish` + `Queue::submit` themselves,
  plus this instrument's own GPU-query resolve/`after_submit` bookkeeping
  immediately around them. `queue.submit` only *enqueues* work (see the CPU
  phases section above), so a healthy frame should show this as the smallest
  of the four; a large reading here points at driver/queue contention, not
  at command-recording cost.

These four appear as a bracketed detail on `world_encode_submit`'s own F3
and tracing line — e.g. `world_encode_submit: 3.10/4.20/5.00 ms (240/240, 0
skip) [world.prepare_buffers: 0.42/0.90/1.10 ms, world.terrain_cull_draw:
1.85/2.60/3.40 ms, world.other_draws: 0.65/1.10/1.50 ms, world.submit:
0.18/0.30/0.40 ms, sections visited: 3880 packed + 612 model]` — not as
separate F3 lines or a new pie-chart wedge, and that is a scope decision, not
an oversight: **nested** checkpoints inside one already-sequential
`FramePhase` cannot share `FrameProfiler`'s single "elapsed since previous
mark" cursor (see `frame_profile.rs`'s own module doc for why `FramePhase`
entries must be chronologically disjoint), so they are windowed
independently and drained through a `gpu::gpu_timing` thread-local rather
than riding through a new `FramePhase` variant or a `RenderStats`/
`RenderState` field — both of those files were under concurrent edit by
other work at the time this landed. A sub-phase with no sample yet reads
`<no reading yet>`, never a fabricated `0.00`, exactly like the GPU segments
below.

The raw CSV dump (`LODESTONE_FRAME_PROFILE_DUMP`) gains four more columns,
`world.prepare_buffers,world.terrain_cull_draw,world.other_draws,world.submit`,
appended after the eight base phase columns — empty, never `0`, for a frame
where `world_encode_submit` itself did not run (see
`FrameProfiler::drain_world_subphases`'s doc for why a skipped frame must
not inherit the *previous* real frame's sub-phase values).

### HUD sub-phases (CPU)

`hud_ui_encode_submit` is the other large CPU phase, and it used to be one
opaque bucket. `app::frame_profile::HudSubphase` breaks it into six, marked
directly from `app/redraw.rs` (no thread-local bridge is needed — unlike the
world's sub-phases, every boundary is inside one function that already has
`&mut self.frame_profile`):

- **`hud.debug_gather`** — building this profiler's own F3 lines and the
  pie-chart snapshot. **This is the observer-effect line.** It is zero
  whenever F3 is closed, and F3 open is the only state in which anyone reads
  the frame rate off the overlay, so an on-screen fps figure has to be read
  against this number rather than as if the overlay were free.
- **`hud.frame_gather`** — chat span and wrap building, tab-list and boss-bar
  snapshots, locator dots, effect icons, hotbar records, and the `HudFrame`
  field assignment. **No GPU work at all**: no encoder exists yet. The
  phase's name says "encode submit" and most of its cost is this, which is
  precisely why the split was worth doing rather than assumed away.
- **`hud.hud_draw`** — `HudRenderer::render_with_item_models`, its own
  encoder and submit.
- **`hud.container_draw`** — the creative/container/recipe-book renderers.
  Reads as `<never ran, N skip>` on a session where nothing was ever opened,
  never as `0.00`.
- **`hud.menu_overlays`** — every `menu::render` overlay draw (pause, death,
  advancements, settings, statistics, links, social, command block, sign
  edit, book edit, spectator) plus the screenshot copy. Several can stack in
  one frame, which is why `menu_overlays_drawn` is a count and not a flag.
- **`hud.gpu_timing_end`** — `RenderState::gpu_timing_end_frame`: the stamp,
  resolve and submit that close GPU timing for the frame. This is **the
  instrument paying for itself**, reported rather than hidden. It is one
  extra command-buffer submission per frame that exists only because GPU
  timing does; if it ever grows past the phases it exists to measure, the
  instrument is the bug.

Three counts ride alongside, for the same reason the world's section counts
do: `chat lines` (roughly tenfold higher with the chat box open — 100 recent
lines against 10 — and the largest single swing in `hud.frame_gather`'s
input), `debug lines`, and `menu overlays`. They appear as a bracketed
detail on `hud_ui_encode_submit`'s own F3/tracing line, exactly like the
world's four.

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

`RenderState` owns one `GpuQueryTimer` with four segments — two single
passes, and two **spans bracketing whole command buffers**:

- **`world_total`** — the entire world command buffer: sky pass, block pass,
  first-person pass, every screen overlay.
- **`world`** — the block pass alone (terrain/entities/block entities/
  particles/weather/outline/debug lines/nametags). This is genuinely **one**
  `wgpu` render pass (`gpu::frame::render_inner`'s "block pass"), not several
  stages reported together — so its cost cannot be decomposed further without
  `TIMESTAMP_QUERY_INSIDE_PASSES`, which this adapter does not have.
- **`first_person`** — the first-person hand/held-item pass alone.
- **`hud_total`** — everything submitted *after* the world command buffer:
  the HUD's own encoders, the container/menu renderers, the screenshot copy.

Between them the two `*_total` spans account for **every** GPU pass the shell
submits in a frame, and that completeness is the point. The first question a
frame-time investigation has to answer is whether the wall being hit is CPU or
GPU, and a per-pass number cannot answer it while any pass is unaccounted for.
`world_total - world - first_person` is the sky pass plus the screen overlays;
that residue is deliberately **not** reported as a fifth segment, because
presenting a subtraction as though it were a measurement is how a counter
starts lying.

`hud_total` ends where the frame's own bookkeeping ends, so it excludes
`present`: the GPU cost of compositing the swapchain image belongs to the
window system, and nothing here can see it.

#### How a span across passes is timed at all

`writes()` can only time a span whose two ends are the *same* render pass —
which is fine for `world` and `first_person` and useless for "the whole
frame". The obvious mechanism for the latter, `CommandEncoder::write_timestamp`,
is unavailable here (see the `TIMESTAMP_QUERY_INSIDE_ENCODERS` note above). A
pass boundary is the only place a timestamp can be written on this adapter,
so `GpuQueryTimer::stamp` opens a pass that does nothing **but** carry a
boundary: an empty render pass on a private 1x1 `R8Unorm` target that is
cleared, discarded and never sampled.

Queue submissions execute in submission order, so a `begin` stamped in one
command buffer and an `end` stamped in a later one bracket everything
submitted between them — which is what lets `world_total` and `hud_total`
cover work recorded in several different files.

**The resolve must execute after both edges**, and that requirement is what
moved it. `resolve_query_set` is a GPU-timeline command: it copies whatever
the query set holds *at the point it executes*. It used to ride
`render_inner`'s own encoder, which is now too early — `hud_total`'s end edge
is stamped after that command buffer is already submitted. It lives in
`RenderState::gpu_timing_end_frame` instead, which `app/redraw.rs` calls once
every encoder of the frame has been submitted. Getting this wrong pairs a
`begin` from this frame against an `end` from the last one; the `end > begin`
guard in `harvest` turns that into a **permanently absent** reading rather
than a wrong number, so the failure mode is an honest blank, never a
fabricated figure.

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

### Pie chart

**What it is.** Vanilla's `Minecraft.renderFpsMeter` draws a filled pie —
one wedge per profiler section, a legend, a darker lower half for a
pseudo-3D read — and number keys walk into a child section while `0` walks
back out. `hud::draw_profiler_chart` (`crates/lodestone-shell/src/hud.rs`) is
a re-derivation of that shape, not a transliteration, feeding from this same
instrument rather than a second one: **"build on what just landed, do not
start a second system"** was the brief this landed under, and the chart's
root-level wedges are exactly [`FramePhase::ALL`]'s eight CPU phases —
the same summary the text lines already print, sized by `mean_ms` instead of
formatted as a string.

**Toggle and navigation.** Shift+F3 (`app::input::KeyOutcome::ToggleProfilerChart`,
handled in `app/lifecycle.rs`) shows or hides the chart independently of the
F3 text overlay itself — `WindowApp::show_profiler_chart`, a plain `bool`
alongside `show_debug` rather than another `Arc<AtomicBool>`, since only
`redraw` (same struct, same thread) ever reads it. F3+1..F3+8 drill into
wedge `0..8` (`app::input::KeyOutcome::ProfilerChartSelect(Some(i))`), F3+0
returns to the root (`ProfilerChartSelect(None)`) — **both are F3 chords, not
a bare number-key press**, because the number row is already the (rebindable)
hotbar selector; a bare press would fight it every time the chart is up. The
selected wedge (`WindowApp::profiler_chart_selected`) persists across frames
exactly like `show_profiler_chart` and resets to the root every time the
chart is shown again, so a stale drill-in from a previous session is never
what greets a fresh Shift+F3.

**What is genuinely flat, and why there is no fabricated hierarchy.**
`FramePhase`'s eight checkpoints are sequential siblings inside one
`redraw` call, not a call tree — there is no real child section to drill
into beneath any of them, so `draw_profiler_chart`'s "detail view" for a
selected wedge is a focused single-wedge readout (mean/p95/p99/samples/skip)
rather than a second, invented level of wedges. Fabricating fake children to
make the navigation feel deeper would be exactly the kind of derived value
this repo's evidence standard warns against — a number with no outside
source. If `FramePhase` ever grows real nested checkpoints (a phase measured
by pushing/popping a sub-timer), that is the natural place to give the chart
a genuine second level; nothing about today's data model forecloses it.

**Beyond vanilla, deliberately: GPU segments and skip counts, never folded
into a wedge's own proportion.** `gpu::gpu_timing::GpuQueryTimer` runs on a
different clock than the CPU phases (a few frames of readback lag; see "GPU
timing" above), so `draw_profiler_chart` lists `gpu world`/`gpu
first_person` as their own rows in the root view's side panel — never as a
child wedge nested under `world_encode_submit`, which would silently claim
the two clocks agree on when the corresponding work happened. Each phase's
`skipped` counter (see "How it works" above — an early `return` in `redraw`
means a phase never got marked this frame, not a fabricated `0.0`) is
likewise a plain row, never smoothed into a wedge's size. `RenderStats`
counters (sections drawn, draw calls, quads) are **not** duplicated onto the
chart — they already have their own F3 text lines (`DebugStats::section_count`/
`quads`/`draw_calls` and friends), and repeating them here would be a second,
driftable copy of a number that already has one honest home.

**A `None` GPU reading stays visibly "no reading yet", never a zero-width
wedge.** This is the one place a zero really would read as "free": a wedge
sized `0.0` and a wedge that was never measured are visually identical, so
the GPU rows are text (`gpu world: <no reading yet>` / `gpu world: 1.23 ms`),
not wedges, precisely so "unmeasured" cannot collapse into "free".

**Colour.** `hud::PROFILER_CHART_COLORS` is a fixed eight-entry palette keyed
by a phase's position in `FramePhase::ALL`, not stored per-slice
(`hud::ProfilerChartSlice` carries no colour field) — vanilla assigns a
section's pie colour by hashing its identity
(`ProfileResults.getPreferredColor`); a fixed palette over a fixed, ordered
eight-phase set is this instrument's equivalent of "stable colour per
section across frames" without needing a hash.

**The draw itself** is `crate::hud::item_icon::ColourStream::triangle` (a
flat-shaded pixel-space triangle in NDC, alongside that type's existing
`rect`/`gradient_rect`) fanned out per wedge at a fixed angular resolution
(`PROFILER_CHART_STEPS`, independent of slice count, so one wide wedge and
eight thin ones both read as round) — see `hud::draw_profiler_chart`'s own
doc comment for the exact layout (bottom-right corner, legend to the pie's
left).

**The one gate that proves this reaches pixels, not just a model:**
`gpu::pixel_gates::profiler_chart_draws_visible_pixels` — this instrument's
own dominant-defect class (`CLAUDE.md`'s island rule) is a subsystem that is
individually correct and reaches zero pixels because nothing calls the draw
branch, so a gate that only asserts on `ProfilerChart`'s fields cannot see
that. It renders the real HUD geometry through a headless GPU adapter with
the chart on and off and requires pixels to move; the neuter (commenting out
`draw_profiler_chart`'s call site) was run and watched fail before this
landed, then restored.

## The benchmark

The live instrument tells you where *this* session's time went. It cannot
tell you whether a change helped, because two sessions never see the same
camera, the same terrain or the same machine load. `just bench-frame`
(`crates/lodestone-shell/benches/frame_profile.rs`) is the reproducible half:

```bash
just bench-frame     # cargo bench -p lodestone-shell --bench frame_profile
```

It walks a **fixed camera path** over a **fixed demo world** — four waypoints
chosen to put the renderer in genuinely different regimes rather than to
sample one regime four times (level, obliquely yawed, high looking down for
maximum sections in view, low looking up for minimum) — and prints, per
waypoint:

- a **CPU-vs-GPU verdict** line;
- CPU medians for the world encode, its four sub-phases, and this
  instrument's own per-frame cost;
- GPU medians for all four segments;
- the counts beside them — sections drawn and visited, the three cull
  buckets, draw calls, quads, entities, resident and reserved bytes, and
  readback stalls.

Medians are appended to `bench-results/frame_profile.jsonl` with machine, git
sha and build profile, and compared against the previous same-machine,
same-scene run (advisory, never a gate).

**Run it on an otherwise idle machine.** A duration gathered while other work
runs gets attributed to the wrong cause. The bench states its own noise
estimate — slowest frame over median, per waypoint — so a run taken under
load says so rather than being quietly believed. That is a property of the
measurement, which is the only thing the process can honestly observe; load
average is the worst available proxy and is not consulted.

### What it asserts, and what it merely records

No millisecond figure passes or fails: a wall-clock duration on a shared
machine is a sample, not a measurement. What *is* asserted is either a count
or a relation between two numbers taken in the same run:

- **`world_total >= world + first_person`.** A span bracketing two passes
  cannot be shorter than the passes it brackets. This is the instrument
  validating itself, and it is not vacuous — it fails outright if the
  bracketing stamps are recorded in the wrong order, if the resolve executes
  before an edge is written, or if the four segment indices are rebased by
  someone reordering the segment-name list. `world_total` is genuinely larger
  (it also covers the sky pass and the overlays), so this is not an equality
  in disguise.
- **Residency does not move under pure rotation.** The counter-validation
  control: turn the camera 180° from one eye position and require
  `vram_bytes` to be **byte-identical**, while requiring `sections_drawn` to
  actually differ. Both halves are load-bearing — if the rotation drew the
  same set, a byte-identical residency figure would prove nothing. This
  reproduces the discriminator that caught `vram_bytes` when it was
  accumulated inside the terrain draw loops *after* the cull: a per-frame
  *drawn* quantity wearing a *residency* label, which moved 26% under exactly
  this input.
- **Every GPU segment produces a reading.** The warm-up already exceeds the
  3-frame readback ring, so a `None` this late is the query pool failing, not
  latency. It fails rather than leaving a blank column, because a blank
  column reads as "that pass is free".

### What the bench cannot see

The demo/packed world needs no vanilla `client.jar`, which is why every
headless GPU test in this crate uses it — and it means the **live-vanilla
model path** (`RenderState`'s model arena, built only from a real
`BlockAtlas`) is not exercised, and that is the path a real session draws
through. `hud_total` is likewise near-zero there for a structural reason and
not a happy one: nothing in the harness submits a HUD. Both are stated in the
bench's own output and module doc rather than left for a reader to infer from
a suspiciously small number.

## How to change it

- **Add a CPU phase**: add a variant to `FramePhase`, add it to `FramePhase::ALL`
  and `FramePhase::name`, bump `PHASE_COUNT`, and call `self.frame_profile.mark(...)`
  at the new checkpoint in `redraw`. The compiler will not catch a missed
  `ALL`/`name` arm — there is no exhaustive match tying them together — so
  double check both after adding a variant.
- **Add a GPU segment for one pass**: pass its name into the
  `&["world_total", "world", "first_person", "hud_total"]` slice
  `RenderState::new` builds the timer with (`gpu/state.rs`) — **append**,
  because the order fixes the query-set indices — then at that pass's
  `RenderPassDescriptor`, set
  `timestamp_writes: self.gpu_timer.borrow().as_ref().and_then(|t| t.writes("your_segment"))`.
  The `.borrow()` there is a **statement-scoped temporary** — it must be
  dropped before `resolve`/`after_submit` (which need `borrow_mut()`) run
  later in the same function, which is why every existing call site is a
  single inline expression rather than a `let`-bound `Ref` carried across
  statements. If you bind it to a variable instead, make sure it goes out of
  scope (or `drop()` it) before the next `borrow_mut()`, or you get a runtime
  `RefCell` panic, not a compile error.
- **Add a GPU segment spanning several passes**: same name-list step, then
  call `GpuQueryTimer::stamp(encoder, Some("name"), None)` where the span
  opens and `stamp(encoder, None, Some("name"))` where it closes. One empty
  pass can carry a `begin` for one segment and an `end` for another at the
  same instant, which is how `gpu/frame.rs` closes `world_total` and opens
  `hud_total` in a single stamp. **Both edges must be submitted before
  `gpu_timing_end_frame` runs its resolve**; an `end` that lands after it
  pairs against the next frame's `begin`, which `harvest` discards, so the
  symptom is a segment that never reports rather than one that reports
  wrongly.
- **Add a `HudSubphase`**: add a variant to
  `app::frame_profile::HudSubphase`, add it to `HudSubphase::ALL` and
  `HudSubphase::name`, bump `HUD_SUBPHASE_COUNT`, and call
  `self.frame_profile.mark_hud(YourVariant, Instant::now())` at the new seam
  in `app/redraw.rs`. No bridge is involved — unlike `WorldSubphase`, every
  boundary is inside one function that already holds
  `&mut self.frame_profile`. The HUD cursor re-bases automatically when
  `FramePhase::WorldEncodeSubmit` is marked, so there is no `begin_hud` call
  site to forget.
- **Change the CPU window size**: `frame_profile::WINDOW`. Larger costs more
  memory and a slower `percentile()` sort (only run from the F3/tracing path,
  never per-frame, so this is cheap even at a few thousand); smaller settles
  faster after a regime change (e.g. joining a server) at the cost of a
  noisier percentile.
- **Change the CSV dump's columns**: `frame_profile_dump::DumpWriter::write_header`/
  `write_row` — both read `FramePhase::ALL`, so a new phase appears in the
  header automatically; you do not need to touch this file for that case.
- **Add a `WorldSubphase`**: add a variant to `gpu::gpu_timing::WorldSubphase`,
  add it to `WorldSubphase::ALL` and `WorldSubphase::name` (bump
  `WORLD_SUBPHASE_COUNT`), then call
  `crate::gpu::gpu_timing::record_world_subphase(YourVariant, ms)` at the new
  checkpoint in `gpu/frame.rs::render_inner` — a single `Instant::now()` local
  plus one call at the boundary, following the existing four checkpoints'
  shape. Nothing on the `app::frame_profile` side needs to change: `mark`
  already drains every slot `take_world_subphases` returns by iterating
  `WorldSubphase::ALL`.

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
- The HUD's GPU cost is covered by the `hud_total` **span**, not by a
  segment inside `HudRenderer`, and that was the point of the span mechanism:
  it needs no change to `hud.rs`, `container/renderer.rs` or
  `menu/render/renderer.rs` at all. If you ever do want a per-pass segment
  inside one of those, note `HudRenderer::new` takes only `&wgpu::Device`
  while `GpuQueryTimer::new` needs a `&wgpu::Queue` for
  `get_timestamp_period()` — that signature is the real obstacle, not the
  query plumbing.
- **`RenderState::take_world_subphase_report` consumes the readings**, the
  same way `FrameProfiler::mark` does. The two must not both run for one
  frame or whichever is second sees `None` for every slot and counts a bridge
  miss. In the shell, `FrameProfiler` is the only caller; the accessor exists
  for `benches/frame_profile.rs`, which drives `RenderState` directly.
- **A `RenderState` driven without `gpu_timing_end_frame` produces no GPU
  timings at all.** That is every headless test and most benches, and it is
  the intended degradation — `gpu_timing_report` keeps reporting `None`,
  which callers must render as "no data" rather than `0.0`.

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
  `record_and_take_round_trip_the_real_elapsed_time_not_a_placeholder`
  (`gpu/gpu_timing.rs`) and
  `world_encode_submit_detail_reports_the_real_sub_phase_time`
  (`app/frame_profile.rs`) are the same control for the world-sub-phase
  bridge: a real, non-round sleep (23 ms and 27 ms) through the exact
  `record_world_subphase`/`mark` path production uses, with the reported
  figure parsed back out of the F3/tracing detail string rather than merely
  asserted present.
- **A high draw-call count is not automatically the answer to "why is
  `world_encode_submit` slow"** — `world.terrain_cull_draw`'s own count
  (`sections visited: N packed + M model`, alongside its timing) is what
  settles whether the cost is proportional to draw-call volume before
  reaching for `crates/lodestone-render/src/strategy.rs`'s unused
  multi-draw-indirect path (`select_strategy`/`build_strategy`) as a fix.
  If `world.terrain_cull_draw` is small relative to `world.other_draws` on a
  real session, the batching question is pointed at the wrong sub-phase.
