# Progressive chunk generation ("mip levels" for worldgen)

## What it is

A design and staged implementation plan for serving distant chunk columns at a reduced
generation stage — shaping, carving and structures but no ores, vegetation or top-layer —
and upgrading them to full generation as the player approaches, so the server can stream a
much larger view radius without paying full generation for columns the player can barely
see. The owner's framing: mip levels for chunk generation. Two hard correctness
constraints come with it, verbatim from the owner:

> just note the user shouldnt be able to modify a chunk that isnt done generating, and if
> a chunk has already been generated (maybe and modified?) then it should be sent over in
> whole, instead of generating a lower stage for it. this will make sure the user can e.g.
> see a structure they made that's really far away

And a fidelity ruling that sets the budget:

> for far away, the user wont really notice light, ores, trees, or anything like that.
> experiment with it to see what distances etc are good

This plan was written read-only against the tree at `a024ed20`; every "exists today" claim
below was verified against source at that commit, not quoted from another doc. Doc status
annotations are the highest-decay content in this repo — re-verify before executing.

---

## What exists today (verified facts the design rests on)

**The generator already has the seam.** `OverworldGenerator::column`
(`crates/lodestone-worldgen/src/overworld/mod.rs`) is externally monolithic but internally
staged, memoised per stage in the sharded `StagedStore`:

| internal stage | slot | contents |
|---|---|---|
| 0a/0b | `ChunkStages::structure_starts` / `structure_refs` | structure placement + the 17×17 refs walk |
| 1–4 | `ChunkStages::pre_ore` (`pre_ore_stage`, body in `overworld/fill.rs`) | aquifer, noise fill, surface rules, **structure piece placement** (`structure_place_stage`), carvers |
| 5 | `ChunkStages::post_ore` (`post_ore_world`) | ores |
| 6 | `vegetation_stage` (`overworld/decorate.rs`) | trees, plants, all vegetation features, generated block entities |
| 7 | `top_layer_stage` | snow/ice freeze pass |
| — | `spawn_stage` (`crates/lodestone-worldgen/src/spawn_stage.rs`) | generation-time creature spawns |

`ticket.rs`'s doc claims "`OverworldGenerator::column` is one monolithic call with no seam
to stop at NOISE or SURFACE" — **that rationale is stale**. The seam exists; it has no
public name yet. Crucially, **structure pieces are placed inside pre-ore** (`fill.rs` calls
`structure_place_stage` between fill and carve), so a "shaped" column already contains
villages, mineshafts, monuments — a distant *generated* structure is visible at the reduced
stage for free.

**Light is not a generation stage in this engine.** `V770ServerProtocol::encode_chunk`
computes real per-column light at encode time (`compute_served_light` →
`lodestone_world::compute_column_light`, the isolated-column flood), and the heightmap sent
is the generator's real `MOTION_BLOCKING` map. So a reduced-stage column ships with real
sky/block light derived from its actual blocks — it never renders black. The
`LightData::Missing`-resolves-to-0 hazard is about *absent* data in hermetic harnesses; our
wire never sends a lightless present section. This dissolves most of the "partial chunks
and light are in tension" worry: the honest option ("ship approximated light, not no
light") is what the pipeline already does for every chunk.

**Chunk upgrade on the wire already has a precedent.** `crate::server`'s
`resend_column_for_light` fallback re-sends a whole column mid-session through
`begin_chunk_batch`/`encode_chunk`/`end_chunk_batch`. Client side, `World::load`
(`crates/lodestone-world/src/world.rs`) *replaces* the resident `LoadedChunk` (returns the
old one), and the shell treats `ClientEvent::ChunkLoaded` as a dirty-region signal
(`net.rs`) that re-meshes. So "second `LEVEL_CHUNK_WITH_LIGHT` for a resident column" is an
established, exercised path — the upgrade mechanism is a re-send, not a new packet.

**Mutation has a single choke point.** Everything that mutates the world goes through
`ChunkSource::set_block` — `region_source.rs`'s module doc states it and hooks persistence
there. `ChunkStore` (LRU cache) → `RegionChunkSource` (disk + permanent edit map + dirty
set) → `OverworldChunkSource` (generator) is the production stack;
`DimensionalSource` wraps it per dimension. Only the **dirty set** is ever saved, and saved
chunk NBT writes `Status = "minecraft:full"` (`chunk_nbt::column_to_nbt_with`).

**Streaming machinery.** `ViewTracker` diffs the view on movement; `send_view_update` feeds
newly-visible coordinates to `ColumnPipeline::enqueue` (the primed sliding window, width =
`available_parallelism`, one column emitted per `select!` pass); a `LoopStallWatch` names
any arm stalling > 200 ms; keep-alive forgiveness is denominated in serviced time. On
wasm32 the window is 1 and generation is inline. Tickets (`ticket.rs`) are a separate
residency axis with a two-state `ChunkStatus` (`Empty`/`Full`).

**Cost figures are stale and mutually inconsistent — this is a finding, not a footnote.**
`chunk-store.md` measured a cold column at **~909 ms** (mean of 4) when the store landed;
`join-scheduler.md`'s window sweep measured a 289-column burst at **3.67 s at window 8**
(~12.7 ms/column parallel, ~33 ms serial implied). Those differ by ~27× and were taken
before/after the worldgen rewrite. The old cost decomposition (vegetation ~62%, ore ~18% —
implying shaped ≈ 20% of full) also predates the rewrite. **Nobody currently knows what a
shaped column saves.** Stage 0 exists because building this design on the 62% figure would
be building on a number this repo has already replaced once.

**Client ceiling facts** (for the honest-maximum question): shell render distance is
`MIN_RENDER_DISTANCE = 2` to `MAX_RENDER_DISTANCE = 32` (`shell/src/config.rs`), server
view radius = render distance + 1, clamped per-connection in
`ViewTracker::set_view_radius`. Server-side residency is **31.1 KiB/column packed,
measured** (139.2 MiB at the slider max — residency figures, not per-frame). Client mesh
VRAM at render distance 8 is ~67 MB live (residency). Meshing is **96.3% of the client
chunk path** (~63 M instructions/column post-optimisation). The terrain draw now has a real
distance ∩ frustum ∩ occlusion cull (`TerrainCull` in `gpu/frame.rs` — the older "no cull"
claim in `client-chunk-cycles.md` is stale), but mesh *generation* cost and VRAM still
scale linearly with resident columns.

---

## The stage model

Two sendable tiers, expressed as a monotone lattice. More tiers are possible later; two is
what the fidelity ruling justifies today.

```
GenStage::Shaped   = stages 0a–4  (structures, fill, surface, carve)  — sendable
GenStage::Full     = stages 0a–7 + spawn stage (today's column())     — sendable
```

plus one orthogonal flag the persistence layer already owns: **modified** (present in
`RegionChunkSource`'s edit map / dirty set). The serving rule is a `max`:

```
served(column) = max(stage_already_generated, stage_requested_by_band)
```

so a Full or modified column is **always sent whole** regardless of distance — the owner's
second constraint holds structurally, not by a check that can be forgotten. Downgrades do
not exist: no code path converts a Full column to Shaped, and the store never overwrites a
higher-stage entry with a lower one.

What a Shaped column *lacks*: ores (invisible inside stone — zero perceptual cost),
vegetation (trees — the one visible difference, see the experiment), top-layer snow/ice
(a surface-colour pop in cold biomes), generation-time creature spawns (deliberately: no
mobs may exist in a chunk the player cannot interact with), and generated block entities
from vegetation (bee nests). What it *keeps*: real terrain shape, caves, rivers, surface
blocks, structure pieces, real light and heightmap at encode.

**Distance bands.** Per connection: Chebyshev distance ≤ `R_full` from the view centre ⇒
Full; beyond, up to the (raised) view radius ⇒ Shaped. `R_full`'s default comes from the
Stage 5 experiment; its **floor is 8** regardless — the ticked area (radius 3), mob
simulation, interaction reach and explosion radii must all sit strictly inside the Full
band with margin, so the edit-authority guard is a backstop rather than a load-bearing
gate. The band is computed in `ViewTracker` from ring distance (the pipeline already knows
it); tickets stay orthogonal (residency, not stage) in this design — see rejected
alternatives for the vanilla-style unification and why not now.

---

## Answers to the six questions

### 1. What does "512" mean, and what is the real ceiling?

The owner wrote "more chunks (eg 512)", so: **512 chunks of render distance.** Deriving
the column count at view radius r+1: `(2(r+1)+1)²`.

| render distance | columns | server RSS @31.1 KiB/col (residency) | client mesh VRAM, linear from 67 MB @ rd 8 (residency) | wire bytes @~55 KB/col |
|---|---|---|---|---|
| 8 | 361 | 15.5 MiB (measured) | ~67 MB (measured) | ~20 MB |
| 32 (today's max) | 4,489 | 139.2 MiB (measured) | ~830 MB | ~250 MB |
| 64 | 16,900 | ~510 MiB | ~3.1 GB | ~930 MB |
| 128 | 66,564 | ~2.0 GiB | ~12 GB | ~3.7 GB |
| 512 | 1,050,625 | ~32 GiB | ~195 GB | ~58 GB |

Every extrapolated figure above is labelled: all are **residency**, scaled linearly from a
measured anchor; none is a per-frame quantity. The 31.1 KiB/column rate was measured flat
across an 8.9× range, so the server column is trustworthy to ~rd 128; the client mesh
column assumes today's per-column mesh representation, which is exactly the thing that
would have to change.

**Honest answer: rd 512 is not reachable by this design or any generation-side design.**
At 512 the problem is storage, meshing and bandwidth, three orders of magnitude before it
is generation. What staged generation buys is the **generation-cost** half of raising the
cap to **48–64** (4× vanilla's area at 64), and it is the necessary substrate (cheap far
terrain) for a future LOD-mesh tier (Distant Horizons-class: merged low-resolution meshes,
sparse column storage, a different wire format) that could reach 128–512. That tier is a
separate design and deliberately out of scope; this plan should not pretend to be it.
Recommendation to the owner, stated plainly: this plan targets rd 64 max; beyond that
requires client-side LOD meshing work that no server-side staging can substitute for.

### 2. Which stages are sendable, and what does the client do with each?

Sendable: `Shaped` and `Full` (table above). The client needs **no changes to render
either** — a Shaped column is a well-formed column; light and heightmaps are computed at
encode from its real blocks by the same `compute_served_light` path every chunk already
uses, so the black-chunk failure mode (`LightData::Missing` → sky 0) is unreachable on the
wire. The perceptual cost of Shaped is the **aggregate skyline**: a forest without trees
reads as a plain, and the moment a boundary chunk upgrades, the horizon silhouette pops.
That — not per-tree visibility — is what the Stage 5 experiment measures. Top-layer snow
is a second, cheaper pop (surface colour in cold biomes); if the experiment shows it,
moving the freeze pass into Shaped is a candidate — but `top_layer_stage` currently runs
after vegetation, so that is a restructuring to *measure first*, not assume.

### 3. How does a chunk get upgraded on the wire?

**Full re-send of `LEVEL_CHUNK_WITH_LIGHT` inside a normal chunk batch.** Established
facts, not assumptions: `World::load` replaces the resident chunk; the shell treats
`ChunkLoaded` as a dirty-region signal and re-meshes; `resend_column_for_light`'s fallback
already exercises mid-session whole-column re-sends against our client. No new packet, no
delta encoding (rejected below). One fact the executing agent must pin with a gate rather
than trust: **neighbouring sections re-mesh at the seam** when a column is replaced — the
`Relit::dirty_sections` transposition history says seams are exactly where a
green-looking re-mesh path hides a stale face.

Upgrades ride the existing `ColumnPipeline` (stage-tagged jobs), so pacing, priority
(nearest-first via `view_order_key`), re-prioritisation while walking, and the one-column-
per-`select!`-pass discipline all come for free — the machinery that fixed the 361-column
unserviced-window disconnect is the machinery upgrades must go through. An upgrade is
strictly cheaper than a first full generation when the staged store still holds `pre_ore`
(the memo makes upgrade = ore + vegetation + top-layer only), and costs a full generation
when evicted — correct either way, cost measured in Stage 0.

### 4. How are "already generated" and "modified" tracked and persisted?

In memory: a `GenStage` field on the server's `ChunkColumn`, set by the generator seam
(`from_generated` marks Full; the new shaped constructor marks Shaped) and stored with the
`ChunkStore` entry. Modified: unchanged — `RegionChunkSource`'s edit map and dirty set,
which already outlive eviction.

Persistence: **Shaped columns are never persisted.** The save path already writes only the
dirty set, and a Shaped column can never enter it (the edit constraint forbids mutation).
So: no new NBT field, `Status = "minecraft:full"` stays truthful for every chunk we write,
a chunk generated to Shaped in a previous session simply regenerates (the generator is
deterministic — byte-identity is already gated), and a saved world round-trips exactly as
today. A chunk **on disk** is served Full at any distance: `RegionChunkSource::column`
already consults disk before the generator, and the stage-aware read must preserve that
disk-wins rule — this is precisely the owner's "see the structure they made from far
away" case, and it gets a dedicated gate with a failing control.

One verification item for the executing agent: what our loader does with a *vanilla* save
containing sub-`full` `Status` chunks (vanilla proto-chunks). The correct rule is "treat
non-full as absent, regenerate"; whether the current parse does that is a fact to
establish, with `.cache/mc/survival/world` as the oracle.

### 5. How is the edit constraint enforced, and where?

**Choke point: `ChunkSource::set_block` at the `ChunkStore` layer** — the same single
funnel persistence already hooks. Two-level enforcement:

- **Player-facing arms** (dig / place / use-item in `dispatch_play_packet`) consult the
  stage authority *before* mutating. On refusal: no server mutation, and the client is
  re-synced with the authoritative block (the existing rejected-placement resync shape),
  so the player sees the placement visibly revert — never a silent drop. Reachable in
  practice only via teleport into a not-yet-upgraded band, since `R_full ≥ 8` keeps reach
  distance inside Full terrain.
- **The `set_block` guard** for everything else (commands, fluids, redstone, explosions,
  mob systems): a write to a non-Full column is **dropped, counted, and warn-logged** —
  it indicates a system operating outside its domain (all simulation is inside the Full
  band by the floor), so it is a defect signal, not a UX path. Counted with a counter the
  gates can read, because a dropped write with no counter is an invisible correctness
  hole.

Deliberately rejected: auto-upgrade-then-apply at the choke point. It would put a
synchronous full generation on whatever thread wrote (the tick thread as often as not) —
the same class as the 909 ms-on-the-tick-thread defect this repo already paid for.

Lock order is part of the design, not an implementation detail (the saved-chunk
self-deadlock precedent): the stage check happens inside the store's existing single cache
lock acquisition in `set_block`; the guard must not call anything that can re-enter the
store (`column`, `block_state`, a scheduled-tick handle) while holding it.

### 6. What crosses chunk boundaries and breaks under partial generation?

- **Vegetation spill is the seam.** A tree in Full chunk C whose canopy overhangs Shaped
  neighbour D: C's pass drops spill into D by design ("the neighbour's own pass produces
  its own copy" — `vegetation_stage`'s doc), and D's own pass never ran. Result: trunks
  with clipped canopies along the band boundary until D upgrades. **The discriminating
  input is a forest column at the band boundary with a Shaped neighbour** — the analogue
  of the water-seam bug's isolated column. Every gate fixture must assert its census
  (trees actually present in the boundary ring); an ocean fixture passes vacuously, and
  the selection-metric hazard applies: ask what the wrong hypothesis scores on the
  fixture before trusting it.
- **Structures do not break**: starts/refs/pieces are all at or below pre-ore, and the
  staged store's pinned closure (`STRUCTURE_CLOSURE_RADIUS = 10`) already handles their
  neighbourhood.
- **Carvers** are inside pre-ore; no new seam.
- **Light** at the band boundary is the isolated-column flood we already ship everywhere;
  upgrades re-send light with the column. No new class of artefact.
- **Heightmap/`MOTION_BLOCKING`** changes on upgrade (trees raise it) — re-sent with the
  column, consumed only client-side today.
- **The staged store's derived constants stop holding at large radii**: `STORE_RETENTION
  = 2048` was derived from a 289-column burst closure (1,369). A shaped sweep at rd 64 is
  16,900 columns; the contiguous window keeps the *pinned* set small so this is churn
  (recompute on upgrade), not corruption — but `store_evictions()` during a banded join is
  a Stage 0 measurement, and the retention may need re-deriving from the band config.

---

## Staged implementation plan

Every stage is independently landable and gated; each gate names its failing control.
Crate ownership per stage is listed so an orchestrator can assign without collisions.
`server.rs` and `tick.rs` are choke files — stages touching `server.rs` (3, 4) should not
run concurrently with each other, and nothing here touches `tick.rs` at all (the
tick loop reads through `ChunkSource::column`, which keeps meaning Full — see Stage 2).

### Stage 0 — measure before building (go/no-go)

**Owns:** a new `#[ignore]`d measurement harness (own test binary, worldgen or server
crate); no production code.

Re-measure, with instructions retired (the `proc_pid_rusage` precedent) and the existing
`gen-counters` stage counters, on real embedded data:

1. Per-column cost of `Shaped` vs `Full`, per internal stage, over ≥ 3 distinct terrains
   (forest, mountains, ocean — census-asserted). This replaces the stale 909 ms / 62% /
   33 ms figures with current ones.
2. Upgrade cost with warm vs evicted `pre_ore` memo.
3. `store_evictions()` and RSS during a simulated banded join at rd 32 / 48 / 64.
4. Shaped-column packed size vs the 31.1 KiB Full figure.

**Gate:** the harness's fixture guards (world-species controls): shaped bytes ≠ full bytes
on a vegetated fixture, shaped bytes == full bytes through stage 4 (pre-ore identity), and
the forest census. **Control:** run the census assertion against an ocean seed and watch
it refuse the fixture.

**Go/no-go:** if Shaped ≤ ~50% of Full, the design pays; if the rewrite has made full
generation cheap enough that encode/mesh dominate (a real possibility the join-sweep
numbers hint at), **stop here and say so** — the right project is then client mesh LOD +
a straight cap raise, and this plan's remaining stages are the wrong spend.

**Result: GO.** Measured in
`crates/lodestone-worldgen/tests/stage0_shaped_vs_full_cost.rs` (release profile, real
embedded production worldgen data via `lodestone_server::overworld_generator`, seed 42),
run with a sibling agent compiling in the same checkout, which is why instructions
retired rather than wall clock carries the upgrade-cost comparison.

*Per-stage cost, cold, three census-verified terrains (4 columns each, 12 total),
`OverworldGenerator::column_timed`'s existing ten-bucket split summed into "shaped"
(stages 0a-4: aquifer + shape + biome + surface + materialize + carve, which includes
`structure_place_stage` — it runs inside the `carve` bucket) vs "full" (shaped + ore +
vegetation + top_layer), both charged the same measured `intern` cost (a conservative
bias — see the harness's module doc for why a real Shaped column's own intern would
likely cost less, not more):*

| terrain | columns | shaped-serve µs (total) | full µs (total) | ratio |
|---|---|---|---|---|
| forest | 4 | 422,382 | 2,222,684 | **0.1900** |
| mountains | 4 | 479,311 | 1,782,607 | **0.2689** |
| ocean | 4 | 249,311 | 841,999 | **0.2961** |
| **all 12** | 12 | 1,151,004 | 4,847,290 | **overall 0.2375, median-per-column 0.1261** |

The gap between the overall ratio (0.2375) and the median-per-column ratio (0.1261) is
itself informative, not noise: each terrain's 4 columns share one generator, so only the
*first* column per terrain pays a cold structure-start closure (one sample hit 381,472 µs
in the `aquifer` bucket against a 992 µs p50 — the closure-warming outlier), which
inflates the *total*-based ratio; the median is closer to steady-state in-session cost.
Both are far under the 0.50 threshold either way. **Control** (the instrument can tell
terrain apart, not just report noise): forest's total vegetation-stage cost (1,491,897 µs)
exceeds ocean's (492,333 µs), asserted and passing.

This lands close to the pre-rewrite "vegetation ~62%, shaped ≈ 20%" estimate this Stage
exists to distrust — a coincidence worth flagging as such rather than treating as
confirmation, since the 909 ms and 33 ms-serial figures it was bundled with were 27×
apart and are exactly what is being replaced.

*Upgrade cost, instructions retired (Darwin `proc_pid_rusage`, release), one forest
column at (10, −64), gen-counters-verified:*

| arm | priming | instructions | `pre_ore_computed` | `structure_starts_computed` |
|---|---|---|---|---|
| evicted (fresh generator) | none | 55,400,314,137 | 25 (the documented 5×5 closure) | 441 (the documented 21×21 closure) |
| warm, narrow priming | `ore_stage_for_profiling` at the target chunk only | 30,841,465,147 | **16** (still cold — the priming's 3×3 closure was narrower than `column()`'s real 5×5 vegetation-read rim) | not measured this arm |
| warm, full-neighbourhood priming | `ore_stage_for_profiling` tiled over a 3×3 grid spaced 2 chunks apart (unions to the full closure) | 522,615,211 | **0** | **0** |

Warm/evicted with full-neighbourhood priming: **0.0094** (≈1% of a cold generation).
**This number should not be read on the same scale as the per-stage 0.24 ratio above** —
the counter columns show why: it is dominated by structure-closure amortisation (441 vs
0 `structure_starts_computed`), not only by skipping ore/vegetation/top-layer. It is the
realistic in-session number (a player approaching a Shaped column from inside the Full
band has, by construction, already caused nearby structure closures to be computed), and
the "evicted" arm is the pathological one Stage 0 was asked for: a genuinely cold
generator's first request near unexplored territory, dominated by the one-time 21×21
structure-starts closure this repo's own `overworld::STRUCTURE_CLOSURE_RADIUS` doc
already names as the widest closure in the pipeline. Both arms confirm
`OverworldGenerator::column`'s documented closure sizes exactly (25 and 441), which is
independent evidence the harness is measuring what it claims to.

*Store pressure — today's all-Full behaviour (no Shaped tier exists yet to band), full
raster sweep from one generator, release, RSS growth from a post-warm-up baseline
(`benches/generation.rs`'s `bench_region_rss` methodology):*

| rd | columns | `store_len()` | `store_evictions()` | RSS growth | bytes/column |
|---|---|---|---|---|---|
| 8 | 289 | 1,369 | 0 | 155.3 MiB | 563,519 |
| 16 | 1,089 | 2,048 (at `STORE_RETENTION`) | 761 | 400.6 MiB | 385,769 |
| 24 | 2,401 | 2,048 | 3,121 | 460.7 MiB | 201,214 |
| 32 | 4,225 | 2,048 | 5,639 | 479.8 MiB | 119,078 |
| 64 | 16,641 | not completed (see below) | not completed | not completed | not completed |

`store_len()` hits the `STORE_RETENTION = 2048` ceiling by rd 16 and stays pinned there;
eviction count grows roughly with swept area past that point. RSS growth *per column*
falls as the sweep widens (563 KiB → 386 KiB → 201 KiB → 119 KiB) and the *total* growth
plateaus around 460-480 MiB from rd 24 onward — the store's own eviction discipline
appears to bound resident memory to a few hundred MiB regardless of how far a session
explores, rather than growing unbounded, which is reassuring for Stage 6's capacity-policy
work but was not derived analytically here (this repo's own history — the 2.9-7.4×
`STRUCTURE_CLOSURE_RADIUS` surprise — is exactly why this table is measured and not
extrapolated from rd 8-32).

**rd 64 did not finish inside this measurement session and its row is honestly reported
as absent, not extrapolated or predicted.** rd 8/16/24/32 each scaled close to linearly in
wall time (34.2, 29.9, 27.3, 28.6 ms/column, from cargo test's own reported duration) —
flat enough that rd 64's 16,641 columns predicted roughly 8 minutes. The real run (sampled
by `ps` repeatedly, never by a blind wait — see `CLAUDE.md`'s rule against diagnosing
health from anything but the child process itself) was still healthy and CPU-bound
(75-93%) after **21+ minutes**, run concurrently with a sibling agent's own build on this
shared checkout, and `sysctl vm.swapusage` showed real memory pressure (up to 4.5 of 5.0
GiB swap used) throughout. Two honest hypotheses, not adjudicated here: ordinary
shared-machine contention (the swap pressure alone could explain a 2-3× slowdown), or a
genuine super-linear cost at this scale — a `2,048`-entry retention ceiling serving a
16,641-column raster sweep means, past roughly the first `STORE_RETENTION`-sized stripe,
almost no column's neighbourhood survives in the store long enough for its raster
neighbour to reuse it, so every column may pay close to a full closure recompute rather
than the partial reuse smaller sweeps still got — precisely the shape of the *measured*
2.9-7.4× `STRUCTURE_CLOSURE_RADIUS` surprise this repo's own history already recorded at a
smaller scale. **Re-run `stage0_store_pressure_rd64` alone on an idle machine** (`cargo
test --release -p lodestone-worldgen --test stage0_shaped_vs_full_cost -- --ignored
--test-threads=1 --nocapture stage0_store_pressure_rd64`) before Stage 6 re-derives
`STORE_RETENTION`/capacity policy at rd 48-64 — if the second hypothesis holds, retention
sized for a 289-1,369-column burst is the wrong constant for a sustained rd-64 session and
needs its own derivation, not a scaled-up one.

**Not measured**: Shaped-column packed byte size vs the 31.1 KiB Full figure. There is no
`column_shaped` seam yet (that is Stage 1's own deliverable), so nothing here can produce
a *true* Shaped `ChunkColumn` to pack and measure — see the harness's module doc. Revisit
once Stage 1 lands `column_shaped`; `chunk_memory.rs`'s
`the_packed_grid_costs_a_fraction_of_the_flat_one_on_a_real_column` is the pattern to
extend.

### Stage 1 — name the generator seam

**Owns:** `crates/lodestone-worldgen` only.

`OverworldGenerator::column_shaped(cx, cz) -> GeneratedColumn` (public seam over the
existing `pre_ore` memo, converting `PreOreResult` to output without ore/veg/top-layer/
spawn stages), and a stage tag on `GeneratedColumn`.

**Gates:** (a) determinism/no-side-channel: `column_shaped` then `column` for the same
chunk yields a Full column byte-identical to calling `column` cold (the memo makes shaped
a pure prefix — this is the property upgrades rely on); (b) counter gate in its own
binary: a shaped sweep bumps `pre_ore_computed` and structure counters only,
`post_ore_computed == 0`, vegetation counter == 0. **Control:** the existing
byte-identity harness with one bit flipped still reports the diff; the counter gate run
against plain `column` must fail (post-ore ≠ 0) — observed failing, not described.

**Result: LANDED.** `OverworldGenerator::column_shaped(cx, cz)` in
`crates/lodestone-worldgen/src/overworld/mod.rs` is exactly `self.pre_ore_stage(cx, cz)` —
the same memoised call `column` itself makes first — fed through a widened
`intern_from_dense(cx, cz, stage: GenStage, …)` with an empty `block_entities` list.
`GenStage { Shaped, Full }` lives in `overworld/output.rs` as a field on `GeneratedColumn`
(`GeneratedColumn::stage()`), re-exported from `overworld`. Both existing call sites
(`column`, `column_timed`) now pass `GenStage::Full`. **Only `GenStage::Full` runs the
`SPAWN` stage** inside `intern_from_dense` — a `Shaped` column carries an empty
`spawn_candidates` unconditionally, satisfying "no mobs may exist in a chunk the player
cannot interact with" independently of whatever `world` it was handed. `motion_blocking`
is computed for both stages (a pure read of whatever blocks are present; nothing
downstream consumes it yet either way).

Gates in `crates/lodestone-worldgen/tests/stage1_shaped_seam.rs`, all `#[ignore]`d (real
generation against the embedded production generator, seed 42) and run
`--release`, `--features gen-counters`, `--test-threads=1`:

- **(a) byte identity** — `stage1_shaped_then_full_is_byte_identical_to_cold_full`, over
  two census-verified terrains (forest and mountains, never ocean — an ocean fixture would
  pass vacuously per this repo's own "coincident hypotheses" rule): `column_shaped` then
  `column` on one generator, compared via `into_raw()` against `column` cold on an
  independent generator. **Passed on both**: forest chunk (10, −64), 39-entry palette,
  32,163 non-air cells; mountains chunk (44, −51), 23-entry palette, 42,644 non-air cells —
  exact tuple equality (palette, blocks, biome_quarts), not a summary statistic. **Control**
  (in the same test, on a clone of the matching side): flipping one block id makes the
  comparison disagree — observed, not described.
- **(b) counter gate** — `stage1_shaped_sweep_touches_only_pre_ore_and_structure_stages`,
  a 3×3 shaped sweep: **`pre_ore_computed=9`, `structure_starts_computed=361`,
  `post_ore_computed=0`, vegetation `stage_entered=0`.** **Control**
  (`stage1_control_full_sweep_trips_the_post_ore_and_vegetation_counters`), the identical
  3×3 sweep through plain `column()` on an independent generator: **`post_ore_computed=25`,
  vegetation `stage_entered=9`** — both nonzero, the counter gate's hypothesis observed
  failing against the thing it exists to distinguish `column_shaped` from.

A third check, `stage1_shaped_column_contains_a_real_structure`, answers the report
question directly: **yes** — a shaped column really does contain a structure. Found via
`structure_starts_placed_in` (mineshafts have `spacing = 1`, the densest structure set to
search), a real `minecraft:mineshaft` start at the room piece's own chunk (−7, −29)
(`pieces[0]` is always the room — `mineshaft.rs`'s own `the room is added first` test),
whose `column_shaped` palette (15 entries) carries real placed structure blocks (rail/
cobweb/plank/fence prefixes). **First cut of this check picked an arbitrary chunk
`structure_starts_placed_in` returned non-empty for and found no telltale block there** —
not a bug: a mineshaft corridor's bounding box can cross a chunk seam while its rails and
planks sit on the other side. Rewritten to target the room piece's own bounding-box centre
instead, which is guaranteed to carry the telltale material. **Control**: a nearby chunk
with no structure start placed in it, (−24, −24), carries none of the telltale blocks —
the detector discriminates rather than matching everything.

`cargo check -p lodestone-worldgen --all-targets` (default and `--features gen-counters`)
and `cargo check -p lodestone-worldgen --target wasm32-unknown-unknown` are clean — the new
code adds no clock or threading call, so it carries none of this crate's documented wasm32
hazards. `cargo test -p lodestone-worldgen --no-fail-fast` ran clean through every binary
it reached (0 failures across every completed suite, including the structure/vegetation
integration tests) before a 580s wall-clock budget cut it off partway through — a timeout,
not a failure; every binary that finished reported zero failures.

### Stage 2 — the server stage model and stage-aware store

**Owns:** `crates/lodestone-server`: `chunk.rs`, `chunk_store.rs`, `region_source.rs`,
`dimension.rs`. Not `server.rs`, not `tick.rs`.

- `GenStage` on the server `ChunkColumn`; `from_generated` tags Full.
- `ChunkSource::column_at(cx, cz, stage) -> ChunkColumn`, **with no default** — a
  defaulted trait method plus wrapper impls is this repo's measured island generator, and
  a compile error naming every unforwarded wrapper (`ChunkStore`, `RegionChunkSource`,
  `OverworldChunkSource`, `DimensionalSource`, `Arc<S>`, every test double in `server.rs`
  and the test crates) is the point. `column()` remains and is *defined* as
  `column_at(Full)` — every existing consumer (tick loop, mob sim, vitals probe,
  commands) transparently keeps full-generation semantics, which is what makes this stage
  landable without touching them.
- `ChunkStore`: entries carry stage; a `Full` request over a `Shaped` entry is a miss
  that regenerates/upgrades (generation with the lock released, exactly the existing miss
  discipline); a `Shaped` request over a `Full` entry is a hit returning Full. The
  first-writer-wins insert rule becomes "higher stage wins, and an entry carrying edits
  is never overwritten" — the second clause is unreachable by construction (edits imply
  Full) and asserted anyway.
- `RegionChunkSource::column_at`: **disk wins at any requested stage.** A chunk with
  saved data is returned Full regardless of the band asking.

**Gates:** monotonicity (counting source: shaped-then-full generates each stage's work
once, full-then-shaped generates nothing on the second read — counts with verdicts, on a
`CountingSource` with no memo, per the chunk-store trap note); disk-wins (open a saved
world, request `column_at(Shaped)` for an edited chunk, assert the edit is present).
**Controls:** neuter disk-wins in place (route around the disk consult) and watch the
gate fail naming the chunk; the existing
`edits_survive_both_a_reread_and_an_eviction` must stay green throughout.

### Stage 3 — edit authority

**Owns:** `crates/lodestone-server`: the `set_block` guard in `chunk_store.rs`, the
player-arm checks in `server.rs` (coordinate with Stage 4's `server.rs` ownership — do
not run concurrently).

As specified in question 5: check-and-resync at the player arms, drop-count-warn at the
choke point, lock order specified, counter exported.

**Gates:** (a) over a real loopback connection (`serve_play.rs` shape), a place packet
targeting a Shaped chunk produces a `BLOCK_UPDATE` resync carrying the *old* state and no
server-side mutation (assert via `block_state` after, count == 0 mutations); (b) the
persistence gate: after a banded session touching many Shaped columns, the save sweep
writes zero of them (a count with a verdict, not an eyeball). **Controls:** force-allow
the guard and watch gate (b) fail with a non-zero count; run gate (a)'s assertion
against a Full chunk and watch the placement succeed (proves the detector distinguishes,
not merely rejects everything).

### Stage 4 — banded streaming and upgrades on the wire

**Owns:** `crates/lodestone-server`: `server.rs` (`ViewTracker`, `send_view_update`),
`join_scheduler.rs`.

- `ViewTracker` computes per-column band from ring distance and `R_full`; tracks the
  stage last *sent* per column (it already owns the sent set).
- `ColumnPipeline` jobs carry a stage; the worker calls `column_at(stage)`.
- `recenter`/`set_view_radius` diffs now yield two sets: newly-visible (send at band
  stage) and band-crossed (Shaped-sent columns now inside `R_full` — enqueue as
  upgrades). Both ride the same pipeline, nearest-first; upgrades never bypass batch
  flow control.
- wasm32 arm: same band logic, inline generation as today (window 1, no threads, no new
  clocks — every timing addition stays `cfg(not(target_arch = "wasm32"))`).

**Gates** (all through a real `IntegratedServer` over loopback — the production path, not
a hermetic harness): (a) join at radius > `R_full`: the wire carries exactly
`(2·R_full+1)²` Full columns and the rest Shaped — derived arithmetic, not an observed
snapshot; assert by *decoding* the served columns and probing a vegetation-bearing cell
(census-asserted fixture) rather than trusting a flag; (b) walking across a band boundary
re-sends exactly the crossed columns, nearest-first, and the client's decoded world
gains the vegetation (the upgrade closes the shaped/full diff to zero — count); (c) the
existing latency gate shape: a play packet is serviced before the last column of a
teleport that enqueues both a full view and its upgrades — the columns-per-unserviced-
window number this design multiplies is the thing this gate bounds; (d)
keep-alive survival across that teleport. **Controls:** (b) with the upgrade set routed
to `None` must fail its echo assertion (the measured precedent); (a) with the band
threshold set to 0 must report all-Shaped and fail the Full count.

### Stage 5 — the experiment the owner asked for

**Owns:** `crates/lodestone-shell` test harness (`#[ignore]`d, GPU-gated); no production
tuning constants land before this runs.

Sweep the stage-vs-distance matrix and put numbers and screenshots in front of the owner:

- **Arms:** same seed, same eye, arm A all-Full, arm B banded at `R_full ∈ {8, 12, 16,
  24, 32}`; a third arm captures a single boundary-chunk upgrade (before/after frames).
- **Captured per point:** full-frame rasterised diff against the all-Full reference
  (never a vertex-sampled probe — a probe smaller than a quad is blind to it; never a
  hardcoded sky constant — the reference frame is the diff baseline, which sidesteps the
  `SkyFrame::clear_color` trap that produced a zero-pixel detector here before), diff
  pixel **count** bucketed by screen region, and the **bounding boxes** of diff blobs —
  checked to localise (a degenerate or suspiciously round box across arms is a broken
  transform, per the `opaque_ink` incident). Instrument hygiene from the repo's own
  table: dummy third-person body source installed; fixture light real (served), not
  hermetic-Missing.
- **Fixture guards:** the boundary ring census-asserts trees (forest) in the Full arm;
  a run whose boundary ring is ocean is refused, not reported as "no pop".
- **What is decided:** the smallest `R_full` at which (i) the steady-state banded frame's
  diff is confined to the horizon band, and (ii) the single-chunk upgrade diff's bounding
  box stays inside that chunk's screen rect at threshold size. No plausible round number
  is predicted; the harness prints the curve and the owner picks the operating point
  interactively (his stated preference). The chosen default lands as a constant with the
  measured curve in its doc comment.
- Also captured while the harness exists: whether top-layer snow pop is visible enough to
  justify restructuring the freeze pass into Shaped.

**Gate/control:** the harness's own premise controls (reference arm diff-to-itself == 0;
a deliberately removed boundary section produces a non-zero, localised diff — the
detector demonstrated firing before any conclusion is read from it).

### Stage 6 — raise the ceiling

**Owns:** `crates/lodestone-shell` (`config.rs`, fog, slider), `lodestone-server`
capacity policy (`chunk_store.rs`), docs.

- `MAX_RENDER_DISTANCE` 32 → 64 (native; browser build keeps a lower cap — debug wasm
  worldgen is ~10× slower and single-threaded, and the singleplayer probe has a 30 s
  deadline; shaped generation *helps* there but the cap raise is not for wasm).
- Re-derive `ChunkStore` capacity policy for banded views (Full-band columns +
  Shaped-band columns at their measured sizes; re-run the RSS pair per the
  chunk-store doc's own instruction; hosted `MAX_CAPACITY` re-argued, not just raised).
- Re-run `join_parallel_efficiency`'s sweep at the new radii — the window optimum was
  measured on 289-column bursts and the U-curve's right side is a cache bound that a
  16,900-column banded sweep may move.

**Gates:** the existing `view_radius_store_capacity.rs` suite extended with a banded row
(its computed-floor discipline unchanged); RSS measured at rd 48/64 and recorded next to
the 139.2 MiB anchor. **Control:** the suite's existing capacity-cap negative control at
the new radii.

---

## What could make this the wrong design

- **The premise may have expired.** The 62%-vegetation / 909 ms cost structure predates
  the worldgen rewrite; if Stage 0 finds Shaped ≥ ~50% of Full, mip-levels buy a factor
  nobody will feel, and the honest conclusion is to spend on client mesh LOD instead.
  This is why Stage 0 is a go/no-go, not a formality.
- **The owner's "512" does not survive the arithmetic** (question 1): staged generation
  addresses generation cost only, which is the third-largest problem at that radius. The
  plan says so rather than agreeing; rd 64 is the deliverable, LOD meshing the named
  follow-on.
- **Rejected: a full twelve-status generation pipeline with per-status neighbour radii** —
  the real model the actual generation server uses. It is the "real" version of
  this design, and it would mean rebuilding the staged store's dependency graph as an
  explicit status machine — a rewrite of a subsystem that already encodes those
  dependencies in its memo slots, for no player-visible gain over two tiers. Revisit only
  if tier count grows past ~3.
- **Rejected: delta upgrades** (multi-block-change packets for the veg diff). A single
  tree is hundreds of cells across multiple sections; a column's vegetation diff
  routinely touches thousands. The whole-column re-send is one packet the client already
  handles, and batch flow control already paces it.
- **Rejected: ticket-graph-driven stages** (map ticket level → stage, vanilla-style).
  Elegant, but tickets are currently residency-only, per-world rather than
  per-connection, and the band is a per-connection wire question; coupling them now
  drags `ticket.rs` and its live gates into every stage. The design leaves a seam (band
  from ring distance is computed where ticket levels are also known) so a later
  unification is a refactor, not a redesign.
- **Rejected: client-side generation of far terrain** (shared seed): breaks server
  authority for modified chunks — the exact case the owner's constraint names — and
  costs the browser build the most.
- **Rejected: auto-upgrade-on-edit** at the choke point (see question 5): synchronous
  full generation on the tick thread.

## Where the risk actually is, ranked

1. **Stage 0 invalidates the payoff** — measured shaped/full ratio too small. Cheap to
   discover, terminal for the design; that is why it is first.
2. **Band-boundary seams** (clipped canopies, snowline pop) read as broken rather than
   as LOD. Perceptual, owner-tunable via `R_full`, but the failure mode is "ugly at
   every boundary all the time", and no unit test measures ugliness — Stage 5 and the
   owner's eyes are the gate.
3. **Latency regressions on teleport/band-cross** — this design multiplies columns in
   flight (view + upgrades). The pipeline and watchdog were built for exactly this, and
   the columns-per-unserviced-window gate (Stage 4c) is the guard; the hazard is an
   implementer bypassing the pipeline "just for upgrades".
4. **Trait-change fan-out** (`column_at` across every `ChunkSource` impl and test
   double). Mechanical, wide, and the no-default choice makes the compiler enumerate it —
   tedious by design, silent-failure-free by design.
5. **Edit-authority holes**: commands (`/setblock`, `/fill`) at distance, explosions or
   fluid at the band edge, and any *future* system that writes outside the ticked area.
   The dropped-write counter is the tripwire; the floor on `R_full` is the real defence.
6. **Store/retention interactions at scale** — `STORE_RETENTION`, capacity policy and
   the window optimum were all derived at rd ≤ 16 geometries; every derived constant
   needs its derivation re-run, and the repo's history says a derived constant left
   stale is a measured 4× regression (the pin-radius incident).
7. **Persistence edge cases** — vanilla saves with sub-full chunks; the "shaped never
   dirty" invariant if some future system writes to columns outside Full (same tripwire
   as 5).
8. **wasm32** — lowest risk: banded logic is target-independent, generation is already
   inline there, shaped columns make the probe's 30 s deadline *easier*; the only trap
   is a new clock or thread in shared code, and `wasm-check`'s 22+ rules exist.
