# Menu screens

## What it is

A catalogue of every individual non-container menu screen in the shell: what each one is, where its
code lives, and what makes it distinctive. See [`ui-framework.md`](./ui-framework.md) for the shared
widget/layout/focus machinery these screens are built from, and
[`container-screens.md`](./container-screens.md) for the inventory-and-menu family, which follows a
different geometry model (slot rects from the *menu* classes, not layout containers).

## How it works

Every screen is a variant of the `Screen` enum in `crates/lodestone-shell/src/menu.rs`, driven by
`UiState` (which screen is open, and the legal transitions between screens) and `MenuNav` (per-screen
input state — cursor, focus, scroll offsets, form fields). A screen's pixels come from
`render::frame_for`, the single shared path that builds a `MenuFrame` from `UiState`/`MenuNav` — a
screen that assembles its own draw calls outside this path is a bug, not a valid shortcut, because
every other subsystem (hit-testing, focus navigation, canvas-scale handling) reads geometry back out
of the same `frame_for` output.

Screens fall into two draw shapes:

- **Full-frame** screens (`owns_frame` lists them) replace the entire draw — the title screen, world
  select, settings. Used when there's no live world behind the screen, or when covering it is correct
  (a full-frame connect screen while nothing is streaming yet).
- **Overlay** screens draw over a still-rendering, still-ticking world — the pause menu, chat, death,
  container screens, the command block editor, the post-login loading screen. These freeze gameplay
  input and release the pointer, but the world keeps meshing, uploading chunks and ticking behind the
  drawn UI. Getting this distinction backwards either freezes world updates that should continue (an
  overlay mistakenly built as full-frame) or lets stale gameplay input leak through a screen that should
  have captured it.

Several screens share infrastructure worth knowing about up front:

- `HeaderAndFooterLayout` (from the shared layout containers) is used by any screen with a title band,
  a scrolling content band and a footer button row — world select and the server list are the two
  canonical consumers.
- A shared tab-bar widget backs both the World Creation screen's three tabs and the Statistics
  screen's three tabs — it is one widget with two consumers, not two implementations.
- Text-entry screens (world creation, world select's search, the accounts sign-in flow, the command
  block editor) use the same `EditBox`/focus machinery documented in `ui-framework.md`, not a
  screen-specific input hack.

## Screens

### Main menu

`Screen::MainMenu` — the title screen: Singleplayer / Multiplayer / Quit, plus icon buttons for
Friends (disabled — 26.2 ships a Friends service, but Lodestone's integration is not implemented), Language, Accessibility, and a
Minecraft Realms row (disabled). Its layout is vanilla's `TitleScreen` reproduced exactly, drawn with
the resource pack's own button art. Lives under `crates/lodestone-shell/src/menu/`: `menu.rs` for the
`Screen`/`UiState` state machine, `menu/nav.rs` for input, `menu/render.rs` for layout and draw,
`menu/servers.rs` for the persisted server list, `menu/status.rs` for background server status pings,
`menu/accounts.rs` for the account list and sign-in flow.

### Pause menu

`Screen::Paused` — the in-game Escape menu, paired with `Sim::end_session` (the teardown that lets a
player leave a session cleanly and start or join another). Its layout is vanilla's `PauseScreen`
`GridLayout` reproduced exactly: ten widgets while the hosted world is unpublished, nine once it is
published (Open to LAN disappears). Advancements and Statistics buttons are live; Report Bugs and Give
Feedback are present-and-disabled. A conditional Server Links row draws outside the grid entirely when
present.

### Local (plugin-opened) menus

Not a `Screen` variant at all — a mechanism (`Menus::open_local` in
`crates/lodestone-game/src/menus.rs`) that lets a plugin open an arbitrary container screen to the
local player with **no server container behind it**, the client-side half of the shop/kit-selector
pattern from the Java plugin ecosystem. It reuses the existing container draw path exactly (no second
renderer); the only distinguishing fact is `LOCAL_MENU_WINDOW_ID` (`i32::MIN`), a window id no server
could ever legitimately allocate, which marks a menu as having nothing to send over the wire. A
server-side plugin opening a menu to a *remote* player is out of scope here — it needs the real
container-open packet family, which needs `lodestone-server`'s container protocol support first.

### Settings

`Screen::Settings` — vanilla's `OptionsScreen` tree as a table plus arithmetic in
`crates/lodestone-shell/src/menu/options.rs`: nine `OptionsList` pages with roughly 140 individual
controls, most of them present-and-disabled (no corresponding persisted option exists yet), plus four
sub-screens that use a different list widget entirely and aren't part of that count — Key Binds,
Language, Telemetry, and Resource Packs. Reached from both the title screen's and the pause menu's
Options button; the root grid's "World Options" cell is a live link to online-play options outside a
world and an inactive placeholder inside one, since a dedicated world-options screen doesn't exist yet.

### World creation

`Screen::CreateWorld` — vanilla's `CreateWorldScreen`, reached from the world list's "Create New
World" button. Collects a world name, seed, world type, game mode, difficulty, three toggles (generate
structures, bonus chest, allow cheats) and online mode, arranged across vanilla's own three tabs
(Game/World/More) using the shared tab-bar widget also used by the Statistics screen. Its model lives
in `menu/create_world.rs`.

Four sub-editors reach off the main tabs, each a fixed-row screen (no scan, no scrollbar): Game Rules
and Data Packs (More tab), Experiments (More tab, three fixed feature-flag toggles), and Customize
(World tab, present but only active while World Type is Flat or Single Biome — cycles a bundled
quick-preset layer stack or a curated fixed biome list). Experiments and Customize both reach real
disk: `crate::saves::create_world_in` writes the chosen feature flags into the new world's `level.dat`
(`lodestone_anvil::level_dat::LevelDat::with_enabled_features`) and, for a customized Flat/Single Biome
world, writes the chosen generator straight into `world_gen_settings.dat`'s
`dimensions.minecraft:overworld.generator` compound alongside a real, resolved seed
(`lodestone_anvil::world_gen_settings::WorldGenSettings::with_overworld_flat_generator`/
`with_overworld_fixed_biome_generator`) — before the server ever opens the directory, since that file's
own lazy-create-on-first-open path errors if it already exists with no seed field. Neither choice is
yet read back by this client's own world-generation launch path, so a freshly created world still
generates the same way it always has from *this* client; only a real vanilla server re-opening the save
folder would see either customization. Game Rules is the one sub-editor with a network effect: its diff
is sent as `SetGameRules` once the session reaches Play. Data Packs is collected but has no consumer at
all (no data-pack loader in this crate yet).

### World select

`Screen::WorldSelect` — vanilla's `SelectWorldScreen`, reached from the title screen's Singleplayer
button: a title, a search field, a scrolling list with one row per world under `saves/`, and six
footer buttons (Play Selected World, Create New World, Edit, Delete, Re-Create, Back). Edit and
Re-Create are present-and-disabled (no screen exists for either yet); Delete is live and opens the
[confirmation screen](#confirmation). This was the first consumer of the shared
`HeaderAndFooterLayout` container. `crates/lodestone-shell/src/saves.rs` is the on-disk save
enumeration this screen reads from — read its module doc before touching world discovery or naming.

### Server list

`Screen::ServerList` — vanilla's `JoinMultiplayerScreen` plus its `ServerSelectionList`, at vanilla's
geometry: a `HeaderAndFooterLayout` title, 36px list rows with a 32×32 favicon, wrapped MOTD and a
status column, and seven footer buttons (three inactive with nothing selected). The persisted list
(`menu/servers.rs`) and the async status pinger (`menu/status.rs`) are older and unchanged in
substance — this screen is a presentation-and-interaction fidelity pass over them, including the
favicon's own click/hover behavior.

### Accounts

`Screen::Accounts` — the account switcher and its device-code sign-in sub-flow, presented with the
same chrome as the server list (a `HeaderAndFooterLayout`, 36px list rows, nine-slice footer buttons).
**There is no accounts screen in vanilla** — Minecraft's launcher picks an account outside the game —
so this screen's geometry is modeled on this repo's own server-list port rather than transcribed from
a jar. The account/sign-in state machine lives in `menu/accounts.rs`; error and status text goes
through `MenuNotice`, a wrapping/clipping primitive that exists specifically because raw sign-in error
strings (service errors, OAuth URLs, keychain errors) can be arbitrarily long and contain no
whitespace to wrap on.

### Resource packs

`SettingsPage::ResourcePacks` — vanilla's `PackSelectionScreen`: two transferable columns over a real
pack repository. Available (left) lists every pack under `resourcepacks/` — directories and `.zip`
archives — with its `pack.mcmeta` description and `pack.png` thumbnail; Selected (right) is the active
priority order, highest first. Clicking a row moves it between columns, per-row buttons reorder it,
and leaving the screen feeds the new order into `ResourceManager`'s pack stack, which a live world
session picks up within a frame or two. A server-pushed pack appears pinned at the top of Selected for
the lifetime of that push — force-enabled, with no transfer/reorder controls, and deliberately excluded
from the persisted local order. `crates/lodestone-shell/src/resources.rs` is where pack discovery and
the pack stack actually live; `menu/packs.rs` is the screen's own column/cursor/scroll model.

### Language

`SettingsPage::Language` — vanilla's `LanguageSelectScreen`, the first screen in this tree to need a
different list widget (vanilla's `ObjectSelectionList`) than the settings tree's own `OptionsList`.
Reachable two ways — from the settings root grid's "Language..." button, and directly from the title
screen's own Language icon, matching vanilla's two entry points — and the two paths leave different
navigation-stack shapes behind them (Escape from the icon path returns straight to the title; Escape
from the grid path surfaces the settings root first). The language list has exactly one entry
(`en_us`) because this client parses no `languages.json` and loads only the one bundled language
table — a real, if minimal, instance of the list mechanism rather than a stub.

### Credits

`Screen::Credits` — reached after exiting the End through the exit portal, vanilla's `WinScreen`
equivalent. Dismissed by Enter, Escape, or its own Done button, all routing through
`UiState::quit_to_title` (the same teardown the pause menu's Disconnect and the death screen's Title
Screen button use). Deliberately does **not** reproduce vanilla's auto-scrolling end poem and Mojang
credits roll: `render::frame_for` has no elapsed-time input to drive a scroll from, and the real poem
and credits are copyrighted text that doesn't belong in this repository regardless. It shows a short,
project-authored placeholder instead, which is enough to prove the screen and its teardown path exist.

### Telemetry data

`SettingsPage::Telemetry` — vanilla's `TelemetryInfoScreen`, built as an honest prose screen: a title,
a two-paragraph description, and four buttons, two of them real (Privacy Statement and Give Feedback,
which open real vanilla URLs in the system browser). View My Data is present-and-inactive. This client
collects no telemetry at all — no event log, no opt-in state anywhere in the workspace — so the
opt-in checkbox and the live pending-events widget vanilla shows are correctly *absent*, not reduced:
vanilla itself omits them whenever there's nothing to opt into, and this client is permanently on that
branch.

### Confirmation

`Screen::Confirm` — vanilla's `ConfirmScreen`: a question, a warning naming the thing at risk, and two
buttons. It's the gate any irreversible action passes through; today the only caller is the world
list's Delete button. It exists as a separate screen rather than a "press Delete twice" pattern
specifically because a double-press is indistinguishable from an accidental double-click — an
unshippable shape for an operation with no undo. Safety comes from two independent properties, both
reproduced from vanilla rather than invented: the affirmative button sits on a different screen at a
non-overlapping rect from the original Delete button (so a stray second click cannot land on it), and
nothing is focused when the screen opens (so a held Enter cannot roll straight through into a
deletion).

### Death

`Screen::Death` — vanilla's `DeathScreen`: "You Died!", the server's death message, a score line, and
Respawn / Title Screen buttons. Draws as an overlay over the still-rendering, still-ticking world, the
same way the pause menu does. Reachable from any live gameplay screen the instant a death packet
lands, matching vanilla's behavior of replacing whatever screen is open. The real behavior change
underneath this screen is that the client now uses a manual respawn policy instead of automatic: before
this, a death packet triggered an unconditional respawn request with no screen and no player choice in
between; now nothing sends a respawn until the Respawn button is pressed.

The death message carries the server's component through unresolved as far as the point where the
session applies the update (`NetUpdate::Death::message` is a `Text`, not a pre-flattened string), which
is the first point downstream holding a language table; from there it is carried as styled, interactive
runs, so a translation key, a click event and a hover payload all survive to pixels rather than
flattening to plain text. The screen renders each run at its own colour rather than one flat line.
Hover shows a `show_text` tooltip, resolved against the live language table at draw time — there is no
per-frame cursor tracker to maintain, because the pointer position this screen needs is already recorded
on every mouse-move regardless of which screen is open. Click is not yet wired to input: the run under
the pointer and its `open_url`-restricted click action (vanilla's own restriction — `run_command`/
`suggest_command`/`copy_to_clipboard` are inert on this screen in vanilla too, not merely unwired) are
both available, but nothing yet calls them from a mouse-button handler.
regardless of which screen is open.

### Loading

Two different mechanisms depending on when it applies, not one screen. Before login, `Screen::Connecting`
is a full-frame screen (nothing is streaming yet, so nothing needs to keep rendering behind it) showing
named connection phases. After login, the loading UI becomes an **overlay** drawn over the
still-rendering world while terrain is loading, because chunks must keep meshing and uploading behind
the text — a full-frame screen would stop that. It clears based on whether the player's own standing
chunk has actually arrived, not when a progress bar visually fills. Both stages use the same panorama
backdrop every other menu screen uses (see `ui-framework.md`) rather than a flat color wash — no
vanilla loading screen ever shows a flat fill, even over a live level.

### Advancements

`Screen::Advancements` — vanilla's `AdvancementsScreen`, reached from the pause menu: five tabs, the
real 26.2 advancement tree, connector lines, frames, icons, a tiled per-tab background, panning, and
hover tooltips. The tree's *shape* comes from the data pack; its *progress* comes from the wire, so
completed advancements draw their real obtained-frame art. The load-bearing fact here: 26.2's
advancement JSON carries no `x`/`y` position fields at all — those are computed server-side by
vanilla's tidy-tree layout algorithm and only ever appear on the wire — so the client has to run that
same layout algorithm itself (`menu/advancement_tree.rs`) rather than reading a position from disk.

### Statistics

`Screen::Statistics` — vanilla's `StatsScreen`, reached from the pause menu. Only the General tab
(vanilla's 77 fixed stats — time played, distance, damage, counters) is a real scrollable list; Items
and Mobs tabs are present-and-inactive. The server answers a stats request with a real snapshot, which
the client decodes and folds into `lodestone_ecs::SessionStatistics`, so the numbers shown are real
rather than placeholder zeros. Its tab bar is the same shared tab-bar widget the World Creation screen
uses.

### Command block edit

`Screen::CommandBlockEdit` — vanilla's `CommandBlockEditScreen`: a command text field (a real
`EditBox`), tab-completion, a read-only Previous Output line, a Track Output toggle, and
Mode/Conditional/Needs Redstone toggles for the block variants. Drawn as an overlay, same shape as
chat and container screens — the pointer is released and gameplay input is frozen, but the world keeps
rendering and ticking behind it. Its tab-completion reuses the chat box's own suggestion walker rather
than duplicating it: since chat's parser only recognises lines starting with `/` but a command
block's text never has a leading slash, the command-block code prepends a synthetic `/` before calling
into the shared walker and shifts the resulting spans back afterward.

The editor resolves the targeted chunk's raw state number into `StateId` once before asking
`command_block_source` for its mode and conditional property. Out-of-census values cannot open the
editor; a valid state that is not one of the three command-block variants also cannot open it. Once
validated, the source helpers use total state-name/property access, so `None` means only
"not a command block", never an invalid generated-table index.

## Dependencies

- `crates/lodestone-shell/src/menu.rs` — the `Screen` enum and `UiState` state machine every screen
  above is a variant or sub-mode of.
- `crates/lodestone-shell/src/menu/{nav,render}.rs` — input handling and the shared `frame_for` draw
  path.
- Per-screen modules under `crates/lodestone-shell/src/menu/` — `servers.rs`, `status.rs`,
  `accounts.rs`, `options.rs`, `create_world.rs`, `world_select.rs`, `packs.rs`, `language.rs`,
  `telemetry.rs`, `confirm.rs`, `command_block.rs`, `advancement_data.rs`, `advancement_tree.rs`,
  `advancements.rs`.
- `crates/lodestone-shell/src/saves.rs` — world enumeration and creation for World Select and World
  Creation.
- `crates/lodestone-shell/src/resources.rs` — pack discovery and the pack stack for Resource Packs.
- The 26.2 jar under `.cache/mc/26.2/{client-src,client.jar}` — behavioral reference only, never
  transliterated.
- [`ui-framework.md`](./ui-framework.md) — the widget, layout, focus and overlay machinery every
  screen here is built from.
- [`container-screens.md`](./container-screens.md) — the container/inventory screen family, which is
  out of scope for this catalogue.
