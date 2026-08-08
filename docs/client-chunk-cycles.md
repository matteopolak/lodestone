# Client chunk-path cycle accounting

## What it is

An instruction-denominated attribution of the **client** chunk path — from a
`level_chunk_with_light` packet arriving to its pixels being submitted — measured over real
generated terrain with macOS's task-level hardware counters. It answers "where do the CPU
cycles actually go?" with a number per stage and a named counter behind each one, and it
carries the controls that prove the counter is a counter. The harness is
`crates/lodestone-shell/tests/client_chunk_cycles.rs`; this doc is how to read it, what it
found, and how to extend it without reintroducing a defect it already caught.

## Why instructions, not wall time

Wall clock on this machine reproduces to **10.8%** peak-to-peak, and one worldgen stage
swung **22% across three runs of an identical binary** while an allocation counter read
905,459 to the digit, 3 of 3 (`DESIGN.md` §12.98, §12.103). A duration here has already been
attributed to the wrong cause outright — a "debug versus release" story that was pure
machine load.

`proc_pid_rusage(getpid(), RUSAGE_INFO_V4, …)` returns `ri_instructions` and `ri_cycles` for
the calling process. It is populated on Apple Silicon, needs no privileges, costs ~600 ns per
read, and reproduces to 0.1–0.6% under concurrent-agent load. Thermal state, DVFS and
P-versus-E-core placement change how *fast* instructions retire, never *which* instructions a
deterministic program executes. `docs/plans/worldgen-cycle-accounting.md` characterises the
instrument; this is its first client-side customer.

**Instructions are the comparator, not the goal.** They cannot see a locality change — see
"Where instructions understate" below, which is not hypothetical here.

## How it works

One `#[test]`, one binary. `ri_instructions` is **process-wide**, so two tests in one binary
would measure each other; a counter gate that shared a binary in this repo once read 502
against a true 256. Every measured stage is single-threaded: `mesh_snapshot_models` is called
directly rather than through `MeshScheduler`, which is the same function the worker pool calls.

The fixture is 25 real columns from `lodestone_server::overworld_chunk_source` — the generator
singleplayer uses — encoded by the production `ServerProtocol::encode_chunk` and decoded
through `lodestone_registry::adapter_for_protocol`. **No version crate is imported**, so this
file cannot become the hardcoded-`v770` dependency in shell code that `just check-seam` exists
to prevent, and the packet id comes out of `ServerDirective::Send` rather than being restated.

Stage boundaries are the ones production already crosses, read from
`crates/protocol/v770/src/adapter.rs:3689-3716`:

| stage | the exact call |
|---|---|
| S1 decode | `handle_packet` into a `WorldSink` that discards |
| S2 insert | `World::load` over pre-cloned `LoadedChunk`s |
| S3a snapshot | `snapshot_section` — the 27-section gather |
| S3b mesh | `mesh_snapshot_models` + `mesh_snapshot_fluids` |
| S4 submit | the **marginal** cost of one more section in `RenderState::render` |

S4 is a difference: the same frame with no terrain resident and then with every fixture
section, divided by the section count. The fixed per-frame cost cancels, leaving the term that
scales with render distance — which is the question, since `gpu/frame.rs:459/480/720` iterate
*every* resident section with no frustum and no distance cull.

## What it measured

Release profile, M5, 2026-08-07, no swap growth and a shrinking compressor across three
readings. Instrument controls: struct size 304/304, kernel scaling 4.0017× against a correct
hypothesis of 4.0, locality separation 11.05×. **Split control: S1 + S2 = 4,131,193 against a
production whole of 4,125,519, ratio 1.0005.**

### One-off, per column reaching meshed geometry

Centre column: 24 sections, 8 with geometry, 30,484 non-air cells, 23 distinct block states,
4,704 fluid cells, 3,203 quads, 54,578 packet bytes.

| stage | instructions | share |
|---|---|---|
| S1 decode | 4,124,674 | 3.7% |
| S2 `World::load` | 6,519 | 0.006% |
| S3a snapshot | 52,952 | 0.05% |
| S3b1 mesh models | 42,083,973 | 37.5% |
| S3b2 mesh fluids | 65,982,635 | 58.8% |
| **total** | **112,245,079** | |

**Meshing is 96.3% of the client chunk path, and fluid meshing alone is more than half of it.**

### Per frame, at the shipped render distance 8

| term | instructions per frame | basis |
|---|---|---|
| terrain draw submission | **17,711,344** | 19,024/section × 931 sections (`45a93e4`) |
| fixed frame cost | 1,734,465 | measured, no terrain resident |
| `World::heap_bytes` (F3) | 494,570 | 1,371/column × 361 |
| loaded-positions `Vec` (F3) | 116,242 | 322/column × 361 |

Draw submission is **36× the `heap_bytes` term**. That reorders the priorities the render plan
already set correctly: culling first.

### The fluid decomposition

`mesh_fluids` (`lodestone-render/src/models.rs:1158`) scans all 4096 cells and `continue`s on
`fluid_at(..) == None`. Splitting the centre column's sections by whether they contain any
fluid cell separates the scan from the geometry:

| arm | sections | instructions/section | per cell |
|---|---|---|---|
| no fluid at all | 3 | 489,335 | 119 per scanned cell |
| contains fluid | 5 | 12,899,732 | **13,711 per fluid cell** |

So the cost is real fluid geometry, not the empty scan — a "does this section contain fluid"
precheck would save only the 119/cell arm. The 13,711 figure is the target: per fluid cell,
`mesh_fluids` makes on the order of thirty **virtual** calls through `&dyn FluidSectionView`
(`neighbor_height_at` ×5, the eight `nh` corner probes, `flow_neighbor_at` ×4, `same` ×6,
`occludes_at`, `partial_occluder_y_range_at` ×4, `overlay_at` ×4), and each one redoes three
`split16`s, three range checks, a snapshot-slot index and a `PalettedContainer::get` bit-unpack.
The same neighbour cells are re-queried many times per cell and again by adjacent cells.

**Caveat on representativeness.** 4,704 of 30,484 non-air cells (15%) in this column are fluid,
so it is water-bearing. An inland column with no lake would put the fluid share near zero. The
*per-fluid-cell* number is the terrain-independent one; the 58.8% share is not.

### The fluid decomposition after issue #542

Same harness, same fixture, same 4,704 fluid cells, three separable commits. Each row is the
**wet** arm (the 5 fluid-bearing sections), so the figure is per *fluid* cell:

| arm | instructions/cell | cycles/cell | IPC | dry section |
|---|---|---|---|---|
| before (`4e0ffdf2`) | 13,709 | 1,793 | 7.64 | 490,885 |
| + `mesh_fluids` generic over the view | 12,406 | 1,622 | 7.65 | 356,874 |
| + padded `FluidGrid` and `cell_at` | 8,709 | 1,166 | 7.46 | 404,542 |
| + `NamedBiomeTint` effects memo | **6,629** | **857** | 7.74 | 404,471 |

`mesh_fluids` for the whole column: **65,965,170 → 32,413,136**; the column's one-off total
**112,215,407 → 78,653,989**. Three notes, each of which was measured rather than assumed:

- **The win is in instructions, not in locality.** This was expected to be a cache-locality
  change and therefore to show up more in cycles than in instructions. It did not: IPC starts
  at **7.64** — near this core's retire width — so the loop was never memory-bound, and
  instructions and cycles fell by 2.07× and 2.09× respectively. The instrument's blind spot
  (see below) was not in play, and checking cost nothing.
- **The largest single term was not the one issue #542 named.** Its diagnosis was the ~50
  virtual calls per cell. Measured, **6,263 of the 13,709 (46%) were one `water_tint_at`**, of
  which **97.8% was `lodestone_assets::tint::biome_effects`** — a linear scan of 66
  `(&str, BiomeEffects)` entries with a string compare per entry, run 25 times per cell because
  vanilla's biome blend is a radius-2 box. Monomorphisation plus the grid together bought 5,000
  instructions/cell; a four-entry memo on the *name* lookup bought 2,080 more.
- **A precomputed grid can make the fluid-free case worse, and did.** The dry arm is the
  4,096-cell scan. With `FluidGrid` filling through the trait's *default* `cell_at` (three
  independent probes) it went **356,874 → 1,021,034** per section, a 2.9× regression on
  exactly the terrain most sections are. Overriding `cell_at` in `SnapshotFluidView` to share
  one `get_block` brought it to 404,542 — below the pre-#542 490,885. **Measure the arm your
  optimisation does not target.**

## Where instructions understate, and this is not hypothetical

`heap_bytes` measured 494,570 instructions per frame at 361 columns, at IPC 4.47 — about 31 µs.
A `samply` capture attributed **1,223 samples** to it, roughly 1.2 s of CPU over a ~94 s
session, or ~7× more time than that instruction count implies. Both can be right: the harness
measures a 25-column world that fits in cache and extrapolates linearly, while `heap_bytes`
pointer-chases the *whole* resident world — tens of megabytes at 361 columns. Its real IPC at
that scale will be far below 4.47, and **instructions cannot see that**. This is exactly the
limitation `worldgen-cycle-accounting.md` names; the covering metric is `ri_cycles`, which the
harness also reports. Read the two together, and treat a large instruction count with a low IPC
as the memory-bound signal it is.

## How to change it, and the gotchas

- **Keep it one test in one binary.** Adding a second `#[test]` here silently corrupts every
  number, because the counter is process-wide and cargo runs tests concurrently.
- **Never `saturating_sub` a difference.** The first version of S4 took one frame per arm after
  4 warm-up frames and reported the loaded frame as *cheaper* than the empty one (4,288,471
  against 7,958,869) — a negative marginal cost, clamped silently to 0 by a `saturating_sub`.
  Lazy Metal pipeline compilation had not settled by frame 4. S4 now warms 40 frames, measures
  the median of 5 windows of 10 frames, and **asserts the difference is positive**.
- **Do not assert the instrument's validity from a microarchitectural belief.** An earlier
  control asserted `IPC > 1.0` to catch instructions/cycles being read swapped. It fired on
  correct code at IPC 0.643: the reference kernel is a serially dependent chain of two 64-bit
  multiplies, latency-bound at ~14 cycles for ~9.1 instructions, so *low* IPC is the right
  reading. The replacement is the locality control, whose expectation is arithmetic — the same
  compiled loop taking the same number of steps over a 4 KiB and a 16 MiB table must retire the
  same instructions while its cycles blow out.
- **`sections_drawn` is not the upload count.** It is incremented only by the opaque loop
  (`frame.rs:480`); a water-only section carries `mesh: None` there while still issuing a water
  draw at `frame.rs:720`. Measured 189 of 195 uploads. `draw_calls` counts both passes, and the
  gap between the two *is* the render plan's U4 target.
- **Page faults retire kernel instructions that count toward this process.** The locality
  control's instruction ratio was 1.17× before the tables were pre-faulted, and remains ~1.16×
  after. That is why the assertion is on the ratio of ratios, not a tight band.
- **The fixture guards are load-bearing.** Five assertions (non-air cells, distinct states,
  indirect palettes, sections with geometry, section count) exist because the *world* species of
  vacuous test cannot be read from the source. Seed 1234 chunk (0,0) is ocean and a light gate
  passed vacuously on it in this repo. If you change `SEED`, read the census the harness prints.

## Configuration

| knob | where | effect |
|---|---|---|
| `SEED` | the test | which terrain; the fixture guards reject a degenerate choice |
| `FIXTURE_RADIUS` | the test | 2 gives a 5×5 block, the minimum for a complete 26-neighbour set with a 3×3 interior |
| `WARMUP_FRAMES` / `FRAMES_PER_WINDOW` | the test | raise if S4's positivity assertion ever fires |
| `WORLD_STATS_PERIOD` | `sim/step.rs` | how often the O(resident-world) F3 fields recompute |

Run it explicitly; it is `#[ignore]`d because it needs real worldgen, the `client.jar` model
bake and a GPU adapter:

```bash
cargo test -p lodestone-shell --release --test client_chunk_cycles -- --ignored --nocapture
```

**Release is not optional** — a debug-profile instruction count measures unoptimised codegen,
not the shipped client. macOS on Apple Silicon is required; the harness fails loudly rather than
returning zeros elsewhere.

## Dependencies

`proc_pid_rusage` from `libSystem` (declared in the test, no crate); `lodestone-server` for the
real generator; `lodestone-registry` for the version-agnostic client adapter and server protocol;
`lodestone-world`, `lodestone-render` and `lodestone-shell`'s `mesher`/`gpu` for the stages; a
vanilla pack under `.cache/mc/<version>/` via `resources::BlockResources::load`.
