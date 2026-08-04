# Which settings actually do something, and what the rest are waiting on

## What it is

A consumption audit of the settings tree: for every option
`crates/lodestone-shell/src/menu/options.rs` puts on screen, whether the value
reaches anything. The tree carries **143 controls**; **40 rows work** and the
rest are present and greyed. This doc records *why* each greyed group is greyed —
i.e. which subsystem it is waiting on — so the next person wiring an option can
tell a five-minute threading job from a new renderer feature.

[`settings-screen.md`](./settings-screen.md) covers the tree's layout and
arithmetic; this doc is only about the producer→consumer chain behind each row.

## How it works

Every option row is one of three states, and the distinction is the whole point
of this document:

| state | meaning | how to recognise it |
|---|---|---|
| **wired** | click → `config::Options` → a consumer → pixels | the cell is `live_cycle`/`live_slider` and carries a `LiveOption` |
| **present, greyed** | vanilla has it; we honour nothing | the cell is `cycle`/`slider`, `live: None`, `is_live() == false` |
| **absent** | not in our census at all | no cell — currently only `LanguageSelectScreen`'s own body and `FontOptionsScreen` |

A wired option is a chain of five links, and **every one of them has been the
missing link at least once in this repo**:

1. a field on `crate::config::Options` (persisted to `options.json`),
2. a `LiveOption` variant naming it,
3. a `live_cycle`/`live_slider` cell on some page, so a player can reach it,
4. an arm in `MenuNav::apply_settings` that mutates and eagerly persists it,
5. a consumer that reads the field and changes what is drawn.

The failure that motivated this doc had **1, 4 and 5 present and 2–3 missing**:
the eight chat fields were persisted, `app.rs` copied all eight into
`hud_frame.chat_options` every frame, and `hud.rs` had magnitude gates proving
the draw honoured them — and every row was drawn greyed, so no player could
reach any of it. Complete at both ends, missing only the middle, and no test in
the crate could see it because each end's tests passed.

### The invariant now enforced

`options.rs`'s `every_live_option_is_reachable_from_some_row` checks the mirror
image (an option honoured by `MenuNav` that sits on no page), and
`nav.rs`'s `app_rs_still_threads_every_chat_option_into_the_hud_frame` checks
link 5 by reading `app.rs`'s source — because `app.rs` is the frame loop and no
unit test can run it, so deleting that copy would otherwise break every chat
option while leaving the suite green.

**As of this doc there are zero persisted-but-unreachable options**: every
non-`keybinds` field of `config::Options` has a row that reaches it.

## The census

### Wired (40 rows, 15 distinct options)

| option | page(s) | type | vanilla default | consumer |
|---|---|---|---|---|
| `guiScale` | Video | cycle | `0` (Auto) | `render::logical_canvas` — every menu and HUD draw |
| `bobView` | Accessibility | toggle | `true` | `Sim::set_view_bobbing` |
| `toggleCrouch` / `toggleSprint` | Controls | cycle (Toggle/Hold) | `false` | `InputState::set_toggle_modes` |
| `invertMouseX` / `invertMouseY` | Mouse | toggle | `false` | `apply_look_inverted` |
| `mouseWheelSensitivity` | Mouse | slider, log `IntRange(-200,100)` → `0.01..=10.0` | `1.0` | hotbar scroll handler |
| `chatColors` | Chat | toggle | `true` | `hud.rs` — strips legacy `§` codes |
| `chatScale` | Chat | slider `UnitDouble` | `1.0` | chat pose scale |
| `chatWidth` | Chat | slider `UnitDouble` | `1.0` | box width, `floor(pct*280+40)` px |
| `chatHeightFocused` | Chat | slider `UnitDouble` | `1.0` | box height when open, `floor(pct*160+20)` px |
| `chatHeightUnfocused` | Chat | slider `UnitDouble` | `70/160` | box height when closed |
| `chatLineSpacing` | Chat, **Accessibility** | slider `UnitDouble` | `0.0` | per-row stride |
| `chatOpacity` | Chat, **Accessibility** | slider `UnitDouble` | `1.0` | text alpha |
| `textBackgroundOpacity` | Chat, **Accessibility** | slider `UnitDouble` | `0.5` | row background alpha |

Three options appear on **two pages each** — that is vanilla's own shape, one
`OptionInstance` placed on both `ChatOptionsScreen` and
`AccessibilityOptionsScreen`, so editing either row moves the other's label too.
This is why `LiveOption` is keyed by the option and not by the row, and why the
live *row* count (40) exceeds the distinct-option count (15).

### Present and greyed, by what they are waiting on

| group | options | blocked on |
|---|---|---|
| **Audio** | all 11 `soundSource.*`, `soundDevice`, `directionalAudio`, `musicFrequency`, `musicToast` | there is no audio subsystem at all — nothing plays sound yet |
| **Window / display** | `fullscreen`, `exclusiveFullscreen`, `fullscreenResolution`, `enableVsync`, `framerateLimit`, `inactivityFpsLimit`, `preferredGraphicsBackend` | runtime window and surface reconfiguration |
| **Renderer quality** | `graphicsPreset`, `gamma`, `mipmapLevels`, `ambientOcclusion`, `biomeBlendRadius`, `particles`, `cloudStatus`, `cloudRange`, `entityShadows`, `entityDistanceScaling`, `improvedTransparency`, `textureFiltering`, `maxAnisotropyBit`, `weatherRadius`, `chunkSectionFadeInTime`, `vignette` | each needs its own renderer knob; several are whole features |
| **Post-process / screen effects** | `screenEffectScale`, `fovEffectScale`, `darknessEffectScale`, `damageTiltStrength`, `glintSpeed`, `glintStrength` | the effects exist in varying degrees; the scale factors are not plumbed |
| **Distances** | `renderDistance`, `simulationDistance`, `fov`, `sensitivity` | **these are not `Options` fields at all** — see the gotcha below |
| **Chat behaviour** | `chatVisibility`, `chatLinks`, `chatLinksPrompt`, `chatDelay`, `autoSuggestions`, `hideMatchedNames`, `onlyShowSecureChat`, `saveChatDrafts`, `reducedDebugInfo` | chat *behaviour* rather than chat *appearance*; the appearance half is now wired |
| **Narrator / high contrast** | `narrator`, `narratorHotkey`, `highContrast`, `highContrastBlockOutline` | no narrator, and high contrast is a resource pack swap |
| **Skin & model parts** | all 7 `modelPart.*`, `mainHand` | needs the parts to reach the entity renderer *and* the serverbound client-settings packet |
| **Menu chrome** | `menuBackgroundBlurriness`, `panoramaSpeed`, `notificationDisplayTime`, `hideSplashTexts`, `darkMojangStudiosBackground`, `hideLightningFlashes`, `backgroundForChatOnly`, `rotateWithMinecart`, `inGameNotification`, `sharePresence` | assorted; mostly small but each needs its own consumer |
| **Input** | `toggleAttack`, `toggleUse`, `autoJump`, `sprintWindow`, `rawMouseInput`, `discreteMouseScroll`, `allowCursorChanges`, `operatorItemsTab` | `lodestone-controller` knobs; the closest to cheap of the greyed groups |

## How to change it

To wire one more option, add the five links above. The chat batch is the worked
example; follow it rather than inventing a second shape.

1. **Field** on `config::Options` (plus its `load_from` read and `save_to`
   write — the serde is hand-rolled, one line each way, so a field added to the
   struct and not to both halves silently fails to persist).
2. **`LiveOption` variant**, doc-commented with the `Options.java` line it comes
   from.
3. **`live_value` arm** transcribing vanilla's stringifier. Return only the
   *value* half — `Cell::label` composes `"caption: value"` via
   `generic_value_label`, so returning `"100%"` reproduces
   `percentValueLabel`'s full output by construction.
4. **Swap the census cell** from `slider`/`cycle` to `live_slider`/`live_cycle`.
   This is the step that un-greys the row, and forgetting it is exactly the
   island this doc exists to describe.
5. **Arm in `MenuNav::apply_settings`** calling a `cycle_*`/`toggle_*` method
   that mutates and then calls `persist_options()` — persistence is eager here
   by rule, because a setting that only saves on exit is the setting a crash
   loses.
6. **Update the census counts** in
   `the_disabled_majority_is_the_point_and_it_is_measured` and
   `the_root_online_button_is_the_one_row_that_changes_with_in_world`. They
   assert exact live-row counts precisely so a new live row has to be declared.

For a `UnitDouble` option, steps beyond that are free: `config::step_unit_double`
handles the click, and `slider_fraction` already returns the live value because
`UnitDouble.toSliderValue` is the identity.

### Gotchas

- **`UnitDouble` needs no range port; anything else does.** `slider_fraction`
  returns `None` for a slider built on an `IntRange` whose range we have not
  ported, so the handle does not draw. That is issue #424, and it is *not* a
  blocker for `UnitDouble` options — all eight chat options were wired without
  touching it.
- **`chatOpacity` is affine.** Its label is
  `percentValueLabel(caption, value * 0.9 + 0.1)`, so a stored `0.0` prints
  `10%`, never `0%` — vanilla's chat text is never fully transparent. The
  plain-percent transcription agrees at `1.0` and nowhere else, which is why the
  gate pins `0.0` and `0.5` too.
- **`percentValueLabel` truncates.** It is `(int)(value * 100.0)`, so `0.999`
  prints `99%`. Predict `floor`, not `round`.
- **`guiScale` is a cycle button, not a slider**, even though it is an int
  range — `ClampingLazyMaxIntRange.createCycleButton()` returns `true`.
- **`renderDistance`, `simulationDistance`, `fov` and `sensitivity` are not
  persisted options here.** `renderDistance` and `sensitivity` live on
  `config::Config`, which is parsed from argv every run and **never written
  back**. Their consumers already exist (`sim.rs`), so a row that appeared to
  set them would be fabricated persistence: the value would revert on restart.
  Wiring them is a *migration* — decide whether argv or `options.json` wins —
  not a threading job, and the consumer edit lands in `sim.rs`, a brokered file.
- **A settings test must not write the real `options.json`.** Use
  `MenuNav::with_paths` with a temp path, as the existing tests do. This is the
  same class as the accounts-screen test that spawned `open` and launched a
  Microsoft OAuth URL in the owner's browser on every run.
- **Sliders are clicked, not dragged.** `SettingsOutcome::Cycle` is the only
  mutation channel, so one click steps the value and wraps. Adding real drag
  handling would change this shape for every slider at once.

## Configuration

- `options.json` at `config::options_path()` — hand-rolled serde in
  `config.rs`'s `load_from`/`save_to`. Missing or corrupt reads as the default
  rather than an error, and out-of-range values are clamped on use rather than
  trusted, because the file is hand-editable.
- `config::UNIT_DOUBLE_STEP` (`0.1`) — how far one click moves a `UnitDouble`
  option. Not vanilla's granularity (vanilla drags continuously); chosen to match
  the 10-percentage-point granularity `percentValueLabel` displays.
- `config::MOUSE_WHEEL_SENSITIVITY_STEP` (`0.25`) — the same idea for the one
  log-mapped slider.

## Dependencies

- `crates/lodestone-shell/src/menu/options.rs` — the census, the widget kinds,
  the label strings and the layout arithmetic.
- `crates/lodestone-shell/src/menu/nav.rs` — owns `Options` and the file;
  `apply_settings` is the single place a row's activation becomes a mutation.
- `crates/lodestone-shell/src/config.rs` — the persisted struct, its serde, and
  the stepping helpers.
- `crates/lodestone-shell/src/app.rs` — copies the chat options into
  `hud_frame.chat_options` each frame. **Brokered**: not edited by option work
  directly, and guarded by a source-scan test instead.
- `crates/lodestone-shell/src/hud.rs` — `ChatDisplayOptions` and the draw that
  honours it, with its own per-field magnitude gates.
- `.cache/mc/26.2/client-src/net/minecraft/client/Options.java` and the
  `gui/screens/options/*` screens — the authority for every type, range,
  default and label string above.
