# Which settings actually do something, and what the rest are waiting on

## What it is

A consumption audit of the settings tree: for every option
`crates/lodestone-shell/src/menu/options.rs` puts on screen, whether the value
reaches anything. The tree carries **143 controls**; most rows are present and
greyed. This doc records *why* each greyed group is greyed — i.e. which subsystem
it is waiting on — so the next person wiring an option can tell a five-minute
threading job from a new renderer feature.

**Re-measured end to end on 2026-08-08, and the greyed groups are not one tier.**
The single most important correction is that **"blocked on a missing subsystem" is
now the minority case**: for most greyed rows the consumer already exists and is
already correct, and what is missing is one push from `config::Options` to it. See
[Three kinds of greyed row](#three-kinds-of-greyed-row-measured-2026-08-08), which
is the table to read before believing any "blocked on" cell further down.

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

### Wired (29 option rows, 25 distinct options)

Counts, because three different ones get conflated here: **51 live *cells*** outside
a world (50 inside — the root's Online button is the one that changes), of which
**29 are option rows**, 9 are Done buttons and 13 are working nav buttons; those 29
rows carry **25 distinct options**, because four are placed on two pages each. All
three numbers are asserted by `the_disabled_majority_is_the_point_and_it_is_measured`
and `the_root_online_button_is_the_one_row_that_changes_with_in_world`, so they
cannot drift here without a build failure — quote them from a test run, not from
this paragraph.

| option | page(s) | type | vanilla default | consumer |
|---|---|---|---|---|
| `guiScale` | Video | cycle | `0` (Auto) | `render::logical_canvas` — every menu and HUD draw |
| `bobView` | Accessibility | toggle | `true` | `Sim::set_view_bobbing` |
| `toggleCrouch` / `toggleSprint` | Controls | cycle (Toggle/Hold) | `false` | `InputState::set_toggle_modes` |
| `invertMouseX` / `invertMouseY` | Mouse | toggle | `false` | `apply_look_inverted` |
| `mouseWheelSensitivity` | Mouse | slider, log `IntRange(-200,100)` → `0.01..=10.0` | `1.0` | `app::scale_scroll` — **both** wheel consumers, hotbar and every menu list |
| `discreteMouseScroll` | Mouse | toggle | `false` | `app::scale_scroll` — `signum` **before** the sensitivity multiply (#444) |
| `chatColors` | Chat | toggle | `true` | `hud.rs` — strips legacy `§` codes |
| `chatScale` | Chat | slider `UnitDouble` | `1.0` | chat pose scale |
| `chatWidth` | Chat | slider `UnitDouble` | `1.0` | box width, `floor(pct*280+40)` px |
| `chatHeightFocused` | Chat | slider `UnitDouble` | `1.0` | box height when open, `floor(pct*160+20)` px |
| `chatHeightUnfocused` | Chat | slider `UnitDouble` | `70/160` | box height when closed |
| `chatLineSpacing` | Chat, **Accessibility** | slider `UnitDouble` | `0.0` | per-row stride |
| `chatOpacity` | Chat, **Accessibility** | slider `UnitDouble` | `1.0` | text alpha |
| `textBackgroundOpacity` | Chat, **Accessibility** | slider `UnitDouble` | `0.5` | row background alpha |
| `sensitivity` | Mouse | slider `UnitDouble` | `0.5` (label reads **100%** — `2.0 * value`) | `sim/step.rs`'s `apply_mouse`, **via `Config::resolve_persisted`** (#443) |
| `renderDistance` | Video | slider `IntRange(2, 32)` | `12` in vanilla, **`8` here** — see `config::DEFAULT_RENDER_DISTANCE` | `sim/build.rs` world radius + `sim/camera.rs` fog, via `resolve_persisted` (#443) |
| `damageTiltStrength` | Accessibility | slider `UnitDouble`, label `percentValueOrOffLabel` | `1.0` | `camera_rig::BobFrame::hurt_roll_degrees`, via `app/redraw.rs` → `RenderState::set_damage_tilt_strength` |
| `panoramaSpeed` | Accessibility | slider `UnitDouble`, label `percentValueLabel` | `1.0` | `menu::panorama::PanoramaRenderer::set_speed`, via `MenuFrame::panorama_speed` |

**Four** options appear on **two pages each** — that is vanilla's own shape, one
`OptionInstance` placed on two screens, so editing either row moves the other's label
too. `chatOpacity`, `chatLineSpacing` and `textBackgroundOpacity` are on
`ChatOptionsScreen` *and* `AccessibilityOptionsScreen`; `showSubtitles` is on
`SoundOptionsScreen` *and* `AccessibilityOptionsScreen`. This is why `LiveOption` is
keyed by the option and not by the row, and why the row count exceeds the
distinct-option count by exactly four.

### Three kinds of greyed row (measured 2026-08-08)

The old table below sorts greyed rows by *subsystem*. That grouping hid the thing
that actually decides how much work each row is, so this is the axis to read
first — **which of the five links is missing**, measured by grepping for each
consumer rather than inferred from the group it sits in:

| kind | what is already true | what is missing | examples |
|---|---|---|---|
| **A — consumer live, no push** | the subsystem exists, is correct, and is called every frame with a **hardcoded constant** | links 1–4, plus swapping the constant for the field | all 11 `soundSource.*`, `fov`, `glintSpeed`, `glintStrength`, `cloudStatus` |
| **B — consumer is itself an island** | the code exists in `lodestone-render`, unit-tested, with **zero shell callers** | a consumer that runs, *then* the option | `screenEffectScale`, `fovEffectScale`, `darknessEffectScale` (they scale `confusion_overlay_triangles` / `portal_overlay_alpha`, which nothing draws) |
| **C — no subsystem** | nothing | the feature | `gamma`, `narrator`, `highContrast`, `entityShadows`, `entityDistanceScaling`, `biomeBlendRadius`, `simulationDistance`, `mipmapLevels`, `rawMouseInput`, `allowCursorChanges` |

Kind A is the important row and it is where the previous version of this doc was
most wrong. Three specific corrections, each measured:

- **There is an audio subsystem, and it has a per-category volume seam.** The old
  table said *"there is no audio subsystem at all — nothing plays sound yet"*. That
  was true when written and is now false in every part: `lodestone-audio` and
  `lodestone-sound` are real crates, `crate::audio::ShellAudio` wraps a device-backed
  `AudioEngine`, and music, ambience and sound subtitles all play. More to the point,
  `lodestone_audio::CategoryVolumes::set_user(category, volume)` is **exactly** the
  eleven sliders' consumer, reachable through
  `AudioEngine::with_mixer` → `Mixer::volumes_mut`, and `SoundCategory::ALL` is
  vanilla's own eleven in order. The eleven sound sliders are kind **A**, not
  "blocked on a subsystem". This is the staleness class `CLAUDE.md` describes: the
  claim was correct and evidenced, and nothing about it looked out of date.
- **`fov` has a consumer.** `Camera::fov_y_degrees` feeds the projection matrix and
  `camera_rig::build_camera` sets it to the module constant `FOV_Y_DEGREES` (70,
  vanilla's default) on every frame. The old table's *"no consumer at all"* is only
  true of the literal string `options.fov`; the effect is fully implemented and
  pinned to one value. Kind **A**.
- **`glintSpeed`/`glintStrength` and `cloudStatus` are the same shape.**
  `lodestone_render::glint::glint_clock(millis, speed)` and
  `GlintUniform::new(…, speed, strength)` take both as parameters, and the two shell
  call sites (`gpu/glint.rs`, `hud/item_icon.rs`) pass `DEFAULT_SPEED`/
  `DEFAULT_STRENGTH`. `SkyFrame::with_cloud_status` really does switch between the
  flat quad and extruded per-cell geometry, and has **zero** production callers, so
  the shell always draws `CloudStatus::default()` (Fancy).

Two traps found while measuring, both of the "grep hit is not a consumer" kind:

- **`main_hand` in `lodestone-shell` is not vanilla's `mainHand` option.** It is the
  *held item* (`RenderState::set_main_hand_source`, a closure yielding the stack
  first person draws). Vanilla's left/right handedness option has no consumer here at
  all — kind C. Grepping the option's name finds 33 files and none of them are it.
- **`mipmapLevels` has no consumer either**, despite `lodestone-render`'s
  `texture::mip_level_count`: that is `log2(max(w,h)) + 1` derived from the atlas
  size, not a user-selectable cap.

### Present and greyed, by what they are waiting on

**The "blocked on" column below predates the kind A/B/C measurement above and is
superseded by it where the two disagree.** It is kept because the per-group
groupings are still the right map of *which* subsystem each row belongs to.

| group | options | blocked on |
|---|---|---|
| **Audio** | all 11 `soundSource.*`, `soundDevice`, `directionalAudio`, `musicFrequency`, `musicToast` | **superseded — see kind A above.** The eleven volume sliders need only a push into `CategoryVolumes::set_user`; `soundDevice` (device enumeration) and `musicFrequency`/`musicToast` are separate and smaller |
| **Window / display** | `fullscreen`, `exclusiveFullscreen`, `fullscreenResolution`, `enableVsync`, `framerateLimit`, `inactivityFpsLimit`, `preferredGraphicsBackend` | runtime window and surface reconfiguration |
| **Renderer quality** | `graphicsPreset`, `gamma`, `mipmapLevels`, `ambientOcclusion`, `biomeBlendRadius`, `particles`, `cloudStatus`, `cloudRange`, `entityShadows`, `entityDistanceScaling`, `improvedTransparency`, `textureFiltering`, `maxAnisotropyBit`, `weatherRadius`, `chunkSectionFadeInTime`, `vignette` | each needs its own renderer knob; several are whole features |
| **Post-process / screen effects** | `screenEffectScale`, `fovEffectScale`, `darknessEffectScale`, ~~`damageTiltStrength`~~, `glintSpeed`, `glintStrength` | `damageTiltStrength` is **live now** — its consumer had been honoured all along; the three `*EffectScale` rows are kind **B** (the effect they scale is itself an island); the two glint rows are kind **A** |
| **Distances** | ~~`renderDistance`~~, `simulationDistance`, `fov`, ~~`sensitivity`~~ | `renderDistance` and `sensitivity` are live; `fov` is kind **A** (`camera_rig::build_camera` pins `Camera::fov_y_degrees` to the constant `FOV_Y_DEGREES`); `simulationDistance` is kind **C** |
| **Chat behaviour** | `chatVisibility`, `chatLinks`, `chatLinksPrompt`, `chatDelay`, `autoSuggestions`, `hideMatchedNames`, `onlyShowSecureChat`, `saveChatDrafts`, `reducedDebugInfo` | chat *behaviour* rather than chat *appearance*; the appearance half is now wired |
| **Narrator / high contrast** | `narrator`, `narratorHotkey`, `highContrast`, `highContrastBlockOutline` | no narrator, and high contrast is a resource pack swap |
| **Skin & model parts** | all 7 `modelPart.*`, `mainHand` | needs the parts to reach the entity renderer *and* the serverbound client-settings packet |
| **Menu chrome** | `menuBackgroundBlurriness`, ~~`panoramaSpeed`~~, `notificationDisplayTime`, `hideSplashTexts`, `darkMojangStudiosBackground`, `hideLightningFlashes`, `backgroundForChatOnly`, `rotateWithMinecart`, `inGameNotification`, `sharePresence` | `panoramaSpeed` is **live now** — `PanoramaRenderer::set_speed` was an island with zero callers; the rest each need their own consumer, and `hideSplashTexts`/`darkMojangStudiosBackground` have no splash text and no Mojang screen to hide |
| **Input** | `toggleAttack`, `toggleUse`, `autoJump`, `sprintWindow`, `rawMouseInput`, ~~`discreteMouseScroll`~~, `allowCursorChanges`, `operatorItemsTab` | see the breakdown below — this group is **not** one tier |

### The two rows wired on 2026-08-08, and why they were the two

Both were **kind A with the push entirely inside `lodestone-shell/src/menu/**`**,
which is why they could land without a brokered file. They are also each other's
mirror image, which is worth keeping as the pair of shapes to look for:

- **`damageTiltStrength`** had links **1 and 5** and was missing **2–4**. The field
  was persisted with real serde, and `app/redraw.rs` already read
  `MenuNav::damage_tilt_strength` into `RenderState::set_damage_tilt_strength` every
  frame — so the whole camera-tilt consumer was live and honoured, and the *only* way
  to reach it was to hand-edit `options.json`, because the row fell through to
  `UNIT_DOUBLE_DEFAULTS`' frozen `1.0`. Exactly the chat batch's failure, one row
  wide.
- **`panoramaSpeed`** had **none of the five** and its *consumer* was the island:
  `PanoramaRenderer::set_speed` existed, was unit-tested, and had **zero callers**,
  so the title screen span at `DEFAULT_SPIN_SPEED` whatever anyone set. The value now
  rides `MenuFrame::panorama_speed`, stamped by `frame_for` beside `gui_scale`.

Two gotchas the pair produced:

- **`MenuFrame::panorama_speed` is an `Option<f32>`, and that is load-bearing.**
  `MenuFrame` derives `Default`, and `0.0` is a *legitimate* value here — a
  deliberately stationary panorama is the whole point of the option. A bare `f32`
  would make any hand-built frame freeze the sky, which is indistinguishable from
  the option working. `None` means "nothing stamped this" and the renderer keeps its
  own speed; the same reason `MenuFrame::cursor` is an `Option` rather than `(0, 0)`.
- **The two labels are different stringifiers and differ only at zero.**
  `damageTiltStrength` is vanilla's `percentValueOrOffLabel` (`0.0` → `OFF`);
  `panoramaSpeed` is the plain `percentValueLabel` (`0.0` → `0%`). Transcribing one
  for the other agrees at every value except the one a player is most likely to pick,
  so the gates pin zero on both.
- **`step_chat_option` is now `step_unit_double_option`.** The rename is not tidying:
  it carries these two Accessibility rows now, and a name claiming otherwise is how
  the next reader concludes there is no generic stepper and writes a second one.

### The brokered patches, for the kind A rows that need a file outside `menu/**`

Each of these is a *push*, not a feature: the consumer exists, runs every frame, and
is currently handed a hardcoded constant. Listed with the exact seam so whoever owns
the file can do it without re-deriving anything.

| option(s) | file | patch |
|---|---|---|
| 11 × `soundSource.*` | `sim/audio.rs` (+ `config.rs` fields) | add `Sim::set_sound_volumes(&Options)` calling `audio_mut(\|a\| …)` → `AudioEngine::with_mixer(\|m\| m.volumes_mut().set_user(cat, v))` for each `SoundCategory::ALL`; call it from `app/redraw.rs` beside `set_view_bobbing`. The `Sim::audio_mut` accessor is private today, which is the only reason this cannot be done from the menu side |
| `fov` | `camera_rig.rs` + `sim/camera.rs` | give `build_camera` an `fov_y_degrees` parameter (or a `Sim::set_fov` seam mirroring `set_view_bobbing`) instead of the module constant `FOV_Y_DEGREES`. Note vanilla's `fov` is an `IntRange(30, 110)`, so it also needs an `INT_RANGE_SLIDERS` entry — not a `UnitDouble` |
| `glintSpeed`, `glintStrength` | `gpu/glint.rs`, `hud/item_icon.rs` | replace `DEFAULT_SPEED`/`DEFAULT_STRENGTH` at the two call sites with the option values. Both are already parameters of `glint_clock` and `GlintUniform::new` |
| `cloudStatus` | wherever `SkyFrame` is built in `gpu/**` | call `SkyFrame::with_cloud_status`, which has zero production callers. Vanilla's option has three states and our `CloudStatus` has two (no `Off`), so the third needs a skip in the sky pass |

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
- ~~**`renderDistance`, `simulationDistance`, `fov` and `sensitivity` are not
  persisted options here.**~~ **`renderDistance` and `sensitivity` now are
  (issue #443).** They were argv-only `config::Config` fields that were never
  written back, so a row for them would have been fabricated persistence. Both
  are now `config::Options` fields with a `LiveOption`, a live row, an
  `apply_settings` arm and a real consumer.

  **The migration resolves precedence in one place rather than at each
  consumer**, and that is the part worth copying. `Config::resolve_persisted`
  folds `options.json` into the argv-parsed `Config` once, at launch, so the
  seven existing readers (`sim/step.rs`'s `apply_mouse`, `sim/build.rs`'s world
  radius, `sim/camera.rs`'s fog, four `app/*` call sites) read the resolved value
  **unchanged** — the migration adds no consumer and touches no brokered file.
  Teaching each site to consult both structs would have been seven chances to
  miss one.

  An explicit flag still wins for that run, which needs
  `Config::sensitivity_given`/`render_distance_given` — `address_given`'s exact
  shape, because `--render-distance 8` is byte-identical to passing nothing and
  the *value* therefore cannot answer "was it given". A resolver that compared
  against `Config::default()` would silently discard the flag;
  `passing_the_default_explicitly_is_still_an_explicit_flag` executes that wrong
  hypothesis and shows it answering false.

  **Known limitation, and it is a real one:** the fold happens at launch, so a
  change made in the settings screen applies on the **next** launch. For
  `renderDistance` that is close to vanilla, which also defers
  (`applyValueImmediately = false`, a 600 ms debounce, because each change
  reloads chunks). For `sensitivity` vanilla applies immediately, so this is a
  departure. Closing it means pushing the value into `Sim` every frame the way
  `app/redraw.rs` already does for `set_mouse_invert` — a brokered file, so it
  needs the orchestrator.

  `simulationDistance` and `fov` stay inactive: neither has any consumer in this
  shell, so wiring them would be the fabrication this bullet used to warn about.
  `the_disabled_majority_is_the_point_and_it_is_measured` now uses
  `simulationDistance` as its inactive control, read off the real page — the old
  control constructed a synthetic `slider("renderDistance", …)` cell, which had
  `live: None` by construction and so was asserting a property of the
  constructor rather than of the tree. It would have kept passing after #443
  made the real row live.
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

## The Input group is three tiers, not one (issue #444)

The table above used to call this group "the closest to cheap of the greyed
groups". That is true of the *plumbing* and false of the work, and the difference
is which subsystem each row needs. Measured while wiring #444:

| option | what it needs | owner |
|---|---|---|
| `discreteMouseScroll` | **nothing new** — `app`'s wheel boundary already existed | **live since #444** |
| `toggleAttack`, `toggleUse` | `InputState::set_toggle_modes` widened to four flags, `Sim::set_toggle_modes` to match, then one line in `app/redraw.rs` | `lodestone-controller` + `lodestone-shell/src/sim/**` |
| `autoJump` | **live since #201** — the consumer existed all along (`lodestone_physics::update_auto_jump`); what was missing was a seam to its `auto_jump_enabled` gate, now `lodestone_ecs::player::AutoJump`. A *second*, simplified probe in `sim/step.rs` was gated on the option while the real detector was not, so the option could not turn auto-jump off; that probe is deleted | `lodestone-ecs` + `sim/**` |
| `sprintWindow` | a timing consumer that does not exist yet | `sim/**` |
| `rawMouseInput`, `allowCursorChanges` | **no subsystem at all** — this shell never changes the OS cursor and has no raw-input mode to toggle | not a wiring job |

The load-bearing correction: **#444's premise that all six rows pass through one
line (`app/redraw.rs`'s `set_toggle_modes` call) is false.** Only `toggleAttack`
and `toggleUse` do. `discreteMouseScroll` goes through the wheel handler,
`sprintWindow` needs a sim consumer, `autoJump` needed a *seam* rather than a
consumer (see its row), and the last two have no consumer to reach. So "unblock that one line and wire all six" is not a plan that exists —
each tier is separate work, and the bottom tier should be closed as won't-do until
a subsystem exists rather than given a row.
