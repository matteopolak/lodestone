# Overworld vein batching

## What it is

The Overworld materialisation path builds a bounded, request-local product for
large copper and iron veins. It avoids retaining a full three-channel volume
while reducing expensive ridged/gap work to stone cells that already pass the
toggle, Y-band, and edge gates.

## How it works

`VeinChunk::prepare_batch` walks the 16×height×16 field in the same `z, x, y`
order as `DenseBlockGrid::from_ordered_state_fn`. It checks the fill result and
Y bands first, samples `vein_toggle` in one vertical run per column and band,
then stores only `(local_index, toggle_value)` candidates. A second pass resolves
the positional random source, ridged channel, richness, gap channel, and raw-ore
choice for each candidate in the original per-position draw order. The resulting
`(local_index, optional_state)` placements are consumed by a cursor during
materialisation, so failed solidness rolls still advance the ordered product.

The only density scratch outside the samplers is one vertical `f64` toggle
column. Candidate and placement vectors are bounded by the input field size.
After materialisation the candidate vector is returned to a thread-local worker
pool, so the largest request pays its allocation once while concurrent workers
do not share or retain generated values.

For the standard 384-block Overworld, the two bands cover 104 rows, so the
candidate capacity is at most 26,624 entries (425,984 bytes at 16 bytes per
entry), plus the 384-value toggle scratch. A worker retains only that bounded
capacity, never generated density or placement values.

## How to change it

Keep the candidate and placement traversal in lockstep with
`DenseBlockGrid::from_ordered_state_fn` (z, x, y). If a gate or draw changes,
update both `state_at`'s test-only scalar reference and `resolve_candidate`, then
run `candidate_batch_matches_scalar_and_stays_compact`. Do not replace the
positional source with a shared stream: each block's draw sequence is
coordinate-derived, but its within-position order is observable.

## Configuration

Veins are enabled only when `ore_veins_enabled` is true and the settings router
contains `vein_toggle`, `vein_ridged`, and `vein_gap`. The settings' horizontal
and vertical noise sizes determine each sampler's interpolation geometry.

## Dependencies

The batch depends on `NoiseChunkSampler`, the Overworld fill field's
`BlockKind` values, `StateId`, and `DenseBlockGrid`'s fixed traversal. The
materialisation caller is `OverworldGenerator::materialize_world` in
`overworld/fill.rs`.
