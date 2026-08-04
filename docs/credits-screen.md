# The credits / end-poem screen

## What it is

`Screen::Credits` (issue #192): the screen reached after vanilla's dragon
fight, on exiting the End through the exit portal — vanilla's `WinScreen`.
Reachable through [`UiState::show_credits`](../crates/lodestone-shell/src/menu.rs),
dismissed by Enter, Escape, or its own Done button, all of which leave through
[`UiState::quit_to_title`](../crates/lodestone-shell/src/menu.rs) — the same
teardown the pause menu's Disconnect and the death screen's Title Screen
button already use, not a fourth copy of "clear session state and go to the
title".

## How it works

Three pieces, one per file, following this tree's usual split:

- `menu.rs` — the `Screen::Credits` variant, `UiState::show_credits` (valid
  from the same live-gameplay screens as `UiState::die`), and
  `quit_to_title`/`on_escape` extended to handle it.
- `menu/nav.rs` — `MenuNav::key_credits`: one control (Done), no cursor to
  move. Enter and Escape both dismiss. There is no `click` arm for this
  screen — it falls through `MenuNav::click`'s generic "hover (a no-op with
  one row) then `key(Enter)`" path, which is enough because the screen has
  nothing a hover needs to move.
- `menu/render.rs` — `credits_frame()`, added to `owns_frame` and
  `frame_for`'s match. Same shape as `error_frame`: one full-width button at
  `Origin::ScreenBottom`, a title at `Origin::ScreenTop`, a body via
  `MenuNotice`.

### What this deliberately does not do

Vanilla's `WinScreen` auto-scrolls roughly 1500 words of the real end poem,
followed by a real Mojang employee credits roll, driven by an elapsed-time
tick every frame, and dismisses on **any** keypress. This screen does neither,
for two separate reasons — see `credits_frame`'s own module comment in
`render.rs` for the full version:

1. **No time source reaches this pipeline.** `render::frame_for` is a pure
   function of `UiState`/`MenuNav` with no elapsed-time parameter. The one
   other timed effect in this menu (the title screen's panorama background)
   is advanced from *outside* this frame-building code, by whatever owns the
   render loop each real frame — wiring a tick in here would need a new
   per-frame call from `app.rs`, which is a queued patch, not something this
   change makes free.
2. **The content is not this project's to reproduce.** The real end poem is a
   copyrighted creative work (Julian Gough's text, commissioned by Mojang),
   and the real credits roll names actual Mojang employees. Neither belongs
   in this repository regardless of the time-source question above.

So the screen shows a short, Lodestone-authored placeholder instead — proving
the screen and its teardown mechanism without inventing scroll geometry that
has nothing to drive it, or copying text that is not Lodestone's.

## Wired vs. decorative

- **Wired**: reaching the screen, dismissing it by Enter/Escape/Done, and the
  teardown (`quit_to_title` clears the session exactly as it already does
  from Paused/Death).
- **Wired since `86fbe0a` — the trigger itself.** This section used to say
  "nothing calls `show_credits` anywhere in the shell today" and that the
  screen was reachable only from a test. `sim.rs`'s `Sim::poll_net` now
  latches `NetUpdate::WinGame` into a plain `won` field (the same shape as
  `death_message`), reset by `Sim::end_session`; `net.rs`'s `forward`
  translates the real decoded `ClientEvent::WinGame` into that
  `NetUpdate::WinGame`; and `app.rs`'s `drive_ui_from_session` reconciles
  `Sim::has_won()` into `UiState::show_credits()` every frame, guarded on
  `screen() != Screen::Credits` so it does not re-latch while the screen is
  up. Proved end to end by `app.rs`'s
  `drive_ui_from_session_opens_credits_on_the_real_win_game_event`: a real
  `WindowApp`, a real `NetUpdate::WinGame` fed through the loopback net
  client, `Sim::poll_net`'s real arm, then the real `drive_ui_from_session`
  call — asserting the screen actually becomes `Credits`, not a direct call
  to `show_credits()`.

## How to change it

- The placeholder text lives in `render.rs`'s `CREDITS_TITLE`/`CREDITS_BODY`
  constants. If a real jar-asset extraction pipeline for `texts/end.txt`
  (and a real, licensed credits list) ever lands — the same way GUI sprites,
  sounds and `en_us.json` strings are already loaded from the user's own
  legitimately-owned game files rather than transliterated into source — this
  is the function to point at it. Nothing about the screen's navigation or
  teardown depends on the text being a placeholder.
- A real auto-scroll needs a `MenuNav` field carrying scroll position plus a
  per-frame `advance(dt)` call from `app.rs`, mirroring `panorama.rs`'s
  `pano.advance(Instant::now())`. Not built here — see "What this
  deliberately does not do" above.
- **The trigger itself is wired** — see "Wired since `86fbe0a`" above; there
  is no queued patch left for this part. The chain is
  `ClientEvent::WinGame` (decoded from vanilla's real `WIN_GAME` game event,
  code `4` — `lodestone-model`'s `event.rs`) → `net.rs`'s `forward` →
  `NetUpdate::WinGame` → `Sim::poll_net`'s `won` field → `app.rs`'s
  `drive_ui_from_session` → `UiState::show_credits()`.

## Configuration

None — this screen has no persisted state.

## Dependencies

- `menu/render.rs` — `Origin::ScreenBottom`/`Origin::ScreenTop`, `MenuNotice`,
  the same primitives `error_frame` uses.
- `menu/nav.rs` — `MenuAction::QuitToTitle`, reused rather than a new
  variant.

## See also

- [Menu UI framework](./ui-framework.md) — the wider screen-framework plan
  (this screen is a small, self-contained addition to it, not part of the
  `OptionsList` census).
- [Pause menu](./pause-menu.md) — `quit_to_title`'s other two callers.
