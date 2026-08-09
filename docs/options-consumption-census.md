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

### Wired (44 option rows, 40 distinct options)

Counts, because three different ones get conflated here: **66 live *cells*** outside
a world (65 inside — the root's Online button is the one that changes), of which
**44 are option rows**, 9 are Done buttons and 13 are working nav buttons; those 44
rows carry **40 distinct options**, because four are placed on two pages each. All
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
| 11 × `soundSource.*` | Sound | slider `UnitDouble`, label `percentValueOrOffLabel` | `1.0` each | `lodestone_audio::CategoryVolumes::set_user`, via `Sim::set_sound_volumes` |
| `fov` | **Root** | slider `IntRange(30, 110)`, label "Normal"/"Quake Pro"/int | `70` | `camera_rig::build_camera` → the projection matrix, via `Sim::set_fov_y_degrees` |
| `glintSpeed` / `glintStrength` | Accessibility | slider `UnitDouble`, label `percentValueOrOffLabel` | `0.5` / `0.75` | **three** sites — `RenderState::set_glint_options` (world + hand) and `IconRenderer::set_glint_options` on *both* `HudRenderer` and `ContainerRenderer` (GUI icons) |
| `cloudStatus` | Video | cycle, 3 states, label **is** the value | `FANCY` | `SkyFrame::with_cloud_status`, via `RenderState::set_cloud_status` |

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
| **A — consumer live, no push** | the subsystem exists, is correct, and is called every frame with a **hardcoded constant** | links 1–4, plus swapping the constant for the field | ~~all 11 `soundSource.*`, `fov`, `glintSpeed`, `glintStrength`, `cloudStatus`~~ — **all five links landed for all fifteen on 2026-08-09. Kind A is now empty.** See [Kind A is closed](#kind-a-is-closed-2026-08-09) |
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
  the shell always draws `CloudStatus::default()` (Fancy). *(Both landed on
  2026-08-09 — `with_cloud_status` has a production caller now, and there turned
  out to be a **third** glint call site. See
  [Kind A: links 1 and 5 are done](#kind-a-links-1-and-5-are-done-2026-08-09).)*

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
| **Audio** | ~~all 11 `soundSource.*`~~, `soundDevice`, `directionalAudio`, `musicFrequency`, `musicToast` | **superseded — see kind A above.** The eleven volume sliders are **fully live since 2026-08-09**, `CategoryVolumes::set_user` through to the row. `soundDevice` (device enumeration) and `musicFrequency`/`musicToast` are separate and smaller |
| **Window / display** | `fullscreen`, `exclusiveFullscreen`, `fullscreenResolution`, `enableVsync`, `framerateLimit`, `inactivityFpsLimit`, `preferredGraphicsBackend` | runtime window and surface reconfiguration |
| **Renderer quality** | `graphicsPreset`, `gamma`, `mipmapLevels`, `ambientOcclusion`, `biomeBlendRadius`, `particles`, ~~`cloudStatus`~~, `cloudRange`, `entityShadows`, `entityDistanceScaling`, `improvedTransparency`, `textureFiltering`, `maxAnisotropyBit`, `weatherRadius`, `chunkSectionFadeInTime`, `vignette` | `cloudStatus` is **fully live since 2026-08-09**, all three states; the rest each need their own renderer knob, and several are whole features |
| **Post-process / screen effects** | `screenEffectScale`, `fovEffectScale`, `darknessEffectScale`, ~~`damageTiltStrength`~~, ~~`glintSpeed`~~, ~~`glintStrength`~~ | `damageTiltStrength` is **live** — its consumer had been honoured all along; the two glint rows are **fully live since 2026-08-09** at all three glint sites; the three `*EffectScale` rows are kind **B** (the effect they scale is itself an island) and are the group's whole remainder |
| **Distances** | ~~`renderDistance`~~, `simulationDistance`, ~~`fov`~~, ~~`sensitivity`~~ | `renderDistance`, `sensitivity` and `fov` are all live — `fov` **fully since 2026-08-09** (`camera_rig::build_camera` takes the degrees instead of pinning `FOV_Y_DEGREES`, and the row is on the **root** page); `simulationDistance` is kind **C** and the group's only remainder |
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

**All four rows below are landed.** They are kept because the *seam* each one
names is still the right map of where the value travels, and because the "what
was missing" column is now the record of what a kind A row costs in practice.

| option(s) | file | patch |
|---|---|---|
| 11 × `soundSource.*` | `sim/audio.rs` (+ `config.rs` fields) | **done** — `Sim::set_sound_volumes(&Options)`, pushed from `app/redraw.rs` beside `set_view_bobbing`. `Sim::audio_mut` never needed widening: `sim::audio` is a *descendant module* of `sim`, so it already saw the private accessor, and that module's own doc says so |
| `fov` | `camera_rig.rs` + `sim/camera.rs` | **done** — `build_camera` takes `fov_y_degrees`, `Sim::set_fov_y_degrees` mirrors `set_view_bobbing`, pushed per frame. `INT_RANGE_SLIDERS` **already had** the `("fov", 30..=110, 70)` row, so nothing was needed there |
| `glintSpeed`, `glintStrength` | `gpu/glint.rs`, `hud/item_icon.rs`, **`hud.rs` + `container/renderer.rs`** | **done at all three sites** — `RenderState::set_glint_options` for the world and hand, then `HudRenderer::set_glint_options` and `ContainerRenderer::set_glint_options` for the 2-D GUI icon pass, all three pushed from `app/redraw.rs`. This row originally said two sites; the third is a separate pipeline with its own uniform, and its two owners each hold their own `IconRenderer`, so it took **two** forwards rather than one |
| `cloudStatus` | wherever `SkyFrame` is built in `gpu/**` | **done** — `gpu/frame.rs`'s `SkyFrame` builder chain now ends in `.with_cloud_status(self.cloud_status)`, fed by `RenderState::set_cloud_status`. `CloudStatus` gained an `Off` variant rather than taking a skip in the shell; see below for why |

### Kind A is closed (2026-08-09)

Fifteen options — the eleven `soundSource.*` sliders, `fov`, `glintSpeed`,
`glintStrength` and `cloudStatus` — now have **all five links**. Links 1 and 5
landed first (a persisted `config::Options` field with hand-rolled serde both ways,
and a live consumer pushed once per presented frame from `app/redraw.rs`); links
2–4 followed in a second pass, because the agent that landed the first half was
scoped out of `menu/**`.

**Every one of the fifteen rows now draws live**, and the last remaining link-5
gap in the batch — the GUI icon glint, below — is closed too, so all three glint
sites carry the player's values. Kind A is empty; what is left greyed is kinds
**B** and **C**, and both need a subsystem rather than a wire.

The `menu/**` half, for the next reader:

| link | where | note |
|---|---|---|
| 2 | `LiveOption` | five variants, not fifteen: **`SoundVolume(u8)` carries an index** rather than eleven variants, because the eleven differ in exactly one number and eleven variants would be eleven chances for a row's accessor and its array slot to disagree |
| 3a | `live_value` | four stringifiers — `percentValueOrOffLabel` for the volumes and both glints, the FOV switch, `CloudStatus.caption()` |
| 3b | the cell | `SOUND`'s eleven, `VIDEO`'s Clouds, `ACCESSIBILITY`'s glint pair, and the root's FOV — which lives in `controls`/`all_controls` rather than an `Entry` table, so **both** had to change |
| 4 | `MenuNav::apply_settings` | `step_sound_volume`, `step_fov`, `step_unit_double_option` × 2, `cycle_cloud_status`; plus `set_live_slider`'s `IntRange` write for FOV, which the *drag* path needs |

Four things this half measured that the first half did not know, and the first two
are the load-bearing ones:

- **`cloudStatus` is the one live option on the tree whose label is the value
  alone.** Its stringifier is `(caption, value) -> value.caption()`, which
  **discards the caption it is handed** — so vanilla's button reads "Fancy", never
  "Clouds: Fancy". Every other live option goes through `genericValueLabel`,
  `percentValueLabel` or `pixelValueLabel`, all three of which compose, and
  `Cell::label` composed unconditionally. That needed a fork,
  `LiveOption::value_is_the_whole_label`. Flipping the fork to the wrong variant
  was executed as a control: `the_kind_a_labels_are_vanillas_own_strings` fails
  with `left: "Clouds: OFF"`.
- **`fov`'s stringifier special-cases the default**, which is the opposite of the
  usual trap. It is `case 70 -> options.fov.min; case 110 -> options.fov.max;
  default -> the integer`, and `en_us.json` gives `"Normal"` and `"Quake Pro"` (no
  exclamation mark). 70 is *also* vanilla's shipped default, so a fresh install
  reads "FOV: Normal" and a transcription that printed the integer would disagree
  with vanilla on the one value every new player sees. Note also that **70 is an
  input where two rival slider-fraction hypotheses coincide**: the bucket map gives
  `40.5 / 81 = 0.5` and the naive endpoint span gives `40 / 80 = 0.5` too, so the
  drag gate uses 90 (`60.5 / 81`, against the span's `0.75`).
- **The eleven volume sliders' real failure mode is a transposed pair, not a
  dropped one**, and it is invisible to a uniform test value: two rows wired to
  each other's slot move the wrong bus while every label reads correctly. Both new
  gates therefore use **eleven distinct** values (`(i + 1) / 16`, dyadic so the
  `f32` comparison is exact and `(int)(v * 100)` is predictable — 6%, 12%, 18%, 25%,
  31%, 37%, 43%, 50%, 56%, 62%, 68%), and
  `sound_rows_index_the_category_they_name` checks each row's index against the
  suffix of its own accessor, which is the one property no compiler sees.
- **The Clouds cycle order is the enum's, not a chosen one.** `CloudStatus` is
  `OFF, FAST, FANCY` and FANCY is the default, so the **first** click wraps to OFF.
  A hand-picked order would leave the default somewhere other than where vanilla's
  third click puts it.

The persisted keys, so a hand edit still works and so nothing has to be
re-derived:

| option | `options.json` key | type | default |
|---|---|---|---|
| 11 × `soundSource.*` | `sound_volume_master` … `sound_volume_ui` | `UnitDouble` | `1.0` each |
| `fov` | `fov` | `IntRange(30, 110)` | `70` |
| `glintSpeed` | `glint_speed` | `UnitDouble` | `0.5` |
| `glintStrength` | `glint_strength` | `UnitDouble` | `0.75` |
| `cloudStatus` | `cloud_status` | `"off"` / `"fast"` / `"fancy"` | `"fancy"` |

The eleven sound keys use vanilla's **singular** `SoundSource.getName()` strings
(`record`, `block`, `player`), not the plural enum variant names, and
`config::SOUND_CATEGORY_NAMES` is the one list all three consumers index.

Six things this half measured that the table above got wrong or did not know:

- **`Sim::audio_mut` being private was never the blocker.** `sim::audio` is a
  descendant module of `sim`, so it already had access — `sim/audio.rs`'s own
  module doc states this, and the census's claim was inherited from a reading of
  the signature rather than from a compile.
- **There are three glint sites, not two.** The world pass and the hand pass both
  go through `gpu::glint::glint_uniform` and are done. The **2-D GUI icon** glint
  is a separate pipeline with its own uniform (`hud::item_icon::GuiGlint`), and
  its owner `IconRenderer` is held by `HudRenderer` and the container renderer —
  so `IconRenderer::set_glint_options` needed a one-line forward in each of
  `hud.rs` and `container/renderer.rs`, plus one call in `app/redraw.rs`.
  **Landed 2026-08-09** — `HudRenderer::set_glint_options` and
  `ContainerRenderer::set_glint_options` are both called from `redraw.rs` beside
  `RenderState::set_glint_options`, and
  `redraw_rs_still_pushes_the_glint_options_to_all_three_sites` is the source-scan
  guard, because `redraw.rs` is the frame loop and no unit test in the crate can
  run it. Two owners rather than one is the part worth remembering: `HudRenderer`
  and `ContainerRenderer` each hold their **own** `IconRenderer`, so pushing to one
  leaves the other at vanilla's default. All three sites key off the same wall
  clock, so a partial push is visible as an out-of-phase shimmer as well as a wrong
  rate.
- **`IconRenderer` could no longer derive `Default`.** A derived one starts both
  glint fields at `0.0` — a stationary, fully transparent shimmer — which is the
  glint silently switched off on every screen. The impl is hand-written now, with
  that reason on it. Any struct that gains an option field whose meaningful
  default is not zero has this hazard.
- **`Off` is a `CloudStatus` variant, not a skip in the shell.** The skip could
  not live in the caller: `SkyRenderer::render` is one call that clears the target
  and draws disc, sunrise, stars, celestial bodies **and** clouds, and it is also
  what paints the below-horizon void — a shell "skipping clouds" would have to
  skip the whole sky. Since the branch has to be inside `render` regardless, the
  third state is stated in the type. It is expressed as two **non-complementary**
  predicates, `CloudStatus::draws_flat_quad` and `draws_extruded_cells`, both
  false for `Off`; a call site that asks "is it not fancy" would draw FAST
  geometry for `Off`, and a gate that only checks the two differ passes with that
  bug in place. Same shape and same reason as `CameraType`'s pair.
- **`cloud_status` is persisted by name, and the ordinal would have been a trap.**
  `Off` is first in the enum, so its ordinal is `0` — which is also what a missing
  or malformed key deserialises to under an ordinal scheme, making "clouds off"
  and "no setting at all" indistinguishable. By name, a malformed key is FANCY,
  vanilla's default. Vanilla's legacy boolean spellings (`"true"`/`"false"`) are
  accepted too, from `CloudStatus.byName`.
- **`glintSpeed`/`glintStrength` cannot be gated at their defaults, and neither
  can `fov`.** `lodestone_render::glint::DEFAULT_SPEED`/`DEFAULT_STRENGTH` *are*
  vanilla's shipped option values (`0.5`, `0.75`), and `camera_rig::FOV_Y_DEGREES`
  *is* vanilla's `70` — so at the default the correct and frozen-default
  hypotheses are byte-identical and a gate there measures only that the code
  runs. The landed gates pick `0.25` for strength (a third of `0.75`, so a stale
  uniform is off by half the alpha range), a zero speed (the one clock-independent
  property of a call site that reads the wall clock), and FOV `90`/`30` (exact
  cotangents: `cot(45°) == 1`, `cot(15°) == 2 + √3`, against the frozen
  `cot(35°) ≈ 1.42815`). Each carries a control that shows the frozen hypothesis
  really is what it claims.

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
   `percentValueLabel`'s full output by construction. **Check whether your
   stringifier actually takes its `caption` argument**: `cloudStatus`' is
   `(caption, value) -> value.caption()` and throws it away, which needs
   `LiveOption::value_is_the_whole_label` rather than a value-half return.
4. **Swap the census cell** from `slider`/`cycle` to `live_slider`/`live_cycle`.
   This is the step that un-greys the row, and forgetting it is exactly the
   island this doc exists to describe. **The root page is not an `Entry` table** —
   `fov` lives in `controls`/`all_controls` directly, and both must change.
5. **Arm in `MenuNav::apply_settings`** calling a `cycle_*`/`toggle_*`/`step_*`
   method that mutates and then calls `persist_options()` — persistence is eager
   here by rule, because a setting that only saves on exit is the setting a crash
   loses. **For an `IntRange` option this is two writes, not one**: the click arm
   here *and* an arm in `set_live_slider`'s `int_range` match, which is the drag
   path. Miss the second and the row still works from the keyboard while a mouse
   drag does nothing.
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

  **`fov` no longer belongs in this bullet** — it went fully live on 2026-08-09,
  and unlike the two above it *does* apply in the current session, because
  `app/redraw.rs` pushes it to `Sim::set_fov_y_degrees` every frame rather than
  folding it in at launch. `simulationDistance` stays inactive: it has no consumer
  in this shell at all, so wiring it would be the fabrication this bullet warns
  about. `the_disabled_majority_is_the_point_and_it_is_measured` uses
  `simulationDistance` as its inactive control, read off the real page — the old
  control constructed a synthetic `slider("renderDistance", …)` cell, which had
  `live: None` by construction and so was asserting a property of the
  constructor rather than of the tree. It would have kept passing after #443
  made the real row live.
- **A settings test must not write the real `options.json`.** Use
  `MenuNav::with_paths` with a temp path, as the existing tests do. This is the
  same class as the accounts-screen test that spawned `open` and launched a
  Microsoft OAuth URL in the owner's browser on every run.
- ~~**Sliders are clicked, not dragged.**~~ **Stale — a real drag path exists**, and
  it predates this doc: `MenuNav::drag_slider` → `set_live_slider` is vanilla's
  `AbstractSliderButton.setValueFromMouse`, reached from the initial mouse-down and
  every subsequent position, and it converts through the *same* tables the handle
  draw uses (`LiveOption::unit_double_mut`, `LiveOption::int_range`). So a slider
  has **two** mutation channels and a new `IntRange` option must add a write to
  both — `set_live_slider`'s match is the one that is easy to miss, because the
  click path works without it and the row then follows the cursor nowhere. The old
  text below is kept because the *click* half is still exactly as described, and it
  is what a keyboard Enter uses:
  `SettingsOutcome::Cycle` steps the value and wraps. Adding real drag
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
