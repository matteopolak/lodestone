# String-dispatch census

## What it is

`cargo xtask check-string-dispatch` inventories Rust `match` expressions whose patterns are string
literals. The report identifies closed registries and closed configuration domains that should be
parsed into types before production dispatch.

## How it works

The command parses Rust sources under `crates/` with `syn`, then records each `match` arm containing a
string literal. It ignores strings produced by enum matches, so serialization, display, and asset-path
output do not appear merely because their result is textual. Findings are classified as generated,
test/benchmark, parse/decode boundary, or production candidates. The command is currently a census:
existing findings are migration work rather than an allowlisted permanent baseline.

Closed JSON domains should derive `serde::Deserialize` on a named enum, using `rename`, internal tags,
or adjacent tags as the format requires. `#[serde(untagged)]` is reserved for alternatives whose
shapes genuinely differ. Dynamic names from plugins or data packs cross the boundary as a parsed
`ResourceKey` and then resolve to a compact built-in enum or an explicit interned extension identity.

## How to change it

The detector lives in `xtask::string_dispatch`. Add a negative control whenever its AST coverage is
expanded, and keep the distinction between string patterns and string results. Do not suppress an
entire file or crate: migrate the closed domain or document why the input is genuinely dynamic.

## Configuration

There are no environment variables or config files. The scan has a minimum source-file count and a
maximum parse-failure fraction so a broken workspace walk cannot report a misleading empty census.

## Dependencies

The implementation uses `syn` and `proc-macro2` from the existing `xtask` dependency graph. It does
not invoke Cargo metadata or compile project crates.
