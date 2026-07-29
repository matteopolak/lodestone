# Pause menu

## What it is

The in-game Escape menu, and `Sim::end_session` — the teardown that lets a
player leave a live session cleanly and start (or join) another one without
carrying anything over. Landed in `c53d022` ("an in-game pause menu, and a
way to leave a session on purpose").

## How it works

### The Escape stack

`Screen::Paused` (`crates/lodestone-shell/src/menu.rs`) is reached from
`Screen::Playing` on Escape (or on focus loss). Its own doc comment states
the design intent directly: "pointer released, player input frozen. The
world behind keeps rendering and — on a live server — keeps ticking; pausing
is a local UI state, not a world stop." That's a deliberate divergence from
single-player vanilla (which halts world ticking while paused) — this client
never owns the world simulation, so it can't stop it, and doesn't pretend to.

`UiState::on_escape` is the full state machine:

```
Playing → Paused          Paused → Playing
Chat | Container → Playing
Error → dismiss
ServerEdit → ServerList    ServerList → MainMenu
Settings → MainMenu, or Paused if opened from there
MainMenu → request_quit
Connecting → no-op
```

The one genuinely stateful part is Options-from-pause. `settings_return:
Screen` records whether `Screen::Settings` was entered via
`open_settings` (title screen only) or `open_settings_from_pause`
(pause only), and `close_settings` returns to whichever it was. That's what
makes Escape from Options a real stack rather than a shortcut back to the
title: opening Options mid-game and pressing Escape lands back on the pause
menu, not on the main menu and not back in gameplay.

### Why `Screen::Paused` must stay out of `owns_frame`

`owns_frame` (`crates/lodestone-shell/src/menu/render.rs`) is the set of
screens the menu renderer treats as owning the entire frame:

```rust
pub fn owns_frame(screen: super::Screen) -> bool {
    matches!(screen, Screen::MainMenu | Screen::ServerList | Screen::ServerEdit | Screen::Settings | Screen::Error)
}
```

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
(`render.rs`) pins this the way `CLAUDE.md`'s evidence standard asks: not
just "pause renders correctly" but "pause is provably absent from the set
that would break it."

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

**Resets** (`crates/lodestone-shell/src/sim.rs`):

- The connection: `net` is dropped (`NetClient::drop` joins its background
  thread first, so nothing races a reset against an in-flight poll), `phase`
  returns to `LocalOnly`.
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
afterward_starts_clean` (`sim.rs`) is written to prove that, not just that
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
- **Pause menu layout and input** — `crates/lodestone-shell/src/menu/nav.rs`
  (`key_paused`, `PauseButton`).
- **Rendering** — `crates/lodestone-shell/src/menu/render.rs` (`owns_frame`,
  `pause_frame`, `render_overlay` vs `render`). If you add a new screen that
  should overlay the world instead of replacing it, follow `Paused`'s
  pattern — a second render entry point with `LoadOp::Load`, kept out of
  `owns_frame` — rather than adding a special case inside the `Clear` path.
- **Teardown** — `Sim::end_session` in `crates/lodestone-shell/src/sim.rs`.
  Adding new per-session state anywhere in `Sim`? Check whether it needs a
  line here — the "what it keeps" list is short and deliberate, so anything
  not explicitly listed as kept should be assumed to need resetting, and the
  acceptance test above is the way to prove it actually is.
- **Wiring**: `App::apply_menu_action`, `crates/lodestone-shell/src/app.rs`,
  is the only call site of `end_session` — driven by `MenuAction::QuitToTitle`
  from `apply_menu_action`'s dispatch over what `nav.key`/mouse-click
  handling returns.

## Configuration

None of its own — no flags gate whether the pause menu exists or how it
renders.

## Dependencies

- `crate::menu::{nav, render}` — the pause screen's own state and layout.
- `crate::net::NetClient` — what `end_session` drops.
- `lodestone-game::{mining::Mining, placement::Placement}` — the prediction
  state `end_session` replaces wholesale rather than resetting in place.
- [`swimming.md`](./swimming.md) — the sprint edge trackers `end_session`
  also resets, for the same "next session starts clean" reason.
- [`main-menu.md`](./main-menu.md) — the screen this session's teardown
  hands control back to.

## Tests

Hermetic, `crates/lodestone-shell/src/sim.rs`:
`end_session_tears_down_and_a_fresh_connect_afterward_starts_clean` (the
acceptance test above). `crates/lodestone-shell/src/menu.rs`:
`quit_to_title_only_leaves_from_pause_and_clears_session_state`.
`crates/lodestone-shell/src/menu/nav.rs`:
`quit_to_title_from_the_pause_menu_leaves_for_the_main_menu`. 
`crates/lodestone-shell/src/menu/render.rs`:
`owns_frame_excludes_paused_so_the_pause_menu_never_replaces_the_world`,
`pause_frame_builds_the_three_buttons_in_order_and_tracks_the_highlight`.
