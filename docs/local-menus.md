# Plugin-opened local menus

## What it is

`Bukkit.createInventory` + `Player.openInventory` for this codebase (issue
[#145](https://github.com/matteopolak/lodestone/issues/145)): a plugin opens an arbitrary
container screen to the local player with **no server container behind it** — the mechanism
behind shops, kit selectors and client-side settings screens in the Java ecosystem.

Scoped to the **client-side** shape, as the issue body directs. A server-side plugin opening
a menu to a remote player has to go through the real container-open packet family, which
needs `lodestone-server`'s container protocol support to exist first.

## How it works

`Menus::open_local(menu, menu_type, title)`
(`crates/lodestone-game/src/menus.rs`) fills the same `opened` slot a server-opened
container fills, so the screen draws through **exactly** the existing path —
`Sim::open_menu` → `ContainerFrame` → `ContainerGeometry`. No second renderer, no new
draw call. The only difference is one private `bool`.

| piece | what it is for |
|---|---|
| `OpenMenu::local` | the authority for "nothing about this may reach the wire" |
| `LOCAL_MENU_WINDOW_ID` (`i32::MIN`) | the window id a local menu carries |
| `Menus::opened_is_local()` | the predicate every wire-facing path consults |
| `Menus::close_local()` | closes **only** a local menu |
| `Sim::{open_local_menu, close_local_menu, click_local_menu, open_menu_is_local}` | the shell-side surface |

Two wire-facing call sites consult it, and both had to change:

- `Sim::close_open_menu` (`sim/session.rs`) sent `ContainerClose` **unconditionally**. That
  unconditional send is what made the old synthetic-event workaround a correctness bug
  rather than a cosmetic one.
- `WindowApp::send_menu_click` (`app/container_input.rs`) now predicts locally and sends
  nothing. The local check comes **before** the connection check on purpose: a local menu
  must work with no connection at all, so bailing on `net()` first would make plugin menus
  dead at the title screen.

## Why the window id is `i32::MIN`

A local menu has no server-side container, so its id must be one a server can never
legitimately allocate *and* one that is obviously wrong if it ever escapes onto the wire.
Vanilla window ids are small positives (`0` is the player's own inventory). An unused small
negative would have been indistinguishable from a protocol quirk in a packet log; `i32::MIN`
cannot be mistaken for anything.

**Consumers must not branch on the id.** Ask `opened_is_local()` — the `bool` is the
authority and the id is a belt to that braces. `menu_for_mut` checks both, so a
server-sourced packet cannot write into a plugin's screen even if one somehow arrived
carrying that id.

## What the old route could not do

Before this, the only way to get a synthetic menu on screen was to push
`ScreenOpened` + `ContainerContent` through `IngestQueue`. It draws, and it is wrong in
three ways that are each invisible until they bite — all three pinned by tests in
`crates/lodestone-game/tests/local_menu.rs`:

1. **`ScreenOpened` alone opens nothing.** The menu is not built until a
   `ContainerContent` arrives, because the container's *size* comes from that packet's
   item count minus 36. A plugin pushing only the open event gets no screen and no error.
2. **A plugin could not supply a pre-built `Menu`.** The content packet's length sizes the
   menu and `build_menu` re-derives the **layout** from the menu-type key, so a plugin's own
   key silently became `Menu::generic` — losing both its stock and any `SpecialLayout` it
   wanted.
3. **It was indistinguishable from a server open in every downstream consumer**, so
   `ContainerClose` and every `ContainerClick` went to the real server naming a window it
   had never heard of.

## How to change it

- **The one-inventory invariant applies to local menus too** (issue #373). `open_local`
  calls `reclaim_inventory` before replacing whatever was open and `hand_inventory_to_opened`
  after, exactly as `ensure_open` does. Skip either and the hotbar goes blank while a plugin
  screen is up.
- **A pending server open is deliberately not cleared** by `open_local`. If its content
  packet arrives it legitimately supersedes the local menu; dropping the pending would
  strand that screen with unknown metadata forever.
- **A server open supersedes a local menu, and that is correct.** `ensure_open` sets
  `local: false`, having already reclaimed the inventory.
- **`Sim::open_menu` is four chained `?`s.** `open_local` must populate `menu_type` *and*
  `title` or the screen silently does not draw — while the player's inventory has already
  moved into it. That is the worst possible failure (invisible screen, husked hotbar) and is
  why the first test asserts all four accessors rather than just `opened()`.

## Configuration

None. Works with or without a live session.

## What is verified, and the controls

9 tests in `crates/lodestone-game/tests/local_menu.rs`. Controls run and observed:

| control | asserts |
|---|---|
| `local_is_true_for_a_plugin_menu_and_false_for_a_server_container` | the predicate is not a function that always returns `true` |
| `close_local_refuses_to_close_a_server_container` | a plugin cannot close a real container behind the player's back |
| `a_server_slot_write_cannot_reach_a_plugin_menu` | the `!local` guard in `menu_for_mut` |
| `control_screen_opened_alone_opens_nothing` | trap 1 of the old route |
| `the_synthetic_event_route_cannot_supply_a_prebuilt_menu` | traps 2 and 3 of the old route |

The last two are the interesting ones: they pin *why the old route was insufficient* rather
than only asserting the new one works.

## Dependencies

`lodestone-game`'s `Menu`/`Menus`/`ClientMenu`; the shell's existing container renderer.
No protocol crate — a local menu has no wire representation by construction.

## Known gaps

- **The server-side shape is not built.** A plugin opening a menu to a *remote* player needs
  the real container-open packet family plus click events coming back, against a
  `lodestone-server` whose container protocol barely exists. Issue #145's body files this as
  a follow-up and that split still holds.
- **`click_local_menu` hardcodes `PlayerCtx::survival()`**, matching the pre-existing
  `send_menu_click` behaviour and for the same reason: `Sim` has no game-mode accessor to
  source a real one from. A creative-mode plugin menu therefore gets survival click
  semantics.
- **Only one menu can be open at a time**, local or otherwise, because `Menus::opened` is a
  single `Option`. `OpenMenu::local` is a `bool` rather than being derived from the window id
  partly so a future multi-local-menu change has somewhere to put a real id.
