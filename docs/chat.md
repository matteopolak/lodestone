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
(`TextCursorUtils.extractAppendCursor`
— the shell's `ChatInput` only ever edits at the end of the line, which is
vanilla's "cursor at end" case). There is **no leading `>`** — vanilla's
`ChatScreen`/`EditBox` never draws one.

The caret blinks at vanilla's real rate: `TextCursorUtils.CURSOR_BLINK_INTERVAL_MS`
is `300`, and `isCursorVisible(millis) == (millis / 300) % 2 == 0`
(`TextCursorUtils.isCursorVisible`). `HudFrame::chat_caret_visible` carries that
boolean; the caller computes it from wall-clock time (see `app.rs`'s
`WindowApp::redraw`, right before it builds the `HudFrame`) rather than this
module owning a clock, matching how every other transient render flag reaches
`HudFrame`.

### The inline command-suggestion "ghost"

`HudFrame::chat_suggestion_ghost` is the greyed-out preview of the top
autocomplete candidate — `EditBox.extractRenderState`'s own `suggestion`
field. Three things port from that method, in the order that matters:

1. **Position is `cursorX - 1`, where `cursorX = font.width(value)` — the
   typed text alone.** The caret contributes no advance of its own in
   vanilla, because there it is a separately blinking overlay rectangle, not
   part of the measured string. This shell's caret *is* part of the drawn
   string (a literal appended `_`, see above), which makes it tempting to
   measure the pen against `{input}_` — that was a real, shipped bug: it
   landed the ghost one whole underscore-width too far right, permanently
   (stable across the blink, but at the wrong x either way).
2. **Draw order is text → suggestion → cursor**, so the caret composites on
   top of the suggestion's leading glyph rather than the other way round.
   Drawing `{input}{caret}` as one string before the suggestion gets this
   backwards.
3. **Gated on `!insert`**, where vanilla's `insert = cursorPos <
   value.length() || value.length() >= maxLength`. This shell's `ChatInput`
   only ever edits at the end of the line (the append-caret convention
   above), so the first disjunct never applies here; the second does —
   `ChatInput::push_char` caps a line at 256 — so a full line suppresses the
   suggestion.

The colour is vanilla's literal `0x808080` (`SUGGESTION_GHOST`); the draw
itself takes no font-shadow parameter here (`Builder::text`'s fixed-advance
fallback font is never shadowed, and the vanilla-font path shadows
everything it draws uniformly, so there is no per-call flag to honour on
either path this shell has, unlike `EditBox`'s own `textShadow` field).

### Up/Down: the sent-line history

`ChatHistory` is vanilla's `ChatComponent.recentChat` — the deque of lines the
player has **sent**, oldest first, capped at `RECENT_CHAT_MAX` (100) with the
oldest dropped. It is a field of `ChatInput` rather than of the screen, which is
the equivalent of vanilla's placement: the deque belongs to the persistent HUD
component and the *screen* holds only a cursor into it
(`ChatScreen.historyPos`/`historyBuffer`), so reopening chat still walks
everything sent earlier in the session. `ChatInput` is app-lifetime for exactly
that reason — only its `buf` is screen-scoped.

`ChatInput::history_up`/`history_down` are `ChatScreen.moveInHistory(∓1)`.
The three behaviours worth knowing before changing anything here:

- The cursor lives in `0..=history.len()`, and `len()` — one past the last entry
  — is the **live slot**. `ChatInput::take` rewinds to it, which is how
  `ChatScreen.init`'s `historyPos = getRecentChat().size()` is reproduced without
  a separate open hook: every path that opens the box clears `buf` through `take`.
- Both ends **clamp**, they do not wrap. Vanilla's `Mth.clamp` then
  `if (newPos != historyPos)` means an arrow at either end leaves the line alone —
  but the key is still consumed, so it must not fall through and be typed.
- The part-typed line is stashed in `historyBuffer` on the way out of the live
  slot and restored on the way back, so glancing at an earlier message does not
  destroy a half-written one.

Recording happens in `WindowApp::handle_chat_history_key`'s Enter arm, reading the
line *before* `handle_chat_key` consumes it with `take`. That split is
load-bearing: `take` is on the Escape path too, and a cancelled line is not
history — vanilla only records under
`handleChatInput(msg, addToRecent = true)`, which Escape never reaches. The line
is normalised first (`normalizeSpace(msg.trim())`), so `"hello    world"` is
recalled as `"hello world"`; consecutive duplicates collapse
(`!msg.equals(peekLast())` — the **last** entry only, not a set).

### Tab: completing player names

Tab forks on the *line*, not the key. `CommandSuggestions.updateCommandInfo`
computes `isCommand = commandsOnly || startsWithSlash`; an ordinary chat line
takes the `else if (!command.isBlank())` branch and is answered **locally** from
`ClientSuggestionProvider.getCustomTabSuggestions()`, with no server round trip.
So this works on any server, including one that has sent us no
`minecraft:commands`.

`ChatInput::set_online_players` is how the names get in — a plain `Vec<String>`
the caller refreshes per keystroke from `Sim::tab_list`, which keeps `chat.rs`
free of a client handle as its own header requires. Pass **every** entry, listed
or not: vanilla's provider reads `getOnlinePlayers()`, not
`getListedOnlinePlayers()`, so a player hidden from the tab overlay is still
completable.

Two rules that are easy to get wrong:

- The replaced span starts at `last_word_index`, which is
  `CommandSuggestions.getLastWordIndex`: the offset just past the **last**
  whitespace *run*, not the position of the last space.
- Matching is `SharedSuggestionProvider.matchesSubStr`, which is "prefix of a
  segment delimited by `._/`" — **not** `contains`. `Notch_The_Second` matches
  `the`; `Another` does not, despite containing it. Ordering then composes
  brigadier's case-insensitive sort with `sortSuggestions`'s partition, which
  moves literal prefix matches ahead of splitter matches.

**Tab is shared with the in-world player-list overlay and the two must not steal
each other.** They cannot: `app::input::resolve_key` short-circuits on
`gate.chat_open` and returns `KeyOutcome::Chat` for every key, so the overlay's
`KeyOutcome::PlayerList` is only ever reached with the chat box shut — vanilla's
own context fork, arrived at through the gate rather than through a binding.
`handle_chat_history_key` deliberately does **not** consume Tab; it only refreshes
the name list and lets the existing Tab arm run, so there is one Tab
implementation rather than two that can drift.

### Word wrap

`hud.rs`'s `wrap_legacy_with` (a free function; `Builder::wrap_legacy` binds it
to the Builder's own font) greedily wraps a legacy-coded line into rows that
fit a pixel width, mirroring vanilla's `GuiMessage.splitLines`
(`ChatComponent.addMessageToDisplayQueue`):
break on a space when the next word would overflow, and hard-break a single
word character-by-character when it alone exceeds the width. A `§`
colour/format code seen before a break is carried onto the continuation line
(a code fully resets formatting to itself —
`from_legacy`, `lodestone-model/src/text.rs` — so tracking only the most recent one
is sufficient).

Widths come from `Builder::legacy_width`, which is real vanilla proportional
glyph advances when a `VanillaFont` is attached (jar present) and the fixed
5×7 advance otherwise (`hud/font.rs`) — the **same** metric the draw call
itself uses, so a layout can never be computed against a font other than the
one that draws.

Each logical `(line, age)` chat entry can now expand into several visual rows,
all sharing that entry's age/alpha. Vanilla stacks a wrapped message's *last*
split line nearest the bottom edge and its earlier lines above it
(`ChatComponent.extractRenderState`/`ChatComponent.addMessageToDisplayQueue`); the draw reproduces that by reversing
each entry's own wrapped rows before stacking them bottom-up.

### Scrolling the scrollback

`crate::chat::ChatScroll` (a field of `ChatInput`) is vanilla's
`ChatComponent.chatScrollbarPos`/`newMessageSinceScroll`
(`ChatComponent.java`), reachable by mouse wheel while the chat box is open
(`app/lifecycle.rs`'s `WindowEvent::MouseWheel` arm, falling through to it
only when the pointer is *not* over the command-suggestion popup — vanilla's
own `commandSuggestions.mouseScrolled` first refusal). Vanilla's constants,
read straight from the decompile rather than invented:

- **History cap**: `ChatComponent`'s own `trimmedMessages`/`allMessages` cap
  at **100**, matching this crate's pre-existing
  `lodestone_game::chat::ChatFeed::DEFAULT_CHAT_CAPACITY` (already 100, no
  change needed) — scrolling can never reach further back than the feed
  already retains.
- **Wheel step**: `ChatScreen.mouseScrolled` clamps the raw notch to
  `±1.0`, then multiplies by `7.0` unless Shift is held (`hasShiftDown()`) —
  so an ordinary wheel click moves 7 lines, Shift+wheel moves 1.
- **Clamp**: `ChatComponent.scrollChat` — `chatScrollbarPos += dir`, then
  clamped above at `total - linesPerPage` and below at `0`, in that order
  (the upper clamp can go negative when everything already fits on screen;
  the lower clamp is what actually pins it there, not a saturating
  subtraction).
- **No jump on a new message while scrolled**:
  `addMessageToDisplayQueue`'s `if (chatting && chatScrollbarPos > 0) {
  newMessageSinceScroll = true; scrollChat(1); }` — the currently-visible
  window stays put instead of snapping to the newest message.
- **Reset on close**: `ChatScreen.removed()` calls `resetChatScroll()`.

**Two deliberate departures from vanilla, both named in `ChatScroll`'s own
doc comment:**

1. **Granularity is one *logical entry*, not one *wrapped visual row*.**
   Vanilla scrolls through already-wrapped `GuiMessage.Line`s, which needs
   real font metrics this crate's `chat.rs` deliberately has none of (see
   this file's own module doc on why `highlight`/`complete` return byte
   spans rather than pixel runs, for the same reason). Entry granularity is
   the exact one-line-per-entry case of vanilla's scheme, and is wrong only
   for a message that wraps into more than one row.
2. **The no-jump compensation runs once per frame (`ChatScroll::sync`), not
   once per push.** The received log
   (`lodestone_game::chat::ChatLog`) is version- and UI-free by design and
   holds no notion of "is chat open" to check against, so `sync` is called
   every frame with the box's current open state and its full history, and
   detects every arrival since the last call by finding where last frame's
   newest line now sits (`chat.rs`'s private `new_arrivals`). This is exact,
   not approximate, whenever the box stayed open across the gap:
   `scrollChat`'s clamp is linear in both the position and the total, so `k`
   sequential single-line increments and one increment of `k` land on the
   same clamped result. `sync(false, ..)` also **is** the reset-on-close
   path — called unconditionally every frame regardless of screen state, it
   collapses "closed" to "reset" directly rather than needing a hook into
   every place the screen can close (Escape, sending without
   `closeOnSubmit`, a disconnect).

The scrollbar (`crate::hud::ChatScrollbar`, drawn in `hud.rs` right after the
scrollback entries) tracks the same three numbers `ChatScroll` exposes
(`scrolled`, `total`, `new_message_since_scroll`) — never a second, re-derived
count — and only appears once there is more history than fits on screen,
matching vanilla's own `virtualHeight != chatHeight` gate. Vanilla's alpha
(`y > 0 ? 170 : 96`) is simplified to a fixed `170/255`: the sign check is on
an internal value that is essentially never positive in practice (it would
need a chat box taller than the canvas itself), so branching on it added risk
without a way to verify the branch here — a named simplification, not a
guessed one.

### Vertical layout: the scrollback's own anchor

The scrollback's bottom edge (`crate::hud::chat_bottom`) and the input box's
own placement (`crate::hud::chat_input_top`) are **two independent literals in
vanilla**, not one derived from the other:
`ChatComponent.extractRenderState`'s `chatBottom = Mth.floor((screenHeight -
40) / scale)` is computed once, before that method ever branches on open vs.
closed, and has no reference anywhere to where the `EditBox` sits
(`this.height - 12`, a different literal in a different class,
`ChatScreen.init`). At the vanilla-default `chatScale` of `1.0` this is simply
`canvas_h - 40.0` — a real, fixed ~26 logical-canvas-pixel gap between the
newest message and the input box, by design.

This HUD used to compute `chat_bottom` as `input_y - INPUT_STRIP_PAD *
chat_pose_scale` while the box was open — literally the input strip's own top
edge — which coupled the scrollback to the input box and erased that gap
(measured: ~1px instead of vanilla's ~26px). Reported by the owner as "a gap
between the bottom of the chat and the bar where I type stuff"; fixed by
porting `chatBottom`'s real expression as `crate::hud::chat_bottom`, used
unconditionally (open or closed), same as vanilla. See
`crates/lodestone-shell/tests/chat_input_gap.rs` for the regression gate,
which rasterises `HudGeometry::build` and measures the two background bands'
actual pixel positions rather than asserting on the formula alone.

### Chat Settings

`crate::hud::ChatDisplayOptions` (a small `Copy` struct on `HudFrame`) carries
the subset of vanilla's `net.minecraft.client.Options` chat fields
(`Options.chatScale`/`Options.chatWidth`/`Options.chatHeightUnfocused`/
`Options.chatHeightFocused`/`Options.chatLineSpacing`/`Options.chatOpacity`/
`Options.textBackgroundOpacity`/`Options.chatColors`)
that this renderer actually consumes:

| field | vanilla option | default | effect |
|---|---|---|---|
| `scale` | `options.chat.scale` | `1.0` | `chat_pose_scale`'s **entire** value — vanilla's `ChatComponent.getScale`, with no HUD-side multiplier on top (see `docs/hud-text-scale.md`; the old "2× legibility factor" this row used to describe was fixed there) |
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
- **The wrap result is cached** (`hud::ChatWrapCache`). Vanilla
  splits once, on arrival — `GuiMessage.splitLines` from
  `ChatComponent.addMessageToDisplayQueue` — and this is the equivalent: the
  cache is owned by `WindowApp::chat_wrap` (the `HudFrame` is rebuilt every
  frame and can hold no state), keyed by the display text plus the box width
  and pose scale, and cleared wholesale when either changes.
  **Gotcha: if the wrap starts depending on a new input, that input must join
  the key** — otherwise the cache serves a stale layout, which looks like a
  wrap bug rather than a cache bug. A `chat_wrap: None` frame (every hermetic
  test) wraps from scratch, so a test never observes the cache unless it
  attaches one.
- **Gotcha (stale, kept for the shape of the mistake)**: `chat_pose_scale` used
  to be `HUD_TEXT_SCALE * chat_options.scale`, folding an ad-hoc HUD-wide 2×
  pitch into the option. Fixed: `chat_pose_scale(opts)` is `opts.scale` alone
  now (`crate::hud::chat_pose_scale`), matching vanilla's
  `ChatComponent.getScale` with nothing layered on top — see
  `docs/hud-text-scale.md` for the fuller history. The lesson survives the
  fix: a *shared* ambient scale constant is exactly what lets an unrelated
  surface's correction silently move this one, or vice versa — prefer a
  surface's own named constant over reaching for one already in scope.
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
