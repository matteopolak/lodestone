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
`lodestone-shell`'s `sim.rs` calls it directly (a test, `sim.rs:6645`), and
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
method (`sim.rs:1854`) and its handful of callers (mostly `sneak` reads in
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

**Residual gap, not fixed here:** something has to call
`InputState::set_toggle_modes` with the live `Options::toggle_sneak`/
`toggle_sprint` values. The natural call site is wherever `Sim` already reads
config into `InputState` each tick — `sim.rs`, off-limits at the time of this
change. See the handoff note below for the exact one-line patch needed.

### #203 — mouse feel

Three options, three different depths of wiring:

- **`mouseWheelSensitivity`** is fully wired: `MenuNav::cycle_mouse_wheel_sensitivity`
  steps the persisted value by `config::MOUSE_WHEEL_SENSITIVITY_STEP` (0.25),
  wrapping additively (not by rounding to a quantized step — see that
  function's doc comment on why a round-trip through an integer step index
  drifts) within vanilla's own slider bounds
  (`config::MIN_MOUSE_WHEEL_SENSITIVITY`/`MAX_MOUSE_WHEEL_SENSITIVITY`,
  `10^(-200/100)..10^(100/100)` from `Options.java:480-482`). The consumer
  (the hotbar-scroll `MouseWheel` handler in `app.rs`) needs a brokered patch
  — see below.
- **`invertMouseX`/`invertMouseY`** are wired as far as this crate can reach:
  `lodestone_controller::apply_look_inverted(yaw, pitch, dx, dy, sensitivity,
  invert_x, invert_y)` negates `dx`/`dy` before `apply_look`'s sensitivity
  curve, matching vanilla's `MouseHandler.turnPlayer`'s
  `player.turn(invertMouseX ? -xo : xo, invertMouseY ? -yo : yo)`
  (negation happens *after* the curve there; the two orders agree
  numerically since the curve has no dependence on the delta's sign — see the
  function's doc comment). `apply_look` itself is untouched, for the same
  reason `movement_intent` is untouched: `sim.rs` calls it directly and this
  crate does not own that file.
- **`sensitivity` (mouse look, not the wheel)** is deliberately still
  inactive. It lives on `config::Config`, parsed from argv every run and never
  written back — see `menu/options.rs`'s `LiveOption` doc. A settings row that
  appeared to persist it would be fabricated.
- **`discreteMouseScroll`/`allowCursorChanges`/`rawMouseInput`** are also
  still inactive: none has a consumer in this shell (no discrete-vs-continuous
  scroll distinction, no OS cursor swap, no raw-input toggle). Wiring the
  label without the behaviour would be the exact fabrication #203 exists to
  fix, one row over — see `menu/options.rs`'s `MOUSE` doc comment.

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

## Handoff: two brokered patches this doc's fixes still need

`lodestone-controller`'s exclusive owner cannot land these — they touch
`app.rs` (brokered through the orchestrator) and `sim.rs` (owned by another
agent at the time of writing). Both are small.

1. **`app.rs`'s `MouseWheel` handler** needs a new arm (or an extension of the
   existing gameplay one) that reads `nav.mouse_wheel_sensitivity()` and
   scales the hotbar-cycle step by it, and — for issue #402, landed alongside
   this — a `Screen::ServerList` arm calling `nav.scroll_server_list`. See
   `docs/server-list.md`'s scrolling section for the second half.
2. **`sim.rs`** needs, wherever it currently calls
   `lodestone_controller::apply_look(yaw, pitch, dx, dy, sensitivity)`, to
   call `apply_look_inverted(yaw, pitch, dx, dy, sensitivity, nav.invert_mouse_x(), nav.invert_mouse_y())`
   instead — and, wherever it feeds keys into `InputState::set`, a call to
   `input_mut(|i| i.set_toggle_modes(nav.toggle_sneak(), nav.toggle_sprint()))`
   run at least once per tick (cheap and idempotent, so it can sit right
   beside the existing `set` calls with no new synchronization).
