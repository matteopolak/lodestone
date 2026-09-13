# Oracle assets: what's on disk under `.cache/mc/`, and who reads it

## What it is

An audit procedure for `.cache/mc/`: server jars, client jars, and generated
world directories used as external test or generation inputs. It distinguishes
an asset that is present locally from one a test, script, or generator actually
consumes.

## How it works

`.cache/mc/` is deliberately gitignored and mutable, so a checked-in table of
its contents or sizes would become stale. Audit one version at a time instead:

```sh
find .cache/mc/<version> -type f | sort
rg -l -F '.cache/mc/<version>' crates xtask scripts
```

The first command identifies available inputs at every depth, including world
data; the second identifies direct consumers. A version number alone is not evidence of a jar dependency. For
example, `vendor/minecraft-data/data/pc/<version>/` is a separate, vendored
dataset. Search for the full `.cache/mc/<version>` path when determining
whether a cached jar is used.

Read protocol numbers from
`crates/lodestone-registry/src/generated/version_table.rs`. Do not infer a
protocol number from a cache-directory name, and do not infer that every
protocol-table target has a corresponding local jar.

Live-oracle scripts can require a booted world as well as a jar. Their runtime
requirements are documented in
[`docs/oracles-and-benchmarks.md`](./oracles-and-benchmarks.md); verify a live
oracle by running its script, not by treating the presence of `world/` as a
successful boot.

## How to change it

- Fetch a server jar with `cargo run -p xtask -- fetch-version --version <ver>`.
- When adding a consumer, keep its `.cache/mc/<version>/...` path explicit so
  the audit can find it. Prefer a committed extract for stable test input when
  a test does not need to start a server.
- When changing a live-oracle script, audit both the cached paths it reads and
  its runtime contract in `docs/oracles-and-benchmarks.md`.
- Treat an empty direct-consumer search as an audit result, not a request to
  remove the cache directory. Cached assets may be retained for a planned
  oracle or for local investigation.

## Configuration

The cache root is `.cache/mc/`. `xtask` fetch commands populate version
directories on demand; live-oracle scripts select their own version and
runtime settings.

## Dependencies

- `xtask`'s `fetch-version` and `version-table` commands.
- `scripts/live-oracles/` for booted-server inputs.
- [`docs/oracles-and-benchmarks.md`](./oracles-and-benchmarks.md) for runtime
  and oracle workflow requirements.
