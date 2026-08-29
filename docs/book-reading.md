# Written books

## What it is

Reading a signed `minecraft:written_book`: its title, author and copy generation on the
item itself, and the screen that opens when you right-click one. Vanilla's
`WrittenBookContent` component plus `BookViewScreen`, the read-only counterpart of
[book-and-quill editing](./book-editing.md).

The component itself was never the gap. `crates/protocol/v770` has decoded
`minecraft:written_book_content` into `lodestone_model::ItemComponents` since the
`EDIT_BOOK` work landed, with byte-level gates in
`crates/protocol/v770/tests/book_content_wiring.rs`, and
`lodestone_game::item::ItemStack::from(&model)` lifts it into a `ComponentValue::WrittenBook`.
What was missing is everything downstream: `ItemStack::written_book_content()` had **zero**
production readers outside its own round trip back to the model. A signed book therefore
decoded perfectly, folded into the menu perfectly, and showed as "Written Book" with no
author and no generation, and right-clicking it did nothing.

## How it works

### The name and the tooltip

Two separate vanilla mechanisms, and conflating them is the trap:

| shows | vanilla | here |
|---|---|---|
| the **title**, as the item's display name | `ItemStack.getCustomName()` falls back to `written_book_content.title().raw()` when no `minecraft:custom_name` is present | `lodestone_game::item::styled_hover_text` |
| **`by <author>`** and the generation line | `WrittenBookContent.addToTooltip` | `crate::container::tooltip::book_lore_lines` |

The title is *not* a tooltip line — it is the name, so it also reaches the held-item
highlight, the anvil rename box's seed and every other `styled_hover_name` caller for free.

**The italic is a separate question with a separate answer.** `getStyledHoverName`
italicises on `has(DataComponents.CUSTOM_NAME)` — the *component*, not on whatever
`getCustomName()` returned. So a written book's title is upright, and an anvil-renamed one
is italic. `styled_hover_text` keeps those two tests apart deliberately; folding the book
title into the `custom.is_some()` branch would italicise every book.

The generation strings (`Original` / `Copy of original` / `Copy of a copy` / `Tattered`,
`en_us.json`'s `book.generation.<n>`) live in one table, `tooltip::book_generation_name`,
shared with the screen, so the two cannot disagree.

### The reading screen

`crates/lodestone-shell/src/menu/book_view.rs` — `BookViewState`, `Screen::BookView`, and
`render::book_view_frame`. `crate::sim::session`'s `Sim::written_book_in_hand` is the
local-use producer: `WindowApp::try_use` (`crates/lodestone-shell/src/app/menus.rs`) forks on it
immediately after its existing `writable_book_in_hand` fork, and opens the screen
client-side. `WrittenBookItem.use` returns `InteractionResult.SUCCESS` after
`player.openItemGui(...)`, so vanilla never reaches the generic use for a book either — the
fork returns rather than falling through, which is why the swing decision in
`sim/actions.rs` is not consulted here even though `written_book` is in its table.

The server-directed path is separate: v770 decodes `ClientboundOpenBookPacket` to
`ClientEvent::BookOpened`; `net::forward` carries its hand selector to `Sim`, and
`WindowApp::drive_ui_from_session` resolves that exact already-synchronised hand. This handles
both signed books and writable books: a writable book opens the existing editor, while a signed
book opens this reader. It intentionally does not fall back to the other hand, because a server
may select an off-hand book while the main hand contains another stack.

Pages remain `lodestone_model::Text` all the way through `BookViewOpen` and `BookViewState`.
The renderer splits the resolved spans at wrapped-line boundaries, so authored text colour is
retained instead of being flattened before rendering. Click and hover metadata stays on the text
but is not interactive; font-weight, italic and other non-colour span styles have no menu-label
renderer yet.

All three book layouts — signed reader, writable editor, and its signing form — draw the same
vanilla `textures/gui/book.png` sheet. It is a loose 256×256 texture rather than a GUI sprite, so
`resources::BOOK_GUI_TEXTURE` registers it as an extra on the **menu** atlas. The atlas rebuilds
on every resource-pack generation, including server-pack installation, and the draw samples the
same top-left 192×192 window that `BookViewScreen`, `BookEditScreen`, and `BookSignScreen` blit.
The reader’s page controls use vanilla’s 23×13 `widget/page_*` sprites at the source-derived
positions, and both book screens use those same row rectangles for rendering, hover, and clicks.
Book frames also select vanilla’s shadowless text overload for every string they emit: reader and
lectern page text, page indicator, editor page/title/author text, and their widgets. This is a
frame-local rendering rule (`MenuFrame::book_background`); ordinary menu and HUD text retain their
normal drop shadows.

### Lecterns

`minecraft:lectern` is not a generic one-row chest. Its content packet contains only the book
slot (slot `0`), while property `0` is the current zero-based page. `Menus::ensure_open` keeps
that one slot intact; `Sim::lectern_book_view` projects the book and page into the same reader
state. The overlay records the server menu id: previous/next send
`ClientAction::ContainerButtonClick` with the new zero-based page, and Done/Escape send
`ClientAction::ContainerClose` through `Sim::close_open_menu`. The server remains authoritative
and a subsequent container-data packet corrects the optimistic page.

Page geometry and word-wrapping are *borrowed* from `book_edit`
(`PAGE_WRAP_CHARS`, `PAGE_LINE_LIMIT`, and `text_area::TextArea` itself, driven read-only)
rather than restated. Vanilla gives both screens the same `TEXT_WIDTH = 114`, and a second
wrap implementation would be free to disagree with the editor's about where a line breaks.

## How to change it

- **Adding a tooltip line** goes in `book_lore_lines`, not in `tooltip_lines` directly —
  see `tooltip.rs`'s own module doc for the two gotchas that apply to every line there.
- **The screen draws through `nav::book_edit_overlay_frame`**, which answers for *both*
  book screens. That is deliberate: `app/redraw.rs`'s overlay block calls it once by name,
  the two screens are mutually exclusive (a stack is either writable or written), and the
  function exists so that hit-testing and drawing share one construction. If you add a
  third book screen, extend that function rather than adding another overlay block.
- **`Screen::BookView` is an overlay, not a full screen**, so `menu::render::frame_for` has
  no arm for it. A new overlay screen that forgets the draw call opens, hit-tests
  correctly, and renders nothing.
- **Keep `BOOK_GUI_TEXTURE` in `MENU_TEXTURES`** when changing the book art. It is deliberately
  not added to the HUD atlas: this renderer owns the texture binding and active-pack reload already
  rebuilds its menu atlas. `MenuFrame::book_background` plus `draw::sprite_region` owns the
  192/256 source crop; drawing the whole 256×256 sheet stretches transparent padding into the
  visible page. The same marker intentionally selects `VanillaFont::draw_plain`; do not change
  the global `Quads::text` default or book page/title text will regain a shadow (or unrelated
  screens will lose theirs).

## What is not ported

Each of these is a real gap, not a decision that the data is absent:

- **Page click and hover events.** Vanilla's pages are full chat components and a
  `ClickEvent` can run a command or open a URL. Lodestone retains those components and their
  spans while rendering, but the menu overlay has no interaction dispatcher, so pages are inert.
- **Page Up / Page Down.** `BookViewScreen.keyPressed` binds GLFW `266`/`267` to the two
  page buttons. `menu::nav::MenuKey` has no variant for either, so the arrow keys stand in;
  wiring the real pair is a keyboard-layer change.
- **Taking a lectern book.** Page navigation and close semantics are implemented, but the
  `LecternScreen` Take Book control is not yet exposed by the overlay.

## Configuration

None. No feature gate, no env var — the screen and the tooltip lines are unconditional.

## Dependencies

`lodestone-model`'s `WrittenBookContent` and `Text`, `lodestone-game`'s `ItemStack`/`Menus`
fold, `crate::menu::text_area::TextArea`, and v770's `OPEN_BOOK` plus
`read_written_book_content` decode. Lecterns also rely on the protocol family's
`ContainerButtonClick` and `ContainerClose` action encoders.
