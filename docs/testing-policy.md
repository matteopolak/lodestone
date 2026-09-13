# Testing policy

## What it is

The rule for what earns a test in this repo. The corpus reached **12,090 test
attributes across 712 files**, and that costs compile time and run time on every
agent's every iteration. A test earns its place by catching a regression that
would realistically happen. Everything else is deleted.

## The standard

**Would this realistically break, and would this test be how we find out?**

If the answer to either half is no, delete the test. Not "is it correct", not
"is it well written" — a well-written test of something that cannot break is
still pure cost.

## Delete on sight

- **Source-text greps.** A test that `include_str!`s a `.rs` or `.md` file and
  asserts it `contains(...)` some string. These break on formatting, pass when
  the code is wrong but the string survives, and hand-rolled Rust lexers written
  to support them have been wrong about lifetimes three separate times here.
- **Doc-content gates.** A test asserting prose states a particular count.
  Documentation drifting is not a runtime defect.
- **Restating the compiler.** Asserting a match is exhaustive, an enum has N
  variants, a struct has a field, a `Default` returns the default, or that a
  constant equals its own literal.
- **Trivial accessors.** Getters, setters, `From`/`Into` that only move a field.
- **Controls for trivial detectors.** A control earns its place when the
  detector is subtle enough to silently stop detecting. A control on an
  `assert_eq!` of two integers is not.
- **Duplicate coverage.** Several tests walking one path with inputs that do not
  discriminate between any two hypotheses. Keep the one with the sharpest input.
- **Tests of scaffolding** — builders, fixtures and helpers that exist only to
  serve other tests.

## Keep

- **Wire and codec correctness**, where the expected value comes from outside our
  own encoder — captured bytes, a jar's generated reports, a hand-decoded example.
  These catch real, silent, cross-version breakage.
- **Algorithms with real arithmetic** — physics, worldgen, light, collision,
  pathfinding. Especially at poles, axes, zero crossings and boundaries.
- **Integration tests that drive the real schedule end to end.** The dominant
  defect class here is the island — a subsystem that is built, green, and
  reaches nothing. Only a test going through production's own construction path
  can see that.
- **Regressions tied to a bug that actually happened** and could recur. Say which
  bug in one line.

## Judgement cases

- **Pixel gates** are expensive and there are many. Keep those covering a whole
  pipeline; delete those asserting a specific pixel count that any legitimate
  visual change would break. A gate needing its expected value updated every time
  someone touches the renderer is an alarm, not a test.
- **Architectural invariant greps** (for instance "this router has no catch-all
  arm") sit between the two lists. They guard something real, but they guard it
  by reading source text. Prefer expressing the invariant in the type system; if
  that is not possible, keep at most one per invariant and never one per call site.

## When deleting

Delete the helpers a removed test orphaned — no dead code, ever. Deleting a test
whose subject is genuinely untested is a real loss, so when a deletion leaves a
behaviour uncovered and that behaviour could break, say so rather than quietly
widening the gap.
