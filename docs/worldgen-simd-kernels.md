# Worldgen SIMD kernels: what vectorising the noise actually bought

## What it is

The `std::simd` vectorisation of `lodestone-worldgen-core`'s noise kernels — Unit 5 of
[`plans/worldgen-rewrite.md`](./plans/worldgen-rewrite.md) — and, more usefully, the profile
that decided how far it was worth taking and the disassembly control that showed the premise
behind it was half wrong. One function is vectorised: `ImprovedNoise::sample_and_lerp`. The
measured whole-column effect is about **2.4% of `C_ss`**, and this document exists mainly so
nobody re-attempts the larger version whose ceiling is recorded here as smaller.

## The profile that scoped the unit

`samply` against a release build of `benches/generation.rs` (embedded server data, seed 42,
the C_ss 12×12 sweep plus the calibration and vegetation benches), weighted by
`samples.threadCPUDelta`. Self time, the leaf clusters that matter:

| leaf | self | owner |
|---|---|---|
| `place_placed_feature::recurse` | 13.50% | U8 |
| SipHash `Hasher::write` + the four `hash_one::<…>` + `reserve_rehash` | ~20% | U3 / U7 |
| **`ImprovedNoise::noise_scaled`** | **11.23%** | **U5** |
| `Field::eval` + `Field::interpolate` | 11.0% | U4 (landed) |
| `SurfaceSystem::try_apply` + `build_surface` | 6.35% | — |
| `PerlinNoise::get_value` | 1.65% | U5 |

Two things follow, and the second is the one that shaped the implementation.

**The noise kernel is the third-largest cluster and the largest single leaf in the numeric
core.** `noise_scaled`'s inclusive time equals its self time — `sample_and_lerp` and
`grad_dot` are inlined into it — so 11.23% is a leaf figure, not a subtree.

**Its cost does not arrive through a batchable seam.** Attributing that 11.23% to callers:

| caller chain | share of the leaf | share of total CPU |
|---|---|---|
| `Spline::compute` ← `AquiferSystem::{from_parts, aquifer_status}` | 42.9% | 4.81% |
| `BlendedNoise::compute` ← `Field::{eval, interpolate}` | 19.8% | 2.22% |
| `PerlinNoise::get_value` ← `Field::{eval, interpolate}` | 12.6% | 1.42% |
| `Spline::compute` ← `Field::{eval, interpolate}` | 9.7% | 1.09% |
| `Spline::compute` ← `SurfaceSystem::top_material` ← `carve_stage` | 10.9% | 1.23% |
| remainder (biome, carver, `build_surface`) | ~4% | ~0.5% |

Only the rows through `Field::interpolate` — about **42%** of the kernel's cost — sit behind
U4's interpolated-corner lattice, the one place a batched fill API could hand the kernel
several independent positions at once. The majority arrives through `Density::compute`, the
*point* interpreter, from `aquifer/` and `surface/` one position at a time. That is why the
vectorisation went **inside** one call rather than across a batch of them: the in-kernel form
needs no caller change and reaches 100% of the cost, where the batched form reaches 42%.

## The disassembly control, and the premise it falsified

The plan's framing — SIMD is "headroom Pumpkin left on the table" — invites the assumption
that the shipped kernel was scalar. **It was not.** Disassembling `noise_scaled` in the
pre-change release binary (`objdump -d --disassemble-symbols=…`, Mach-O arm64):

| | pre-change | post-change |
|---|---|---|
| total instructions | 220 | 195 |
| `fmul.2d` | 17 | 15 |
| `fadd.2d` | 15 | 14 |
| `fsub.2d` | 6 | 4 |
| `sshll.2d` (integer widen) | **12** | **0** |
| `scvtf.2d` (int → float) | **12** | **0** |
| `zip1.2d` / `zip2.2d` (the `simd_swizzle!`s) | 0 | 3 / 3 |

LLVM had already found the eight-way parallelism in the gradient dots. What it could not
remove was the per-call conversion of the `i32` `GRADIENT` entries to `f64` — 24 instructions
of pure marshalling on every sample. **Most of the measured win is deleting those, not
introducing lanes.**

Note the arithmetic in the last column is a *prediction*, not a reading: eight `f64` lanes are
four 128-bit NEON registers, so `gx*x + gy*y + gz*z` is 3 muls and 2 adds per register (12 and
8), plus the lerp tree's two vector levels (2+1 muls, 2+1 adds) — 15 `fmul.2d`, exactly what
the disassembly shows. A count that disagreed would have meant the lane structure was not the
one intended.

## What the four arms measure

All four in one binary against the same 65,536 positions, all four **bit-identical** to each
other, timed as median-of-paired per-rep ratios:

| arm | ratio | ns/position |
|---|---|---|
| shipped scalar (`i32` table) | 1.000 | 5.955 |
| scalar code, `f64` table, **no `std::simd`** | 0.891 | 5.305 |
| **shipped: `std::simd`, 8 lanes in-kernel** | **0.791** | 4.712 |
| `std::simd`, batched over 4 independent positions | 0.756 | 4.487 |

So the 1.26× total splits roughly evenly: **1.12× from the table type, which needs no nightly
feature at all**, and a further 1.13× from the explicit lane structure. Anyone weighing
whether `#![feature(portable_simd)]` is worth its unstable-API risk should weigh it against
~1.1% of `C_ss`, not against the whole 2.4% — backing the feature out and keeping the `f64`
tables recovers about half the win on stable.

**On the timing method, because the first attempt was wrong.** A sum-over-reps ratio measured
on this machine swung **0.317 … 1.685 across five runs of the same binary** — four other build
agents were live, 790% aggregate CPU, 1.19 GB swap in use. Summing lets one scheduling spike
in one arm dominate. Timing each arm once per rep, adjacent in time, and taking the median of
the per-rep ratios reproduces to three decimals across runs; the ratio of per-arm *minima*
agrees with it to 0.001, which is the cross-check that the estimator is measuring the kernel
rather than the machine. **The absolute nanosecond figures above are still only comparable
within one run.**

## Why the batched form did not ship

It is the faster arm and the plan's nominally preferred shape, so the reasons are worth being
explicit about:

1. **It reaches less.** 42% of the kernel's cost × a 1.32× local win is a smaller whole-column
   saving than 100% × 1.26×.
2. **It needs a lane-parallel `Field::eval`, and that is a parity hazard rather than merely
   work.** `Mul` does not evaluate its second operand when the first is exactly `0.0`, and a
   skipped subtree can contain a cache-slot write, so which nodes an evaluation touches is
   position-dependent (`docs/worldgen-density-engine.md`, *How to change it*). Lanes at
   different positions diverge on exactly those branches, and masking them changes *which
   slots get written*, not only the cost.
3. The remaining 58% would still need the in-kernel form anyway, so the two are not
   alternatives — the batched form is strictly additional complexity on top.

## Parity: why this is safe by construction, not by tolerance

The two rules the kernel is written to, both from the plan's SIMD policy:

- **Lanes across independent positions only.** The eight corners of one noise lattice cell are
  eight independent expressions. Each lane computes `((gx*x) + (gy*y)) + (gz*z)` — the scalar
  `dot`'s exact association — so there is no reassociation available to get wrong.
- **Never across an accumulation chain.** The octave sums in `PerlinNoise::get_value` and
  `BlendedNoise::compute` are untouched and still sequential. A horizontal add over the eight
  corners would be a different summation order and a different world.

The trilinear reduction keeps `Mth.lerp3`'s nesting exactly; only *sibling* nodes at one level
of that fixed tree share a vector (4 lerps, then 2, then a scalar root). And there is **no
`mul_add` anywhere** — `StdFloat` is deliberately not imported, because fused rounding differs
from vanilla's separate multiply-then-add.

Two non-obvious traps this kernel deliberately does not take:

- **The multiply by a `0.0` gradient component stays a multiply.** Six of the sixteen gradients
  have a zero component, and folding `0.0 * x` to `0.0` loses the sign of zero: `0.0 * -x` is
  `-0.0`, and `-0.0 + -0.0` is `-0.0` where `0.0 + -0.0` is `0.0`. Equal under `==`, *not*
  equal under `to_bits`, and every parity gate here reads bits.
  `tests::zero_gradient_component_keeps_sign_of_zero` pins it.
- **No transcendental was introduced.** Nothing here computes a `pow`/`log`/`exp`, so the
  host-libm-dependence hazard that bit U4 and U9 (`DESIGN.md` §12) does not apply — the
  gradient tables are exact small integers re-spelled as `f64`, not a computed quantity.

## Evidence

Bit-identity against our own previous output is `decode(encode(x))` in disguise, so the JVM
oracles are the anchor and both were run:

| gate | result |
|---|---|
| `noise_parity` (JVM `NoiseOracle` dump) | **1224/1224 probes bit-exact** |
| `chunk_parity` (JVM `DensityChunkOracle`) | **98304/98304 = 100.0000%**, hashed *and* bounded-dense |
| `interpolation_order` (inverted guard) | still distinguishes the two nestings |
| `lodestone-worldgen` suite | 121 passed / 0 failed / 4 ignored, 20 binaries |
| `lodestone-worldgen-parity` + `-core` | 41 passed / 0 failed |
| `column_is_byte_identical_across_two_independently_constructed_generators` | ok |
| `mth_parity` (sin table, 65,536 entries) | ok |
| `cargo check -p lodestone-worldgen-core --target wasm32-unknown-unknown` | clean |
| in-crate bit-identity vs an independent scalar transcription | 20,000/20,000 positions |

The in-crate test deserves a note, because it is the one that localises a break to this
function rather than to the whole pipeline. `noise::improved::tests` holds a deliberately naive
scalar transcription of vanilla's `sampleAndLerp`, reachable only from the test module, used
**purely as a bit-equality oracle**. That is not a production scalar twin — no seed can travel
it — and the distinction matters, because the plan bans a `#[cfg]` fallback precisely so two
arms cannot generate two worlds. It carries two premise checks: that >19,000 of the 20,000
positions really have all three lerp factors non-zero (integer coordinates would make every
factor `0.0`, `lerp(0.0, a, b) == a` exactly, and the whole reduction tree the identity — a
test passing while measuring nothing), and that the transposed gradient tables agree with
`GRADIENT` on all 16 entries with a control that a collapsed-to-constant table would fail.

**No `C_ss` speedup is claimed.** There is one implementation of this kernel by policy, so the
two arms are different builds of one symbol and cannot be interleaved in one process; the
repo's two-arm rule exists because non-interleaved worldgen timings have been mis-attributed
before. The 2.4%-of-column figure is the *profile share* (11.23%) multiplied by the *kernel
ratio* (0.791) — an arithmetic projection from two measurements, not an end-to-end measurement,
and it is well inside the run-to-run spread of `C_ss` on this machine.

### The counter, and what it cannot tell you

`Snapshot::noise_corner_batches` counts eight-lane gradient batches, one per
`sample_and_lerp`. It is the island check, and it is predictive: a `PerlinNoise` stack with `k`
non-zero amplitudes costs exactly `k` batches per `get_value`, derived from the amplitude list
rather than read back from a run. `tests/simd_kernel_counter.rs` asserts 4 batches for
`[1,1,1,1]` and **2** for `[1,0,1,0]` — the second is the discriminating case, since a counter
placed per octave *slot* rather than per *sample* would read 4.

It is a separate binary because `counters` is process-global and the lib binary's other tests
instantiate `NormalNoise`; a delta measured alongside them races and fails in the direction
that reads as a regression.

**What it cannot prove is that the lanes stayed vectorised.** No counter can see LLVM
scalarising a `Simd` op. That question has exactly one instrument here, the disassembly table
above, and it is also the instrument that caught the false premise — so re-run it, do not
re-derive it, if this kernel is ever changed.

## How to change it

- **Re-run the disassembly before believing a speedup.** The pre-change control is the whole
  reason this document says "the table type" rather than "SIMD". `nm <binary> | grep
  noise_scaled` for the mangled symbol, then
  `objdump -d --disassemble-symbols=<sym> <binary>`; on Mach-O the vector forms are
  `fmul.2d`-style suffixes, **not** `v0.2d` operands, and a grep written for the latter reports
  zero on a fully vectorised function. That mistake happened here first.
- **Never `mul_add`, and never fold a `0.0` gradient multiply away.** Both change bits. See
  above.
- **Do not widen the lanes past 8.** Eight `f64` is already four NEON registers; more lanes
  only add `zip`/`ext` shuffling. `wasm32`'s SIMD128 is likewise 2-lane `f64`.
- **Adding a lane batch across positions means touching `Field::eval`**, and that is where the
  `Mul` short-circuit lives. Read `docs/worldgen-density-engine.md`'s *How to change it* first;
  the ceiling it would be buying is 42% of this kernel, recorded above.
- **The `f64` gradient tables are a hand-written transpose of `GRADIENT`.** `GRADIENT` is
  `#[cfg(test)]` for exactly that reason: its only remaining job is to be the independent
  statement the transpose is checked against. Do not delete it, and do not "simplify" the check
  into deriving one from the other.
- The timing harness is not committed — it is a scratch binary. Its shape is worth recreating
  rather than trusting old numbers: four arms in one process, bit-identity asserted over the
  same inputs the timing uses, median-of-paired ratios, and a re-run to confirm the ratio
  reproduces before it is quoted.

## Configuration

`#![feature(portable_simd)]` in `crates/lodestone-worldgen-core/src/lib.rs`, on the pinned
nightly (`rust-toolchain.toml`). No feature flag selects the kernel — there is one
implementation and no scalar fallback, deliberately: a dual path is two worlds from one seed,
each arm individually "correct" and invisible to a single-arm test run.

`gen-counters` turns `noise_corner_batches` from inert to live, and must be forwarded as
`gen-counters = ["lodestone-worldgen-core/gen-counters"]` from `lodestone-worldgen` or it
silently reads zero (`tests/gen_counters_forward.rs` is that gate).

## Dependencies

`noise/improved.rs` depends on `math` (`floor`, `lerp`, `smoothstep`), `rng`, `counters`, and
`std::simd`. Its consumers are `noise::perlin::PerlinNoise` (the octave stack) and
`noise::blended::BlendedNoise`, and through them the whole density graph. Evidence comes from
`scripts/worldgen-oracle/`'s `NoiseOracle` and `DensityChunkOracle` dumps and the decompiled
`ImprovedNoise.java` / `NoiseChunk.java` under `.cache/mc/26.2/src`.
