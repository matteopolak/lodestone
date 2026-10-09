# Plan: crate and file splits

## What it is

The remaining open splits of large files and crates, judged on contention rather than tidiness: many agents edit one shared checkout, so a hot file is a lock. The `server.rs`, `mobs.rs` and v26-2 `adapter.rs` splits are done and are the playbook for the rest.

## Open work

| target | verdict | notes |
|---|---|---|
| `lodestone-shell` GUI half to a new `lodestone-gui` crate | split | `menu/`, `hud`, `container/`, `chat`, `overlay`, `tablist`, `config`, `keybinds`, `resources`, `asset_objects`, `platform`, `saves`, `offline_identity`, `skin_fetch` and the GUI shaders |
| `crates/versions/26.2/src/server_protocol.rs` | split after the above settles | serverbound mirror of the adapter split; hot and co-edited with the server |
| `menu/nav` by screen family | after `lodestone-gui` exists | only contended within the menu |
| `menu/options.rs`, `entities.rs`, `lodestone-render/src/entity.rs`, `gpu/` | leave | large but not contended; `gpu/` is the draw seam entangled with `app/redraw.rs` and `sim/` |
| `lodestone-data`, `sin_table.rs`, golden traces | never split | generated; `lodestone-data` is already split along generated vs hand-written |

## The `lodestone-gui` split

The menu/HUD/container set is about half self-contained by commit history (44-48% of its commits touch nothing else in the shell), unlike `gpu/` and the `sim`/`net`/`app` core. A cross-reference audit found only five symbols escaping the set, to move first while still inside the shell:

- `sim::SessionEnd`/`SessionEndKind`: plain UI value types, move into the GUI crate.
- `audio::subtitles::{SubtitleCaption, SubtitleArrow}`: move the types.
- `gpu::entities::entity_texture_from_image`: a texture-upload helper that belongs in `lodestone-render`.
- `camera_rig::{BobFrame, HURT_DURATION_TICKS}`: move or pass in.
- `blocks::{DemoClassifier, ShellClassifier}`: move `blocks.rs` or keep `resources.rs` in the shell.

`menu`, `hud` and `container` genuinely cycle (`logical_canvas`, advancement sprites, the sprite pipeline builder and the vanilla font), so they land together in one commit. `effects` is a model with no wgpu and travels with `hud`.

What it buys: edits in `sim/`, `gpu/`, `app/` or `net.rs` stop recompiling the menu's code; and a GUI crate that compiles without `lodestone-render`, `wgpu`'s window feature, `tokio` net or any protocol family turns "the menu does not reach into the renderer or network" into a compile error. What it does not buy: browser bundle size, which is a data-shape problem (the generated tables in `lodestone-data` stay live through LTO regardless of crate).

Trade-off: a broken `lodestone-gui` fully blinds a shell agent's diagnostics, while a broken shell no longer affects a GUI agent.

Order, each step its own green commit:

1. Move the escaping symbols; re-run the audit and require zero escapes, with a control showing the audit ran.
2. Create the crate with a manifest and empty `lib.rs`; `cargo xtask check-connected` must fail until the shell depends on it.
3. Move files in dependency order: `platform`, `config` + `keybinds`, `chat`, `overlay`, `asset_objects`, `resources`, `saves`, `offline_identity`, `skin_fetch`, `tablist`, then `hud` + `container` + `menu` together.
4. Fix the instruments below in the same commit as the code they guard.

## How to change it

### Path-shaped instruments a split silently blinds

| instrument | hardcodes | failure |
|---|---|---|
| `cargo xtask connectedness` | `crates/lodestone-server/src/server.rs` as the serverbound second hop, and `crates/versions/<family>/src/server_protocol.rs` | a moved file reads as UNCLASSIFIED or STRANDED, a degraded report with exit 0 |
| `cargo xtask check-deletable` / `conformance` | existence of `src/server_protocol.rs` | a family looks like it lacks `ServerProtocol` |
| `scripts/wasm-check.sh` `WASM_CRATES` and the copy in `xtask` | explicit crate list | a new crate is not compiled for wasm32 |
| confinement rules (both copies) | `crates/<crate>/src` plus a basename allowlist | a new crate has no rules; a moved file leaves dead allowlist entries |
| `wgsl_valid.rs`, `no_production_source_names_testsupport.rs` | one crate's `src/` | moved shaders/sources become unguarded |
| `[profile.dev.package.lodestone-worldgen*]` | crate names | a split crate reverts to `opt-level = 0`, a slow suite |

Teach the instrument the directory form before splitting a file it reads. For a new crate, in the same commit: add it to `WASM_CRATES` in both places; give it `instant-confinement`, `systemtime-confinement` and `thread-spawn-confinement` rules allowlisting `platform.rs`, `accounts.rs` and `status.rs` (which really call `std::thread::spawn` and trap on wasm32); remove those from the shell's allowlists; confirm the run prints `confinement rules that actually ran: N/N`; and copy the `no_wgsl_is_inlined_in_rust_sources` and testsupport gates. The script/xtask parity test diffs both lists, so editing only one fails loudly.

Not affected: `#[path]` harnesses, CI jobs (they call `just --workspace` and the manifest glob picks up new crates), `web/Cargo.toml` (but its `Cargo.lock` changes), and `default-members`. Verify the new crate is version-free with `just check-seam`.

### Migration method for a file split

1. A pure rename commit first (`X.rs` to `X/mod.rs`); `git show --stat` must report zero insertions and deletions.
2. One domain per commit, each a pure move and independently green.
3. Verify each move with a normalized-line multiset diff against the pre-split file in both directions, counted by a program that reads files (a `diff | grep -c` control once reported 0 for about 15,000).
4. Pin the `cargo test -p <crate> --no-fail-fast` count before and after at one sha.
5. After landing in a contended file, grep one distinctive symbol per edit and require a nonzero count; a concurrent wholesale rewrite leaves a clean tree and no diff.
6. Re-run `connectedness`, `check-connected` and `wasm-check` and record before and after; `SKIPPED` or `UNCLASSIFIED` is a failure.

Extracting the arms of a long loop into named functions is a separate change from moving files; mixing them makes the multiset diff unusable. Private helpers reaching private fields need `pub(super)` promotion; inherent `impl` blocks across sibling files need none.

## Open questions

- The rebuild-cost gain is a mechanism, not a measurement; measure both arms concurrently and prefer a counter over wall-clock time.
- Whether the GUI crate really compiles without the renderer and window features decides whether the boundary enforces anything. Check after the move and do not add those dependencies back.
