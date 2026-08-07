# Mesh fill rate

## What it is

A headless measurement of the owner's own metric — **how long, standing still,
the whole render distance takes to reach the GPU as meshed geometry** — driven
through the real `Sim`, the real integrated server and the real mesh worker pool.
It exists because that figure was once measured, attributed and then made
unobservable when the diagnostics that produced it were deleted, and because the
answer it gives is not the one the mesher is usually blamed for.

`crates/lodestone-shell/tests/mesh_fill_rate.rs`, `#[ignore]`d:

```bash
cargo test -p lodestone-shell --release --test mesh_fill_rate -- --ignored --nocapture
```

Run it in `--release`. In debug, cold worldgen dominates and the answer is about
a different subsystem.

## What it found

At the shipped `render_distance = 8` (server `view_radius = 9`, so 361 streamed
columns and 289 visible), on `2b214469`:

| | baseline | with the palette prefilter |
|---|---|---|
| visible columns meshed | 56–71 / 289 | **289 / 289** |
| columns delivered to the client store | 91–96 / 361 | **361 / 361** |
| time to fill | never (still 71/289 at 600 s) | **6.3–6.9 s** |
| rings complete (of 0..=8) | 0–3, ring 4 partial | **all 9** |
| mesh events per section | 1.00–1.03 | 1.01–1.02 |

**The mesher was never the bottleneck.** In the baseline the dirty queue, the
forced queue and the worker backlog were all `0` for 26 million consecutive
frames while nothing progressed, `frames with a full queue` was `0`, and mesh
events per section was `1.000` — no rebuild, so no neighbour-invalidation
cascade. Meshing did all the work it was given, in 1.8 s, and then had nothing to
do because **chunks stopped arriving**.

The term was on the server tick thread, in the random-tick scheduler:
`section_has_randomly_ticking_block` (`crates/lodestone-server/src/random_tick.rs`)
scanned all 4096 blocks of every section of every column, every tick, calling the
**string** predicate `is_randomly_ticking` on each. `sample(1)` put **97.6% of
the tick thread** (3,939 of 4,034 samples) in that one predicate. Measured cost:

* **2.108 ms per column** (24 sections × 4096 = 98,304 predicate calls at 21.45 ns)
* at 361 columns that is **761 ms of scanning per 50 ms tick — 15× over budget**
* the palette prefilter is **38.7 µs per column, 54× cheaper**; 361 columns is
  then 14 ms, which fits

Generation shares that thread, so past a few tens of resident columns it gets
effectively no time and delivery stops permanently. That is the reported
"standing still, ~10 chunks render, additional ones take over a minute" — it is a
hard wall, not a slowdown, which is why standing still never helps.

The healthy shape, once the gate is cheap, is **linear**: per-ring completion
deltas of 0.18–1.15 s across all nine rings, ~17 ms/column, dominated by cold
worldgen.

## How it works

Everything from the integrated server down to `Sim::drain_meshes` is the real
production path, reached the way `app::begin_singleplayer` reaches it:

```text
NetClient::open_singleplayer(view_radius = render_distance + 1)
  → Sim::attach_net → Sim::step
      → run_schedule(Update) → FrameSet::Terrain → heal_dirty_columns
      → poll_net → on_column_arrived → mark_column_dirty + mark_neighbours_dirty
  → Sim::drain_removals + Sim::drain_meshes   (the app's own frame order)
```

It omits exactly one thing: `RenderState::upload_section` and the draw loops,
because there is no GPU. That makes every number an **optimistic bound** — the
real client additionally pays buffer creation per drained mesh and an unculled
draw call per resident section (`gpu/frame.rs`), both of which lengthen the frame.
If this harness says the fill is slow, the real client is slower.

Reported per ring (Chebyshev distance from the spawn column) and in aggregate:
columns and sections meshed, `store` (columns the server has actually
delivered), dirty/forced queue depth, worker backlog, mesh events per section,
and deferred first-builds.

## Which number is the claim

**It depends on what was limiting, and the harness says which.** If the per-frame
heal budget is the constraint, frames-to-fill is the frame-rate-independent
quantity and `frames / fps` is the wait a player sees. That was the assumption
this harness was built on and the first run falsified it: `frames with a full
queue` was `0 / 26,168,839`. So the frame count is an artifact of how fast the
harness spins (unthrottled, ~10^5 fps) and converting it to seconds invents a
fictional number — the output only makes that conversion when the budget actually
saturated, and says so explicitly when it did not.

When meshing is not the constraint, **wall time and `store` are the claim**.
`store` is the disambiguation that matters: a plateau with `store` also flat
means the server stopped delivering; a plateau with `store` still climbing would
mean delivery is fine and the mesher is dropping columns.

## The control, and why the first one was worthless

The intended control was `gamerule random_tick_speed 0`, which should make
`tick_chunk` return at its `tick_speed == 0` guard before reaching the scan. It
changed nothing (58/289 against 56/289) — and **that null result carried no
information, because the control's premise was false**: `tick.rs` passes the
hardcoded `DEFAULT_RANDOM_TICK_SPEED` to `tick_chunk`, and
`GameRules::random_tick_speed()` has **zero production callers**. The game rule
is an island; setting it cannot affect the tick loop.

The control that works is an A/B on the mechanism itself, in a throwaway
worktree, with one function changed and the harness byte-identical. Note the
first attempt at *that* also measured nothing, and the harness's own precondition
assert caught it: a detached worktree has no `.cache/mc` (it is untracked), so
vanilla assets are missing, `MeshPolicy::id_spaces_agree` goes false and
`mesh_column_inner` returns early — zero sections meshed, for reasons having
nothing to do with the patch. **Symlink `.cache` into any worktree you measure
meshing in.**

## How to change it, and the gotchas

* **Do not turn the observed number into a threshold** while the defect is
  unfixed — a threshold picked from a broken baseline locks the break in. The
  only assertion is that the measurement happened at all (`visible_meshed > 0`),
  which is what caught the missing-assets case above.
* **Render distance is read from `Config::default()`**, not hardcoded, so this
  measures whatever the shipped default actually is. It is 8, and the owner's
  persisted `options.json` carries no `render_distance` key — `Options::from_json`
  defaults a missing or out-of-range value to `DEFAULT_RENDER_DISTANCE`.
* **It writes to its own directory under `std::env::temp_dir()`**, never
  `saves::default_world_dir()`, so it cannot touch a developer's real world. A
  fresh directory means every column generates cold, which is the honest
  first-join case; pointing it at an existing save measures a warm one instead
  and will look much faster.
* **`LODESTONE_MESH_FILL_TICK_SPEED=<n>`** sends `gamerule random_tick_speed <n>`
  once in-world. It is retained as documentation of a premise-false control and
  as a live check on whether that game rule has been wired up: if it ever starts
  changing the outcome, the island has been closed.
* The two arms are two invocations of one binary rather than two `#[test]`s, so
  neither can pollute the other's counters.

## Configuration

| knob | where | effect |
|---|---|---|
| `render_distance` | `Config::default()` / `options.json` | the fill target; `view_radius` is this `+ 1` |
| `DIRTY_COLUMN_BUDGET` | `mesher.rs` | columns healed per frame, per queue (forced, then dirty) |
| `DEADLINE` | this test | 120 s; long enough to prove a plateau is a plateau |
| `LODESTONE_MESH_FILL_TICK_SPEED` | env | the premise-false control arm described above |

## Dependencies

`lodestone::sim::Sim`, `lodestone::net::NetClient::open_singleplayer`,
`lodestone::mesher::{TerrainMesh, SectionKey, DIRTY_COLUMN_BUDGET}`,
`lodestone_ecs::{hold_read, chunks::ChunkWorld}`, `lodestone_model::ClientAction`,
`lodestone_registry::server_protocol_for_protocol` (so it needs a hostable
family — the default `live` feature), and vanilla assets under `.cache/mc/<ver>`
for the classifier's id space to match the store's.
