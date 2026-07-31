# Version table (epic #343 groundwork)

## What it is

For each of the sixteen versions GitHub epic #343 committed Lodestone to supporting — the
latest patch of every major Minecraft release from 1.7.10 through 26.2 — this is the
checked-in, provenance-tracked record of that release's **protocol number**, its save-format
**`DataVersion`**, and its **release date**:

```
1.7.10  1.8.9  1.9.4  1.10.2  1.11.2  1.12.2  1.13.2  1.14.4  1.15.2
1.16.5  1.17.1  1.18.2  1.19.4  1.20.6  1.21.11  26.2
```

It does not implement anything — no new protocol family, no translation layer. It is the
reference data those will eventually be checked against, and it exists now because
`CLAUDE.md` records four separate prior instances of hand-derived protocol figures being
wrong in this repo. Every number here traces to a source outside the code that will
eventually consume it.

The data lives in `crates/lodestone-registry/src/generated/version_table.rs` (generated,
do not hand-edit) with a hand-written public API and full provenance documentation in
`crates/lodestone-registry/src/version_table.rs`. Read the module docs there for the
complete methodology write-up; this doc is the narrative version plus the things that
turned out to disagree with the briefing this work started from.

## How it works

`cargo run -p xtask -- version-table` (implemented in `xtask/src/lib.rs`, functions
`version_table_report`/`resolve_version_table_entry` onward) does, per version, in this
priority order:

1. **Mojang's version manifest** (`https://launchermeta.mojang.com/mc/game/version_manifest_v2.json`)
   gives the release date (`releaseTime`) and the URL of that version's own JSON, which in
   turn gives the vanilla server download (URL + SHA-1 + size) — reusing the same
   `parse_version_manifest`/`parse_asset_downloads` code path as the existing
   `fetch-assets`/`fetch-version` commands.
2. **The jar's own `version.json`** (root of the vanilla server jar) is the authority for
   `protocol_version` and `world_version` (the `DataVersion` used in level/chunk NBT — a
   different number from the protocol version) when the jar has one. The tool only reads a
   jar it finds already cached at `.cache/mc/<version>/server.jar`, or — with the explicit
   `--fetch-missing` flag — downloads it first via the existing `fetch_version` (SHA-1
   verified, cached, same as `xtask fetch-version`).
3. **`vendor/minecraft-data`'s `data/pc/common/protocolVersions.json`** is the fallback,
   used only where the jar has no `version.json` — cross-check-grade, never authoritative,
   per `CLAUDE.md`'s "Data sources, in order".
4. Where *both* sources are available, they must agree exactly, or `xtask version-table`
   hard-errors rather than silently preferring one. This makes an agreeing row a real,
   continuously-re-checkable cross-check rather than a one-time manual comparison — run
   `cargo run -p xtask -- version-table --check --fetch-missing` at any point and it
   re-derives every figure from scratch and fails loudly on drift or disagreement.

```bash
cargo run -p xtask -- version-table                  # regenerate the checked-in table
cargo run -p xtask -- version-table --check           # drift guard, no network unless a
                                                       # target version's jar is missing
                                                       # and --fetch-missing is also passed
cargo run -p xtask -- version-table --fetch-missing   # also fetch every currently-uncached
                                                       # target version's jar first
```

`crates/lodestone-registry`'s own test suite (`cargo test -p lodestone-registry`) is
hermetic and network-free — it only checks the *shape* of the committed table (exactly
sixteen versions in release order, strictly increasing protocol/data versions, the
jar/minecraft-data source boundary sitting where it should) so routine CI does not need
network access at all.

## The table

| version | protocol | data version | released | source |
|---|---|---|---|---|
| 1.7.10 | 5 | 18 | 2014-05-14 | minecraft-data only — no jar fetched |
| 1.8.9 | 47 | 95 | 2015-12-03 | minecraft-data only — jar cached, confirmed no `version.json` |
| 1.9.4 | 110 | 184 | 2016-05-10 | minecraft-data only — no jar fetched |
| 1.10.2 | 210 | 512 | 2016-06-23 | minecraft-data only — no jar fetched |
| 1.11.2 | 316 | 922 | 2016-12-21 | minecraft-data only — no jar fetched |
| 1.12.2 | 340 | 1343 | 2017-09-18 | minecraft-data only — jar cached, confirmed no `version.json` |
| 1.13.2 | 404 | 1631 | 2018-10-22 | minecraft-data only — jar fetched, confirmed no `version.json` |
| 1.14.4 | 498 | 1976 | 2019-07-19 | jar `version.json`, cross-checked against minecraft-data — **agree** |
| 1.15.2 | 578 | 2230 | 2020-01-17 | jar `version.json`, cross-checked — **agree** |
| 1.16.5 | 754 | 2586 | 2021-01-14 | jar `version.json`, cross-checked — **agree** |
| 1.17.1 | 756 | 2730 | 2021-07-06 | jar `version.json`, cross-checked — **agree** |
| 1.18.2 | 758 | 2975 | 2022-02-28 | jar `version.json`, cross-checked — **agree** |
| 1.19.4 | 762 | 3337 | 2023-03-14 | jar `version.json`, cross-checked — **agree** |
| 1.20.6 | 766 | 3839 | 2024-04-29 | jar `version.json`, cross-checked — **agree** |
| 1.21.11 | 774 | 4671 | 2025-12-09 | jar `version.json`, cross-checked — **agree** |
| 26.2 | 776 | 4903 | 2026-06-16 | jar `version.json`, cross-checked — **agree** |

(Release dates truncated to the date here; the checked-in table keeps the full ISO-8601
`releaseTime` timestamp from the manifest.)

### The jar-`version.json` boundary is empirically 1.13.2 → 1.14.4

`version.json` at the jar root is publicly documented as introduced in 18w47b, a 1.14
snapshot. This repo now has direct evidence bracketing that inside the epic's own version
list: 1.13.2's server jar (fetched this session) has no `version.json`; 1.14.4's (also
fetched this session) does, and reads `protocol_version: 498, world_version: 1976`. No
version in `EPIC_343_VERSIONS` between those two exists, so the boundary is settled for
every version this table covers without needing to check the snapshots in between.

### Every place minecraft-data was compared against the jar: zero disagreements

For the nine versions where both a jar `version.json` and a `minecraft-data`
`protocolVersions.json` entry exist (1.14.4 through 26.2), the two agreed exactly on both
`protocol_version` and `data_version`/`dataVersion` in every case — including 26.2, where
`vendor/minecraft-data` has no full per-version data directory (`data/pc/26.2/` does not
exist) but its cross-version `protocolVersions.json` index still carries a correct,
matching entry (`776`/`4903`). `xtask version-table` hard-errors on disagreement rather
than picking a winner, so this is a standing, re-runnable check, not a one-time
observation — see "How it works" above.

## Corrections to the briefing this work started from

- **"1.7.10 has no minecraft-data coverage at all" is not quite right.** There is no
  per-version data directory (`vendor/minecraft-data/data/pc/1.7.10/` does not exist — the
  closest is a generic `data/pc/1.7/`, aliased from `minecraftVersion: "1.7.10"` in its own
  `version.json`), but the cross-version `data/pc/common/protocolVersions.json` index does
  carry an explicit `1.7.10` entry: `version: 5, dataVersion: 18`. That is still the
  weakest-attested row in this table for a real reason — no jar was fetched for it (nothing
  in the epic's version list between it and 1.13.2 to bracket a boundary against, and its
  own protocol/data-version numbers have no jar to check them against at all, unlike every
  version from 1.8.9 onward which at least has *a* cached jar even where that jar predates
  `version.json`) — but "no coverage at all" overstates it. It has coverage; it just has no
  independent second source.
- **Not every claim needed a jar to settle.** The briefing anticipated needing "sixteen
  jars unless [...] genuinely the only way." Twelve were fetched (four were already
  cached: 1.8.9, 1.12.2, 1.16.5, 26.2; eight more were fetched this session: 1.13.2, 1.14.4,
  1.15.2, 1.17.1, 1.18.2, 1.19.4, 1.20.6, 1.21.11). The remaining four (1.7.10, 1.9.4,
  1.10.2, 1.11.2) were deliberately **not** fetched: all four predate 1.13.2, whose jar was
  fetched and confirmed to still lack `version.json`, so fetching them would establish
  nothing beyond what fetching 1.13.2 already did — there being "the only way to get
  protocol_version" is exactly the bar the briefing set for downloading, and for these four
  it is not met. `xtask version-table --fetch-missing` will pull them (and re-verify
  everything else) if that changes.

## How to change it

- Add or remove a target version: edit `EPIC_343_VERSIONS` in `xtask/src/lib.rs`
  (currently the one place the sixteen-version list is spelled out for generation
  purposes; `crates/lodestone-registry/src/version_table.rs`'s test module keeps its own
  independent copy specifically so a network-free `cargo test -p lodestone-registry` still
  catches the generated table silently losing or reordering a version) and regenerate.
- The generator hard-errors on jar/minecraft-data disagreement by design — do not "fix" a
  future disagreement by picking one source in the generator; that removes the whole point
  of cross-checking. Investigate and record the actual explanation instead (see
  `CLAUDE.md`'s evidence standards).

## Configuration

No environment variables. Two CLI flags on `xtask version-table`: `--check` (drift guard,
no write) and `--fetch-missing` (also downloads any target version's jar not already
cached under `.cache/mc/<version>/server.jar` — the only network/disk-heavy path).

## Dependencies

- Network access to `launchermeta.mojang.com` / `piston-meta.mojang.com` (only reached
  with `--fetch-missing`, or implicitly by `--check` if a jar is missing and
  `--fetch-missing` is also passed).
- `vendor/minecraft-data` (gitignored vendor checkout; must be present at
  `vendor/minecraft-data/data/pc/common/protocolVersions.json` for any fallback or
  cross-check to resolve).
- `.cache/mc/<version>/server.jar` (gitignored; the existing `fetch-version` cache
  convention, reused rather than duplicated).
