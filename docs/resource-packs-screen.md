# Resource Packs screen

## What it is

`SettingsPage::ResourcePacks` (issue #415): vanilla's `PackSelectionScreen`,
reached from the root settings grid's own "Resource Packs..." button
(flipped from `no_screen` to `nav()`). Landed as a **deliberately reduced**
selection list rather than vanilla's own drag-between-two-lists screen —
this client's asset-loading layer has no `PackRepository` analogue at all,
and #415 itself invited "land a simpler selection list and declare the
divergence" as the honest alternative to leaving it unbuilt.

## How it works

- `crates/lodestone-shell/src/menu/packs.rs` — the whole model: `PackEntry`
  (id/title/source), `AVAILABLE_PACKS` (always empty), `SELECTED_PACKS`
  (always exactly one entry — this client's own built-in assets),
  `PacksNav` (a plain cursor, no scroll, no search), and `frame`.
- `crates/lodestone-shell/src/menu/options.rs` — `SettingsPage::ResourcePacks`,
  the `SettingsNav` plumbing, `settings_frame`'s branch — same shape as
  every other non-`OptionsList` page in this tree.
- `crates/lodestone-shell/src/menu/nav.rs` — `MenuNav::key_packs`,
  `apply_packs`, and the hover/click/key routing guards.
- `crates/lodestone-shell/src/menu/render.rs` — `Origin::Packs`. The footer
  (Open Pack Folder, Done) reuses `Origin::Settings(Placement::Footer)`
  directly — vanilla's own footer for this screen is geometrically
  identical to `SettingsPage::Accessibility`'s.

## What is deliberately not built, and why each cut is safe

- **`AVAILABLE_PACKS` is always empty.** This client discovers no external
  packs — there is no packs directory, no filesystem watcher, no zip/
  directory pack detector. `/usr/bin/grep -rn 'PackRepository\|
  resourcepacks\|pack\.mcmeta' crates/lodestone-shell/ crates/lodestone-assets/`
  outside this module finds nothing.
- **No transfer controls** (vanilla's select/unselect/move-up/move-down
  sprite buttons, or real drag gestures). With `AVAILABLE_PACKS` permanently
  empty and `SELECTED_PACKS`'s one entry never removable by construction,
  there is nothing for a transfer control to do — building the icon/sprite
  mechanism now would be inactive chrome with no state it could ever
  change.
- **No search box**, unlike the Language screen's. Language kept one
  because filtering its one real entry is still a real predicate reachable
  by typing. Here the combined real content across *both* lists is the
  same one entry, and duplicating the `EditBox` + focus + `MenuKey::Char`
  wiring to filter a single always-present row is disproportionate to what
  it would buy.
- **No drag-and-drop-file hint text.** Vanilla's own `pack.dropInfo` text
  ("Drag and drop files into this window to add packs") is omitted rather
  than drawn with no file-drop handling behind it — showing it would be
  exactly the "vanilla's labels without vanilla's function" trap.

## Wired vs. decorative

- **Wired**: reaching the screen and back (Escape/Done → Root), viewing
  both lists' real (if minimal) content, cursor navigation.
- **Present-and-inactive**: **Open Pack Folder** — there is no packs
  directory to open.
- **Correctly absent, not decorative**: everything in "What is deliberately
  not built" above.

## Configuration

None — this screen has no persisted state of its own.

## Dependencies

- `super::options` — `SUB_HEADER_HEIGHT`, `FOOTER_HEIGHT`, `footer_rects`,
  `Placement::Footer`, `title_y` (page-independent for any non-`Root`
  argument).
- The 26.2 jar's `assets/minecraft/lang/en_us.json`
  (`resourcePack.title`, `pack.available.title`, `pack.selected.title`,
  `pack.openFolder`, `pack.nameAndSource`, `resourcePack.vanilla.name`,
  `pack.source.builtin`, `gui.done`) and
  `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/packs/
  {PackSelectionScreen,TransferableSelectionList}.java` for the geometry
  this screen does keep.

## See also

- [Language screen](./language-screen.md) — the precedent this screen's
  "one real entry, decorative where the effect does not exist" reduction
  follows, one step further.
- [Telemetry screen](./telemetry-screen.md) — the sibling screen issue #415
  built alongside this one.
- [The settings tree](./settings-screen.md) — the root page this screen is
  reached from, and the census this page's nav button moves.
