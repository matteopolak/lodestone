# The portable clock (`lodestone-time`)

## What it is

`lodestone-time` is the workspace's one sanctioned way to read a clock. It wraps
`web-time` — `Instant` (monotonic) and `epoch_duration()` (wall-clock time since
the Unix epoch) — and every other crate that needs a clock depends on
`lodestone-time` instead of on `web-time` directly. It replaces three separate,
independently-arrived-at copies of the identical seam:
`crates/lodestone-shell/src/platform.rs` (which was the original, fullest
version), plus an inline `web_time::Instant::now()` in `lodestone-net`'s ping
timer and a `web_time::{SystemTime, UNIX_EPOCH}` import in
`lodestone-particle`'s entropy-seeded RNG.

## How it works

`std::time::Instant::now()` and `std::time::SystemTime::now()` both **compile**
for `wasm32-unknown-unknown` and **panic at runtime** (`RuntimeError:
unreachable`, measured by executing the compiled wasm in a real VM). With the
browser profile's `panic = "abort"`, that is not a recoverable error — it is the
tab dying — and no `cargo check` at any feature setting can see it, because the
call type-checks perfectly.

`web-time`'s non-wasm arm is `pub use std::time::*`, so `lodestone_time::Instant`
*is* `std::time::Instant` on native — the same type, not a newtype, so native
behaviour is provably unchanged. On `wasm32` it is backed by `performance.now()`
(monotonic, specified) and `epoch_duration()` by `Date.now()`. `web-time` was
picked over a hand-rolled newtype because it is API-identical to
`std::time::Instant` and because `winit`'s wasm arm already types
`ControlFlow::WaitUntil` as `web_time::Instant` — a private newtype would not
type-check against it. See `crates/lodestone-time/src/lib.rs`'s crate docs for
the fuller history (it is the primary source; this doc summarises it).

`Duration` is not wrapped or re-exported: it is shared between `std::time` and
`web_time` (the latter simply re-exports the former), so a `Duration` produced
from `lodestone_time::Instant` interoperates with `std::time::Duration`
arithmetic with no conversion anywhere in the workspace.

## How to change it

- Need a `web_time` item this crate does not re-export yet? Add it to
  `crates/lodestone-time/src/lib.rs` and re-export it from there — do not reach
  for `web_time` (or `std::time::Instant`/`SystemTime`) directly in a dependent
  crate. `scripts/wasm-check.sh`'s per-crate `instant-ban`/`systemtime-ban`
  confinement rules grep the `std::time::` paths out of every wasm-linked crate's
  `src/` and will (correctly) fail if one leaks back in.
- `lodestone-time` is held to the identical rule as everyone else, with an empty
  allowlist: it has no special exemption to spell `std::time` directly, because
  everything it re-exports comes from `web_time`, and that crate's own non-wasm
  arm resolving to `std::time` happens inside `web-time`'s source, not this
  crate's.
- Adding a new dependent: use `lodestone-time = { workspace = true }`, never a
  direct `web-time = "1"` — the point of this crate is that it is the one and
  only depender on `web-time` in the graph.
- **Gotcha**: `web_time::Instant` and `std::time::Instant` are different Rust
  types even though they behave identically on native. A partial migration (some
  call sites converted, some not) fails to *compile* rather than silently mixing
  clocks, which is a feature — but it does mean a crate-wide migration has to
  land in one commit per crate, not span several.

## Configuration

None. No feature flags, no env vars — the crate is unconditional on both
targets, which is deliberate (see "why `web_time` rather than a hand-rolled
newtype" in the crate docs): declaring the dependency for both targets is what
lets a signature like `fn tick(now: lodestone_time::Instant)` be written once
instead of behind a `cfg`.

## Dependencies

- `web-time` (the only external dependency; nothing else, deliberately, since
  nearly every crate in the workspace ends up depending on this one transitively).

## Where the confinement is enforced

`scripts/wasm-check.sh` (mirrored in `cargo xtask wasm-check`) carries two rules
naming this crate specifically (`lodestone-time instant-ban`,
`lodestone-time systemtime-ban`, both with an empty allowlist), plus one
`instant-ban`/`systemtime-ban` pair per crate that depends on it
(`lodestone-particle`, `lodestone-net`, `lodestone-ecs`, `lodestone-server`,
`lodestone-worldgen`) and the shell's `platform.rs`-allowlisted pair for
`lodestone-shell`. See `docs/browser-shell-port.md` for the fuller wasm-hazard
picture this is one piece of.

### Crates that were audited and excused, not migrated

Four crates carry a raw `std::time::Instant`/`SystemTime` call and were
deliberately **left alone with a documented reason** rather than converted,
because each is either dev-dependency-only or has its clock call sites
structurally confined behind a gate that already keeps them out of a wasm
build. Converting them would cost nothing at compile time, but the rule this
repo follows is: a file that genuinely never reaches `wasm32` is named and
excused, not converted for tidiness (`CLAUDE.md`). Each has its own
`instant-ban`/`systemtime-ban` confinement-rule pair too, using the bare
`Instant::now(`/`SystemTime::now(` spelling rather than the qualified
`std::time::` path — none of the four depends on `lodestone-time`, so there is
no legitimate `lodestone_time::Instant::now()` call in them to avoid catching,
and their real call sites are a mix of qualified and unqualified spellings.

| crate | file | why it is excused |
|---|---|---|
| `lodestone-auth` | `browser_login.rs`, `migrate.rs` | both modules are declared `#[cfg(not(target_arch = "wasm32"))]` at `lib.rs`, so neither ever compiles into a wasm32 build |
| `lodestone-world` | `world.rs` | the one call site is inside `#[cfg(test)] mod tests` (a synthetic-fill timing print) — test code is never part of a `--lib` build, wasm or native |
| `lodestone-testsupport` | `lib.rs` | the crate is a `[dev-dependencies]` entry of every one of its dependents (including `lodestone-shell`, deliberately — see that crate's own `Cargo.toml` comment), so its lib target is never linked into a production or wasm build at all |
| `lodestone-allocbench` | `main.rs` | a native-only allocator-benchmark binary; nothing in the workspace depends on it, and it is already excluded from any `--all-features` sweep for its allocator mutual-exclusion |

`lodestone-ecs`'s existing `async_task.rs` allowlist entry is the same shape:
its only clock hits are inside a `#[cfg(not(target_arch = "wasm32"))] #[cfg(test)] mod
tests`.
