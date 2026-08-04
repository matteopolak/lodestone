# Chat

## What it is

The chat box: the outbound input line (`crate::chat::ChatInput`), the received
scrollback (`lodestone_game::chat::ChatLog`, folded into legacy `§`-coded
strings at read time), and the HUD draw that renders both
(`crate::hud::HudGeometry::build_inner`'s chat block, `crates/lodestone-shell/src/hud.rs`).
This doc covers the **rendering** half — input caret, word wrap, and the
persisted Chat Settings that shape it — not the wire path (`chat.rs`'s own
module docs cover composing an outbound line) or the log itself
(`lodestone-game/src/chat.rs`).

## How it works

### Input line

`HudFrame::chat_input` carries the in-progress line, `Some` only while the
chat box is open. The draw (`hud.rs`, just after the `chat_open` local) emits
a translucent background strip sized to the chat box, then the typed text
with a trailing `_` standing in for vanilla's append-caret
(`TextCursorUtils.extractAppendCursor`,
`.cache/mc/26.2/client-src/net/minecraft/client/gui/components/TextCursorUtils.java:15-17`
— the shell's `ChatInput` only ever edits at the end of the line, which is
vanilla's "cursor at end" case). There is **no leading `>`** — vanilla's
`ChatScreen`/`EditBox` never draws one.

The caret blinks at vanilla's real rate: `TextCursorUtils.CURSOR_BLINK_INTERVAL_MS`
is `300`, and `isCursorVisible(millis) == (millis / 300) % 2 == 0`
(`TextCursorUtils.java:9,20-22`). `HudFrame::chat_caret_visible` carries that
boolean; the caller computes it from wall-clock time (see `app.rs`'s
`WindowApp::redraw`, right before it builds the `HudFrame`) rather than this
module owning a clock, matching how every other transient render flag reaches
`HudFrame`.

### Word wrap

`hud.rs`'s `wrap_legacy_with` (a free function; `Builder::wrap_legacy` binds it
to the Builder's own font) greedily wraps a legacy-coded line into rows that
fit a pixel width, mirroring vanilla's `GuiMessage.splitLines`
(`ChatComponent.addMessageToDisplayQueue`,
`.cache/mc/26.2/client-src/net/minecraft/client/gui/components/ChatComponent.java:284-285`):
break on a space when the next word would overflow, and hard-break a single
word character-by-character when it alone exceeds the width. A `§`
colour/format code seen before a break is carried onto the continuation line
(a code fully resets formatting to itself —
`lodestone-model/src/text.rs:626-644` — so tracking only the most recent one
is sufficient).

Widths come from `Builder::legacy_width`, which is real vanilla proportional
glyph advances when a `VanillaFont` is attached (jar present) and the fixed
5×7 advance otherwise (`hud/font.rs`) — the **same** metric the draw call
itself uses, so a layout can never be computed against a font other than the
one that draws.

Each logical `(line, age)` chat entry can now expand into several visual rows,
all sharing that entry's age/alpha. Vanilla stacks a wrapped message's *last*
split line nearest the bottom edge and its earlier lines above it
(`ChatComponent.java:164-168,288-297`); the draw reproduces that by reversing
each entry's own wrapped rows before stacking them bottom-up.

### Chat Settings

`crate::hud::ChatDisplayOptions` (a small `Copy` struct on `HudFrame`) carries
the subset of vanilla's `net.minecraft.client.Options` chat fields
(`.cache/mc/26.2/client-src/net/minecraft/client/Options.java:271-404,508`)
that this renderer actually consumes:

| field | vanilla option | default | effect |
|---|---|---|---|
| `scale` | `options.chat.scale` | `1.0` | pose-scale multiplier on top of this HUD's fixed 2× legibility factor |
| `width_pct` | `options.chat.width` | `1.0` | box width, via `chat_width_px` (vanilla's `ChatComponent.getWidth`) |
| `height_pct_unfocused` | `options.chat.height.unfocused` | `70.0/160.0` | box height (and row cap) while closed |
| `height_pct_focused` | `options.chat.height.focused` | `1.0` | box height (and row cap) while open |
| `line_spacing` | `options.chat.line_spacing` | `0.0` | extra fraction of a line inserted between rows |
| `text_opacity` | `options.chat.opacity` | `1.0` | text alpha, as `text_opacity * 0.9 + 0.1` |
| `background_opacity` | `options.accessibility.text_background_opacity` | `0.5` | per-row background fill alpha |
| `colors` | `options.chat.color` | `true` | `false` strips every legacy `§` code before drawing |

At every field's default, the draw is byte-identical to the pre-options
behaviour — an untouched install looks exactly as it did before these fields
existed.

These are persisted on `crate::config::Options` (`chat_scale`, `chat_width`,
`chat_height_unfocused`, `chat_height_focused`, `chat_line_spacing`,
`chat_opacity`, `chat_background_opacity`, `chat_colors`), following the same
`0.0..=1.0`-degrade-to-default / write-only-if-non-default convention as
`mouse_wheel_sensitivity` and `view_bobbing`. The shell (`app.rs`) reads
`self.nav.options()` and folds them into `HudFrame::chat_options` once per
frame.

**Deliberately not landed**: vanilla's `chatVisibility` (System/Hidden
filtering) needs a per-line message-source tag that
`lodestone_game::chat::ChatLog::recent` currently flattens away before it
reaches the HUD; `chatLinks`/`chatLinksPrompt` need click detection this HUD
has none of; `chatDelay` is a message-arrival rate limit, not a render
concern. Landing an option field with no reader is the exact defect
`CLAUDE.md` calls the dominant one here, so these stay out until something
upstream can consume them — see that file's `ChatDisplayOptions` doc comment
for the same note in code.

## How to change it

- **Add a Chat Settings row**: add the field to `crate::config::Options`
  (`config.rs`), thread it into `crate::hud::ChatDisplayOptions` at the
  `app.rs` call site that builds the `HudFrame`, and make the draw in
  `hud.rs`'s chat block actually read it. Do the last step first if you can —
  an option nothing reads is the failure mode to avoid.
- **Change the wrap algorithm**: `wrap_legacy_with` in `hud.rs`. It is a free
  function taking a width-measuring closure specifically so it can be
  unit-tested against a hand-specified width table (see
  `wrap_uses_real_per_glyph_widths_not_a_flat_character_count`) without a GPU,
  an atlas, or a loaded jar.
- **Gotcha**: `chat_pose_scale = scale * chat_options.scale` folds this HUD's
  own fixed 2× legibility factor (`scale`, shared with the F3 debug overlay)
  together with the option. Do not apply the option to the shared `scale`
  local directly — that would also rescale the debug overlay.
- **Gotcha**: the logical canvas (`b.w`/`b.h`) is already in vanilla's
  `guiScaledWidth`/`Height` units (see `logical_canvas`'s own doc), which is
  why `chat_width_px`/`chat_height_px` need no unit conversion against it.

## Configuration

`options.json` (next to `servers.json`, see `crate::config::options_path`):
`chat_scale`, `chat_width`, `chat_height_unfocused`, `chat_height_focused`,
`chat_line_spacing`, `chat_opacity`, `chat_background_opacity`, `chat_colors`.
All optional; a missing, malformed, or out-of-`0.0..=1.0`-range value degrades
to vanilla's own default (`chat_colors` degrades to `true`, matching vanilla).

## Dependencies

- `lodestone_client::ClientAction` — the outbound seam `chat.rs`'s
  `compose_chat_action` lowers a typed line onto.
- `lodestone_game::chat::ChatLog` — the received scrollback, folded to legacy
  strings at read time; owned by the sim, not this module.
- `crate::hud::vanilla_font::VanillaFont` — real proportional glyph metrics,
  when a jar is present.
