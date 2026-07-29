# In-flight work — paused 2026-07-28

Six agents were mid-flight when this session paused. **Nothing in this batch has been
compiled**: agents are barred from `cargo` runs (shared `target/` lock), and the single
central `cargo check --workspace --all-targets` pass had not run yet. Treat every item
below as *written, not proven*.

Delete this file once the batch is landed or abandoned — it is a pause artifact, not a
subsystem doc.

## The one thing to do first

```bash
cargo check --workspace --all-targets
```

That is the first real signal on ~1,900 lines across 20 files. Expect failures; they are
cheap to fix and located, not mysterious.

**One failure is already known**, found incidentally by another agent rather than by the
check: `crates/lodestone-shell/src/menu/nav.rs:987` — a shadowed `nav` binding in that
file's own test module. It belongs to the GUI-scale work. Fix it first so it does not mask
everything alphabetically later, which is the `cargo test` fail-fast trap in
[`CLAUDE.md`](../CLAUDE.md).

**A second gap blocks mob equipment and is not a compile error**, so the check will not
find it: `crates/lodestone-shell/src/net.rs`'s `entity_snapshot()` (~`:1178`) builds
`EntitySnapshot` from `EntityView` and **drops `equipment` on the floor**. Until that
carries through, anything `entities.rs` does with equipment reads nothing. Same shape as
the velocity/`on_ground` fix documented at `net.rs:1190`.

Then, separately, the two new container tests whose *runtime* result was never observed:

```bash
cargo test -p lodestone-shell --test container_screen
```

## State per workstream

| workstream | files | state |
|---|---|---|
| **Double-tap W to sprint** | `lodestone-controller/src/input.rs` | Complete. Six unit tests written, **none executed**. |
| **Inventory carried stack + count size** | `lodestone-shell/src/{container.rs,hud.rs,hud/item_icon.rs}`, `tests/container_screen.rs` | Complete; `cargo check -p lodestone-shell --all-targets` passed. Two new tests unexecuted. |
| **bevy migration Stage 0** | new `crates/lodestone-ecs/` (complete, standalone), root `Cargo.toml` (kept); `lodestone-client/{state.rs,Cargo.toml}` cutover **drafted then reverted to HEAD** — zero diff right now | **Crate done, integration not started.** `Inner.world_age`/`time_of_day` untouched (not deleted, not mirrored — single source of truth, unchanged from pre-session). `app.rs`/`handle.rs`/`lodestone-shell/Cargo.toml`/`wasm-check.sh` never touched. wasm-check.sh **not run**. See detail below the table. |
| **GUI scale + settings screen** | `lodestone-shell/src/{config.rs,menu.rs,menu/nav.rs,menu/render.rs}` | **Written end-to-end, compilation unconfirmed.** Settings screen is reachable (not an island) per static review; see detail below the table. |
| **Break particles + item collision** | `lodestone-shell/src/{sim.rs,entities.rs}`, `lodestone-model/src/event.rs` | Particle guard **fixed** (see Open questions — the hypothesis was wrong, the real cause was tick 1 vs tick 2). Item collision **done** via shared `lodestone_physics::move_entity`, with a positive test and a negative control. `Reported<T>` enum defined but **applied to nothing** — see below. |
| **Display slots (was: mob equipment + first-person arm)** | `lodestone-assets/src/{icon.rs,model.rs,lib.rs}`, `lodestone-render/src/{block_models.rs,entity.rs}`, `lodestone-shell/src/gpu.rs` | Display-slot plumbing **complete and consumed** — dropped items now pose from the item's declared `ground` instead of hardcoded constants. Mob equipment and the first-person arm are **not built** (zero pixels); designs recorded below. |

### bevy migration Stage 0 — detail (see `docs/bevy-migration.md`)

**Exact disk state right now (`git status`):**
- `crates/lodestone-ecs/` — new, untracked, **complete standalone crate**:
  `Cargo.toml` (`bevy_app`/`bevy_ecs` `default-features = false, features = ["std"]`,
  `parking_lot`) + `src/{lib.rs, schedules.rs, sets.rs, resources.rs, plugin.rs, handle.rs,
  runner.rs}`. Depends on no other `lodestone-*` crate, so it cannot destabilise anything
  else in this shared checkout regardless of what else is mid-edit. Has `#[cfg(test)]`
  unit tests, **not executed** (`cargo test` barred this session). A
  `cargo check -p lodestone-ecs --all-targets` was launched near the end of this session to
  sanity-check it standalone; it had not returned when the session paused (a from-scratch
  `bevy_app`+`bevy_ecs` native build took ~4m28s on an earlier, smaller placeholder-only
  check, so this is plausibly just slow, not stuck). **Re-run
  `cargo check -p lodestone-ecs --all-targets` first** — its result is unknown.
- `Cargo.toml` (workspace root) — modified, kept. Added `lodestone-ecs` to
  `[workspace.dependencies]` (path dep) and `bevy_app = "0.19"` / `bevy_ecs = "0.19"`
  (`default-features = false, features = ["std"]`) / `parking_lot = "0.12"`. `Cargo.lock`
  updated to match, also kept.
- `crates/lodestone-client/src/state.rs`, `crates/lodestone-client/Cargo.toml` — the
  cutover was **fully drafted, then reverted to HEAD** (`git checkout HEAD -- <both
  files>`) on an earlier pause instruction, before a later message countermanded it and
  said to stop reverting. Net effect: **these two files currently have zero diff from
  HEAD.** `Inner.world_age` / `Inner.time_of_day` are present and unmodified
  (`state.rs:188-189`, fold at `:668-674`) — not deleted, not mirrored, single source of
  truth exactly as before this session touched anything.
- `crates/lodestone-shell/src/app.rs`, `crates/lodestone-client/src/handle.rs`,
  `scripts/wasm-check.sh`, `crates/lodestone-shell/Cargo.toml` — **never touched this
  session.**

**Next action for whoever resumes this — mechanical, not exploratory.** Every API call
below was checked against the real `bevy_ecs`/`bevy_app` 0.19 source on disk
(`~/.cargo/registry/src/*/bevy_ecs-0.19.0/`, `.../bevy_app-0.19.0/`), not assumed:

1. `crates/lodestone-client/Cargo.toml`, in `[dependencies]`, add:
   `lodestone-ecs = { workspace = true }`.
2. `crates/lodestone-client/src/state.rs`:
   - `use lodestone_ecs::{EcsHandle, WorldTime};`
   - Delete `world_age: i64` / `time_of_day: i64` from `struct Inner` (~:188-189) and their
     init in `impl Default for Inner` (~:202-203).
   - Delete the `ClientEvent::TimeChanged { .. } => { self.world_age = ...; self.time_of_day
     = ...; }` arm from `Inner::apply` (~:668-674) — falls through to the existing
     `_ => {}`, so nothing else needs to change there.
   - Add `ecs: EcsHandle` to `struct SharedState`, alongside `inner`/`world`/`notify`.
   - `SharedState::default()`: `let ecs = lodestone_ecs::new_handle();
     ecs.write().insert_resource(WorldTime::default());` then include `ecs` in `Self { .. }`.
   - `SharedState::apply(&self, event)`: special-case `ClientEvent::TimeChanged { world_age,
     time_of_day }` **before** delegating to `Inner::apply` — write into
     `self.ecs.write().resource_mut::<WorldTime>()`'s two fields; the `else` branch keeps
     calling `inner.apply(event)` exactly as today. (This can't live inside `Inner::apply`
     itself — that method has no access to sibling `SharedState` fields like `ecs`.)
   - `SharedState::time(&self) -> (i64, i64)`: read
     `self.ecs.read().resource::<WorldTime>()` instead of `inner.world_age`/`time_of_day`.
     **Keep the `(i64, i64)` return type** — do not change it to return `WorldTime`
     directly. `crates/lodestone-shell/tests/live_entity_light_time_of_day.rs:88` calls
     `.world_time().1` and that test is outside anyone's edit allowlist; changing the shape
     breaks it for no benefit. `ClientHandle::world_time()` in `handle.rs` needs **no code
     change**, only its doc comment is worth updating to say it now reads the ECS resource.
3. `crates/lodestone-shell/Cargo.toml`, in `[dependencies]`, add:
   `lodestone-ecs = { workspace = true }`.
4. `crates/lodestone-shell/src/app.rs`: add `ecs: lodestone_ecs::app::App` to `WindowApp`,
   built in `WindowApp::new()` as `let mut ecs = lodestone_ecs::app::App::new();
   ecs.add_plugins(lodestone_ecs::CorePlugin);`. In `redraw()` (~:760-777), call
   `self.ecs.update();` immediately before `self.sim.step(dt);`.
5. Add `"lodestone-ecs"` to `scripts/wasm-check.sh`'s `CRATES` array (~:85-111) and **run
   it — this is still the single biggest open question, completely unchanged from before
   this session started.**

**Known, deliberate consequence of the above, not an oversight:** `SharedState`
(`lodestone-client`, net thread) and `WindowApp` (`lodestone-shell`, winit thread) end up
with **two separate `bevy_ecs::World`s**, at least until a later stage unifies them behind
one `Arc<RwLock<World>>` per `docs/bevy-migration.md` §4.1. `WorldTime` is only ever
inserted into `SharedState`'s `World` (step 2 above); `WindowApp`'s `App` (step 4) is a
genuinely empty scaffold — `CorePlugin` deliberately does **not** insert `WorldTime` itself
(see the doc comment on `CorePlugin` in `plugin.rs`), specifically so this split doesn't
silently become two diverging copies of the same clock.

**Contradictions / gaps found in the plan and the Stage 0 briefing** (asked for explicitly —
not smoothed over):

1. **The file-edit allowlist this work was scoped under cannot produce a compiling
   deliverable as written.** It permitted `state.rs`, `handle.rs`, `app.rs`, workspace
   `Cargo.toml`, `wasm-check.sh`, plus creating `lodestone-ecs/` — but omitted
   `crates/lodestone-client/Cargo.toml` and `crates/lodestone-shell/Cargo.toml`. Neither
   `state.rs` nor `app.rs` can name `lodestone_ecs::*` without its crate's manifest
   declaring the dependency; there is no Rust-level way around that. Both need exactly one
   new line (see steps 1 and 3 above). Not a judgment call — the deliverable is unreachable
   without it.
2. **`bevy_reflect` really is droppable — confirmed, not just assumed, but Cargo.lock alone
   is misleading.** `Cargo.lock`'s raw per-package `dependencies` array lists `bevy_reflect`
   under `bevy_ecs` even with `default-features = false, features = ["std"]` requested,
   which looks at first glance like §3's claim is wrong. It isn't: `cargo tree -p
   lodestone-ecs -e features` (the actual feature-resolved graph) shows **zero**
   `bevy_reflect` activation, and the downloaded crate's own `Cargo.toml` confirms
   `bevy_reflect` is `optional = true`. Cargo.lock's flat list is not filtered by activated
   features — worth recording so nobody re-loses time to the same false alarm.
3. **§4.1(c) undersells why Stage 0 needs `EcsHandle` at all.** The plan frames
   `Arc<RwLock<World>>` as being for "outsiders" once a driver owns the real `World`
   outright. But *given the allowlist gap in point 1*, the handle isn't a nice-to-have for
   future bot code — for Stage 0 specifically it's the only way `SharedState` (the net-thread
   side, today's closest thing to "the driver" for scalar state) can hold a `bevy_ecs::World`
   at all without also touching `spawn.rs`/`net.rs`. Worth stating explicitly that Stage 0's
   real `WorldTime` owner ends up being `lodestone-client`, not `lodestone-shell`, and that
   unifying the two `World`s is pushed to a later stage — see the two-`World`s note above.

### GUI scale + settings screen — detail

**Scope for this workstream was `lodestone-shell/src/{menu.rs, menu/**, config.rs}` and
tests for those only** — `app.rs`, `hud.rs`, `container.rs` were explicitly out of bounds.
Every change below respects that; nothing outside those paths was touched.

**Files and what's in each:**

- `crates/lodestone-shell/src/config.rs` (+~280 lines, all new — no existing code
  changed): `calculate_gui_scale(desired, framebuffer_width, framebuffer_height) -> u32`,
  a line-for-line port of vanilla's `Window.calculateScale`
  (`.cache/mc/26.2/client-src/com/mojang/blaze3d/platform/Window.java:445-463`), plus
  `AUTO_GUI_SCALE` (`= 0`), `MAX_MANUAL_GUI_SCALE` (`= 8`, our own cap — see doc comment,
  vanilla's is effectively unbounded and dynamically clamped, which would mean threading a
  live framebuffer size into the pure nav layer), and `Options` (persisted settings,
  currently just `gui_scale`) with `load`/`load_from`/`save`/`save_to`/`options_path()`.
  `options_path()` reuses `menu::servers::data_dir()` — writes `options.json` **beside**
  `servers.json`, not a second config location. Tests hand-derive expected scales from
  vanilla's own algebra (e.g. 854×480 auto → 2, 1280×720 auto → 3, 3840×2160 auto → 9) rather
  than re-tracing this implementation, per the repo's evidence-standards rule.
- `crates/lodestone-shell/src/menu.rs`: added `Screen::Settings`, `UiState::is_settings()`,
  `open_settings()`/`close_settings()` (title-screen-only guard, same shape as
  `open_server_list`), extended `is_menu()` and `on_escape()` (`Settings → MainMenu`).
  Extended the existing `menu_screens_never_grab_the_cursor_or_take_gameplay_input` test to
  include it; added 3 new tests.
- `crates/lodestone-shell/src/menu/nav.rs`: added `MainButton::Options` ("OPTIONS"),
  inserted into `MAIN_BUTTONS` as `[Singleplayer, Multiplayer, Options, Quit]` — deliberately
  placed so `Multiplayer` stays index 1 and `Quit` stays last, which is what the two existing
  wrap-navigation tests key off; neither needed to change. `MenuNav` gained `options` /
  `options_path` / `options_save_error` fields, `gui_scale()` / `options_save_error()`
  accessors, and a `with_paths(path, options_path)` constructor — `with_path(path)` (the
  existing, widely-used-in-tests signature) now derives `options_path` as
  `path.parent().join("options.json")`, so every existing call site keeps compiling
  unchanged and gets an isolated options file for free. `key_settings()`: Up/Down step
  `gui_scale` by ±1, wrapping `0..=MAX_MANUAL_GUI_SCALE` (`AUTO_GUI_SCALE..=8`), Escape
  returns to the main menu; saved **eagerly** on every change (same rule as the server list —
  no guaranteed clean-shutdown hook). ~6 new tests, including a real-file persistence
  round-trip and a forced-write-failure case mirroring the server list's own.
- `crates/lodestone-shell/src/menu/render.rs`: `MenuFrame` gained a `gui_scale: u32` field.
  `frame_for()` now post-processes its match result with one `.map(|mut f| { f.gui_scale =
  nav.gui_scale(); f })` **after** the per-screen match — so every screen's frame carries the
  current scale, not just Settings'. `owns_frame()` extended with `Screen::Settings`; a new
  match arm builds its one-row frame ("GUI SCALE: AUTO" / "GUI SCALE: N"). The actual DPI
  fix is `logical_canvas(gui_scale, framebuffer_w, framebuffer_h) -> (f32, f32)`, a new pure
  function: it calls `calculate_gui_scale` and divides the (physical) framebuffer size by the
  result — vanilla's `guiScaledWidth`/`Height`. `MenuRenderer::render()` calls it before
  calling `geometry()`, replacing the old `geometry(frame, width as f32, height as f32)` with
  `geometry(frame, logical_w, logical_h)`. **`render()`'s own signature is unchanged** — the
  scale rides on the `MenuFrame` precisely so `app.rs`'s call site (`menu.render(device,
  queue, surface.view(), &frame, w, h)`, out of scope) needed no edit. `geometry()` itself
  was **not** touched — it stays pixel-space/scale-agnostic, which is why none of its
  existing tests needed updating (only the `frame_with` test helper and the
  `owns_frame_agrees_with_frame_for_on_every_screen` screen list/count, both purely
  mechanical for the new field/variant). 3 new tests on `logical_canvas` itself.

**Is Settings reachable, or an island?** Reachable, by static trace (not a live/GPU run):
main menu → `Down` ×2 → `Options` button → `Enter` → `ui.open_settings()` →
`Screen::Settings`. `app.rs`'s input routing (read, not edited — confirmed at `app.rs:1206,
1214, 1329`) gates on `crate::menu::render::owns_frame(self.ui.screen())` generically, not
per-screen-name, so once `owns_frame` includes `Settings` the existing winit→`MenuKey`→
`nav.key()` path should reach it with no `app.rs` change needed. Likewise `draw_menu()`
(`app.rs:683-726`) calls `frame_for()` unconditionally and draws whatever it returns, so a
`Settings` frame should render through the same path `ServerList` does today.
**Not verified live** — no window/GPU run was done, only reading the call sites.

**Does changing the scale actually move pixels?** By the same static trace: yes, and on
every screen, not just Settings — `frame_for()` stamps `nav.gui_scale()` onto *every*
returned `MenuFrame` (see above), and `MenuRenderer::render()` recomputes `logical_canvas`
from that field every single frame, so pressing Up/Down on the settings screen should resize
the whole menu (title screen included) starting the very next frame. This was the "prove the
model works on something real" requirement, wired end-to-end:
`nav.gui_scale()` → `MenuFrame.gui_scale` → `logical_canvas()` → `geometry()`'s canvas size.

**Compilation: unconfirmed at pause.** A `cargo check -p lodestone-shell --all-targets` was
launched in the background before the "no cargo" instruction landed this session; it had
produced no output by the time this note was written (likely queued behind another agent's
`target/` lock, going by the ~4m28s bevy build time noted in the Stage 0 section above).
Every changed struct-literal site was reviewed by eye (all `MenuFrame` literals in
`frame_for`'s 5 arms now either set `gui_scale` or use `..Default::default()`; `frame_with`'s
test helper updated) but this is real risk, not zero risk — expect at most a small
typo/import/borrow-checker fix, not a structural problem.

**Handoff this workstream owed but could not do itself** (`hud.rs`/`container.rs` are out of
scope): both files draw in **device pixels with an implicit hardcoded ×2** and need to
become scale-derived, multiplying by `calculate_gui_scale`'s result (or reading it off
whatever plumbing carries it to them — not designed here, since it touches files outside
this scope). No such inventory of the *specific* constants (`22.0` cell size, `6.0` margins,
`18.0` line pitch, etc.) was produced this session — that line-by-line audit is unstarted and
should be the first step of the follow-up, before any HUD-side edit.

**Next concrete action for whoever resumes:**
1. `cargo check -p lodestone-shell --all-targets`, fix whatever it reports.
2. `cargo test -p lodestone-shell --no-fail-fast` (per `CLAUDE.md`'s fail-fast warning — do
   **not** use plain `cargo test -p`, it stops at the first failing binary). Watch especially:
   `config::tests::auto_scale_*` / `a_manual_scale_is_*` / `scale_never_drops_below_one_*`
   (hand-derived vanilla parity), `nav::tests::options_button_sits_between_*`,
   `nav::tests::settings_up_down_cycles_the_gui_scale_and_persists_through_a_real_file`,
   `render::tests::owns_frame_agrees_with_frame_for_on_every_screen` (now expects `reached ==
   10`), and the three `logical_canvas_*` tests.
3. If those are green: do a live/GPU run (`--headless` or windowed) and actually watch the
   Settings screen resize the menu on Up/Down — this session's "reachable"/"moves pixels"
   claims above are both static-trace, not observed.
4. Then start the `hud.rs`/`container.rs` constant audit named above — that inventory is the
   actual deliverable blocking the rest of the GUI-scale work, and it was not produced.

**One correction to the original briefing:** it described the Retina bug as "no DPI scaling
at all" as if a DPI factor were simply missing from the inputs. That's not quite right —
`winit`'s `window.inner_size()` (what `lodestone-render`'s `SurfaceTarget` is built from, and
what `WindowEvent::Resized` delivers on later resizes) **already returns physical pixels**,
so the framebuffer size the app tracks already *is* DPI-inclusive; there is no separate scale
factor to plumb in. The actual bug was narrower: `geometry()`'s fixed pixel constants were
being laid directly into that physical size instead of a scale-*divided* logical size. The
fix is one divide (`logical_canvas`), not a new DPI input — worth knowing so nobody goes
looking for a `scale_factor()` call that was never the missing piece.

## Named islands (deliberate, not defects — but they are zero-pixel today)

Per rule 1 in [`CLAUDE.md`](../CLAUDE.md), these are recorded rather than left to be
rediscovered:

- **`crates/lodestone-ecs/`** — nothing consumes it. Stage 0 stopped before the
  `app.rs`/`handle.rs` cutover.
- **`VersionAdapter::tool_mining`** (landed earlier in `875f452`, not this batch) — fully
  implemented and tested, and **called from nowhere in `lodestone-shell`**. `sim.rs`'s
  `bare_hand_break_inputs` still hardcodes `BreakInputs::default()` for the tool fields and
  never reads the held item, so a pickaxe currently mines no faster than a fist. Wiring this
  is a high-value, well-scoped next task. Note when wiring it: `requires_correct_tool` and
  `correct_tool` are **inverses** bare-handed — feeding one straight into the other makes
  stone break in 45 ticks instead of 151, which is the defect pinned by
  `bare_hand_on_stone_is_151_ticks_not_45`.
- **Widened display slots in `lodestone-assets`** — `icon.rs` previously kept only the
  `gui` slot and discarded `thirdperson_righthand`/`firstperson_righthand`, even though
  `model.rs` already parsed them. The parse side is widened; **no render consumer reads
  the new slots yet**. This one line gates three features at once: the first-person arm,
  mobs holding items, and dropped items (which currently pose with hardcoded constants).

## Open questions that were the actual deliverable

- **Break particles — RESOLVED, hypothesis was wrong.** The conjunction below *does* fire
  (`continue_`'s same-target branch pushes `swing()` unconditionally, after the `if finished`
  block). The real defect was different and one layer up: vanilla's `continueAttack` runs in
  the **same client tick** as `startAttack`, so the chip appears from tick 1, not tick 2 —
  the doc comment claiming otherwise was itself wrong. Guard now captures `target()` both
  before and after the call and ORs them. Kept here only as a record of the wrong turn:
- ~~**Break particles.** Hypothesis, *unconfirmed*: the guard
  `self.mining.target() == Some(pos)` **and** the action list containing `SwingArm` may be
  **mutually exclusive** — `SwingArm` is emitted on dig *start*, while `target()` is only
  `Some` once a dig is already underway. If so the particle call can never fire on any
  tick. Trace the state machine; do not accept this from the note. A previous hypothesis
  on this same bug sent an agent to entirely the wrong file, and separately
  `Particles::breaking_block` was once found to have **zero call sites** in the tree.~~
- **wasm go/no-go for `bevy_ecs`.** Still **not run**, unchanged from before this session.
  `scripts/wasm-check.sh` needs `"lodestone-ecs"` added to its `CRATES` list first (not
  done yet — see the Stage 0 detail section above, step 5) — that edit plus the run is the
  only acceptable evidence, and [`bevy-migration.md`](./bevy-migration.md) calls it the
  migration's single biggest go/no-go. **If `bevy_ecs` is not wasm-clean, the plan
  changes** — resolve this before fanning out Stage 1.
- **`Inner.time_of_day` must be deleted, not mirrored.** This is Stage 0's authority test.
  **Confirmed at this pause: neither.** `state.rs` is back at HEAD (see detail above) —
  `Inner.world_age`/`time_of_day` are exactly as they were before this session, a single
  source of truth. `lodestone_ecs::WorldTime` exists as a type but is not constructed from
  any real event yet, so there is no second copy in play — the cutover simply hasn't
  started, as opposed to having started and left two sources live. The exact remaining
  diff is in the Stage 0 detail section above.

## Known-incomplete, deliberately deferred

- `movement_intent(&self.input)` is computed once per **frame**, outside the
  `while accumulator >= TICK_DT` loop in `sim.rs`, so a slow frame running several ticks
  reuses one intent for all of them. **Pre-existing**, unrelated to sprint, and explicitly
  kept out of that change so it would not be smuggled in. Needs its own scope.
- `InputState::tick()` must be called **inside** that 20 Hz loop, not per frame — per-frame
  placement makes the double-tap window frame-rate dependent. Was requested of the agent
  owning `sim.rs`; confirm it is present and correctly placed.
- Two `Option<Option<T>>` sites remain in `lodestone-client/src/state.rs` (~`:134`, `:177`),
  held by Stage 0. The named-enum conversion matters beyond style: a dropped item sends its
  item id **exactly once at spawn**, so any layer treating "absent" as "cleared" blanks it a
  tick later and the item goes invisible.
- Third-person player body — deferred on purpose; the arm comes first.
- `--host` typed explicitly still lands on the main menu; the fix is to compare against
  `Config::default()`.
