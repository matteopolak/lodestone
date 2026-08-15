# Pause menu

## What it is

The in-game Escape menu, and `Sim::end_session` — the teardown that lets a
player leave a live session cleanly and start (or join) another one without
carrying anything over. Landed in `c53d022` ("an in-game pause menu, and a
way to leave a session on purpose").

## The layout is vanilla's, and it is not the three-button stack

`Screen::Paused` reproduces `PauseScreen.createPauseMenu`
(`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/PauseScreen.java:91-183`)
whole: **ten** widgets while the hosted world is unpublished (`PAUSE_BUTTONS`),
**nine** once it is published (`PAUSE_BUTTONS_PUBLISHED` drops Open to LAN —
see "Which Options row, and why" below). Seven are live in the unpublished
list (six once published, since Open to LAN is the one that disappears); the
rest are present-and-disabled. The rects live in `render::pause_slot`. A
sixth, *conditional* row — Server Links — is drawn outside this grid
entirely; see "A row outside the grid: Server Links" below.

Vanilla builds it with a `GridLayout`, so the rects are not obvious. Working
through `GridLayout.arrangeElements` (`GridLayout.java:25-89`) and
`AbstractLayout.AbstractChildWrapper::setX/setY` (`AbstractLayout.java:73-85`):

- two columns, `rowSpacing`/`columnSpacing` **0**, default cell padding
  `(4, 4, 4, 0)` — left, top, right, bottom (`PauseScreen.java:93`);
- the widest cell is the 204 px full-width button plus 4+4 padding = 212, split
  by `Divisor` into columns of **106 each**, so the grid is **212 wide**;
- row heights are `[70, 24, 24, 24, 24]` (row 0 carries `paddingTop(50)`,
  `PauseScreen.java:98`), giving y offsets `[0, 70, 94, 118, 142]` and a grid
  **166 tall**;
- `FrameLayout.alignInRectangle(grid, 0, 0, W, H, 0.5F, 0.25F)`
  (`PauseScreen.java:181`) puts the grid's origin at
  `(floor((W-212)/2), floor((H-166)/4))` — the alignment is a truncating
  `(int)` cast (`FrameLayout.java:113-116`), hence the floors.

Grid-relative, therefore. Row 3 (Options) is the one row that forks — see
"Which Options row, and why" below — but the fork changes only that row's
children, not any other row's offset: the grid keeps its five rows and its
212×166 size in both states (`the_pause_grid_size_matches_whether_or_not_lan_
is_published` pins that), so Disconnect sits at the same `+4, +146` either
way.

Index columns below are `pause_slot`'s own two match arms — unpublished
(`PAUSE_BUTTONS`, 10 rows) and published (`PAUSE_BUTTONS_PUBLISHED`, 9 rows) —
so they diverge from Options onward:

| unpub. # | pub. # | widget | offset from grid origin | size | state |
|---|---|---|---|---|---|
| 0 | 0 | Back to Game | `+4, +50` | 204×20 | live |
| 1 | 1 | Advancements | `+4, +74` | 98×20 | live (issue #167) |
| 2 | 2 | Statistics | `+110, +74` | 98×20 | live (issue #188) |
| 3 | 3 | Report Bugs (icon) | `+60, +98` | 20×20 | **disabled** |
| 4 | 4 | Give Feedback (icon) | `+84, +98` | 20×20 | **disabled** |
| 5 | 5 | Friends (icon) | `+108, +98` | 20×20 | **disabled** |
| 6 | 6 | Player Reporting (icon) | `+132, +98` | 20×20 | live (issue #189) |
| 7 | 7 | Options… | `+4, +122` | 98×20 unpub. / 204×20 pub. | live |
| 8 | — | Open to LAN (unpublished only) | `+110, +122` | 98×20 | live |
| 9 | 8 | Disconnect | `+4, +146` | 204×20 | live |

Unpublished, row 3 is a half-width pair — same 106 px columns and 8 px gutter
as row 1's Advancements/Statistics — and the grid holds all ten widgets
above. Published, Open to LAN's cell is simply absent and Options reclaims
the full 204 px row, same shape as row 0/row 8. Only three widgets remain
genuinely disabled — Report Bugs and Give Feedback open an external Mojang
link this client does not implement, and Friends needs a Microsoft-account
social graph — see "Advancements, Statistics and Player Reporting: no longer
disabled" below for the other three.

The "Game Menu" heading is a `StringWidget` at `W/2 - textWidth/2, 40`
(`PauseScreen.java:87-88`), the title being `menu.game`.

Three consequences that a layout built from memory gets wrong:

- **A full-width pause button starts at `W/2 - 102`, not `W/2 - 100`** — it is
  204 wide, not 200, because the 204 comes from `BUTTON_WIDTH_FULL`
  (`PauseScreen.java:53`) rather than `Button.BIG_WIDTH`.
- **The half-width pair has an 8 px gutter**, not the title screen's 4: each 98 px
  button sits 4 px into its own 106 px column, so they land at `W/2-102` and
  `W/2+4`. The title screen's pair is `W/2-100` / `W/2+2`.
- **The icon row is centred inside its colspan-2 cell**, the only cell with
  `alignHorizontallyCenter` (`PauseScreen.java:154`):
  `lerp(0.5, 4, 212 - 92 - 4) = 60`, then its own `LinearLayout` spaces four 20 px
  children 4 px apart — 60, 84, 108, 132.

### Which Options row, and why

**Stale as of issue #535: this client now hosts its own worlds, so the fork
below is real and the row is no longer fixed.** Vanilla forks on
`minecraft.hasSingleplayerServer()`, splitting the row into Options + Open to
LAN (`PauseScreen.java:157-159`) or giving Options the full 204 px alone
(`PauseScreen.java:161-163`). This client forks on a different, narrower
condition — whether the hosted world is *published* — because it has no
`MultiplayerOptionsScreen` unpublish/toggle form behind the button, unlike
vanilla's: once published, a second press of Open to LAN has nothing left to
do, so `MenuNav::pause_buttons` omits the row entirely rather than leaving it
present-and-broken. See [`open-to-lan.md`](./open-to-lan.md#the-shell-caller)
for the full trace and `PauseButton::OpenToLan`'s own doc for why this is an
*omission* rather than a disabled row. `pause_menu_grid_with`'s `published`
argument picks between the two arrangements; `pause_slot`/`pause_block` do
too, each caching one arranged grid per state.

The last button is `CommonComponents.disconnectButtonLabel(isLocalServer)` in
vanilla — "Save and Quit to Title" locally, "Disconnect" remotely
(`CommonComponents.java:53-55`). We use "Disconnect" for both: singleplayer here
is the local dev world with no persistence, so "Save and Quit" would promise a
save that does not happen.

### Advancements, Statistics and Player Reporting: no longer disabled

An earlier version of `nav.rs` **omitted** Advancements and Statistics
entirely, on the grounds that neither has a client-side subsystem to open
onto, so either button would reach zero pixels — this repo's dominant defect
class. That reasoning was right about the *action* and wrong about the
*position*: a greyed-out button where vanilla puts one is faithful UI rather
than an island, and vanilla itself greys buttons out on this very screen
(`playerReportingButton` with no players to report, `PauseScreen.java:148-151`).
So both landed present-and-`PauseButton::enabled() == false`, matching that
reasoning — but that was an interim state, not the current one.

**All three of Advancements, Statistics and Player Reporting are now live**,
each once its own screen landed: Advancements (issue #167, `update_advancements`
decoded, real progress — see `advancements.md`), Statistics (issue #188 — the
screen is real and honestly shows zero everywhere, since nothing decodes
`award_stats` yet; see `statistics-screen.md`), Player Reporting (issue #189,
opens the Social screen — see `social-interactions.md`). `PauseButton::enabled`
lists all three. **Only Report Bugs, Give Feedback and Friends remain
genuinely disabled** — the first two open an external Mojang link this client
does not implement, and Friends needs a Microsoft-account social graph this
client does not have.

The greyed look is vanilla's own — `widget/button_disabled` plus a
`0xFF_A0_A0_A0` label — not an invented one. See
[`main-menu.md`](./main-menu.md) for the sprite-selection rule, the nine-slice
borders, and the click-on-a-disabled-button trap.

### The backdrop is vanilla's exact value now

`Screen`'s in-world menu background is `textures/gui/inworld_menu_background.png`
tiled at 32 px (`Screen.java:405,418-419`). That file, decoded straight out of
`client.jar`, is a 16×16 greyscale+alpha PNG in which **every pixel is grey 0,
alpha 64** — and `menu_background.png` (the out-of-world variant) is byte-for-byte
identical. So there is no dirt texture to reproduce and nothing lost by drawing
one quad instead of tiling: `render::OVERLAY_BG` is `[0, 0, 0, 64/255]`, and
`the_pause_overlays_backdrop_is_vanillas_measured_black_at_alpha_64` pins the
exact floats.

**The blur vanilla applies behind the pause screen is implemented now** — see
[`menu-blur.md`](./menu-blur.md) for the six-pass box blur itself. `pause_frame`
sets `MenuFrame::blur = true` (vanilla's `PauseScreen` never overrides
`isInGameUi()`, so the default fork applies). It runs at vanilla's default
radius (`Options.BLURRINESS_DEFAULT_VALUE = 5`) as a hardcoded constant, not a
live `menuBackgroundBlurriness`-equivalent setting — that doc's "How to
change it" is where wiring a real option would go.

### A row outside the grid: Server Links

Vanilla's Server Links button (`menu.server_links`, opens a dialog listing
links a server announced) is **not** one of `PAUSE_BUTTONS`/
`PAUSE_BUTTONS_PUBLISHED` — `pause_slot`'s `ServerLinks` arm places it just
below the whole grid (`dy: grid_h + padding`) rather than as a sixth grid row,
the same "outside the arranged tree" shape `MainButton::Accounts` already uses
on the title screen. `MenuNav::pause_buttons` appends it dynamically, and only
when the server actually announced a link — vanilla's own
`!serverLinks.isEmpty()` gate, reproduced as an *omission* rather than a
disabled row, matching `PauseButton::OpenToLan`'s own precedent. It opens
`Screen::ServerLinks`; see `crate::menu::server_links`'s own module doc for
the screen behind it.

## How it works

### The Escape stack

`Screen::Paused` (`crates/lodestone-shell/src/menu.rs`) is reached from
`Screen::Playing` on Escape (or on focus loss). Its own doc comment states
the design intent directly: "pointer released, player input frozen. The
world behind keeps rendering and — on a live server — keeps ticking; pausing
is a local UI state, not a world stop." That's a deliberate divergence from
single-player vanilla (which halts world ticking while paused) — this client
never owns the world simulation, so it can't stop it, and doesn't pretend to.

`UiState::on_escape` is a single exhaustive match, now well past twenty arms
as more screens landed (`CommandBlockEdit`, `SignEdit`, `WorldSelect`,
`Accounts`, `Death`, `Credits`, `Social`, `Statistics`, `ServerLinks`,
`Advancements`, `CreateWorld`, `Confirm`, …) — this is **not** that full
match, only the transitions this doc's own reader needs, the ones that
actually touch pause:

```
Playing → Paused          Paused → Playing
Chat | Container → Playing
Error → dismiss
ServerEdit → ServerList    ServerList → MainMenu
Settings → MainMenu, or Paused if opened from there
MainMenu → request_quit
Connecting → no-op
```

Read `menu.rs`'s `on_escape` itself for every other screen's arm — most of
them are a documented no-op or single-level unwind that some screen-specific
key handler (`MenuNav::key_*`) intercepts before `on_escape` is ever reached
in practice, kept here only so the match stays exhaustive for a caller that
reaches this fallback some other way.

The one genuinely stateful part is Options-from-pause. `settings_return:
Screen` records whether `Screen::Settings` was entered via
`open_settings` (title screen only) or `open_settings_from_pause`
(pause only), and `close_settings` returns to whichever it was. That's what
makes Escape from Options a real stack rather than a shortcut back to the
title: opening Options mid-game and pressing Escape lands back on the pause
menu, not on the main menu and not back in gameplay.

### Why `Screen::Paused` must stay out of `owns_frame`

`owns_frame` (`crates/lodestone-shell/src/menu/render/frame.rs`) is the set of
screens the menu renderer treats as owning the entire frame. It has grown well
past its original five as more full-screen menus landed — `MainMenu`,
`ServerList`, `ServerEdit`, `WorldSelect`, `Settings`, `Accounts`,
`Connecting`, `Error`, `Credits`, `Social`, `Statistics`, `CreateWorld` and
`Confirm` as of this writing — but the one fact this section exists for has
not changed: `Screen::Paused` is not, and cannot safely be added to, that set.

That set drives a **`Clear` render pass** — appropriate for the main menu,
which has no world behind it to preserve. `Screen::Paused` is deliberately
excluded, because pause has to draw *over* a world that is still being
rendered (and, on a live server, still ticking): adding it to `owns_frame`
would replace the world with the clear colour for as long as the game
stayed paused. Instead, the pause overlay goes through a second render
entry point, `MenuRenderer::render_overlay`, which uses
`wgpu::LoadOp::Load` instead of `Clear` — draw on top of whatever the world
pass already put in the target, don't erase it first. `App`'s draw loop
calls this after the world/HUD/container passes, gated on
`self.ui.is_paused()`. The negative-control test
`owns_frame_excludes_paused_so_the_pause_menu_never_replaces_the_world`
(`crates/lodestone-shell/src/menu/render/tests.rs`) pins this the way
`CLAUDE.md`'s evidence standard asks: not just "pause renders correctly" but
"pause is provably absent from the set that would break it."

### The HUD keeps drawing behind it (issue #61)

Keeping the world rendering is only half of it. The **HUD** has to keep drawing
too, and for a while it did not: `app.rs` computed one boolean

```rust
let crosshair = self.ui.is_playing();
hud_frame.hotbar       = crosshair.then(|| self.sim.selected_slot());
hud_frame.hotbar_items = crosshair.then_some(hotbar_records.as_slice());
```

and used it for both the aiming reticle *and* the hotbar, so opening the pause
menu (or the inventory, or the chat box) took the hotbar with it. One boolean,
two questions, and the name told you which question its author had in mind.

Vanilla's answer is unambiguous, and it is about the **world**, not about play:

| source | says |
| --- | --- |
| `GameRenderer.java:377,389` | `readyForLevelRendering = resourcesLoaded && advanceGameTime && level != null` is what the GUI is handed — it asks about the *level*, never about `screen` |
| `Gui.java:152-156` | `hud.extractRenderState(...)` runs under that flag **alone** |
| `Gui.java:171-189` | the open screen is extracted *after* the HUD, i.e. painted on top |
| `Hud.java:218-221` | `Hud.extractRenderState` gates only on F1 (`isHidden`) and `LevelLoadingScreen` |
| `Hud.java:534-562` | hotbar, hearts, hunger, XP bar, held-item name — **game mode** gates only |

So the dim you see over an open inventory is not the HUD switching off; it is the
screen's own translucent background drawn over a HUD that is still there.

`app::hud_follows_world(Screen)` is now that predicate — true for `Playing`,
`Chat`, `Container`, `Paused`, and (issue #103) `Death`; false for
`Connecting` (no world yet) and every menu screen. `HudFrame::crosshair` keeps
`is_playing()`.

Exactly two vanilla HUD elements *do* consult `screen()`, and neither is a vital:
the potion-effect icons (`Hud.java:486-488` — suppressed only when the screen
`showsActiveEffects()`, overridden `true` by `InventoryScreen` and
`CreativeModeInventoryScreen`, which draw their own) and the subtitle overlay
(`Hud.java:238-241`). **The crosshair is not one of them** — `Hud.java:439-470`
gates on camera type and spectator mode only, so vanilla's reticle is still there
behind an open inventory, dimmed.

**Issue #51 is now closed on the dimming side** — see `docs/container-screen.md`'s
"The dim behind the panel". `ContainerRenderer` now draws its own full-canvas
gradient (vanilla's `extractTransparentBackground`) **after** the HUD pass in
`app.rs`, exactly the shape this section always said the fix had to take —
"There is no per-element dimming here, and adding one would be the wrong shape:
the dim belongs to the screen's background pass, not to an alpha on every HUD
widget" turned out to be the right call, not just the interim one. The hotbar
now genuinely reads darker behind an open chest or the local inventory screen,
proven pixel-for-pixel by `tests/container_background_pixels.rs`.

Hiding **the crosshair** specifically remains a deliberate, *separate*
divergence, not fixed by that change: `hud_frame.crosshair` is still driven by
`is_playing()` alone (`app.rs`), so it still disappears rather than drawing and
dimming like vanilla's does. A background pass to dim behind it now exists —
the blocking reason above is gone — but nobody has wired the crosshair itself
back into `hud_follows_world`'s set yet; that is a small, separate follow-up,
not a consequence anything here does automatically.

### Leaving a session: the pause menu → `Sim::end_session`

The button path: `MenuNav::key_paused`'s `PauseButton::QuitToTitle` arm
calls `ui.quit_to_title()` (`UiState`, `menu.rs`) — which only fires from
`Screen::Paused`, and only moves the screen state (`screen = MainMenu`,
clears `kind`/`error`). It's deliberately *not* routed through the
session-failure path — this isn't an error, and screen state is not where
the actual network teardown happens. `App::apply_menu_action`'s
`MenuAction::QuitToTitle` arm is what calls `self.sim.end_session()` and
re-syncs cursor grab. Mouse clicks on the pause row reach the same
`MenuAction::QuitToTitle` through the ordinary click → `MenuKey::Enter` →
`nav.key` path, so keyboard and mouse converge on one teardown call.

### `Sim::end_session`: what it resets, what it keeps

**Resets** (`crates/lodestone-shell/src/sim/session.rs` — the whole session
lifecycle moved there in seam 10 of the `sim.rs` decomposition; see
[`docs/sim-dissolution.md`](./sim-dissolution.md)):

- The connection: `net` is dropped (`NetClient::drop` joins its background
  thread first, so nothing races a reset against an in-flight poll), `phase`
  returns to `LocalOnly`.
- Screen-adjacent session flags added since this list was first written:
  `death_message` (issue #103's death screen must not survive a
  quit-to-title), `won` (issue #192's credits screen, same reason),
  `lan_published` (the pause menu must offer Open to LAN again next session —
  see [`open-to-lan.md`](./open-to-lan.md)), and the dimension-edge/portal
  state `reset_dimension_state` clears (a stale `applied_dimension` would make
  the next session's login look like a dimension change and drop freshly
  streamed terrain).
- Every read-model `poll_net` feeds: chat log, tab list, scoreboard, status
  effects, title/action-bar, health/food/dead/respawn-count/experience,
  entity interpolation state, the local entity id, the teleport-count
  diagnostic.
- In-flight prediction: `mining` and `placement` are **replaced wholesale**,
  not just stopped — both carry a monotonic sequence counter with no public
  reset, and `Mining` separately tracks a post-break cooldown that `stop()`
  alone doesn't clear. `attacking` clears; the sprint/input edge trackers
  (see [`swimming.md`](./swimming.md)) reset to their `Sim::new` values so
  the next session's first packet isn't suppressed as a redundant resend of
  the old session's last-known state.
- Meshing: in-flight mesh jobs for the old server's chunks are drained and
  discarded rather than left to land silently in whatever session comes
  next; `dirty_columns`/`mesh_drops` clear; every section this session ever
  uploaded is queued into `pending_removals`, the app's existing per-frame
  drain path.
- The player: reset to the same spawn `Sim::new` would use (the demo
  surface for a fixture `Sim`, a pre-session placeholder for a real client —
  a live reconnect immediately overrides this with the new server's login
  teleport). `fly` clears, `target` clears, all input released.
- `status`: recomputed with the exact rule `Sim::new` uses, so the debug
  overlay reads a fresh "local world"/"live world" string instead of
  whatever the old session last wrote (e.g. a disconnect reason).

**Deliberately kept**: GPU pipelines/buffers and loaded assets
(`vanilla_atlas`, `language`, `version_data`) — these are config- or
asset-derived, not session state, and `Sim::new` itself never reloads them
on `attach_net` either, so a teardown holds the same line. `particles` is
also left untouched on purpose: every particle already expires on its own
within a couple of seconds, and nothing drives its tick/extract once the
title screen stops calling into the render path, so a leftover burst is
inert rather than a visible bug.

### Reconnect is the real acceptance test

Clearing fields is necessary but not sufficient — the actual claim
`end_session` makes is that a **second** connection afterward behaves
exactly like the first. `end_session_tears_down_and_a_fresh_connect_
afterward_starts_clean` (`sim/tests.rs`) is written to prove that, not just that
fields end up empty: it populates chat/health/entity-id from a first
session, calls `end_session()`, asserts the reset, then attaches a
**second** `NetClient`, drives it to `Connected` with a different entity
id, and asserts the new session's chat log is still empty. That last
assertion is the negative control — without it, "chat is empty after
end_session" could just mean nothing had polled yet, not that the reset
actually took.

## How to change it

- **Screen state machine** — `UiState` in `crates/lodestone-shell/src/menu.rs`
  (`on_escape`, `quit_to_title`, `open_settings`/`open_settings_from_pause`/
  `close_settings`, `settings_return`).
- **Pause menu input and the widget list** —
  `crates/lodestone-shell/src/menu/nav.rs` (`key_paused`, `PauseButton`,
  `PauseButton::{enabled, icon}`, `step_enabled`).
- **Pause menu layout** — `crates/lodestone-shell/src/menu/render/title_pause.rs`
  (`pause_menu_grid_with`, `pause_block`, `pause_slot`, `pause_grid_size`); the
  hand-derived `PAUSE_GRID_W`/`PAUSE_GRID_H` constants still live in
  `crates/lodestone-shell/src/menu/render.rs`, and `Origin::PauseGrid` is in
  `crates/lodestone-shell/src/menu/render/origin.rs`. Change a rect here and
  nowhere else: `row_rect` resolves the slot and `app.rs`'s `menu_row_at` calls
  `row_rect`, so the draw and the hit-test cannot disagree.
- **Rendering** — `owns_frame` is
  `crates/lodestone-shell/src/menu/render/frame.rs`; `pause_frame` is
  `crates/lodestone-shell/src/menu/render/screens.rs`; `render_overlay` vs
  `render`, `build` vs `geometry` are in
  `crates/lodestone-shell/src/menu/render/renderer.rs`/`draw.rs`. If you add a
  new screen that should overlay the world instead of replacing it, follow
  `Paused`'s pattern — a second render entry point with `LoadOp::Load`, kept
  out of `owns_frame` — rather than adding a special case inside the `Clear`
  path. [`Screen::Death`](./death-screen.md) (issue #103) is the first screen
  that actually did this, end to end.
- **The blur behind the overlay** — see [`menu-blur.md`](./menu-blur.md)'s own
  "How to change it"; setting `MenuFrame::blur` is the pause frame builder's
  job, the pass itself is shared with every other blurred overlay.
- **Teardown** — `Sim::end_session` in
  `crates/lodestone-shell/src/sim/session.rs`.
  Adding new per-session state anywhere in `Sim`? Check whether it needs a
  line here — the "what it keeps" list is short and deliberate, so anything
  not explicitly listed as kept should be assumed to need resetting, and the
  acceptance test above is the way to prove it actually is.
- **Wiring**: `App::apply_menu_action`,
  `crates/lodestone-shell/src/app/menus.rs`, is the only call site of
  `end_session` — driven by `MenuAction::QuitToTitle`
  from `apply_menu_action`'s dispatch over what `nav.key`/mouse-click
  handling returns.

## Configuration

None of its own — no flags gate whether the pause menu exists or how it
renders. The blur behind it is a hardcoded radius, not a setting; see
[`menu-blur.md`](./menu-blur.md)'s own Configuration section.

## Dependencies

- `crate::menu::{nav, render}` — the pause screen's own state and layout.
- `crate::menu::server_links` — the screen the conditional Server Links row
  opens.
- [`menu-blur.md`](./menu-blur.md) — the backdrop blur behind the overlay.
- `crate::net::NetClient` — what `end_session` drops.
- `lodestone-game::{mining::Mining, placement::Placement}` — the prediction
  state `end_session` replaces wholesale rather than resetting in place.
- [`swimming.md`](./swimming.md) — the sprint edge trackers `end_session`
  also resets, for the same "next session starts clean" reason.
- [`main-menu.md`](./main-menu.md) — the screen this session's teardown
  hands control back to.

## Tests

Hermetic, `crates/lodestone-shell/src/sim/tests.rs`:
`end_session_tears_down_and_a_fresh_connect_afterward_starts_clean` (the
acceptance test above). `crates/lodestone-shell/src/menu.rs`:
`quit_to_title_only_leaves_from_pause_and_clears_session_state`.
`crates/lodestone-shell/src/menu/nav.rs`:
`quit_to_title_from_the_pause_menu_leaves_for_the_main_menu`,
`a_disabled_button_is_hoverable_but_cannot_be_activated`,
`keyboard_navigation_steps_over_every_disabled_button`.
`crates/lodestone-shell/src/menu/render/tests.rs`:
`owns_frame_excludes_paused_so_the_pause_menu_never_replaces_the_world`,
`pause_frame_builds_vanillas_ten_widgets_in_order_and_tracks_the_highlight`,
`the_pause_screen_rects_are_vanillas_own` (the hand-derived grid above, asserted
against `pause_slot` rather than read out of it),
`the_published_pause_frame_drops_open_to_lan_and_reflows_options`,
`the_pause_grid_size_matches_whether_or_not_lan_is_published`,
`a_changed_cell_padding_moves_every_pause_rect` (issue #394's negative control:
change one `LayoutSettings` padding value on the real builder and watch every
rect assertion go red),
`every_vanilla_widget_is_on_screen_and_none_overlap`,
`the_button_sprite_matches_vanillas_enabled_hovered_rule`,
`nine_slice_borders_come_from_the_mcmeta_not_a_constant`,
`the_pause_overlays_backdrop_is_vanillas_measured_black_at_alpha_64`,
`frame_for_defers_to_an_overlay_for_server_links` (the Server Links screen
reached from the pause row's conditional button).
