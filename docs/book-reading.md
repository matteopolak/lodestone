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
producer: `WindowApp::try_use` (`crates/lodestone-shell/src/app/menus.rs`) forks on it
immediately after its existing `writable_book_in_hand` fork, and opens the screen
client-side. `WrittenBookItem.use` returns `InteractionResult.SUCCESS` after
`player.openItemGui(...)`, so vanilla never reaches the generic use for a book either — the
fork returns rather than falling through, which is why the swing decision in
`sim/actions.rs` is not consulted here even though `written_book` is in its table.

The screen has **no wire traffic at all**, which is the one structural difference from the
editor: Done and Escape are the same thing, and `MenuNav::activate_book_view_row` returns
`MenuAction::None` for every row.

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

## What is not ported

Each of these is a real gap, not a decision that the data is absent:

- **Page click and hover events.** Vanilla's pages are full chat components and a
  `ClickEvent` can run a command or open a URL. `BookViewOpen::from_pages` flattens each
  `lodestone_model::Text` to plain text on the way in, so such a book reads correctly and
  is inert. The data is on the wire and in the model; only the screen drops it.
- **`textures/gui/book.png`.** The page draws as plain labels over the standard dimmed
  backdrop, not over the parchment sprite — the same simplification `book_edit` already
  makes, for the same reason (the menu overlay stream has no atlas). Consequently the page
  text is the menu's ordinary light colour, not `BookViewScreen`'s `PAGE_TEXT_STYLE` black,
  which is only legible against that sprite.
- **Page Up / Page Down.** `BookViewScreen.keyPressed` binds GLFW `266`/`267` to the two
  page buttons. `menu::nav::MenuKey` has no variant for either, so the arrow keys stand in;
  wiring the real pair is a keyboard-layer change.
- **A lectern's book.** Vanilla's `LecternScreen` extends `BookViewScreen` with a Take Book
  button and a `ServerboundContainerButtonClickPacket` for page turns. Nothing here opens
  a lectern.

## Configuration

None. No feature gate, no env var — the screen and the tooltip lines are unconditional.

## Dependencies

`lodestone-model`'s `WrittenBookContent`, `lodestone-game`'s `ItemStack`/`Menus` fold,
`crate::menu::text_area::TextArea`, and `crates/protocol/v770`'s
`read_written_book_content` for the decode. Nothing outbound.
