# Death screen

## What it is

Vanilla's `DeathScreen` (issue #103): the screen that appears when the local
player's health reaches zero — the "You Died!" title, the server's death
message, a score line, and two buttons (Respawn / Title Screen). It draws as
an overlay over the still-rendering, still-ticking world, the same way
[`Screen::Paused`](./pause-menu.md) does, and gates a respawn the client used
to send automatically with no screen at all.

This landed alongside `PlayerVitals` (`1c0c700`), which made drowning damage
real down to zero health — the first way a served session can actually kill
the player, which is what made the screen's absence visible rather than
hypothetical.

## The actual behaviour change: manual respawn, not just a screen

Before this, `lodestone-client`'s `RespawnPolicy::Automatic` (the library
default) answered every `Death` event with an unconditional
`ClientAction::Respawn` — the death packet arrived and left again inside one
library call, before the shell ever got a chance to react. The shell's
`Sim::poll_net` rode along: it marked the player `Dead` (freezing movement)
and waited for the server-confirmed respawn, but nothing on screen ever
showed it, because nothing gated the respawn on a click.

`crate::net::run` (`crates/lodestone-shell/src/net.rs`) now builds the
`ClientBuilder` with `.respawn_policy(RespawnPolicy::Manual)`. That is the one
real behaviour change; everything else in this doc is UI built on top of it.
With the policy manual, `Sim::poll_net`'s `NetUpdate::Death` arm still marks
the player dead and records the message (`Sim::death_message`), but nothing
sends a respawn until `Sim::respawn` is called — which only happens from the
death screen's Respawn button (`MenuAction::Respawn` →
`WindowApp::apply_menu_action` in `app.rs`).

**`ClientAction::Respawn` already had an encoder before this change** —
`crates/protocol/v770/src/adapter/serverbound.rs::V770Adapter::encode_client_action`'s `ClientCommand { action: 0 }` (vanilla's
`ServerboundClientCommandPacket.Action.PERFORM_RESPAWN`, ordinal 0) — and
already had exactly one caller: the automatic policy's `auto_actions.push(...)`
in `lodestone-client/src/driver.rs`. So this was not the "fully encoded, zero
callers" island pattern this repo has hit before (`InteractEntity::Attack` was
exactly that); it was an encoder with an *automatic* caller and no *manual*
one reachable from the shell. `Sim::respawn` is that second caller.

## How it works

### `Screen::Death`

`crates/lodestone-shell/src/menu.rs`. Reachable from every live gameplay
screen — `Playing`, `Chat`, `Container`, `Paused` — via `UiState::die`, which
matches vanilla: `ClientPacketListener` replaces whatever screen is open the
instant the death packet lands, not only from `Playing`. Left only by
`UiState::respawn_confirmed`, called once `Sim::is_dead()` goes false (the
server's respawn confirmation), never by a click alone — the button sends the
request and waits, it does not jump the screen itself.

**Escape does nothing here.** Vanilla's `DeathScreen.shouldCloseOnEsc()`
returns `false`; `UiState::on_escape`'s `Death` arm
and `MenuNav::key_death`'s handling of `MenuKey::Escape` are both deliberate
no-ops — every sibling screen in this file calls `ui.on_escape()` for Escape,
this is the one that must not.

### Reconciliation: `Sim::is_dead()` drives the screen, not the reverse

`WindowApp::drive_ui_from_session` (`app.rs`), which already reconciles
`UiState` from `SessionPhase` every frame, does the same for death:

```rust
if self.sim.is_dead() {
    if !self.ui.is_death() { self.ui.die(self.sim.death_message().map(str::to_string)); }
} else if self.ui.is_death() {
    self.ui.respawn_confirmed();
}
```

The `!self.ui.is_death()` guard makes `die` fire once per death rather than
re-latching the message every frame the screen is up. `respawn_confirmed`
needs no such guard — `UiState` already refuses it off `Screen::Death`.

### Rendering: an overlay, exactly like `Screen::Paused`

A live server holds a dead player with no chunk stream until it respawns (see
`CLAUDE.md`'s dead-player note) — this screen must not itself stop the world
rendering or ticking while that holds, or it reintroduces a *different* cause
of the same symptom. So it follows `Screen::Paused`'s established pattern
(`docs/pause-menu.md`'s "How to change it" names this explicitly as the
pattern for a new overlay screen) rather than joining `render::owns_frame`'s
`Clear`-pass set:

- `render::death_frame(nav, message)` builds the `MenuFrame` — not
  `render::frame_for`, which returns `None` for `Screen::Death` by design
  (`owns_frame` excludes it, and the two are asserted to agree in
  `owns_frame_agrees_with_frame_for_on_every_screen`).
- `App::redraw` draws it with `MenuRenderer::render_overlay` (`LoadOp::Load`)
  after the world/HUD/container passes, gated on `self.ui.is_death()` —
  right beside the identical `is_paused()` block.
- `hud_follows_world` includes `Screen::Death`: vanilla's
  `Hud.extractRenderState` gates only on F1/`LevelLoadingScreen`, never on
  which screen is open, so the hotbar/hearts/hunger keep drawing (dimmed by
  the death screen's own background) behind it.
- `menu_row_at`, the `CursorMoved`/`MouseInput` window-event arms, and
  `KeyGate.menu` all gained `|| self.ui.is_death()` beside their existing
  `|| self.ui.is_paused()` — the whole keyboard/mouse belongs to this screen
  while it is up, the same way it belongs to the pause overlay.

### Layout: vanilla's own rects, from the jar

`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/DeathScreen.java`,
cited inline in `render.rs`:

| element | vanilla expression | source |
|---|---|---|
| title | `middleLine / 2, 30`, scale ×2 | `DeathScreen.extractRenderState` |
| death message (if any) | `middleLine, 85`, scale ×1 | `DeathScreen.extractRenderState` |
| score | `middleLine, 100`, scale ×1, always drawn | `DeathScreen.extractRenderState` |
| Respawn button | `width/2-100, height/4+72, 200×20` | `DeathScreen.init` |
| Title Screen button | `width/2-100, height/4+96, 200×20` | `DeathScreen.init` |

`middleLine = width / 2`, so the title is centred on **`width / 4`** — the
screen's left quarter, not its centre — which is exactly what
`render::Origin::DeathTitle` reproduces rather than "fixing". Every other
centred heading in this file (`Origin::ScreenTop`) is on `width / 2`; a
layout built from memory gets this one wrong. The two buttons reuse
`Origin::TitleTop` (`floor(height/4) + 48`, the same anchor the title screen's
own stack uses) plus a `24`/`48` px offset, since `height/4 + 72` and `+ 96`
are exactly that anchor plus those two numbers — not a coincidence, both
screens lay out from `this.height / 4`.

### `MenuLabel` gained a `scale` field

Every vanilla label before this drew at scale 1.0 implicitly
(`render::build`'s `frame.vanilla` loop hardcoded it) — nothing needed
anything else. The death screen's title needs ×2
(`DeathScreen.TITLE_SCALE`, read in `DeathScreen.extractRenderState`), so `MenuLabel::scale` is now an explicit field
and `build` reads it instead of the constant. The three pre-existing call
sites (`pause_frame`'s "Game Menu", the title screen's version/copyright
corners) all set `scale: 1.0`, unchanged in effect.

### `MenuNav`/`DeathButton`

`crates/lodestone-shell/src/menu/nav.rs`, mirroring `PauseButton`/
`PAUSE_BUTTONS` exactly: a `death: usize` highlight field, `DeathButton`
(`Respawn`, `TitleScreen`), `DEATH_BUTTONS`, `death_button()`/`death_index()`
accessors, a `hover` arm, and `key_death` (Up/Down wrap with
`wrap_prev`/`wrap_next` — both buttons are always enabled, so there is no
disabled row to step over the way `key_main`/`key_paused` do). `Enter` on
Respawn returns the new `MenuAction::Respawn`; `Enter` on Title Screen calls
`ui.quit_to_title()` and returns the existing `MenuAction::QuitToTitle` —
`UiState::quit_to_title`'s guard was widened from `Screen::Paused` alone to
`Screen::Paused | Screen::Death`.

## How to change it

- **Layout** — `crates/lodestone-shell/src/menu/render.rs` (`death_slot`,
  `Origin::DeathTitle`, `death_frame`). Change a rect here and nowhere else:
  `row_rect` resolves the slot and `app/menus.rs`'s `menu_row_at` calls `row_rect`,
  so the draw and the hit-test cannot disagree — same rule as every other
  vanilla-laid-out screen (see `docs/pause-menu.md`).
- **Navigation/actions** — `crates/lodestone-shell/src/menu/nav.rs`
  (`key_death`, `DeathButton`, `MenuAction::Respawn`).
- **Screen state machine** — `crates/lodestone-shell/src/menu.rs`
  (`Screen::Death`, `UiState::die`/`respawn_confirmed`, the `on_escape`/
  `session_failed`/`quit_to_title` arms).
- **The respawn action and death state** —
  `crates/lodestone-shell/src/net.rs` (`net::run`'s
  `RespawnPolicy::Manual`, `NetUpdate::Death { message }`) and
  `crates/lodestone-shell/src/sim/session.rs` (`Sim::death_message`,
  `Sim::respawn`).
- **Wiring** — `WindowApp::apply_menu_action`'s `MenuAction::Respawn` arm (in
  `app/menus.rs`) and `drive_ui_from_session`'s reconciliation block (in
  `app/session.rs`).

### What was deliberately left out, named rather than half-done

- **Hardcore mode.** Nothing in this client decodes a client-visible hardcore
  flag, so the title is always `deathScreen.title` ("You Died!") and the
  first button is always `deathScreen.respawn` ("Respawn") — vanilla's
  hardcore fork (`deathScreen.title.hardcore` "Game Over!",
  `deathScreen.spectate`, no confirm dialog) never takes.
- **The Title Screen confirm dialog.** Vanilla pops a `ConfirmScreen`
  ("Are you sure you want to quit?") before disconnecting, unless hardcore.
  This client's Title Screen button goes straight to `MenuAction::QuitToTitle`
  — the same simplification `docs/pause-menu.md` already makes for the pause
  menu's Disconnect button, not a new one invented here. The full
  disconnect/report flow this would otherwise route through
  (`draftReportHandled`, `disconnectWithSavingScreen`) is `quit_to_title`'s
  existing, already-covered path.
- **The score line is always "Score: 0".** Vanilla's score is
  `LocalPlayer.getScore()`, synced through `Player`'s `DATA_SCORE_ID` entity
  metadata field — nothing in this workspace decodes that field. The line
  draws at vanilla's position with the only value available, the same
  "present, honestly simplified" choice `docs/main-menu.md`/`docs/pause-menu.md`
  make for a present-but-disabled button, rather than omitting a line vanilla
  always shows.
- **The death message renders untranslated.** `net::forward` flattens
  `ClientEvent::Death`'s `message: Text` with `to_plain_string()`, not through
  the language table — most death causes are `translatable` components (e.g.
  `death.attack.generic`), so they currently render as their raw key rather
  than resolved English. A generic-but-present message was judged good enough
  for this change; threading the death message through `Sim::translator()`
  (already used for chat/title/action-bar) the way those are is a small,
  separate follow-up. **Still true after the disconnect-reason fix**, which fixed the identical
  bug for `NetUpdate::Disconnected`/`Screen::Error` (see `main-menu.md`'s
  "The disconnect reason goes through the language table" section) but
  deliberately left this one alone — that fix's sweep for other pre-stringified
  `Text` found this exact gap and confirmed it is still open, not a new find.
- **No gradient backdrop.** Vanilla's `DeathScreen.extractBackground` is a
  reddish `fillGradient` (`DeathScreen.extractDeathBackground`); this screen draws with
  `render::OVERLAY_BG`, the same flat 25%-black dim `Screen::Paused` uses,
  because `menu/render.rs`'s `Quads::rect` takes one flat colour with no
  per-vertex gradient. Reproducing the gradient means extending that
  primitive for one screen; left for polish, same spirit as the title
  screen's missing panorama (`docs/main-menu.md`).
- **No 20-tick button-disable delay.** Vanilla's `DeathScreen.tick`
  disables both buttons for the first 20 ticks after the screen opens
  (`delayTicker`), so a stray Enter carried over from whatever the player was
  doing at the moment of death cannot instantly re-trigger. This client's
  buttons are live immediately — `nav.rs` has no per-screen tick clock to
  hang a delay off yet, and `MenuKey::Enter` only ever arrives from a fresh
  key-down or click, not a held key, so the actual footgun vanilla's delay
  guards against does not reproduce here the same way.

## Configuration

None of its own — no flags gate whether the death screen exists or how it
renders. `RespawnPolicy` itself is a `lodestone-client` builder option; the
shell hardcodes `Manual` in `net::run` rather than exposing a flag, since a
client that shows a death screen but still auto-respawns underneath it would
be a screen the player could never actually use.

## Dependencies

- `lodestone_client::RespawnPolicy` — the policy `net::run` sets to `Manual`.
- `crate::menu::{nav, render}` — the death screen's own state and layout,
  following `Screen::Paused`'s established overlay pattern.
- `crate::hud::VanillaFont` — the real-glyph text this screen draws with (via
  `MenuFrame::vanilla`/`labels`), same as the title and pause screens.
- [`pause-menu.md`](./pause-menu.md) — the overlay-over-the-world pattern this
  screen follows, and the `Sim::end_session` teardown that `death_message`
  also resets.

## Tests

Hermetic, `crates/lodestone-shell/src/menu.rs`:
`die_reaches_the_death_screen_from_every_live_gameplay_screen_and_carries_the_message`,
`respawn_confirmed_only_leaves_from_death_and_clears_the_message`,
`escape_does_not_leave_the_death_screen`,
`quit_to_title_from_the_death_screen_leaves_for_the_main_menu`,
`a_disconnect_while_dead_reaches_the_error_screen` (the `CLAUDE.md` hazard,
covered directly: a genuine disconnect while dead must still reach
`Screen::Error`, not strand the player on a Respawn button that can never get
an answer).

`crates/lodestone-shell/src/menu/nav.rs`:
`hovering_a_death_row_moves_the_highlight`,
`death_screen_keyboard_navigation_wraps_between_the_two_buttons`,
`enter_on_respawn_asks_the_app_to_respawn_and_stays_on_the_death_screen`,
`enter_on_title_screen_leaves_for_the_main_menu`,
`escape_does_nothing_on_the_death_screen`.

`crates/lodestone-shell/src/menu/render.rs`:
`death_frame_builds_vanillas_two_widgets_in_order_and_tracks_the_highlight`,
`the_death_screen_rects_are_vanillas_own` (hand-derived from the Java source,
asserted against `death_slot` rather than read back out of it),
`the_death_screens_title_is_anchored_on_the_left_quarter_not_the_centre` (the
`width/4` trap named above), plus `Screen::Death` folded into the existing
`owns_frame_agrees_with_frame_for_on_every_screen` and
`every_vanilla_widget_is_on_screen_and_none_overlap` sweeps.

`crates/lodestone-shell/src/net.rs`:
`ClientEvent::Death`'s message now asserted to cross into `NetUpdate::Death`
flattened, in the existing `forward`-translation test.

Live, `crates/lodestone-shell/tests/live_death_respawn.rs` — **run against
the real `lodestone-survival` 26.2 oracle for this change** (not just
described): the negative control (`recover_from_death = false`) reproduced
the pre-fix stuck-on-death bug (`phase=Ended("player died")`, `health=0.0`);
the fixed path, with the test standing in for the screen's Respawn click by
calling `Sim::respawn()` once `is_dead()` was observed, reached
`phase=Connected, respawns=1, health=20.0, loaded=23` chunks with the
player's own column loaded — i.e. `RespawnPolicy::Manual` genuinely stops the
automatic respawn, and `Sim::respawn` genuinely gets a real vanilla-behaviour
server to respawn the player and resume streaming.

```text
cargo test -p lodestone-shell --lib menu:: --no-fail-fast
cargo test -p lodestone-shell --features live \
  --test live_death_respawn -- --ignored --nocapture
```
