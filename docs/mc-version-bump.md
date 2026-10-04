# Minecraft Reference Version

## What it is

The single source of truth for which Minecraft release the repo's gitignored reference cache (`.cache/mc/<version>`) is read from, how a reader says it follows that release or is tied to one, and the procedure for moving to the next release.

## How it works

The current version is one line in the repo-root `mc-version` file (`26.3` today). Every reader resolves it the same way:

| Reader | Spelling |
|---|---|
| Rust (shell, tests, xtask) | `lodestone_mc_cache::cache_root()`, `client_jar()`, `current_version()` |
| `Justfile` | the `mc_version` variable |
| `scripts/*.py` | `scripts/mc_version.py` (`cache_root()`) |
| `scripts/*.sh`, `web/Trunk.toml` | `${LODESTONE_MC_VERSION:-$(cat mc-version)}` |

`LODESTONE_MC_VERSION` overrides the file for one process. `LODESTONE_ASSETS` names a cache directory directly and wins over the version lookup. `cache_root` looks for `.cache/mc/<version>` in the nearest ancestor of the working directory (the binary runs from the repo root, tests from a crate directory), then in the repo the crate was built from. There is no scan over sibling version directories and no "highest sorting wins" rule.

Some readers are tied to one release on purpose: tests whose expected values are the 26.2 canonical block ids, transcribed strings or protocol-776 layouts, the live oracle server (which runs 26.2 because the integrated server hosts 776), and the two-release diff tools under `crates/lodestone-data/tools/` and `crates/versions/26.3/tools/`. They name the pin, so they are greppable and visibly different from "current":

- Rust: `lodestone_mc_cache::pinned_26_2_root()` / `PINNED_26_2`.
- `Justfile`: the `pinned_mc` variable.
- Scripts and tools that must keep a literal path: an entry in `xtask/check-mc-version.toml` with a reason.

`cargo xtask check-mc-version` (`just check-mc-version`, and the `mc_version_lint` tests under `cargo test -p xtask`) fails on any non-comment `.cache/mc/<digit>` literal in `.rs`, `.py`, `.sh`, `.toml` or the `Justfile` that is not allowlisted, and on allowlist entries that no longer match anything. Its unit test plants a literal and requires the detector to fire.

## How to change it

Bump to a new release `X`:

1. Fetch it: `cargo run -p xtask -- fetch-assets --version X`, `fetch-sounds --version X`, and `fetch-version --version X` for the server jar. Run the server's data generator with `--reports` in `.cache/mc/X`.
2. Decompile the client and server jars: `just decompile X` (`.cache/vineflower.jar` into `.cache/mc/X/client-src` and `.cache/mc/X/src`).
3. Stage the built-in pack: write `X` to `mc-version`, then `just stage-resources` (see [`built-in-resource-pack.md`](./built-in-resource-pack.md); the visual pack's declared range must include the new resource format).
4. Run `cargo xtask check-mc-version`, then the asset-reading gates (`cargo test --workspace --no-fail-fast -- --ignored` for the shell and render GPU gates).
5. Pinned readers do not move with `mc-version`. Each family that hosts or joins `X` takes its own data from `X` through the generators in [`protocol-26-3-era.md`](./protocol-26-3-era.md); only when nothing names the old release is its cache directory deletable.

Gotchas: the cache root is a plain directory lookup, so a missing `.cache/mc/X` reads as "no assets" (audio and textures off), not as an error naming the version; the failure message prints the version it looked for.

## Configuration

- `mc-version` — the current release.
- `LODESTONE_MC_VERSION` — per-process override, honoured by Rust, the `Justfile`, `scripts/mc_version.py` and the shell hooks.
- `LODESTONE_ASSETS` — a cache directory used verbatim.
- `xtask/check-mc-version.toml` — reviewed exceptions to the literal check.

## Dependencies

`lodestone-mc-cache` (std only; a regular dependency of `lodestone-shell`, a dev-dependency of the crates whose tests read the cache, and a dependency of `xtask`).
