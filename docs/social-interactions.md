# Social Interactions screen

## What it is

`Screen::Social` (issue #189): vanilla's `SocialInteractionsScreen`, reached
from the pause menu's Player Reporting icon button
(`crates/lodestone-shell/src/menu/nav.rs`'s `PauseButton::PlayerReporting`,
now live). An online-player list with a per-player **Hide in Chat**/**Show in
Chat** toggle (vanilla's own terms — `gui.socialInteractions.hide`/`.show`,
not "mute") and a **Report** button that stays permanently inactive.

Vanilla itself only shows the real list in a multiplayer session
(`multiplayer.socialInteractions.not_available` = "Social Interactions are
only available in Multiplayer worlds") — this client's own [`SessionKind`]
already carries that fact, so [`crate::menu::social::available_for`]
reproduces the fork exactly rather than inventing a singleplayer-only
placeholder.

## How it works

- `menu/social.rs` — the whole model: [`SocialEntry`] (id + name),
  [`SocialNav`] (cursor, scroll, the roster snapshot, and the persisted
  hidden-player set), [`SocialControl`]/[`SocialPlacement`] (mirroring
  [`crate::menu::key_binds::KeyControl`]/`KeyPlacement`'s shape — a name label
  plus two right-anchored buttons per row, not `OptionsList` geometry),
  [`entries_from_tablist`] (lowers a live
  [`lodestone_game::tablist::TabList`] into the narrow `SocialEntry` shape
  this screen needs), and [`frame`] (the two-way fork: the real list, or
  vanilla's own unavailable message).
- `config.rs` — [`HiddenPlayers`]: the persisted hidden-player set, in its own
  `hidden_players.json` beside `options.json`/`servers.json`. **Not** a field
  on [`Options`] — see [`HiddenPlayers`]'s own doc: `Options` derives `Copy`
  deliberately, and a `Vec`/`BTreeSet` field would take that away from every
  call site that copies an `Options` by value.
- `menu/nav.rs` — `MenuNav::social` (the screen's live state, alongside
  `settings`/`world_select`), `key_social`/`apply_social`, and
  `PauseButton::PlayerReporting`'s `enabled()`/Enter arm.
- `menu/render.rs` — `Origin::Social`, wired into `owns_frame` and
  `frame_for`'s match.

## Wired vs. decorative

- **Wired**: reaching the screen from the pause menu and back
  (Escape/Done → Paused), the singleplayer/multiplayer availability fork
  (real — `SessionKind` is already known), per-row Hide/Show
  (`SocialNav::click_row` toggles and persists immediately through
  `HiddenPlayers::save_to`, the same eager-persistence rule
  `docs/keybindings.md` documents for rebinding), and a disconnect while the
  screen is open reaching `Screen::Error` (mirrors the death-screen gate).
- **Decorative — the Report button**: always inactive, in every session kind.
  It needs secure chat signing (a `ChatSession`/signed-message context), and
  `/usr/bin/grep -rn 'SecureChat\|ChatSession\|signed_chat'` over `crates/`
  finds nothing — issue #189's own scope explicitly says not to build a fake
  or unsigned report path, so this is the honest state until that dependency
  lands, not a stub. `PauseButton::PlayerReporting`'s doc used to be the only
  place this dependency was written down (a trap the issue itself flagged,
  since comments drift); `social`'s module docs carry it now instead.
- **Decorative — the online-player list itself, until a queued patch lands**:
  `entries_from_tablist` is pure and tested, but **nothing calls it in
  production yet.** Feeding it the live `TabList` needs a per-frame (or
  per-change) call from `app.rs` into `MenuNav::refresh_social`, and `app.rs`
  is brokered for this batch of work — see the issue's own report for the
  exact patch. Until then, the list is empty by construction on every real
  session, which is correctly-empty (no `TabList` was ever fed in) rather than
  a fabricated "no players online".
- **Decorative — hiding a player has no consumer yet either.** This module
  only *records* the choice (`HiddenPlayers`); nothing in `chat.rs`
  (off-limits for this batch of work — see the issue's file-ownership note)
  reads it back to actually suppress a hidden player's messages. So today,
  toggling Hide persists correctly and changes nothing else on screen — an
  honestly declared island half, not an implied feature. Wiring `chat.rs` to
  consult `HiddenPlayers` is the natural follow-up.

A hidden choice cannot be "wrong" the way a cycled option value can: there is
no derived state that could drift from it, and toggling it back is a single
click either way — self-healing is trivial by construction.

## What is deliberately not built

Vanilla's screen has three tabs (All/Hidden/Blocked). **Blocked** is
Microsoft-account-managed (`gui.socialInteractions.blocking_hint` = "Manage
with Microsoft account") — decorative in the same way the Online settings
page's seven controls are (no account social graph behind it), so a tab for
it would be geometry over nothing. **Hidden** is a filtered view of the same
data **All** already has. Given both, three tabs over one flat list add
geometry without proportionate value at this scope, so this screen is a
single flat list instead — a documented reduction, not a silent one.

## How to change it

- **The queued patch**: `app.rs` needs to call
  `nav.refresh_social(social::entries_from_tablist(tab_list, Some(local_uuid)))`
  wherever it already has access to the live tab list and the local player's
  UUID, on the same cadence it would refresh any other per-frame HUD state.
- **Wiring the Report button** needs secure chat signing to exist first
  (`ChatSession`/message signatures) — not a menu-side change at all once
  that lands; `SocialControl::is_live` is the one place to flip.
- **Wiring Hide/Show to actually suppress chat** is a `chat.rs` change (off
  limits for this batch): read `crate::config::HiddenPlayers::load()` (or
  thread it through the same state `chat.rs` already holds) and skip a
  message whose sender is hidden.
- **Adding the Hidden/Blocked tabs**, if a later pass decides the reduction
  above should be undone: `SocialControl`/`SocialPlacement` would need a tab
  index the way `crates/lodestone-shell/src/menu/key_binds.rs`'s categories
  do, and `frame` would filter `nav.entries()` by tab before laying out rows.

## Configuration

- `crates/lodestone-shell/src/config.rs` — `HiddenPlayers`
  (`hidden_players.json`), `hidden_players_path()`.

## Dependencies

- `lodestone-game` — `tablist::{TabList, PlayerListEntry, GameProfile}`, the
  same crate `crate::tablist`'s HUD overlay already depends on.
- `menu/options.rs` — `SUB_HEADER_HEIGHT`, `FOOTER_HEIGHT`, `LIST_TOP_INSET`,
  `WIDGET_H`, `SMALL_BUTTON_WIDTH`, `Placement::Footer` — reused for this
  screen's footer exactly the way `menu/key_binds.rs` reuses them for its own
  non-`OptionsList` screen.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
  verbatim (`gui.socialInteractions.*`, `menu.playerReporting`,
  `multiplayer.socialInteractions.not_available`).

## See also

- [Keybindings](./keybindings.md) — the eager-persistence rule this screen's
  Hide/Show toggle follows.
- [Pause menu](./pause-menu.md) — the Escape stack this screen hangs off.
