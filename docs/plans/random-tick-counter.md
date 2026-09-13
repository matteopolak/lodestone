# Plan: incremental random-tick section counters

## What it is

The implemented replacement for `random_tick.rs`'s per-tick section scan uses an incrementally
maintained per-section counter — kept correct by every mutation path, so
"does this section randomly tick" becomes an O(1) integer compare instead of an O(blocks)
scan per column per tick. The implementation must preserve the documented block-state and
random-draw behaviour, including the independent reference checks below.

## Verified current state

The 4096-block string scan put **97.6%** (3,939 of 4,034 samples) of the integrated server's
tick thread in `is_randomly_ticking`; it measured **2.108 ms/column** (98,304 calls × 21.45 ns),
while rings 5–8 never delivered. The random-tick loop iterates `tick_area`, not the streamed view:
`tick_area` is `mob_area` (`open_in_memory_with_mobs_using`, `integrated.rs`) at radius `view_radius.clamp(1, 3)`
(`run_async`, `crates/lodestone-shell/src/net.rs`) — a 7×7 square, **49 columns**, as `integrated.rs` states independently.
This is **103 ms per 50 ms tick, 2.07× over budget** against a `50 / 2.108 = 23.7`-column
headroom. Use the 49-column value when extending this analysis. The interim fix (classify
the palette once, scan palette **indices**) measures **38.7 µs/column, 54× cheaper**, and the
The historical fill-time figure is not retained as a current measurement. The counter path removes
the O(blocks) per-column, per-tick scan; its residual performance evidence belongs in a
reproducible benchmark.

**Reference behaviour:**

- Four bounded counters per section: non-empty blocks, fluids, ticking blocks, and ticking fluids.
- The single per-block write path maintains all four incrementally: decrement for the previous
  state if it ticked, increment for the new one. No other mutation path exists — every block write
  in the reference implementation funnels through this one method.
- A recount pass recomputes all four with one palette-aware counting pass; it is
  called by the constructor that adopts an existing, already-populated block container (the
  deserialization path). The empty-section constructor does **not** recalc — zero
  counters are correct for an empty section. The copy constructor copies the counters.
- A section's overall ticking flag is the OR of its block-ticking and fluid-ticking flags —
  **blocks OR fluids**, not blocks alone. The reference chunk-tick driver gates
  each section's tick-speed position draws on that OR. The only reference fluid that ticks at all
  is **lava**. See "Fluids" below for why
  this matters and why the fluid counter is still out of scope.

**Finding, not assumption: `ChunkColumn` has no per-section structure.**
`crates/lodestone-server/src/chunk.rs` stores one flat `blocks: Vec<u16>`
(`[(y_local * 16 + z) * 16 + x]`) indexing one **column-wide** `palette: Vec<String>`, plus
`min_y`/`height` and biome quarts. Sections exist only as implicit 16-row windows that
`random_tick.rs` and `chunk_nbt.rs` slice out arithmetically. So there is no existing struct to
hang a counter on; the counter must be a new per-column vector, one entry per 16-row window.
Two load-bearing consequences of the flat representation:

- The palette is **append-only**. `intern` pushes; nothing ever removes, remaps, or compacts a
  palette entry. (Verified by reading `chunk.rs` in full — `set_biome_quarts` touches only
  biomes; `palette`/`blocks` are `private`, so no other module can mutate them.) A per-palette-id
  classification therefore stays valid for the life of the column.
- `Clone` is derived, and `ChunkStore::column` hands out clones (3.1 µs each, measured in
  `chunk_store.rs`'s own record). A derived clone copies the new counter fields for free —
  the same property required of any retained copy.

## Implemented representation

`ChunkColumn` now contains these private fields alongside `palette` and `blocks`:

```rust
/// palette_ticking[id] == is_randomly_ticking(&palette[id]) — the persistent
/// form of the mask `randomly_ticking_palette_mask` rebuilt per tick until now.
palette_ticking: Vec<bool>,
/// section_ticking[s] = how many cells in 16-row window s hold a ticking state.
/// `u16`: maximum 4096 cells per section.
section_ticking: Vec<u16>,   // len = height.div_ceil(16)
```

Public read API (the consumer seam for `random_tick.rs`):

```rust
pub fn section_is_randomly_ticking(&self, section_min_y: i32) -> bool  // count > 0
pub fn has_randomly_ticking_block(&self) -> bool                       // any section > 0
```

Maintenance follows these mutation boundaries:

| reference boundary | ours | mechanism |
|---|---|---|
| the single block-write method, ±1 | `ChunkColumn::set_block` | read `old_id` before writing; if `palette_ticking[old] != palette_ticking[new]`, `±1` on `section_ticking[y_local / 16]` |
| constructor adopting a container → recount pass | `ChunkColumn::from_generated` | one O(cells) counting pass (`recalc_ticking_counts`, retained as a named production function) |
| empty-section ctor, no recalc | `ChunkColumn::new` | all-air ⇒ all-zero counters, correct by construction |
| copy ctor copies counters | `#[derive(Clone)]` | free |
| palette insertion | `intern` | classify each **new** palette entry once as it is appended: `palette_ticking.push(is_randomly_ticking(name))` |

Decrement policy: plain `-=` guarded by a `debug_assert!(count > 0)`, **not** `saturating_sub`.
A saturating decrement would mask exactly the maintenance bug this plan exists to prevent; an
underflow must panic in debug builds, loudly, at the mutation that caused it.

The predicate stays where it is: `random_tick::is_randomly_ticking` is the single definition
(it fans out to `growth_tick`'s crop/sapling/leaf predicates), and `chunk.rs` calls into it.
That is a same-crate module cycle (`random_tick` already uses `crate::chunk`), which Rust
permits; duplicating the predicate in `chunk.rs` would be the two-tables fork CLAUDE.md's
router incident warns about. Note the predicate is property-sensitive (`leaves_should_decay`
reads state properties), and palette entries are **full** state strings including properties,
so per-palette-entry classification is exact — the same argument the interim mask already
relies on.

The counters are **derived state, never serialized**. `chunk_nbt` does not write them; a column
loaded from disk rebuilds them (see census below). This means widening `is_randomly_ticking`
in a future change (new ticking family) cannot strand stale persisted counts — every counter
in existence was computed by the predicate compiled into the running binary.

### Rejected alternatives

- **Counter outside `ChunkColumn`** (a `(cx, cz) → counts` map in the scheduler or store):
  rejected. Columns are cloned out of the store, mutated as clones inside `tick_chunk`, and
  re-persisted cell-by-cell through `ChunkSource::set_block`; the mob sim keeps its own
  retained columns (`mobs.rs`'s `ChunkWorld`). An external map would have to observe every one
  of those flows — the "maintained on some paths and not others" failure, which is worse than
  the scan because it is wrong silently. Inside `ChunkColumn`, the counter travels with the
  data it describes through every clone, cache, and retained copy.
- **Lazy recount on first query** (`OnceCell`): rejected — complicates `Clone` and mutation
  invalidation for no gain; the eager pass is <0.1% of the generation cost that precedes it.
- **Per-section structs (the reference layout)**: the right long-term representation but a
  wholesale `ChunkColumn` rewrite touching every consumer of `raw_blocks()`/`raw_palette()`
  (`chunk_nbt`, the wire encoders, the store); out of scope here and not needed for O(1)
  decisions.

## Mutation-path census (the correctness crux)

Grepped as producers across the whole tree (`.set_block(`, `.set_solid(`, `from_generated(`,
`ChunkColumn::new(`), then each production path read. **Every block mutation in the server
crate funnels through `ChunkColumn::set_block`** — `palette` and `blocks` are private fields,
so this is compiler-enforced, not conventional. The full production census:

**Constructors** (initial count):

1. `ChunkColumn::new` — all-air; counters zeroed. Callers: `chunk_nbt::column_from_nbt` (region load,
   `chunk_nbt.rs`), `WorldgenChunkSource::column`, test fixtures.
2. `ChunkColumn::from_generated` — bulk adoption of the generator's palette + grid; the one
   place `recalc_ticking_counts` runs. Callers: `OverworldChunkSource::column` (every unedited
   request), `OverworldChunkSource::set_block` (edit-map seeding), `RegionChunkSource` via
   `self.column`.
3. `Clone` — `ChunkStore::column` (per-tick reads), `OverworldChunkSource::column` (edit-map
   hits). Derived; copies counters.

**The single mutator**, `ChunkColumn::set_block`, reached from:

- `ChunkColumn::set_solid` (delegates; `WorldgenChunkSource`, shell worldgen fixtures).
- **Player edits through the wire**: `server.rs`'s dig/place arms → `ChunkSource::set_block`
  impls, each of which mutates a retained `ChunkColumn` in place:
  `OverworldChunkSource::set_block` (edits map, `chunk.rs`), `ChunkStore::set_block`
  (cached entry, `chunk_store.rs`, plus forwarding to the inner source),
  `RegionChunkSource::set_block` (disk-seeded edits map, `region_source.rs`).
- **The tick loop** (`run_tick_loop`, `tick.rs`): random-tick mutations (grass/crop/sapling/leaf handlers
  mutate the passed column inside `tick_chunk`), scheduled-tick redstone writes,
  and everything `propagate_and_react` writes (gravity settles, dust power, hopper/door
  immediate flips) — all `column.set_block`, then re-persisted per event via `world.set_block`.
- **Mob grazing**: `ChunkWorld::set_block` (`mobs/world.rs`) on its own retained
  columns (sheep eat grass → dirt; `EatBlockGoal`).
- **World spawn platform**: `world_spawn.rs`.
- **Region load**: `chunk_nbt::column_from_nbt` (`chunk_nbt.rs`) — builds via `new` + per-cell `set_block`, so the load
  path needs **no separate recalc**: incremental maintenance covers it O(1) per cell. (Generation
  and load really are different entry points with different mechanisms — bulk-adopt-then-recalc
  vs. build-through-the-mutator — and the parity gate below fixtures both.)

**Paths that do not touch blocks**: `set_biome_quarts` (biomes only). **Paths that do not
exist**: palette remap/compaction (append-only, verified), any direct field write outside
`chunk.rs` (private fields).

**How a future mutation path is prevented from forgetting:**

1. **The compiler**: private fields force any new mutation through `set_block`/`intern` or
   through `chunk.rs` itself, where the recalc function and this plan's comment live.
2. **A permanent parity gate** (Gate A below): incremental counters vs. an independent recount
   after a scripted mutation storm — any future in-file bypass (e.g. a hypothetical bulk-fill
   or palette compaction that forgets the counters) diverges there.
3. **A debug tripwire in the consumer**: `tick_chunk` gains a `debug_assert!` comparing the
   counter decision against the definitional index-scan per section (debug builds only — this
   is the interim fix's 38.7 µs scan, affordable in every `cargo test` run, absent from
   release). A desync introduced by any future path fails the nearest debug run at the point
   of consumption, with the section named.

## Cost of the initial count — counter, not duration

Per this repo's rule, quantified by operation counts (this machine's wall clock reproduces to
10.8% at best):

- `from_generated` recalc: exactly **98,304 palette-index reads + `palette.len()` predicate
  evaluations**, once per column construction. Construction already writes/moves all 98,304
  cells and follows a full generation (~ms scale, per `chunk_store.rs`'s 909 µs regeneration
  record vs. 3.1 µs clone) — the pass adds less than one additional read of data just written.
- Region load: **zero extra passes** — O(1) counter delta inside each `set_block` the loader
  already makes.
- Per-tick saving: the interim scan re-reads up to 98,304 indices per column per tick
  (49 columns × 20 Hz — see the correction at the top of this document, not 361); the
  counter replaces that with ≤ 24 integer compares per column.
  Break-even is therefore **before the first tick completes**: one construction pass costs what
  one tick's scan of the same column cost, and the scan ran 20×/s forever.

The instrumented counter below pins this with an operation count rather than a stopwatch.

## Implemented counter path and residual work

Ordering rule: nothing is an island at any intermediate step. U1 and U2 land back-to-back in
the same session — U1's only consumer-in-waiting is U2, and that window is closed by landing
them as one sequence (U1's own gates consume the counters meanwhile, and the plan's answer to
"what consumes this?" for U1 is, explicitly, U2).

`ChunkColumn` now maintains the counters in `set_block`, `intern`, and `from_generated`.
`recalc_ticking_counts` handles bulk adoption, while the public section and column queries feed
the scheduler. The mutation census and gates below remain the maintenance contract.

### Counter maintenance gates

`palette_ticking` + `section_ticking` + `recalc_ticking_counts` + maintenance in
`set_block`/`intern`/`from_generated` + the two public read methods. `tick_chunk` consumes the
queries through its whole-column early exit and per-section gate.

**Gate A (counts are right), hermetic, permanent.** After *every step* of a scripted mutation
storm, `section_ticking` must equal an **independent recount written in the test** — a direct
walk of `raw_blocks()` applying `is_randomly_ticking` to `raw_palette()` entries, deliberately
*not* calling the production mask/scan helpers, so a shared bookkeeping bug cannot pass both
arms. (The predicate itself is shared: it is the spec's definition of "ticking", disclosed —
the thing under test is the bookkeeping, not the classification.) The storm must include, and
the gate must **assert it included** (hard preconditions that fail, never skip):

- a section crossing **0→1** (grass placed into an all-stone section) and **1→0** (the last
  ticking block in a section removed);
- a **ticking→ticking** replacement (crop age advance) with the count asserted *unchanged*;
- a same-state rewrite (no-op delta);
- a non-ticking→non-ticking write (stone→dirt — dirt does **not** tick; only grass does);
- a write in the column's top and bottom sections (partial-window indexing);
- **both construction entry points**: a real `OverworldChunkSource` column at a surface chunk
  (`from_generated` + recalc) *and* the same column round-tripped through `chunk_nbt`
  (`new` + per-cell `set_block`), with counters asserted equal across the round trip.

**World-species check** (the unreadable vacuity): before the storm, assert the fixture column
has ≥1 section with count > 0 **and** ≥1 with count == 0. A generated all-stone or all-air
column structurally cannot exercise a ticking counter; these preconditions make that fixture
fail instead of pass vacuously. The fixture is a real generator column (production transport),
not a hand-rolled source — the test resolves the implementation production serves.

**Control, observed failing:** a `#[cfg(test)]` method `corrupt_section_ticking(section, delta)`
desyncs one counter, and the gate's comparison is asserted to **diverge** (`assert_ne`) — a
permanent, always-run second arm proving the detector discriminates, not a described one.

### Scheduler counter gate

`tick_chunk` uses `column.has_randomly_ticking_block()` and
`column.section_is_randomly_ticking(section_min_y)` for the production decisions. Its
per-section `debug_assert!` compares each counter result with the definitional index scan. The
scan helpers stay `#[cfg(test)]` and power that tripwire and Gate B; they are neither production
hot-path code nor dead code.

**Parity spec: RNG draw order and count.** The per-(column, section, tick) boolean is the
*only* input that decides whether `tick_speed` position draws happen; identical booleans ⇒
identical `position_state` LCG stream ⇒ identical picked positions ⇒ identical behaviour-RNG
draw pattern. So proving boolean equality under mutation, plus one end-to-end sequence check,
proves the whole property.

**Gate B (draw-sequence parity), hermetic, permanent.** Drive `RandomTickScheduler::tick_chunk`
for K ticks over real generator columns at a **non-default** `tick_speed` (e.g. 7 — so nothing
silently assumes 3), with scripted mid-run `set_block` edits that flip a section 0→1 and 1→0
between ticks. In parallel, the test **replays the expected sequence itself** from public
primitives: for each tick, compute each section's boolean by the definitional index scan, and
for each true section advance a shadow state through `next_random_tick_pos` `tick_speed`
times. Assert (1) the scheduler's final `position_state` equals the shadow state exactly, and
(2) the `RandomTickEvent` streams are equal. The expected value originates outside the code
under test: the shadow replay never consults the counters.
Vacuity guards, asserted not assumed: total draws > 0; at least one scripted flip changed the
subsequent tick's draw count (else the script never exercised the transition the counter
exists to track); the fixture preconditions from Gate A apply.

**Control:** run Gate B's comparison once with a counter corrupted via the U1 hook and assert
the sequences **diverge** — the same permanent-second-arm shape as Gate A's.

What would make these gates vacuous, named: an all-stone fixture (killed by the world-species
preconditions); a shadow replay that calls the production decision helpers (killed by writing
it against `raw_blocks` + `next_random_tick_pos` directly); a script whose flips never change
a decision (killed by the flip-changed-draw-count assertion); `tick_speed` divisible into a
degenerate 0 (killed by asserting draws > 0).

### Residual performance evidence

- (a) `#[cfg(test)]` `AtomicU64` evaluation counter inside `is_randomly_ticking` (same crate,
  visible to these gates only).
- (b) Gate: `tick_chunk` on an already-constructed column performs **exactly 0** predicate
  evaluations (the O(1) claim, as a count); construction of a real generator column performs
  **exactly `palette.len()`** (the one-time cost, as a count — predicted, not merely signed,
  per the magnitude-species rule: both hypotheses are computable, `0` vs `≥ palette.len()`
  per tick, and the measurement must land on the first).
- (c) Control: the construction arm's non-zero count is itself the proof the instrument
  counts; additionally assert the counter increments across a bare `is_randomly_ticking`
  call so a broken instrument cannot report two vacuous zeros.

### Residual live confirmation (non-blocking)

Run a reproducible release profile and verify that no production counter decision performs the
definitional scan. The hermetic gates carry correctness; a live profile is confirmation only.
The `random_tick_speed 0` control remains unsuitable as a counter-path performance oracle until
the game-rule store is wired into the tick loop.

## Fluids: out of scope, with the boundary marked

The reference section gate combines ticking blocks and ticking fluids, and lava is the
one fluid that ticks. This crate models **no** fluid random ticks, `is_randomly_ticking`
names no fluid, and the interim scan — the parity spec — is blocks-only. A
`section_ticking_fluids` counter today would have zero producers and zero consumers: an
island by construction. **Not built.** The disclosed consequence: our LCG position stream is
not reference-comparable for any section whose only ticking content is lava, today and after
this plan equally — the counter changes nothing there. When lava gains a random-ticking
behaviour, the same change must (1) add the fluid counter maintained at the same three
sites, and (2) widen the section gate to the OR — a code comment at the gate site in
`tick_chunk` will say exactly this and point here.

## The `random_tick_speed` island: split out, with a finding that changes its shape

**Recommendation: do not fold in.** Current state:

- `tick.rs` passes `DEFAULT_RANDOM_TICK_SPEED` (hardcoded 3), confirmed, and no
  `GameRulesHandle` appears anywhere in `tick.rs`.
- But the world-level store is itself unwired: `game_rules.rs`'s
  `GameRulesHandle` (with `random_tick_speed()`) is consumed **only** by `commands.rs`, and
  `ServerCommands`/`CommandContext` have **no production constructor**.
- Production's actual `gamerule` command path is a *different* store:
  `server.rs`'s per-connection `WorldAdminState.game_rules: HashMap<String, String>`
  (`server.rs`), which stores and echoes without applying.

So the real fix is not "read the rule directly" — it is: give `GameRulesHandle` a production
authority, make the `gamerule` parse path write into it (retiring or bridging the per-connection
HashMap — otherwise we create a second two-stores fork, and this repo has paid for that twice
in `ingest` vs `session`), and thread it into the tick loop (additive parameter on
`run_tick_loop_with_weather`, the established `sleep_vote`/`weather` shape, so the four
non-production call sites don't change). That is the game-rules-wiring work's scope; it touches two choke-point
files other agents are in (`tick.rs`, `server.rs`), and none of it changes the counter
representation. What this plan does instead to stay compatible: Gate B runs at a non-default
speed, so the `tick_speed` parameter is proven live end-to-end through the new decision path —
when random-tick-speed and game-rules wiring land, only the value's *source* changes, and nothing in these units assumes 3.
The `mesh_fill_rate` speed-0 control stays a known-disconnected subject until then (its doc
already records the null-that-carried-no-evidence incident).

## Implementation assumptions

The reference mechanism has four counters, including the blocks-or-fluids gate; lava is the
only ticking fluid. The server uses `ChunkColumn`'s flat, private, append-only representation,
and every inspected `ChunkSource::set_block` implementation reaches the documented mutator.
Region loading builds through `set_block`; `GameRulesHandle` remains unwired; and
`cargo xtask docs-index` scans `docs/plans/`.

Check these low-risk assumptions at implementation time: `u16` suffices for
`section_ticking` (4096 cells maximum); the same-crate module cycle
`chunk.rs → random_tick.rs` introduces no build concern (Rust permits module cycles within a
crate, and `random_tick → chunk` already exists); `growth_tick`'s predicates are pure
functions of the state string (their call sites treat them so).

## Measurement and scope notes

- Use the measured values **97.6%, 2.108 ms, 38.7 µs, and 54×** with the 49-column tick area:
  103 ms and 2.07× over budget. The 6.3–6.9 s view-fill range is a run range, not a single
  representative duration.
- The blocks-or-fluids gate means a future fluid tick changes the same consumer boundary. Add
  the fluid counter and widen the gate in the same change that introduces that producer.
- Wiring `random_tick_speed` requires one authoritative world-level rules store and a live path
  from command parsing through the tick loop; it is intentionally separate from the counter.
- Client-side `refresh_stats` and uncalled per-section draw loops are outside this plan. Their
  measurements are unaffected by the counter work.
