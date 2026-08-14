# Plan: which files and crates to split, and which to leave alone

## What it is

A read-only architecture pass over the workspace's largest files and crates, deciding per candidate
whether to split now, split later, or leave — judged on **contention**, not on tidiness. Development
here runs many agents concurrently against a single shared checkout with no per-agent worktrees, so a
large file is a throughput problem: it is a lock. Every verdict below is backed by a commit-level
measurement of who actually edits what, and the plan carries the migration method, the ordering, and a
full enumeration of the path-shaped instruments a split would silently blind. Written 2026-08-14
against a verified tree; the `protocol/v770/src/adapter.rs` split (`d983d0e7`..`5ac277f8`) is the
precedent, including its warning.

---

## The measurement the verdicts rest on

Two windows, both from `git log --name-only` and counted with a program rather than a pipeline.

**Last 14 days: 1,371 commits in the repo.**

| file | commits touching it | share of all commits |
|---|---|---|
| `crates/lodestone-server/src/server.rs` | **105** | **7.7%** |
| `crates/lodestone-server/src/mobs.rs` | 48 | 3.5% |

Of the 105 commits touching `server.rs`, only **11** touch nothing else in the repo. Its top
co-editors are `lodestone-server/src/lib.rs` (42), `protocol/v770/src/server_protocol.rs` (39),
`lodestone-server/src/protocol.rs` (39), `tests/serve_play.rs` (24), `integrated.rs` (22),
`mobs.rs` (17), `tick.rs` (17).

**`lodestone-shell`: 456 of those 1,371 commits touch it.** Bucketed by first path segment under
`src/`:

| module | commits | commits touching nothing else in the shell |
|---|---|---|
| `app/` | 133 | — |
| `sim/` | 128 | — |
| `menu/` | **122** | **59 (48%)** |
| `gpu/` | 98 | 26 (27%) |
| `hud` | 60 | — |
| `container/` | 37 | — |
| `net.rs` | 36 | — |
| `mesher.rs` | 31 | 10 |
| `{hud, container, menu}` as a set | 194 | **85 (44%)** |
| `{sim, net, app}` as a set | 216 | 67 (31%) |

That table is the whole argument. `menu/` and the GUI set are **half self-contained**; `gpu/` and the
`sim`/`net`/`app` core are not. A split follows the 44–48% column, not the line count.

**Last 7 days, most-touched files** (for the file-level verdicts): `server.rs` 64,
`shell/app/redraw.rs` 46, `server/lib.rs` 36, `v770/server_protocol.rs` 32, `shell/hud.rs` 26,
`server/protocol.rs` 25, `server/mobs.rs` 25, `shell/gpu.rs` 20, `shell/menu/nav.rs` 18.

Note `app/redraw.rs` at 46 touches in 7 days — it is only 1,485 lines and never appeared in the
line-count survey, yet it is the second most contended file in the repo. **Line count and contention
are different axes and the survey only measured one of them.**

---

## Verdicts

### 1. `crates/lodestone-server/src/server.rs` — 13,059 lines — **SPLIT NOW** (file, not crate)

The single highest-value change in this document. 7.7% of every commit in the repo passes through
this one file, and only 11 of 105 of those commits are confined to it — so it is not merely busy, it
is the join where unrelated units meet.

**Split into `server/` (`mod.rs` + domain files), not into a new crate.** `lodestone-server` is
already 90 files; the crate is not a monolith, this one file is.

Thirteen clusters lift out cleanly — self-contained, not dependent on `serve_play`'s locals beyond
what is already passed as arguments, and mostly non-generic. In rough descending order of cleanness:

| target file | contents |
|---|---|
| `server/brewing.rs` | `BrewingSlot`, `BrewingInsertOutcome`, `bottle_from_item`, `brewing_slot_for`, `insert_into_brewing_stand`, `BREWING_STACK_CAP` |
| `server/composter.rs` | `ComposterUseOutcome`, `ComposterStep`, `composter_state`, `apply_composter_use`, the three `*_BEHAVIOR_SEED` consts |
| `server/stall.rs` | `LoopStallWatch`, `ticks_since`, `STALL_FLOOR`, `STALL_REPORT` |
| `server/pickup.rs` | `TakenItem`, `Pickups`, `collect_nearby_items`, `AbsorbedOrb`, `collect_nearby_orbs`, `TAKE_XP_DELAY_TICKS` |
| `server/drops.rs` | `spawn_dropped_stacks`, `apply_item_dropped` |
| `server/combat.rs` | `BowDraw`, `LaunchIntent`, `launch_intent`, `ItemInUse`, `UseItemOutcome`, `apply_use_item`, `finish_consuming`, `apply_release_use_item`, `spawn_player_projectile`, `find_item_slot`, `consume_one`, `apply_attack` |
| `server/container_click.rs` | `apply_container_clicked`, `read_menu`, `read_workstation_menu`, `workstation_result`, `apply_workstation_clicked`, `apply_enchanting_clicked`, `apply_rename_item`, `apply_container_button_click`, `apply_recipe_placed`, `apply_carried_item_changed`, `apply_creative_mode_slot_set` |
| `server/container_screen.rs` | `OpenContainer`, `ContainerSync`, `container_state`, `sync_open_container`, `container_title`, `workstation_menu_type`, `open_container_screen`, `open_crafting_table_screen`, `open_workstation_screen`, `open_enchanting_screen` |
| `server/view.rs` | `ViewTracker`, `ViewUpdate`, `join_view_rings`, `send_view_update`, `encode_column`, `JOIN_PRESTREAM_RADIUS`, `JOIN_STREAM_BATCH_COLUMNS`, `MAX_CLIENT_VIEW_RADIUS` |
| `server/entity_stream.rs` | `EntitySource`, `stream_pass`, `NoEntities`, `EntityStreamer` |
| `server/portal.rs` | `PortalTrip`, `travel_through_portal` |
| `server/persist.rs` | `player_store`, `persist_player`, `PLAYER_SAVE_EVERY_VITALS_TICKS` |
| `server/entry.rs` | the ten `serve_connection*` wrappers — ~705 lines of pure argument forwarding |

That removes roughly 5,000 lines. What **stays** in `server/mod.rs` is `serve_play` (both the native
and the `wasm32` arm), `dispatch_play_packet`, `serve_connection_inner` and `apply_use_item_on` —
still large, but tractable, and no longer sharing a file with brewing tables.

**Three things make this harder than the adapter split, and an implementer must be told all three.**

- **The visibility churn is much larger.** The adapter split moved `impl` blocks on one public type,
  where inherent methods stay visible across the module tree for free. Here there are 72 fully-private
  top-level functions, 16 private structs/enums and 21 private consts, and the helpers reach private
  *fields*: `ViewTracker`'s `center`/`loaded`/`radius`/`max_radius` are read directly by
  `travel_through_portal`; `OpenContainer`'s `window_id`/`pos`/`shape`/`state_id`/`container_size` are
  read from about a dozen sites; `SourceRef` is `pub(crate)` but its two most-used methods `get` and
  `dimension` are module-private while only `generate` is not. Expect to promote most of the 16 types
  and 40–60 of the functions to `pub(super)`.
- **The tests do not partition for free.** There is one flat `#[cfg(test)] mod tests { use super::*; }`
  of ~1,942 lines and ~90 functions at the end of the file. Either partition it by hand alongside each
  domain commit, or leave it whole in `server/mod.rs` reaching `pub(super)` items — the second is the
  cheaper first pass and is what this plan recommends.
- **`serve_play` does not delegate its state.** It takes 30 parameters and declares 45 `let` bindings
  before its `loop { select! { … } }`, so ~75 live names; the `vitals_tick` arm alone is ~500 lines
  inline and the `read_packet` arm ~280. **Extracting those arms into named functions is a separate
  change from the module split and must not be folded into it** — a pure-move series stops being
  verifiable by multiset diff the moment one commit also restructures. `dispatch_play_packet`'s
  47-parameter list is the same story; leave it alone in this pass.

Generic bounds are not an obstacle: the whole file uses one shallow, repetitive set
(`T: Transport, P: ServerProtocol, S: ChunkSource + 'static, E: EntitySource`) with no associated
types, no HRTBs and no `where Self:` chains, so a moved helper repeats its bounds verbatim and nothing
more.

### 2. `crates/lodestone-server/src/mobs.rs` — 10,552 lines — **DONE, and still not finished**

**Landed within minutes of this plan's own commit** — literally: `refactor(mobs): rename mobs.rs
to mobs/mod.rs` (`8f62d012`) landed at 02:22:49 on 2026-08-14, and this file's own commit
(`b1d88b28`) landed at 02:24:20 the same morning, 91 seconds later. The "SPLIT NOW" verdict below
was already executed by another agent before this plan reached `main`; nobody came back to
re-verdict it. `refactor(mobs): split species-keyed tables into mobs/species.rs` (`2ea08277`)
followed, and by 2026-08-14 (re-verified) `crates/lodestone-server/src/mobs/` holds
`block_ids.rs` (89 lines), `items.rs` (238), `golem.rs` (366), `world.rs` (424),
`falling_blocks.rs` (442), `lightning.rs` (464 — new content, not part of this split),
`projectiles.rs` (519), `vehicles.rs` (601), `species.rs` (740) and `orbs.rs` (824) — essentially
the exact seam list below. **`mod.rs` itself is still 7,305 lines**, the largest single file in
the split and still the biggest piece of `MobSim`'s `impl` block; the extraction moved the
easy, dependency-free wins out but did not touch the file that motivated this entry. Re-run the
14-day commit-touch measurement before deciding whether `mod.rs` itself now needs its own split —
this section's contention figures predate the split and are no longer a measurement of the
current file.

48 commits in 14 days, 17 of them shared with `server.rs`. Lower contention than `server.rs` but a
**much** cleaner seam, which is why it should go first: it is the rehearsal that proves the method
inside `lodestone-server` at a fraction of the risk.

Two independent seams:

- **Pure data lifts out untouched**, with no `MobSim` dependency at all: `mobs/species.rs`
  (`is_hostile_species`, `is_leashable_species`, `avoided_species`, `tempt_food`, `breeding_food`,
  `TameMechanism`, `tame_mechanism`, `horse_temper_gain`, `horse_breeding_items`, `tame_feed_heal`,
  `mob_experience_reward`), `mobs/golem.rs` (`GolemCell`, `SNOW_GOLEM_PATTERN`, `IRON_GOLEM_PATTERN`,
  `golem_pattern_matches`, `find_golem_pattern`, `GolemSpecies`, `GolemConstruction`),
  `mobs/block_ids.rs` (`canonical_state_string`, `state_id_by_name`, `default_state_id_by_block`,
  `census_to_pathfinding_type`).
- **`impl MobSim` (~3,880 lines, ~110 methods) is already stratified by entity kind** and splits
  across sibling files as additional inherent `impl` blocks — `mobs/projectiles.rs`, `mobs/items.rs`,
  `mobs/orbs.rs`, `mobs/falling_blocks.rs`, `mobs/vehicles.rs`. This is exactly the adapter pattern,
  with **zero visibility churn**, because inherent-impl methods stay visible to the whole crate module
  tree as long as the type is.

And the tests travel for free: `mobs.rs`'s ~2,610 test lines already live in **13 separately named**
`#[cfg(test)]` modules (`follow_range_tests`, `anger_tests`, `primitives_tests`, `block_cues_tests`,
`hostility_category_tests`, `falling_block_tests`, `experience_orb_tests`, `vehicle_tests`,
`baby_shape_tests`, `golem_tests`, `leash_tests`, `wandering_trader_tests`), each mapping onto one of
the new files. That is the single biggest difference from `server.rs`.

`ChunkWorld` and its `PathWorld`/`RayView` impls (~360 lines) are also a clean `mobs/world.rs`.

### 3. `crates/lodestone-shell` — 129,846 code lines — **SPLIT THE CRATE. The seam is real.**

This is the finding the plan exists to report, and it is the opposite of what the module list suggests
at a glance.

**Take the whole GUI half out as `lodestone-gui`**, below `lodestone-shell`:

> `menu/`, `hud` (`hud.rs` + `hud/`), `container/`, `chat.rs`, `overlay.rs`, `tablist.rs`,
> `config.rs`, `keybinds.rs`, `resources.rs`, `asset_objects.rs`, `platform.rs`, `saves.rs`,
> `offline_identity.rs`, `skin_fetch.rs`, plus the GUI half of `src/shaders/`
> (`menu.wgsl`, `menu_sprite.wgsl`, `hud.wgsl`, `hud_sprite.wgsl`, `hud_glint.wgsl`,
> `container.wgsl`, `container_bg.wgsl`, `effects.wgsl`, `panorama.wgsl`).

**The evidence that the seam holds.** Grepping every `crate::` reference in that file set, with
comment lines dropped and with a control proving the grep ran (1,145 references matched in total —
run it with `set -- …` and `"$@"`, because zsh does not word-split an unquoted `$var` and the first
attempt at this audit returned a vacuous empty result):

| target of the reference | count |
|---|---|
| inside the proposed set (`menu`, `config`, `hud`, `platform`, `resources`, `overlay`, `keybinds`, `offline_identity`, `container`, `saves`, `chat`, `tablist`, `asset_objects`, `skin_fetch`) | 1,128 |
| **escaping the set** | **17** |

Seventeen references out of 1,145, and they resolve to **five symbols**:

| escaping symbol | sites | disposition |
|---|---|---|
| `sim::SessionEnd` / `sim::SessionEndKind` | 9 (2 production, 7 test) | plain UI-facing value type — **move it into `lodestone-gui`**; `sim` then imports it back |
| `audio::subtitles::{SubtitleCaption, SubtitleArrow}` | 3 | data types in the hotbar subtitle row — move `audio/subtitles.rs` down, or move the two types |
| `gpu::entities::entity_texture_from_image` | 2 (`container/player_preview.rs`) | a wgpu texture-upload helper — belongs in `lodestone-render` |
| `camera_rig::{BobFrame, HURT_DURATION_TICKS}` | 1 (`menu/nav.rs`) | move the two names, or pass the values in |
| `blocks::{DemoClassifier, ShellClassifier}` | 1 (`resources.rs`) | move `blocks.rs` down too, or keep `resources.rs` in the shell |

Notably, **`hud` has zero real references to `gpu`, `sim` or `net`** — every apparent one is a doc
link. The same is true of `menu`'s apparent edges to `net::run_session`, `net::LAN_DEFAULT_PORT`,
`app::menus`, `effects::EffectsRenderer` and `tablist::tab_list_view`: all doc comments. `chat.rs` has
**no** `crate::` edges at all; `config.rs` has exactly one (`keybinds`); `container/` reaches outside
itself only into `hud`. The raw occurrence counts badly overstate the coupling, which is why the
naive read of this crate is "hopelessly entangled" and the measured read is not.

**The cycle inside the set is real but confined.** `menu ↔ hud ↔ container` genuinely cycle: `hud` and
`container` call `menu::render::logical_canvas` (11 + 16 sites) and `container` reads
`menu::advancements::ADVANCEMENT_SPRITES`, while `menu` builds its sprite pipeline through
`hud::item_icon::build_sprite_pipeline` and lays out text with `hud::VanillaFont`. That cycle is
**why all three go into one crate together** rather than into three. Do not attempt to separate them
in this pass.

**What this buys, stated honestly in both directions.**

- *Contention.* 85 of the 194 commits touching `{menu, hud, container}` touch nothing else in the
  shell — those become commits in a different crate from the `sim`/`net`/`app`/`gpu` core, so two
  agents stop sharing one `src/` tree and one lib target.
- *Rebuild cost.* rustc's compilation unit is the crate, so today an edit anywhere in the shell
  recompiles all ~130k code lines and relinks its test binaries. After the split, an edit in
  `sim/`, `gpu/`, `app/` or `net.rs` — the larger half of the traffic — no longer recompiles menu's
  ~60k lines. **The reverse is not true**: `lodestone-shell` depends on `lodestone-gui`, so a menu
  edit still rebuilds both. Claim the first, not the second.
- *Diagnostics, and this is a genuine trade rather than a pure win.* `CLAUDE.md`'s measured table
  says a broken **sibling** crate still lets your diagnostics through, a broken **dependency** emits
  nothing at all for you, and a broken **own lib** hides your own crate's test-file errors. Today, a
  menu agent's broken lib partially blinds a sim agent (same crate, test-file errors vanish). After
  the split, a broken `lodestone-gui` **fully** blinds a shell agent — worse — while a broken shell
  no longer affects a gui agent at all — better. Since shell-core carries more traffic than the GUI
  set, the net is favourable, but an implementer should know the failure mode changes shape.
- *Enforcement the type system currently cannot express.* `lodestone-gui` compiling without
  `lodestone-render`, `wgpu`-window features, `tokio` `net`, or any protocol family would make
  "the menu does not reach into the renderer or the network" a **compile error** instead of a
  convention. Check this after the move and, if it holds, do not add those dependencies back.

**What it does *not* buy: bundle size.** Do not justify any of this by the wasm ceiling. The browser
payload is over its gzip ceiling because `.rodata` is 8,004,211 B — ~76% of the 10.47 MB — and the
named contributor is `lodestone-data/src/generated/` at ~4.9 MB of Rust. Those tables are reached
through lookup functions, so LTO keeps them live regardless of which crate they sit in. **The bundle
is a data-shape problem and a crate boundary cannot move it.**

### 4. `crates/protocol/v770/src/server_protocol.rs` — 7,282 lines — **SPLIT LATER**

32 commits in 7 days and 39 co-edits with `server.rs` — genuinely hot, and the adapter method applies
essentially unchanged (it is the serverbound mirror of the file that was just split). It is *later*
only because it contends with the same work `server.rs` does: the two are edited together 39 times, so
splitting both at once puts two refactors in the path of one feature stream.

**Its own trap:** `cargo xtask connectedness` reads this exact path (`classify_serverbound_decode` on
`crates/protocol/<family>/src/server_protocol.rs`) and `check-deletable`/`conformance` test for its
existence to decide whether a family implements `ServerProtocol` at all. Teach the instrument the
directory form *before* the split, exactly as was done for `adapter/`.

### 5. `crates/lodestone-shell/src/hud.rs` — 7,841 lines — **SPLIT NOW**, cheaply

26 commits in 7 days, and a `hud/` directory already exists holding `item_icon.rs` (2,559),
`vanilla_font.rs` (1,723), `anim.rs` (653) and `font.rs` (337). The top-level file is the remainder.
This is a small, well-understood move that can be folded into the `lodestone-gui` extraction as its
first commits rather than run as its own unit.

### 6. `crates/lodestone-shell/src/menu/nav.rs` — 9,037 lines — **SPLIT LATER**

18 commits in 7 days. It is the screen-navigation state machine, and it is the file most likely to be
touched by any menu feature, so it *is* a lock — but only within the menu subsystem, which the crate
split already isolates. Split it by screen family (main/pause, server list, world select, options,
accounts, social) **after** `lodestone-gui` exists, so the two refactors do not share a file.

### 7. `crates/lodestone-shell/src/menu/options.rs` (6,073) and `src/entities.rs` (5,852) — **LEAVE**

9 and 10 commits in 7 days respectively. Large, but not contended enough to pay for a refactor. Revisit
if either crosses ~20 commits a week.

### 8. `crates/lodestone-render/src/entity.rs` — 6,317 lines — **LEAVE**

Does not appear in the 7-day top-30 by commit count. Size without contention is not a reason.

### 9. `crates/lodestone-shell/src/gpu/` — **LEAVE**

Already a directory (16,165 lines across 20 files). Only 26 of 98 `gpu`-touching commits touch nothing
else in the shell — the worst purity in the table — so a further boundary here would not decouple
anything. `gpu.rs`'s 20 touches in 7 days are real, but they are entangled with `app/redraw.rs` and
`sim/` by nature: it is the draw seam.

### 10. `crates/lodestone-server`, `lodestone-render`, `lodestone-physics`, `lodestone-worldgen` as crates — **LEAVE**

None is a monolith. `lodestone-server`'s 80,478 lines are spread over 90 source files; the problem
there is two files, addressed above. `lodestone-worldgen` already spun `lodestone-worldgen-core` out
and carries a `[profile.dev.package]` override that must be kept in step — a live reminder that a
crate split silently drops profile overrides unless someone copies them.

---

## What NOT to split, and why

- **`crates/lodestone-physics/tests/support/golden_traces.rs` (21,282 lines) — a test fixture.**
  Machine-generated golden traces consumed by the physics suite. It has no contention, no readers, and
  splitting it would only make the generator's output harder to regenerate. It should not have been on
  the survey.
- **`crates/lodestone-physics/src/sin_table.rs` (8,207 lines) — a generated table.** Same reasoning.
- **`crates/lodestone-data` (150,721 lines) — a data crate, not a monolith.** `src/generated/` is
  **142,855** lines against **4,724** hand-written across the rest of `src/`, wired in through 28
  `#[path = "generated/…"]` module attributes in `lib.rs`. It is already split, along the only line
  that matters (generated vs hand-written), and further division would break the generator contract
  for no throughput gain. Its size is a *bundle* problem (see above), and the fix for that is the
  shape of the tables, not the shape of the crate.
- **Test modules as their own units** — `menu/render/tests.rs` (7,288), `sim/tests.rs` (5,708),
  `container/tests.rs` (4,096), `app/tests.rs` (3,143), `server.rs`'s ~1,942-line `mod tests`.
  Splitting these is cheap and does reduce collisions, but do it **inside** the owning split as
  opportunistic tidying, never as its own commit series — a test file that moves on its own is a
  refactor with no feature behind it and no way to tell a lost test from a renamed one.

---

## Order, and what can run concurrently

Four tracks. Within a crate the work serialises (a broken lib blinds every test target in that crate);
across crates it does not.

```
Track B  mobs.rs  ──►  Track A  server.rs  ──►  (Track D  v770/server_protocol.rs)
Track C  lodestone-gui                                   ── fully concurrent with all of the above
Track E  instrument fixes  ── must LAND BEFORE Track A and Track D
```

1. **Track E first, and it is small.** Teach `cargo xtask connectedness` the directory form for
   `crates/lodestone-server/src/server.rs` and for `crates/protocol/*/src/server_protocol.rs`, the
   same way `adapter_source_paths` already accepts either a flat `src/adapter.rs` or a
   `src/adapter/` rooted at `mod.rs`. Land it, and record the instrument's output at that sha as the
   baseline. **Nothing in Track A or D may start before this.**
2. **Track B — `mobs.rs`.** The rehearsal. Cleanest seam, tests travel with their code, no visibility
   churn on the `impl MobSim` half.
3. **Track A — `server.rs`.** After B, same crate. This is the high-value one.
4. **Track C — `lodestone-gui`.** Different crate, different agents, runs start-to-finish alongside
   B and A. Its own internal order is given below.
5. **Track D — `v770/server_protocol.rs`.** After A, because the two files are co-edited 39 times in
   14 days and running both refactors at once puts two moving targets in one feature's path.

Later, and only after C lands: **Track F**, splitting `menu/nav.rs` by screen family inside the new
`lodestone-gui`.

**Track C's internal order** (each step is its own commit, each independently green):

1. Move the five escaping symbols down or sideways — `sim::SessionEnd`/`SessionEndKind`,
   `audio::subtitles`' two types, `camera_rig::{BobFrame, HURT_DURATION_TICKS}`,
   `gpu::entities::entity_texture_from_image` (into `lodestone-render`), and either `blocks.rs` or
   `resources.rs`. **Still inside `lodestone-shell`.** After this step, re-run the escape audit and
   require it to report **zero** — with the 1,145-reference control alongside it, because an audit
   that prints nothing has to be distinguishable from an audit that did not run.
2. Create `crates/lodestone-gui` with a manifest and an empty `lib.rs`. Add the workspace dependency
   entry. Confirm `cargo xtask check-connected` fails at this point (nothing depends on it yet) and
   that adding the `lodestone-shell` → `lodestone-gui` edge in the next step is what makes it pass —
   that is the control proving the instrument is watching.
3. Move the files, one subsystem per commit, in dependency order: `platform.rs`, `config.rs` +
   `keybinds.rs`, `chat.rs`, `overlay.rs`, `asset_objects.rs`, `resources.rs`, `saves.rs`,
   `offline_identity.rs`, `skin_fetch.rs`, `tablist.rs`, then `hud`, then `container`, then `menu`.
   `hud`/`container`/`menu` cycle, so those three land as **one** commit, not three.
4. Fix up the instruments (next section) in the same commit that moves the code they guard, never in
   a follow-up.

---

## Migration method

The `adapter.rs` series is the playbook and it transfers unchanged to Tracks A, B and D.

1. **A pure rename commit first.** `X.rs` → `X/mod.rs`, zero content change. `git show --stat` must
   report `1 file changed, 0 insertions(+), 0 deletions(-)`, exactly as `d983d0e7` did. If it reports
   anything else, the commit is not a rename and the rest of the series loses its baseline.
2. **One domain per commit**, each independently green, each a **pure move**. Nothing is renamed,
   reordered *within* a function, or restructured in a move commit.
3. **Verify each move with a normalized-line multiset diff** against the pre-split file at a pinned
   sha: strip leading/trailing whitespace from every line, drop blanks, sort, and diff the two
   multisets in **both** directions. This is what catches a lost or duplicated line regardless of how
   the content was reordered, and it is the only check that does. **Count the differences with a
   program that reads a file** — a `diff | grep -c '^<'` control has reported 0 here where the truth
   was about 15,000.
4. **Pin the test count before and after at the same sha**, with `--no-fail-fast`:
   `cargo test -p lodestone-server --no-fail-fast`. The adapter series recorded 796/796 both ways at
   one sha, and that is the bar. A count gathered while another agent is mid-edit is a sample, not a
   measurement — re-run rather than reason about a drift.
5. **After landing in a contended file, grep for one distinctive symbol per edit and require a
   non-zero count.** `server.rs` and `mobs.rs` are exactly the files where a concurrent wholesale
   rewrite silently discards your work, and a wholesale rewrite leaves a clean tree, a green build and
   no diff to read. The marker grep is the only signal.
6. **Commit with the pathspec form**, naming only the files in your own track, and check
   `git diff --cached` is empty immediately before — a count, with a verdict that depends on the count.

**A crate split (Track C) needs five things the file splits do not**, and this is where the precedent
stops applying:

- a manifest, with the dependency set **narrowed deliberately** (see the enforcement point above) and
  `lodestone-testsupport` under `[dev-dependencies]` only;
- `cargo check -p lodestone-shell --no-default-features` still green — the new crate must be
  version-free, or the version seam moves without anyone deciding that it should;
- the wasm crate list and the confinement rules extended to the new crate (below), in both the script
  and the xtask;
- a copy of the `no_wgsl_is_inlined_in_rust_sources` gate and of
  `no_production_source_names_testsupport.rs` in the new crate;
- `web/`'s own `Cargo.lock` regenerated — it is a separate workspace, and `just wasm-check` /
  `just run-wasm` are what prove it.

---

## What would break

This is the section the adapter split paid for. That refactor was verified thoroughly and still
**silently blinded `cargo xtask connectedness`**, which reported the whole v770 family as `SKIPPED`
with exit 0 because the scanner hardcoded a flat `src/adapter.rs`. Every item below is the same shape.

### Path-shaped instruments

| instrument | what it hardcodes | what a split does | loudness |
|---|---|---|---|
| `cargo xtask connectedness` | `crates/lodestone-server/src/server.rs` as the serverbound **second hop**, joined by `serverbound_variant_is_connected` | if the file becomes `server/mod.rs`, `dispatch_path.exists()` is false and every serverbound packet is reported **UNCLASSIFIED** with the reason "server.rs is absent" | **soft — a degraded report, not a failure** |
| `cargo xtask connectedness` | as above | if `server.rs` still exists but the `ServerBound::` arms move to a sibling, every variant reads as **STRANDED** | loud, but *wrong* — it looks like a real regression |
| `cargo xtask connectedness` | `crates/protocol/<family>/src/server_protocol.rs` in `classify_serverbound_decode` | Track D breaks it identically | soft |
| `cargo xtask check-deletable` / `conformance` | tests for `src/server_protocol.rs`'s existence to decide whether a family implements `ServerProtocol` | Track D makes a family look like it does not implement it | soft |
| `cargo xtask check-connected` | reachability from shipped `bin`/`cdylib` roots, plus `xtask/check-connected.toml` | a new crate with no dependant **fails** until wired | loud (use it as the control) |
| `scripts/wasm-check.sh` `WASM_CRATES` **and** `xtask`'s copy | an explicit crate list | a new crate is **not compiled for wasm32** unless added to both | **silent** |
| the 24 confinement rules, both copies | `label\|crates/<crate>/src\|pattern\|allowlist` | a **missing** directory is a hard FAIL; a **new** crate simply has no rules | missing dir: loud. New crate: **silent** |
| `crates/lodestone-{shell,render}/tests/wgsl_valid.rs` | scans one crate's `src/` for `@vertex`/`@fragment` | `.wgsl` moving to a new crate leaves it unguarded | **silent** |
| `crates/lodestone-shell/tests/no_production_source_names_testsupport.rs` | one crate's sources | same | **silent** |
| `cargo xtask docs-index` | scans `docs/`, `docs/plans/`, `docs/research/`, `docs/roadmap/` | only affected if a doc is added or renamed; regenerate and commit the index **alone and immediately** | loud |
| `[profile.dev.package.lodestone-worldgen*]` in the root manifest | crate names | a split crate silently reverts to `opt-level = 0`, which reports as a *slow* suite, never as a failure | **silent** |

**The confinement case, concretely, because it is the one that bites Track C.** The
`lodestone-shell thread-spawn-confinement` rule allowlists `mesher.rs, accounts.rs, status.rs`, and
`accounts.rs`/`status.rs` are `menu/accounts.rs` and `menu/status.rs` — both carry a real
`std::thread::spawn`, which **traps** on wasm32. Allowlist matching is `grep -vF "/$f:"`, i.e. a bare
**basename** matched recursively, so moving a file deeper inside the same crate is safe and moving it
across a crate boundary is not. Moving `menu/` to `lodestone-gui` therefore leaves two trapping calls
in a crate with **no rules at all**, plus two dead allowlist entries in the crate they left. The same
applies to `platform.rs`, which is the sole allowlisted file for `lodestone-shell`'s
`instant-confinement` and `systemtime-confinement` rules and would move to `lodestone-gui`.

So Track C must, in the same commit that moves the code:

- add `lodestone-gui` to `WASM_CRATES` in **both** `scripts/wasm-check.sh` and `xtask`;
- add `lodestone-gui` copies of the `instant-confinement`, `systemtime-confinement` and
  `thread-spawn-confinement` rules, allowlisting `platform.rs`, `accounts.rs` and `status.rs`;
- remove `accounts.rs`, `status.rs` and `platform.rs` from the `lodestone-shell` rules' allowlists
  (leaving `mesher.rs`), so a dead entry cannot silently cover a future file of the same name;
- confirm the run prints `confinement rules that actually ran: N/N` with the new N.

The parity test between the script and the xtask **parses both sources and diffs them**, so editing
one and not the other fails loudly. That is the one guard in this list that already works.

### Checked negatives — things that would break and do not

- **No `#[path]` harness is affected.** Every `#[path]` in the workspace is either
  `lodestone-data/src/lib.rs`'s 28 generated-table modules or one of two `*_native.rs` forks in
  `lodestone-render`. None is in `lodestone-shell` or `lodestone-server`.
- **CI needs no job edits.** Every job calls a `just` recipe with `--workspace`, and the root
  manifest's `members = ["crates/lodestone-*", …]` glob picks up a new `crates/lodestone-gui`
  automatically. The one job to watch is `check-seam`, which names `-p lodestone-shell` explicitly —
  verify the new crate is version-free rather than assuming it.
- **`web/Cargo.toml` needs no dependency edit.** It depends only on `lodestone-shell` and states that
  everything else arrives transitively. Its `Cargo.lock` will change.
- **`default-members = ["crates/lodestone-shell"]`** stays correct.
- **`check-isolation` and `check-deletable` are keyed on `crates/protocol/`**, so Tracks A, B and C do
  not touch them. Track D does.

---

## Verification, proportionate

The owner tests interactively and does not want heavy gates. **No pixel gates, no live oracles, no
new test suites** — a pure-move refactor cannot change behaviour, and the whole point of the multiset
diff is that it proves that more cheaply than any runtime assertion.

Per split, and nothing more:

1. `cargo test -p <crate> --no-fail-fast` before and after, **at one pinned sha**, counts recorded and
   equal. Let cargo write to a file and read its real exit status from the file with a program — a
   wrapper's "exit code 0" has been wrong here against a real status of 101.
2. Normalized-line multiset diff, both directions, empty.
3. `just check` and `just check-seam`.
4. **Re-run the instruments and record the numbers before and after** — this is the step the adapter
   split skipped: `cargo xtask connectedness`, `cargo xtask check-connected`,
   `cargo xtask wasm-check` (must print `rules that actually ran: N/N`, with the new N). A number that
   is byte-identical before and after is the pass; a number that changed is a finding, and
   `SKIPPED`/`UNCLASSIFIED` is a **failure**, never a pass.
5. Marker grep in the contended file, non-zero count required, with the verdict depending on the
   count.

---

## Open questions — things that could not be settled without running code

- **The rebuild-cost figure for Track C is a mechanism, not a measurement.** rustc's unit is the
  crate, so the direction of the effect is certain; the magnitude is not. Anyone who wants a number
  must measure both arms **concurrently** — a duration taken on this machine while other agents build
  is attributed to the wrong cause, which has already happened here once with a
  debug-versus-release story that was pure machine load. Prefer a counter (codegen units, or files
  recompiled) over a wall-clock duration.
- **Whether `lodestone-gui` really compiles without `lodestone-render` and without `wgpu`'s `window`
  feature** is the question that decides whether the boundary enforces anything. The static evidence
  is good (`hud` has zero real references to `gpu`) but `container/player_preview.rs` and
  `menu/render/renderer.rs` both take `&wgpu::Device`/`&wgpu::Queue`, so `wgpu` itself is certainly
  still needed. Only a real `cargo check -p lodestone-gui` settles which *features* of it are.
- **Whether `resources.rs` belongs in `lodestone-gui` or stays in the shell.** It is the one file in
  the set with a two-way pull — `hud`/`hud/vanilla_font.rs` call `resources::vanilla_manager`, and
  `resources.rs` itself pulls `blocks::{DemoClassifier, ShellClassifier}`. Both dispositions are
  defensible; whoever does step 1 of Track C should try the cheap one (move `blocks.rs` down as well)
  and fall back to leaving `resources.rs` in the shell with the font path taking an injected
  parameter.
- **How many of `server.rs`'s ~90 tests actually belong to each extracted domain.** They are one flat
  `use super::*` module, so the partition is a judgement call this plan deliberately defers; leaving
  them whole in `server/mod.rs` is the recommended first pass and costs only that `mod.rs` stays
  ~2,000 lines longer than it needs to be.
