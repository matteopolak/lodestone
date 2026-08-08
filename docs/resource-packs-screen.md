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
- **`menu/render/draw.rs`** — `draw_widget` grew one additive branch, gated on
  `MenuRow::favicon`, that draws the thumbnail plus a title/description pair.
  No other slotted row anywhere sets `favicon`, so the branch cannot move a pixel
  on a pre-existing screen.

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
- **The per-row move buttons are this client's shape, not vanilla's.** Vanilla
  draws hover-revealed 32 px sprite zones over the pack icon
  (`TransferableSelectionList.Entry.render`). Two right-anchored square buttons
  per row is `menu/key_binds.rs`'s existing row shape, which this pipeline
  already draws and hit-tests. The bitmap font is upper-case 5×7 with no arrow
  glyphs, hence `U`/`D` rather than `▲`/`▼`.

## What is deliberately not built

- **Pack-format validation.** Vanilla checks `pack_format` against the host's and
  warns/confirms on an incompatible pack. `PackMeta::accepts` already exists and
  `DiscoveredPack::pack_format` already carries the number — it is simply not
  consulted. **This client is more permissive than vanilla here:** an old pack
  loads and its stale paths silently resolve to nothing.
- **A search box** and the **drag-and-drop-file hint** (`pack.dropInfo`). The
  hint would advertise a file-drop handler that does not exist, so the header
  stays the generic 33 px `OptionsSubScreen` band rather than vanilla's taller
  one.
- **A scrollbar per column.** Both columns share one vertical band, so
  `packs::list_spec` declares it once — enough for the rows to be clipped to it
  (`Origin::is_scrolling_list_row`) and for the wheel to reach the focused
  column — but the thumb reflects whichever column the cursor is in, not both. At
  `MIN_SCALED_HEIGHT` that band shows four rows; beyond that the cursor
  auto-scrolls and the wheel works, which is what keeps every row reachable.
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
