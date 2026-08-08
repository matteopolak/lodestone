# Explosion performance

## What it is

A measured profile of what an explosion costs on the integrated server, and a plan
for making it cheap without changing a single output. Vanilla's own explosions
visibly stall a Java tick when several fire at once; the goal here is identical
physics at a different cost, so a TNT cannon does not freeze the game.

The correctness half is [explosion block destruction](./explosion-blocks.md). This
doc is the follow-up work, deliberately written down rather than built: two efforts
in this repo have already aimed at the wrong stage before anyone measured stage
shares.

## The measurement

`crates/lodestone-server/tests/explosion_cost_profile.rs`, `#[ignore]`d:

```
cargo test --release -p lodestone-server --test explosion_cost_profile \
    -- --ignored --nocapture
```

Instructions retired via `proc_pid_rusage(RUSAGE_INFO_V4)`, not wall clock — this
host reproduces wall clock to 11–19% with sibling agents compiling and instructions
to 0.16–0.21%, and every conclusion below is a ratio.

### What the first run said

Release build, radius 3.0 (a creeper), 1352 rays:

| arm | instructions | note |
|---|---|---|
| one blast in solid stone | 10,312,138 | 24 cells claimed; short rays, cold column |
| one blast in **open air** | 33,475,709 | 577 cells claimed; the longest rays, so the worst case |
| eight overlapping blasts (a cannon) | 36,283,606 | 8 cells changed |
| per ray, open air | 24,760 | |

And the three innermost operations of the march, measured in isolation over 200,000
probes each:

| operation | instructions/call | share |
|---|---|---|
| `ChunkSource::block_state` (allocates a `String`) | 758.9 | 39.6% |
| `block_states::state_id` (parses it back to an id) | 1,138.6 | 59.4% |
| the flat `StateId`-indexed resistance lookup | 18.0 | **0.9%** |

Both predictions, stated before the run, held: the flat table is a rounding error
and the world read plus the string→id resolution are **99.1%** of the per-step cost.

**One methodological note, because the first attempt got a too-good number.** The
flat-table arm initially measured `0.0` instructions per call — LLVM had hoisted the
whole lookup out of the loop because the id was loop-invariant. Every probe now
varies its input across a small set. A `0.0` reading is not a result.

### The headline

A single open-air creeper blast is **~33.5M instructions**. At a plausible 2–3
instructions per cycle on this machine that is on the order of 5–10 ms against a
50 ms tick — one creeper is a fifth of a tick, and a cannon is the whole thing. That
is the number the plan below has to move, and it is dominated by ~17,600 ray steps
each paying ~1,900 instructions to turn a bit-packed palette entry into a `String`
and back into an id.

## The plan

Ordered by measured payoff. Every item must leave the destroyed set, the entity
damage and knockback, the drops, and the RNG draw sequence **byte-identical**; the
honest gate for each is a before/after instruction count *plus* an identical
destroyed set on a fixed seed.

### 1. A section-granularity dense id cache, shared across one tick's explosions

The whole optimisation, by the numbers above. `cell_resistance` is already the
single world-read point of the module precisely so this drops in one place: replace
the `String` + `state_id` round trip with a dense array of block-state ids over the
affected region, populated once per section touched.

**Granularity is the thing that matters, and there is a measurement for it.** In
this repo's density engine a one-slot last-position cache hit **2.1%** while a map
over the same graph hit **78.2%** — because consecutive accesses alternate between a
few positions and evict a single slot before it is read. Ray stepping has exactly
that pattern: 1352 rays all crossing the same handful of sections. So cache at
section granularity, and **share the cache across every explosion in the same
tick** — that is the cannon case, where blasts overlap heavily.

Expected: removes ~99% of the per-step cost, i.e. most of the 33.5M.

Preserves: everything. The cache is a pure read-through of the same data.

### 2. Deduplicated neighbour updates

N destroyed blocks × 6 neighbours, with heavy overlap. Collect into a set and notify
once per position. Not yet a cost here (block destruction does not notify at all
today), so this is a *do it this way when you add it* note rather than a saving.

### 3. One batched light recompute over the affected region

Instead of per-block. The `LIGHT_UPDATE` encoder now exists, so a batched resend has
a real carrier. Again currently unpaid — destruction does not relight — so this is a
constraint on the future wiring.

### 4. Coalesced section-level block-change packets

A creeper's crater is up to 27 positions in one or two sections; a cannon's is many
more. One section update beats N block updates on the wire and on the client's
mesher. Not observable in the world state, so it costs no exactness.

### 5. Per-entity exposure sampling

`getSeenPercent` fires its own ray grid per entity against real collision shapes.
Not measured here (it lives in `lodestone-entity` and is not this unit's), but it is
the candidate that goes *quadratic* with a chain reaction's worth of dropped items in
range. Measure before touching it — the same rule that produced this doc.

## What is not allowed

- **Cross-tick budgeting.** Spreading a chain reaction over several ticks changes
  *when* things happen, which is observable and therefore not 1:1. The goal is to
  make each explosion cheap enough that a cannon does not stall a tick, not to defer
  it.
- **Approximating the ray count, the step size or the exposure sampling.** Those are
  the physics. 1352 rays and `0.3`-block steps are not tunables.
- **Reordering the destroyed set to suit a faster data structure**, if drops are ever
  implemented — with one caveat, which is that vanilla's own drop order is a
  Fisher–Yates shuffle of a `HashSet` iteration order and therefore not reproducible
  outside the JVM at all. See [explosion block destruction](./explosion-blocks.md).
  The *draw count* of that shuffle is reproducible and must be consumed
  (`shuffle_draws`).

## The client half

Re-meshing is the client's cost: a large blast dirties many sections at once. The
mesher already drains dirty columns near-and-in-front first with a per-frame budget,
so the ordering is sensible, but a creeper's crater spans up to two sections and a
cannon's spans many — enough to exceed a per-frame budget. Not measured here (the
shell is another agent's), and the finding to carry forward is that a blast should
probably raise the priority of the sections it dirtied rather than rely on the
generic near-and-in-front ordering, since the player is by definition looking at the
explosion.

## Configuration

None. The profile test takes no arguments and asserts no cost — it prints numbers and
asserts only that the instrument moved and that the arms are ordered as the
arithmetic requires. A cost threshold on a machine shared with other agents would be
a flake.

## Dependencies

`proc_pid_rusage(RUSAGE_INFO_V4)` (macOS), the same instrument
`join_parallel_efficiency.rs` and the worldgen generation bench use;
`lodestone_server::explosion_blocks`; `lodestone_data::{block_blast, block_states}`.
