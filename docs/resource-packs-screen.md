# Resource Packs screen

## What it is

`SettingsPage::ResourcePacks` (issue #415): vanilla's `PackSelectionScreen`, two
transferable columns over a real pack repository. **Available** on the left lists
every pack in the user's `resourcepacks/` folder — directories *and* `.zip`
archives — with its `pack.mcmeta` description and `pack.png` thumbnail;
**Selected** on the right is the priority order, highest first. Clicking a row
moves it between the columns, per-row buttons reorder it, and leaving the screen
feeds the order into `ResourceManager`'s pack stack so the next atlas and model
build see it.

This landed in two passes. The first was a declared reduction (Available
permanently empty, Selected permanently one non-removable entry, no transfer
controls), honest at the time because nothing in the shell knew what a packs
*directory* was. `crates/lodestone-assets` always did the hard half — a
`ResourceSource` is a directory tree *or* a zip, and `ResourceManager` is an
ordered stack with vanilla's override semantics — it was simply only ever handed
one source.

## How it works

```
menu/packs.rs      PacksNav: two columns, cursor, per-column scroll, transfer,
                   reorder, commit
resources.rs       resource_packs_dir(), scan_resource_packs{,_in}(),
                   selected_packs()/set_selected_packs(), open_pack_stack()
config.rs          SelectedPacks — the persisted order (resource_packs.json)
lodestone-assets   ResourceManager::from_priority_order — the one reversal
```

- **`crates/lodestone-shell/src/resources.rs`** is where the whole thing becomes
  real. `scan_resource_packs_in` walks the folder, accepts a directory or a
  `.zip`, opens it as a `ResourceSource`, and reads `pack.mcmeta` +
  `pack.png` into a `DiscoveredPack` (vanilla's `FolderRepositorySource`, whose
  pack id is `"file/" + filename`). `open_pack_stack` lays the selected packs on
  top of `client.jar` — and **every** `load_*` in that module goes through it, so
  a pack overriding GUI sprites, item art, the sky, the container panels or the
  block textures is picked up by whichever loader owns that art.
- **`crates/lodestone-shell/src/menu/packs.rs`** holds `PackRow`, `PacksControl`,
  `PacksNav` (both columns, the cursor, a scroll offset per column) and `commit`.
- **`menu/options.rs`** — `SettingsPage::ResourcePacks`, the `SettingsNav`
  plumbing, `settings_frame`'s branch. `SettingsNav::activate` calls
  `PacksNav::reset`, which is where the folder scan happens.
- **`menu/nav.rs`** — `key_packs`, `apply_packs` (which calls `packs::commit`
  before `leave_packs`), the hover/click guards, and the `active_list` /
  `scroll_active_list` arms.
- **`menu/render/draw.rs`** — `draw_pack_entry`, this screen's own row draw, plus
  `draw_arrow` for the two reorder buttons. A pack row is routed there by
  `MenuRow::pack`, tested inside the `slot` arm (the rect *is* the slot, unlike the
  three other list screens, whose `getRowLeft()` needs its own `row_rect` arm).

## The trap: the UI list and the manager stack are reversed

`ResourceManager` stores sources **lowest priority first**. This screen shows
**highest priority at the top**. Get it backwards and nothing errors: every pack
loads, nothing warns, and the pack on top overrides nothing.

Both directions are attested from the record definitions, not from a summary of a
call site:

| claim | source |
|---|---|
| the **last** pack handed to `MultiPackResourceManager` wins | `FallbackResourceManager.push` appends (`:55`); `getResource` walks `for (int i = fallbacks.size() - 1; i >= 0; i--)` (`:65`) |
| the **first** row of the Selected column is that last pack | `PackSelectionModel`'s constructor does `Collections.reverse(this.selected)` (`:36-37`), and `commit` reverses back (`:52`) |
| the built-in pack is the bottom of both | `Pack.Position.BOTTOM` + `fixedPosition`, inserted at index 0 by `Position.insert` (`Pack.java:145-157`) |
| a newly enabled pack goes to the **top** | `FolderRepositorySource.DISCOVERED_PACK_SELECTION_CONFIG` is `Pack.Position.TOP` (`:31`) |

`ResourceManager::from_priority_order` is the single place the reversal lives, and
its doc carries the same citations. If you add a second caller, use it rather
than a hand-written `.rev()`.

## How to change it

- **The built-in pack is pinned by construction, not by a flag.** `PacksNav::rebuild`
  appends `PackRow::builtin()` after the user's rows and `controls()` gives it no
  move buttons; `selected_ids()` filters it out, so `SelectedPacks` cannot
  persist it and nothing can deselect it. Do not add an `enabled` flag for it —
  that would make the invariant representable-but-false.
- **The scan is `PacksNav::reset`'s job**, called on entering the page. There is
  no filesystem watcher.
- **`discover`, `persist` and `open_pack_folder` are `#[cfg(test)]` forks**, not
  `cfg!(test)` early returns, so no unit test reads the developer's pack folder,
  rewrites their `resource_packs.json`, or spawns a file manager. Same for
  `resources::load_persisted_selection`. Keep that shape (`CLAUDE.md` §12.44).
- **When the selection changes, nothing rebuilds live.** Each consumer picks it
  up at its own next build: the block atlas and models per session
  (`sim/build.rs`'s `BlockResources::load`, i.e. next world join), the GUI/item
  atlases and friends when their owner is next constructed. `load_particle_atlas`
  is the one exception — it caches in a `OnceLock` on purpose (two consumers must
  share one object), so it keeps whatever stack was live at its first call.
- **A pack row is a list entry, not a button.** `draw_pack_entry` transcribes
  `TransferableSelectionList.PackEntry.extractContent` (`:136-219`): the 32×32
  `pack.png` mosaic at the content box's top-left, the name at `+34, +1`, up to two
  description lines at `+34, +12` in vanilla's `-8355712` grey, all clipped/wrapped
  to `MAX_DESCRIPTION_WIDTH_PIXELS` (157), and the selection's 1 px outline over a
  black interior. It draws **no** `widget/button*` nine-slice — that is what the
  wrong version did, and the row gate in `menu/render/tests.rs` asserts the absence
  of `ROW_BG`/`ROW_SEL` for exactly that reason.
  - It *was* a button for one release, because the draw dispatched on
    `MenuRow::favicon` and a pack with no `pack.png` (including the built-in row,
    the only row an empty folder shows) never matched. Worth remembering how it
    survived: every test on this screen asserts on the **frame data**, which
    carried the icon and the description correctly the whole time. Nothing tested
    the draw, so a dispatch fault sat behind a green suite.
  - The hover overlay is vanilla's (`transferable_list/select`, `unselect`, over an
    `0xA0909090` dim) but it is an **indicator, not a hit zone**: in this client the
    whole row transfers the pack. The `_highlighted` variant still tracks the cursor
    being over the icon, as `mouseOverIcon` does.
- **The per-row move buttons are this client's shape, not vanilla's.** Vanilla
  draws hover-revealed 32 px sprite zones over the pack icon's two right quadrants
  (`TransferableSelectionList.PackEntry.extractContent`, `:187-209`). Two
  right-anchored square buttons per row is `menu/key_binds.rs`'s existing row
  shape, which this pipeline already draws and hit-tests. What they carry *is*
  vanilla-shaped: a triangle (`MenuRow::arrow` → `draw_arrow`, four stacked 1 px
  rows of 1/3/5/7) rather than the `U`/`D` letters they shipped with — the fallback
  bitmap font is upper-case 5×7 and has no arrow glyph, so the arrow is geometry.

## What is deliberately not built

- **Pack-format validation**, and with it the incompatible row's red content box
  and `pack.incompatible` name swap (`TransferableSelectionList.java:137-144`).
  `PackMeta::accepts` already exists and `DiscoveredPack::pack_format` already
  carries the number, but **nothing in this client declares a host `pack_format`**
  to compare against, and the scan drops `pack.mcmeta`'s `supported_formats` range
  — so a guessed host number would paint a warning over packs that are in fact
  fine. Painting nothing is the honest reduction; a wrong warning is not. **This
  client is more permissive than vanilla here:** an old pack loads and its stale
  paths silently resolve to nothing.
- **A search box** and the **drag-and-drop-file hint** (`pack.dropInfo`). The
  hint would advertise a file-drop handler that does not exist, so the header
  stays the generic 33 px `OptionsSubScreen` band rather than vanilla's taller
  one.
- **A scrollbar per column.** Both columns share one vertical band, so
  `packs::list_spec` declares it once — enough for a bar to be drawn, for the rows
  to be clipped to the band (`Origin::is_scrolling_list_row`) and for the wheel to
  reach the focused column — but the thumb reflects whichever column the cursor is
  in, not both. At `MIN_SCALED_HEIGHT` that band shows four rows; beyond that the
  cursor auto-scrolls and the wheel works.
  - The **last row is reachable**, measured rather than assumed:
    `wheeling_to_the_clamp_brings_the_last_row_fully_inside_the_band` drives the
    wheel to `max_scroll` at three canvases and compares `packs::row_y` — the
    expression the *draw* uses — against the band. It was asked because of a
    separate "settings scrolling doesn't reach the end" report; whatever that is, it
    is not this band arithmetic.
- **Filesystem watching**, and **live reload** on Done.

## Configuration

| what | where |
|---|---|
| the packs folder | `<data dir>/resourcepacks/` — alongside `saves/`, `servers.json`, `options.json`; `LODESTONE_DATA_DIR` overrides the root |
| the selected order | `<data dir>/resource_packs.json` — a JSON array of `"file/<name>"` ids, **highest priority first**. Missing or corrupt is empty, never an error |
| the built-in pack | `client.jar`, found by `resources::asset_root` (`LODESTONE_ASSETS`, else `.cache/mc/<ver>`) |

A pack renamed on disk is deselected, because the id is the filename — the same
in vanilla.

## Evidence

`crates/lodestone-shell/tests/resource_pack_stack.rs`, two `#[ignore]`d gates
(they need a real `client.jar`). Build the fixture, then run them:

```bash
ROOT=/private/tmp/lt-packs-spot
mkdir -p "$ROOT/resourcepacks/folderpack/assets/minecraft/textures/block"
# folderpack: pack.mcmeta (plain-string description) + a 4x4 magenta pack.png
#             + a 4x4 magenta assets/minecraft/textures/block/stone.png
# zippack.zip: the same, with a *text-component* description and cyan textures
LODESTONE_PACKS_FIXTURE=$ROOT LODESTONE_DATA_DIR=$ROOT \
  cargo test -p lodestone-shell --test resource_pack_stack \
  -- --ignored --nocapture --test-threads=1
```

Two packs deliberately override the **same** in-jar path with different flat
colours, so each gate's expected value originates in the fixture rather than in
the code under test, and the other pack's colour is the control a reversed stack
would produce:

- `a_folder_and_a_zip_are_both_discovered_and_the_top_of_the_order_wins` — both
  kinds discovered with descriptions (one plain string, one text component) and
  decoded icons; the top of the order wins in **both** directions through the
  production `open_pack_stack`; a path neither pack carries still comes from the
  jar.
- `the_selected_packs_pixels_reach_the_stitched_block_atlas` — the last link.
  `BlockResources::load(true)` is what a live session calls, and `Atlas::rgba` is
  uploaded verbatim, so this reads `minecraft:block/stone`'s placed region out of
  the real atlas. Measured: **rgb(143,143,143) with no packs, rgb(255,0,255) with
  the folder pack selected.** The no-pack read is the control — without it, a
  vanilla stone that happened to be magenta would pass.

The **draw** is gated separately, hermetically, in `menu/render/tests.rs` — and it
is the gate this screen was missing:

- `a_resource_pack_row_draws_its_icon_and_description_not_a_centred_button_label`
  measures a flat-coloured `pack.png` filling the 32×32 icon column, the
  description's grey on its own line past that column, and **zero**
  `ROW_BG`/`ROW_SEL`/`ROW_OFF` in the row — the button fill the wrong draw painted.
  It checks the built-in row too, which has no icon at all: the case the
  `favicon`-gated version missed.
- `a_move_button_draws_a_triangle_pointing_its_own_way` asks *where* the arrow's ink
  is, not whether there is any: an up arrow's top half must carry less than its
  bottom half and a down arrow the reverse, so a letter or a pair drawn the same way
  round fails.
- `every_sprite_id_the_vanilla_screens_name_exists_in_the_real_pack` (`#[ignore]`d)
  now also asserts the four `transferable_list/*` ids and the loose
  `misc/unknown_pack` resolve in the real jar — a typo there draws nothing, silently.

## Dependencies

- `lodestone-assets` — `ResourceSource`/`DirectorySource`/`ZipSource`,
  `ResourceManager::{new, from_priority_order}`, `PackMeta`/`PackDescription`,
  `Image::decode_png`.
- `super::options` — `SUB_HEADER_HEIGHT`, `FOOTER_HEIGHT`, `footer_rects`,
  `Placement::Footer`, `SMALL_BUTTON_WIDTH`, `WIDGET_H`, `title_y`.
- `super::render` — `Origin::Packs`, `FaviconMosaic`/`head_mosaic` (the
  thumbnail goes through the same box-filtered drawable a server favicon and an
  account head already use), `widget::ListSpec`.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` (`resourcePack.title`,
  `pack.available.title`, `pack.selected.title`, `pack.openFolder`,
  `pack.nameAndSource`, `resourcePack.vanilla.name`, `pack.source.builtin`,
  `gui.done`) and `.cache/mc/26.2/client-src/net/minecraft/` →
  `client/gui/screens/packs/{PackSelectionScreen,PackSelectionModel,TransferableSelectionList}.java`,
  `server/packs/repository/{PackRepository,FolderRepositorySource,Pack}.java`,
  `server/packs/resources/{MultiPackResourceManager,FallbackResourceManager}.java`.

## See also

- [Language screen](./language-screen.md) — the sibling `ObjectSelectionList`
  screen whose row/scroll shape this one follows.
- [Telemetry screen](./telemetry-screen.md) — the third screen issue #415 built.
- [The settings tree](./settings-screen.md) — the root page this is reached from.
- [Server resource packs](./server-resource-pack.md) — the *other* resource-pack
  path: a pack a server pushes, which is unrelated to this folder.
