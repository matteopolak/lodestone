# `profile-cost-table.py` fixtures

Provenance for the three fixtures `scripts/test-profile-cost-table.py` asserts against.
See `docs/roadmap/benchmarks.md`'s "Profiling workflow" section for why this gate exists.

## `samply-0.13.1-real.json.gz` + `samply-0.13.1-real.json.syms.json`

A **real `samply` 0.13.1 capture** — the one fixture here whose shape nobody on this side
authored, which is the entire point of keeping it. Recorded 2026-08-07 on macOS/arm64.

Subsampled to **every 11th sample (311 of 3421)** to keep it ~3 KB. Absolute totals are
therefore ~1/11 of the real capture; the stack distribution, the tables and every
structural field are untouched. Note the sidecar name: samply's
`with_extension("syms.json")` drops only the *last* extension, so `p.json.gz` yields
`p.json.syms.json`.

Reproduce it with:

```bash
rustc -g -C opt-level=2 -C debuginfo=2 u20probe.rs -o u20probe
samply record --save-only --unstable-presymbolicate -o p.json.gz -- ./u20probe 200000000
```

```rust
// u20probe.rs
use std::hint::black_box;
#[inline(never)]
fn u20_hot_alpha(n: u64) -> u64 {
    let mut a = black_box(0u64);
    for i in 0..n { a = black_box(a.wrapping_add(i ^ (a >> 3))); }
    a
}
#[inline(never)]
fn u20_hot_beta(n: u64) -> u64 {
    let mut a = black_box(1u64);
    for i in 0..n { a = black_box(a.wrapping_mul(31).wrapping_add(i)); }
    a
}
#[inline(never)]
fn u20_hot_gamma(n: u64) -> u64 { u20_hot_alpha(n) ^ u20_hot_beta(n / 4) }
fn main() {
    let n: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(20_000_000);
    let mut s = 0u64;
    for k in 0..8 { s = s.wrapping_add(u20_hot_gamma(black_box(n + k))); }
    println!("{s}");
}
```

`black_box` is load-bearing: the first attempt at this probe was optimised to nothing and
ran in 0.00 s, producing a capture with **zero samples** that looked exactly like a
samply failure.

**Why the probe has this shape.** `u20_hot_gamma` calls `alpha(n)` and `beta(n/4)`, so
the expected ratio of leaf cost — about **4:1** — comes from the probe's own loop bounds,
*outside* the code under test. Any misattributing join lands somewhere else entirely
(the swapped hypothesis is 0.25). Every function name in this capture is an unresolved
hex address at record time, so the sidecar join is fully load-bearing: `0 resolved` would
mean the table is addresses, not symbols.

## `collide-per-thread-v55.json` / `collide-shared-v56.json` (+ `.syms.json` sidecars)

Hand-built and deliberately tiny (3 samples, 100 units of CPU delta, hand-derivable
expected weights). Two properties they exist for, neither of which a real capture
conveniently supplies:

- **A cross-library address collision**: RVA `0x1000` resolves to `liba::alpha_symbol` in
  `liba` and `libb::beta_symbol` in `libb`. This is what makes the `(library, address)`
  join key testable rather than merely asserted.
- **Both layouts, one profile.** `v55` is the per-thread layout with `prefix` stacks (what
  samply 0.13.1 writes); `v56` is the hoisted `profile["shared"]` layout with
  `prefixOffset` stacks. They encode the same profile and must produce identical tables.

`collide-per-thread-v55.json`'s second thread ("worker") deliberately **reuses function
indices 0 and 1 for different symbols**, so a script that resolved every thread against
thread 0's tables — the easy mistake in the per-thread layout — is detectable rather than
plausible.

These two are synthetic, so they only prove what we chose to model: they carry the exact
weights and the collision, and the real capture above carries the format.
