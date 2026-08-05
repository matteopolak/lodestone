# Sprint food gate, toggle sneak/sprint, and mouse feel

## What it is

Three small, related fixes to `lodestone-controller`'s input model and the
settings screen's mouse/controls pages:

- **#200** — sprint is now gated on food level, matching vanilla's
  `LocalPlayer.canStartSprinting`.
- **#202** — `key.sneak`/`key.sprint` can be hold-to-activate (vanilla's
  default) or press-to-toggle, matching vanilla's `ToggleKeyMapping`.
- **#203** — `invertMouseX`/`invertMouseY`/`mouseWheelSensitivity` went from
  labels-only to real, persisted, wired options.

All three live mostly in `lodestone-controller/src/input.rs` (the pure,
platform-independent input model shared with the browser client) and
`lodestone-controller/src/ecs.rs` (the `GameTick` system that turns it into a
`MovementIntent`), plus the usual `config.rs`/`menu/options.rs`/`menu/nav.rs`
settings-screen wiring on the shell side.

## How it works

### #200 — the food gate

`InputState::movement_intent` never read food level at all. Vanilla's real
gate is `LocalPlayer.canStartSprinting` → `isSprintingPossible` →
`hasEnoughFoodToDoExhaustiveManoeuvres()`, which is
`foodData.hasEnoughFood() || abilities.mayfly` (`Player.java:1592-1594`), and
`hasEnoughFood()` is `foodLevel > 6.0` (`FoodData.java:92-94`) — a **strict**
cutoff, so exactly 6 does not qualify.

`movement_intent(state: &InputState) -> MovementInput` is **unchanged** —
`lodestone-shell`'s `sim.rs` calls it directly (a test via
`crates/lodestone-controller/src/input.rs::movement_intent`), and
this crate does not own that file. The gate lives in a new function,
`movement_intent_with_food(state, sprint_allowed_by_food: bool)`, which the
caller feeds a pre-resolved bool rather than food state itself — this crate
holds no food or ability data, that is server-reported `lodestone-ecs` session
data one layer up.

The production path is `ecs::compute_movement_intent`, a `GameTick` system
already registered in `TickSet::Intent`. It now queries
`Option<&lodestone_ecs::session::Vitals>` and
`Option<&lodestone_ecs::session::Abilities>` alongside its existing
components, computes

```
sprint_allowed_by_food = vitals.and_then(|v| v.food).is_none_or(|f| f > 6) || abilities.is_some_and(|a| a.may_fly)
```

and passes it through `ecs::swim_adjusted_intent` (also extended with the same
parameter) into `movement_intent_with_food`. `Vitals`/`Abilities` are both
`Option` because they start absent until
`lodestone_ecs::session::insert_session_components` runs — absence resolves to
*allowed*, matching `Vitals`'s own "no report yet" convention, not to "zero
food".

**Residual gap, not fixed here:** `sim.rs`'s own `movement_intent()` accessor
method (`sim.rs::Sim::movement_intent`) and its handful of callers (mostly `sneak` reads in
tests) still go through the ungated function. `sim.rs` is owned by another
in-flight agent at the time of this change, so it was not touched. If that
accessor ever becomes the real sprint-decision path (it currently is not —
`compute_movement_intent` is), it needs the same treatment.

### #202 — toggle sneak/sprint

`InputState` gained `toggle_sneak`/`toggle_sprint` (config, not per-key
state) plus `sneak_key_down`/`sprint_key_down` (raw physical state, tracked
only to detect a press edge). `InputState::set_toggle_modes(sneak, sprint)`
sets the config; it is cheap and idempotent to call every tick.

`set(Action::Sneak, held)`/`set(Action::Sprint, held)` now branch: hold mode
(the default) behaves exactly as before — the public `sneak`/`sprint` fields
mirror the physical key. Toggle mode instead flips `sneak`/`sprint` **only on
a fresh press edge** (`held && !was_down`); a release does nothing. This
matches `ToggleKeyMapping::setDown` exactly
(`ToggleKeyMapping.java:33-40`): `if (needsToggle) { if (down) setDown(!isDown()); }`.

Nothing downstream (`movement_intent`, the double-tap-sprint window) had to
change: `sneak`/`sprint` are still "the effective `isDown()` value", which is
exactly what vanilla's own consumers read too — toggle mode is invisible to
everything except `set` itself.

`release_all()` (cursor released / window loses focus) still clears
`sneak`/`sprint` to `false` in toggle mode too, matching
`KeyMapping.releaseAll()` → `ToggleKeyMapping.release()`'s unconditional
`reset()`. But it preserves the `toggle_sneak`/`toggle_sprint` **option**
across the reset, the same way it already preserved `mouse_dx`/`mouse_dy` —
losing the option itself on every cursor-release would be a much stranger bug
than losing a held key.

**Landed:** `Sim::set_toggle_modes(sneak, sprint)` (`sim.rs`) stores the two
bools; `app.rs`'s `redraw()` pushes `nav.toggle_sneak()`/`toggle_sprint()`
into it once per frame, *before* `Sim::step`, and `Sim::step` itself calls
`input_mut(|i| i.set_toggle_modes(...))` at the top of every call — one push
per frame, but that covers every catch-up tick that frame runs, since the
option cannot change mid-frame. See
`sim::tests::toggle_sneak_option_reaches_live_input_and_survives_key_release`/
`toggle_sprint_option_reaches_live_input_and_survives_key_release` for the
end-to-end proof (with a hold-mode negative control), not just that the
setter exists.

### #203 — mouse feel

Three options, three different depths of wiring:

- **`mouseWheelSensitivity`** is fully wired: `MenuNav::cycle_mouse_wheel_sensitivity`
  steps the persisted value by `config::MOUSE_WHEEL_SENSITIVITY_STEP` (0.25),
  wrapping additively (not by rounding to a quantized step — see that
  function's doc comment on why a round-trip through an integer step index
  drifts) within vanilla's own slider bounds
  (`config::MIN_MOUSE_WHEEL_SENSITIVITY`/`MAX_MOUSE_WHEEL_SENSITIVITY`,
  `10^(-200/100)..10^(100/100)` from `Options.java:480-482`). **Landed:**
  `app.rs`'s `MouseWheel` handler now scales the hotbar-cycle step by
  `nav.mouse_wheel_sensitivity()` through `accumulate_scroll`, a fractional
  carry mirroring vanilla's own `ScrollWheelHandler.onMouseScroll` (so a
  sensitivity below `1.0` takes more than one notch to move a slot, and one
  above `1.0` can cross several in one notch, rather than a threshold on the
  old fixed ±1 step) — see `app::tests::accumulate_scroll_*` for the exact
  scaled amounts and the direction-reversal reset. The multiplayer server
  list's own `MouseWheel` arm (issues #402/#445, `nav.scroll_server_list`) is a
  separate, unscaled arm — it passes `dy` straight through to
  `widget::ScrollList::mouse_scrolled`, which moves vanilla's
  `scrollRate = defaultEntryHeight / 2` (**18 px** for a 36 px row) per notch.
  It needs no accumulator precisely *because* its offset is pixels: a fraction
  of a notch is already a meaningful number of pixels, where `cycle_slot` takes
  a discrete slot step and so must accumulate fractions until one is due. See
  `docs/server-list.md`'s scrolling section — and note this bullet described the
  arm as row-quantized until #445, which it no longer is.
- **`invertMouseX`/`invertMouseY`** are wired end to end: `Sim` gained
  `invert_mouse_x`/`invert_mouse_y` fields and `Sim::set_mouse_invert`,
  pushed from `app.rs`'s `redraw()` (`nav.invert_mouse_x()`/`invert_mouse_y()`)
  once per frame, *before* `Sim::step` so the frame the option changes
  already sees it. `Sim::apply_mouse` now calls
  `lodestone_controller::apply_look_inverted(yaw, pitch, dx, dy, sensitivity,
  invert_x, invert_y)` instead of plain `apply_look` — it negates `dx`/`dy`
  before `apply_look`'s sensitivity curve, matching vanilla's
  `MouseHandler.turnPlayer`'s
  `player.turn(invertMouseX ? -xo : xo, invertMouseY ? -yo : yo)` (negation
  happens *after* the curve there; the two orders agree numerically since the
  curve has no dependence on the delta's sign — see the function's own doc
  comment). See `sim::tests::invert_mouse_x_negates_the_yaw_delta_exactly`/
  `invert_mouse_y_negates_the_pitch_delta_exactly` for the exact-magnitude
  proof (not just a sign flip) — the yaw one has to compare through a
  wrap-safe angular delta, since `apply_look` wraps yaw into `[-180, 180)`
  and a fixture whose starting yaw sits near that seam can wrap the plain and
  inverted runs on opposite sides.
- ~~**`sensitivity` (mouse look, not the wheel)** is deliberately still
  inactive.~~ **Live since #443** (`afba832`): it moved off the argv-only
  `config::Config` onto the persisted `config::Options`, and
  `Config::resolve_persisted` folds it in at launch so the seven existing readers
  needed no edit. **Known limitation: the fold happens at launch, so a change made
  in the settings screen applies on the next one.** Closing that needs a
  `Sim::set_sensitivity` in `sim/step.rs` plus a per-frame push from
  `app/redraw.rs` beside `set_mouse_invert` — it is *not* the ~2-line
  `app/redraw.rs` change it looks like, because there is no setter to call yet.
- ~~**`discreteMouseScroll`**~~ is **live since #444**. Its consumer is
  `app::scale_scroll`, which reproduces `MouseHandler.onScroll`
  (`MouseHandler.java:189-192`): `(discrete ? signum(dy) : dy) * sensitivity`,
  computed **once** and handed to both wheel consumers — the hotbar and every menu
  list — exactly as vanilla hands one `scaledYOffset` to `ScrollWheelHandler` and
  `screen().mouseScrolled(..)`. The order is load-bearing: scaling before `signum`
  would cap wheel speed at one notch and silently break the sensitivity row, which
  is what `discrete_scrolling_takes_the_sign_before_sensitivity_scales_it` executes
  as its wrong hypothesis rather than describing.
- **`allowCursorChanges`/`rawMouseInput`** remain inactive, and they are a
  different tier from the row above rather than the same one: neither has a
  *subsystem* to gate, not merely an unwired consumer. This shell never changes the
  OS cursor and has no raw-input mode. Wiring either label would be the exact
  fabrication #203 exists to fix. See `docs/options-consumption-census.md`'s
  three-tier breakdown of the Input group.

## How to change it

- `InputState`'s toggle fields are private; go through `set_toggle_modes` and
  `set`, not direct field access.
- The food-gate threshold is `input::MIN_FOOD_LEVEL_TO_SPRINT` (6) — a single
  named constant, not inlined, so a future difficulty-mode exception (if one
  is ever added) has one place to look.
- The mouse-wheel-sensitivity slider's step and bounds are
  `config::MOUSE_WHEEL_SENSITIVITY_STEP`/`MIN_MOUSE_WHEEL_SENSITIVITY`/
  `MAX_MOUSE_WHEEL_SENSITIVITY`. If the click-step model is ever replaced with
  a real draggable slider, these are the constants to re-derive.
- Every new `LiveOption` variant needs an arm in three places kept in sync by
  the compiler (exhaustive matches): `options::live_value`,
  `nav::apply_settings`, and a persisted field + accessor on
  `config::Options`/`MenuNav`. Missing any one is a compile error, not a
  silent gap — that is deliberate.

## Configuration

Five new `Options` fields, all in `options.json` beside `gui_scale`:
`toggle_sneak`, `toggle_sprint`, `invert_mouse_x`, `invert_mouse_y` (all
default `false`, vanilla's own defaults, and write no key when untouched),
`mouse_wheel_sensitivity` (default `1.0`, vanilla's own `logMouse(0)`, and
degrades to `1.0` — not `0.0` — on a corrupt or out-of-range value, since a
`0.0` multiplier would silently disable the scroll wheel entirely).

## Dependencies

- `lodestone-ecs::session` — `Vitals`/`Abilities`, the server-reported food
  level and fly permission the food gate reads.
- `lodestone-ecs::player` — `LocalPlayer`, `MovementIntent`, `Submersion`, the
  components `ecs::compute_movement_intent` already queried before this
  change.
- The 26.2 jar at `.cache/mc/26.2/client-src` — `Player.java`, `FoodData.java`,
  `ToggleKeyMapping.java`, `Options.java`, `MouseHandler.java` — behavioural
  reference only, never transliterated.

## Landed: the `app.rs`/`sim.rs` patches this doc's fixes were waiting on

Closes #402 (the server-list half), #203 and #202. Both patches this doc
originally described as "brokered, not yet landed" have landed, once
`sim.rs`/`app.rs` had a clean window:

1. **`app.rs`'s `MouseWheel` handler** is now two arms: the existing
   gameplay one scales the hotbar-cycle step by `nav.mouse_wheel_sensitivity()`
   through `accumulate_scroll` (see #203 above), and a new
   `Screen::ServerList` arm calls `nav.scroll_server_list` with the real
   canvas height from `RenderTarget::size`/`logical_canvas` — see
   `docs/server-list.md`'s scrolling section for that half's own detail.
2. **`sim.rs`** now calls `apply_look_inverted` from `apply_mouse` (see #203
   above) and applies `Sim::set_toggle_modes`'s pushed option to the live
   `InputState` once per frame at the top of `Sim::step` (see #202 above).

`lodestone_controller::apply_look_inverted` also needed re-exporting from
`lodestone-controller/src/lib.rs`'s `pub use input::{...}` list — it existed
in `input.rs` since the original change but was not on that list, so nothing
outside the crate could name it.
