# Redstone execution model: remaining work

## What it is

Open work on the redstone execution model after its main layers shipped. The shipped design (typed palette-derived reaction classes, reach-driven multi-column view) is documented in [`../redstone-execution.md`](../redstone-execution.md); this page keeps only what is undone and the constraints that govern it.

## Constraints that still govern

- **Vanilla is the oracle for observable behaviour, not for implementation.** The bar is observational equivalence with vanilla update order, quirks included (quasi-connectivity, depth-first cascade interleaving, drain tie-breaks, dust's duplicate notifications). Any optimisation decides who gets enqueued, never in what order work runs.
- **Edges mirror notification topology, never read sets.** A piston reads the cell above but vanilla never notifies it when that changes; the difference is the BUD quirk, so an index built from read sets destroys it.
- **A node is a cell position.** A per-dust-network supernode is rejected: each dust cell's power is client-observable per cell, settle order interleaves with neighbour reactions, and change-gating already bounds redundant recomputes. Revisit only with an order-sensitive oracle corpus.
- **Memoization crates (`comemo`, `salsa`) are rejected.** Redstone is a mutable spatial grid whose side-effect order is observable, and the hot cost was parsing, not recomputation.
- **Derived data is per-column and never persisted.** Cell tables and any index are dropped on unload and rebuilt from the palette; a global graph fights the `&mut ChunkColumn` borrow story.

## Open work

- **Write-plan unification.** Make every reaction return one `WritePlan` (ordered `(pos, new_state)` writes, scheduled ticks, a fan-out policy per write: none, one neighbour pass, dust's 7-centre set, rail's conditional extras). Piston `begin_move` and tripwire's `CalculatedState` already produce this shape; single-cell families would wrap mechanically.
- **Listener index (conditional).** A per-column reverse index of who vanilla's notification topology reaches, pre-sorted in fan-out order, with a border registry. Build it only if counters show notification enumeration still dominating after the classification; the expectation is that it will not, and it would introduce stale-edge defects the current design cannot have. If built, add a debug tripwire comparing index-driven enqueue sets to rediscovery.
- **Cross-chunk tick ordering.** The scheduled-tick queue is one world-wide container tie-broken by global insertion order. The reference collects due entries from per-chunk containers, so two ticks scheduled in the same game tick at the same priority in different chunks can drain in a different relative order. No oracle discriminates this yet.
- **Contraption-scale differential against a live server.** Existing expected values are outside-sourced constants or counter identities, which cannot catch an ordering divergence over many ticks. Needs an order-sensitive oracle corpus (T-junction, locked-repeater latch, observer chain, BUD rig) captured from the live server.
- **Border semantics.** A circuit straddling the ticked-area edge sees the unloaded half as absent, as vanilla does at an unloaded border; gate this against the real server.

## How to measure

Counters, not durations: wall clock reproduces at about 10.8% here, instruction counts at 0.16-0.21%. Use feature-gated thread-local counters (`redstone_counters`) and validate the instrument first with inputs that cannot affect the quantity: a lit circuit at rest across 100 ticks must read zero on every counter, placing stone 20 blocks away must move none, and a lever on-settle-off-settle must return to baseline. Derive predictions in a separate script (for a lever feeding a 15-dust run, the 7-centre by 6-direction arithmetic), never round numbers.

For migrations, a differential ratchet drives old and new dispatch over a fixture corpus in one process and asserts identical event and schedule sequences. It compares two things we control, so it is a migration tool, never the oracle. See [`../oracles-and-benchmarks.md`](../oracles-and-benchmarks.md) for the benchmark harness.
