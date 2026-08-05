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
- **Wired since — the online-player list itself.** This section used to say
  "nothing calls `entries_from_tablist` in production yet"; that queued patch
  has landed. `app.rs`'s per-frame update now calls
  `crate::menu::social::entries_from_tablist(&tab_list,
  self.sim.local_uuid())` and feeds the result to `MenuNav::refresh_social`
  (`menu/nav.rs::MenuNav::refresh_social`), so the roster shown is the real, live tab list, local
  player excluded, every frame.
- **Decorative — hiding a player has no consumer yet, and it is a larger gap
  than it looks.** This module only *records* the choice (`HiddenPlayers`);
  nothing reads it back to suppress a hidden player's chat. The obvious next
  step — a `chat.rs` change consulting `HiddenPlayers::load()` — turns out not
  to be enough on its own: **the received chat pipeline carries no sender
  identity to filter on.** `crate::chat`'s own module doc says the received
  log is not even here (it moved to `lodestone_game::chat::ChatLog`), and that
  log's `push_player`/`ChatLog::push_player` take only a pre-decorated `Text`
  and a `MessageTrust` — no UUID, no name field to match against
  `HiddenPlayers::contains(id: Uuid)`. Traced to the wire: `lodestone_model`'s
  `ClientEvent::Chat { text, kind, ack }` has never carried a sender either.
  For the one clientbound chat format that actually has one on the wire —
  26.2's real signed `ClientboundPlayerChatPacket` — the sender UUID **is**
  decoded and then thrown away: `crates/protocol/v770/src/adapter.rs`'s
  `PLAYER_CHAT` branch reads it into a binding named `_sender` and never uses
  it. `DISGUISED_CHAT`/`SYSTEM_CHAT` never had one on the wire to begin with
  (the server pre-decorates the display name into the text), and the legacy
  `v340`/`v735` chat packets carry no sender field at all. So today, toggling
  Hide persists correctly and changes nothing else on screen — an honestly
  declared island half, not an implied feature — and closing it is **not** a
  `chat.rs`-only change: it needs a `sender: Option<Uuid>` field threaded onto
  `ClientEvent::Chat` (`lodestone-model`), populated at four/five adapter call
  sites (`lodestone_model`/`protocol/*` are off limits to this batch), then
  carried through `NetUpdate::Chat` (`net.rs`) and `sim/net_apply.rs`'s
  `Chat` arm (both brokered for this batch) before `chat.rs` ever gets a
  chance to consult `HiddenPlayers`. Scope is mechanically small — mostly
  one-line `None`/`Some(sender)` additions across roughly fifteen to twenty
  files — but every file it touches is outside this batch's file ownership,
  so it is a real follow-up issue rather than "a real, small win" inside
  `chat.rs` alone, and it should be filed as one instead of attempted here.

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

- **The roster refresh already landed** — see "Wired since" above; there is
  no queued patch left for this part.
- **Wiring the Report button** needs secure chat signing to exist first
  (`ChatSession`/message signatures) — not a menu-side change at all once
  that lands; `SocialControl::is_live` is the one place to flip.
- **Wiring Hide/Show to actually suppress chat** needs the sender-identity
  plumbing described above *first*: a `sender: Option<Uuid>` field on
  `lodestone_model::ClientEvent::Chat`, populated from the one decode site
  that already has it (`protocol/v770/src/adapter.rs`'s `PLAYER_CHAT` arm)
  and `None` everywhere else, carried through `NetUpdate::Chat` and
  `sim/net_apply.rs`'s `Chat` arm. Only once a sender reaches `chat.rs` does
  reading `crate::config::HiddenPlayers::load()` there become a real, small
  change rather than a change with nothing to key on.
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
