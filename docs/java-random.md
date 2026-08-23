# `java.util.Random` (`lodestone-javarandom`)

## What it is

The workspace's one implementation of `java.util.Random` — a 48-bit truncated
linear congruential generator, bit-exact against the Java specification. Every
vanilla system that needs a seeded, reproducible draw uses it: particle bursts
(`lodestone-particle`), the enchanting-table book animation
(`lodestone-shell`'s `block_entities.rs`), the lightning bolt's procedural
geometry (`lodestone-render`'s `lightning_bolt.rs`), seeded sound-variant
selection (`lodestone-audio`'s `select.rs`), and the ghast model's nine
seeded tentacle lengths (`lodestone-assets`'s `entity_models::ghast_model`).

Before this crate existed the identical algorithm was reimplemented **six**
times across the workspace — one more than the count a first grep for
obvious copies found: `entity_models::ghast_model` had its own local
`struct JavaRng`, missed by the first pass and found only by grepping for the
LCG multiplier constant across the whole tree afterwards. Five of those six
are now this one crate; the sixth, `lodestone-worldgen-core::LegacyRandomSource`,
deliberately still carries its own copy of the LCG core — see "The one
deliberate holdout" below.

## How it works

`JavaRandom` holds a single 48-bit scrambled `i64` seed and implements exactly
the methods vanilla code in this workspace calls:

| method | Java equivalent | notes |
|---|---|---|
| `new(seed)` | `new Random(seed)` | scrambles with `(seed ^ 0x5DEECE66D) & (2^48-1)` |
| `from_entropy()` | `new Random()` | seeded from `lodestone_time::epoch_duration`, never `SystemTime::now()` directly |
| `set_seed(seed)` | `Random.setSeed(seed)` | same scramble as `new` |
| `next_i32()` | `nextInt()` | `next(32)` |
| `next_i32_bound(bound)` | `nextInt(bound)` | see "The bound gotcha" below |
| `next_i64()` | `nextLong()` | `(next(32) << 32) + next(32)`, sign-extended |
| `next_bool()` | `nextBoolean()` | `next(1) != 0` |
| `next_f32()` | `nextFloat()` | `next(24) / 2^24` |
| `next_f64()` | `nextDouble()` | `((next(26) << 27) + next(27)) * 2^-53` |
| `roll(bound: u32) -> u32` | — | `next_i32_bound` with the sign stripped, for weighted-selection call sites |

All arithmetic in the LCG step and in `next_i32_bound`'s rejection loop uses
Rust's `wrapping_*` operators, matching Java's `int`/`long` overflow semantics
exactly. This matters: one of the five original copies
(`lodestone-render`'s lightning bolt) used plain, non-wrapping arithmetic in
its rejection loop. It happened to be harmless for the bounds that call site
actually used (11 and 31, both far from the point where the sum could
overflow `i32`), but it was a real inconsistency versus the other four, and
exactly the kind of thing that stays invisible until a bound close to
`i32::MAX` is introduced somewhere. Consolidating removed it rather than
propagating it.

### The bound gotcha

`nextInt(bound)` is **two different algorithms**, not one formula:

- **Power of two**: `(bound * next(31)) >> 31` — a multiply-and-shift, no
  rejection.
- **Everything else**: draw `next(31)`, reduce modulo `bound`, and **retry**
  if that would bias the low end of the range (Java's overflow-based
  rejection test).

An implementation with only the modulo branch, or only the fast path, agrees
with `java.util.Random` for *some* bounds and silently disagrees for others —
the divergence does not show up until a specific bound is exercised. Every
caller in this workspace that draws from a mix of power-of-two and other
bounds (the enchanting-table book draws `nextInt(4)` *and* `nextInt(40)` in
the same tick) is implicitly relying on both branches being correct.

### The one deliberate holdout: `LegacyRandomSource`

`lodestone-worldgen-core::rng::legacy::LegacyRandomSource` wraps the same LCG
core this crate implements, but was not folded in:

- It implements worldgen's `RandomSource` trait alongside
  `XoroshiroRandomSource`, so noise routers and feature placers can be generic
  over which algorithm a seed was told to use. `JavaRandom` has no such trait
  and should not grow one for a single caller.
- It carries `next_gaussian` — a cached Box-Muller pair whose implementation
  needs to interleave direct seed manipulation with the cache check in a way
  that resists being expressed as a call through an opaque `next(bits)`
  method. None of the four consolidated copies needed gaussian at all.
- It drives `LegacyPositionalFactory` (`at(x, y, z)`, `from_hash_of(name)`)
  for vanilla's position- and name-seeded streams, which nothing outside
  worldgen needs.
- `lodestone-worldgen-core`'s own `Cargo.toml` states its `serde_json`
  dependency is deliberately the crate's *only* non-`std` dependency, because
  "this is a leaf, and every unit scheduled against it wants it to stay one."
  Adding even a small internal dependency breaks that documented invariant.

The duplication that remains — `LegacyRandomSource::next`/`next_int_bounded`
restating the same LCG step and rejection loop this crate has — is therefore
deliberate and bounded, not an oversight: the algorithm is frozen by the Java
specification and does not drift on its own, the way most duplicated logic
does over time.

## How to change it

Every method's behaviour is fixed by the `java.util.Random` specification.
Do not "improve" the arithmetic — a change that looks like a simplification
(dropping the rejection loop, using a float reciprocal instead of a division
by an exact power of two, using plain instead of wrapping `i32` arithmetic)
is a silent behavioural change for every consumer, all of which expect
byte-exact replay from a seed.

Adding a new method is fine; guessing its semantics from the method name is
not — check the Java specification (or 26.2's decompiled
`LegacyRandomSource`/`BitRandomSource` under `.cache/mc/26.2/`) for the exact
formula, and add a test whose expected value comes from **outside** this
crate: a published `java.util.Random` sequence, a hand-expanded LCG in a
throwaway script, or a captured JVM golden file (`lodestone-audio`'s
`tests/select.rs` has one). Never assert a new method against this crate's
own output.

## Configuration

None — pure integer/float arithmetic, no environment, no feature flags.

## Dependencies

`lodestone-time`, for `from_entropy`'s one clock read
(`lodestone_time::epoch_duration`, not `SystemTime::now()`, which traps on
`wasm32`). Every other method needs nothing beyond `std`.

Depended on directly by `lodestone-particle`, `lodestone-audio`,
`lodestone-render`, `lodestone-shell` and `lodestone-assets` — a small leaf
crate, so none of them pulls in an unrelated subsystem just to get an RNG.
