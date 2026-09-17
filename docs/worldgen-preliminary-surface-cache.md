# Preliminary surface cache

## What it is

`PreliminarySurfaceCache` is the bounded request memo for the low-detail
surface estimate used by Overworld aquifer and surface stages. A production
`OverworldBatchLease` owns one cache for the admitted request; scalar calls use
the generator-owned region cache so adjacent columns retain preliminary work
without allocating a request table each time.

## How it works

Each request first applies the existing integer snap `(coordinate >> 2) << 2`.
An `AquiferSystem` keeps a fixed direct-mapped local table for the block-fill
loop; only a local miss reaches a shared cache. A shared cache has 32 fixed
open-addressed shards of exact coordinate keys and integer values. Scalar
admission locks the selected shard through evaluation. A batch admission takes
the relevant shard locks in index order, reserves missing keys, releases the
locks for one fixed-width compiled graph walk, then reacquires them in the same
order to commit; a condition variable makes a racing scalar or batch caller
wait on a reserved key rather than compute it twice. There is no per-entry allocation,
`Arc`, once-guard, or hash-map node. A full shard evaluates a value while
locked and replaces one deterministic slot, which keeps the retention bound
explicit without leaving an untracked full-table path. The aquifer's bounded
construction-time grid and the surface stage's four chunk-corner requests use
the batch path; irregular block-fill misses remain scalar.

The cache is created by `OverworldBatchLease` and passed through the pre-ore
request into both the chunk-bound aquifer and surface scan. Structure height
probes are an independent generator-owned consumer, so `OverworldGenerator`
keeps one bounded 8,192-entry region cache. Batch leases use an equivalent
request cache; scalar columns and structure probes use the generator-owned
region cache. Both carry the compiled `PointProgram`. Direct aquifer and
Nether/End surface constructors retain independent fallback caches for their
standalone lifetimes. A request never rounds, clamps, or translates its key
beyond the specified snap; negative and translated coordinates are distinct
entries.

The `gen-counters` instrumentation records requests and actual density
computations. A bounded adjacent-ring control demonstrates the overlap while
comparing generated chunk fingerprints and the integer aquifer skip bound.

The adjacent-ring control records request, hit, miss, and retained-entry counts
for the current production data. Retained bytes include entry storage, bounded
pending reservations, the fixed point-scratch tables, and the lazily allocated
eight-lane batch value table; allocator headers are excluded.
Full-shard misses replace entries in a deterministic round-robin slot under the
selected shard lock.

The adjacent-ring control compares scalar and batch chunk fingerprints, covers
a translated negative-coordinate request, and the cache-level concurrency
tests prove one computation for eight racing callers of the same exact key plus
concurrent evaluation for two distinct shards. Instruction/cycle figures from
older cache-only controls are not a claim about this batched evaluator; rerun a
paired production measurement when the complete server workspace is green.

## How to change it

Keep the snap expression and the cache key in lockstep. If the preliminary
density function or its coordinate semantics change, update both constructors
that provide it and rerun the adjacent-ring counter/fingerprint control. Do
not move the request lookup into `block_at`; the aquifer-local table is what
keeps request-lock admission off the per-block hot path. Any capacity change
must remain a power-of-two bounded table and must not merge coordinate keys.

## Configuration

Preliminary request counters are compiled only with the `gen-counters` feature
and are inert otherwise. The request, region, shard, and per-shard point-scratch
capacities are implementation constants in `aquifer/mod.rs`. The focused measurement test
accepts `PRELIM_CACHE_ARM=scalar` or `PRELIM_CACHE_ARM=batch` to isolate one
arm; without it, it runs both and compares fingerprints.

## Dependencies

The cache uses 32 `Mutex`/`Condvar`-protected bounded `Vec`s of compact entries
plus one bounded batch scratch table. It is fed by the Overworld `Builder`'s
preliminary density tree and is consumed by `AquiferSystem`, `SurfaceSystem`,
and `OverworldGenerator`'s batch/fill wiring.
The focused regression control uses the embedded server generator and `sha2`
for chunk fingerprints.
