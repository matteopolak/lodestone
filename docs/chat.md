# Chat

## What it is

The chat box: the outbound input line, the received scrollback, and the HUD draw that renders both,
including in-line editing (caret, selection, history, word motion) and interactive text (clickable
links, hover tooltips, command suggestions). This covers the client-side rendering and editing surface;
the wire path for composing an outbound line and the log itself live in `lodestone-game`.

## How it works

### Input line and caret

The chat input is a real `EditBox` (see [`ui-framework.md`](./ui-framework.md) for the shared edit-box
mechanics), not a bare string — which is what gives it a movable caret, a selection, word-wise motion,
Home/End, and clipboard operations, all ported directly from vanilla's own text-field behavior rather
than written bespoke for chat. There is no leading `>` glyph; vanilla's chat input never draws one.

The caret has two shapes, chosen by whether the caret sits strictly before the end of the line (or the
line is at its length cap): a one-pixel vertical bar when inserting mid-line, or a literal drawn `_`
character when appending at the end. Position is measured from the width of the text before the caret,
not the whole line — measuring against the whole line (or against `{text}_` including the caret's own
glyph) is a real, previously-shipped bug, since a trailing underscore has no width of its own in
vanilla's model. The caret blinks at vanilla's own fixed interval, driven by wall-clock time rather than
a tick count, matching how every other transient render flag reaches the HUD frame.

An inline greyed-out autocomplete "ghost" — the top suggestion candidate — draws just before the caret,
under three rules transcribed from vanilla: its position is measured from the text before the caret
alone (the caret contributes no width of its own to that measurement, even though this HUD's append
caret is a drawn character); it draws in the order text, then ghost, then caret, so the caret composites
over the ghost's leading glyph; and it's suppressed whenever the line is at its length cap or the caret
isn't at the very end, since a mid-line caret has no sensible completion to extend.

### Sent-line history and player-name completion

Up/Down recall previously **sent** lines, oldest to newest, capped and deduplicated against immediate
repeats and whitespace-normalized before comparison. The history buffer belongs to the input component
itself (not the screen), so reopening chat still walks everything sent earlier in the session; both
ends of history clamp rather than wrap, and a partially-typed line is stashed and restored around a
history excursion so browsing history never destroys unsent text. Recording into history only happens
on an actual send, never on a cancelled (Escape'd) line.

Tab-completion for an ordinary (non-command) chat line is answered entirely locally from the online
player list — no server round trip, so it works even against a server that never sent a command tree.
It must complete against **every** online player, not just those visible in the tab-list overlay (the
two are different, deliberately unfiltered-vs-filtered projections over the same roster — see
[`hud.md`](./hud.md)). Matching is "is a prefix of a segment delimited by `._/`", not a substring
match, and word-boundary detection for both history and tab-completion breaks only on whitespace, never
on punctuation — from the end of `say hi:there`, one backward word-skip lands right after `hi`, not
after the colon, which is a subtly-wrong-feeling rule easy to get wrong without a fixture built
specifically to expose it. Suggestions complete against the text up to the caret, not the whole line,
since those can differ once the caret can move; the popup's own selection-splice logic accounts for
this so accepting a suggestion mid-line only replaces the completed word, not the whole input.

Chat's Tab and the in-world player-list overlay's Tab share one key and cannot steal from each other:
whichever context is open handles the key, by construction of the input-gating layer rather than by any
special-cased binding.

### Command-tree traversal

The packet decode boundary initially has flat integer node positions, but `CommandTree::new` validates
them before the shell sees the tree. Chat starts at `CommandTree::root_id` and advances only through
`CommandNodeId` values returned by `effective_children_from`; it looks nodes up with `node_for`. This
keeps a command-node position distinct from a byte offset in the input, a packet id, or another raw
integer. A handle describes a position, not ownership of a particular tree, so chat keeps it only for
one synchronous walk and never carries it over a command-tree replacement.

### Word wrap and scrollback layout

Received lines are greedily word-wrapped against real font metrics (the same metrics the draw itself
uses, so layout can never be computed against a different font than the one that renders), breaking on
a space when the next word would overflow and hard-breaking a single overlong word character by
character. A formatting code seen before a wrap point carries onto the continuation line. A wrapped
message's rows stack with its *last* visual row nearest the bottom edge, matching vanilla.

The scrollback's bottom edge and the input box's own vertical position are two independent constants in
vanilla, not one derived from the other — coupling them (deriving the scrollback's bottom from the
input strip's own top edge) silently collapses a real, by-design ~26-logical-pixel gap between the
newest message and the input box down to almost nothing, which is exactly the shape of bug this once
shipped. Both surfaces also sit at a real vanilla horizontal text inset (4 logical pixels), independent
of the shared HUD margin used elsewhere — and this inset has to stay in sync across four separate
things that must never drift apart: the draw itself, the background plate (which pads a different
amount than the text column), the suggestion popup's anchor, and the hover/click hit-test region.
Getting only one of the four right reads as "the chat looks slightly off" rather than as a null result.

Scrolling moves by whole logical entries (not wrapped visual rows), clamps rather than wraps, does not
snap back to the newest message on arrival while the player has scrolled up, and resets on close — all
matching vanilla's own scrollback behavior. The scrollbar only appears once there's more history than
fits the visible window, and reads its position from the same scroll state the scrolling logic itself
exposes rather than a second, independently-derived count.

### Chat display options

A handful of persisted options shape the chat surface, mirroring vanilla's own chat settings: overall
scale (see [`hud.md`](./hud.md)'s note that this is the *entire* scale factor with no additional
HUD-side multiplier on top), box width and height while focused/unfocused, extra line spacing, text and
background opacity, and whether legacy color codes are stripped. At every option's default value the
draw is byte-identical to having no options system at all. Message-visibility filtering (system vs.
chat-only) and arrival rate limiting are deliberately not modelled — the former needs a per-message
source tag the log currently discards before it reaches the HUD, and the latter isn't a rendering
concern at all.

### Interactive text: links, hover tooltips, and click actions

A chat message's click and hover events, and its shift-click insertion, are carried as an additional
inheriting property of each text span — exactly how color and formatting already inherit down a text
tree, including across a legacy-code split. Hit-testing reuses the exact same wrapping and layout
functions the draw itself calls, so a resize or an options change can never leave a click or hover
target aimed at stale geometry.

**Style field names differ by protocol era, and getting this wrong is silent.** Modern protocols spell
the two event fields in snake case; older ones spell them in camel case, and the argument of a click
action is named for the action (`url`, `command`, `path`, and a *numeric* `page`) rather than being a
string under a single `value`. Both spellings and both argument shapes are read, newest first. A
mismatch here does not produce an error — it produces a message with no interactivity at all, which
looks identical to a message that never had any.

**Hover payloads are typed, one shape per action.** `show_text`'s payload is a component; `show_item`'s
is an item stack (an id, a count and a component patch); `show_entity`'s is a type, a UUID and an
optional name. Modelling all three as a single component field is a lossy decode in disguise: a payload
compound carries no `text` and no `translate` key, so parsing one as a component yields an *empty* node
— an item hover then paints nothing at all. The one exception is the oldest `show_item` form, whose
payload really was a component holding serialised item data; a payload with no readable item id keeps a
component payload for exactly that case.

An item hover's tooltip body is gathered by the **same function an inventory slot's tooltip uses**, so
one stack cannot read two different ways on two surfaces, and it honours the player's own
advanced-tooltips option for the same reason. An entity hover composes its three documented lines —
name, type (through the entity type's own description key, resolved against the language table), UUID.
What is not reproduced is the item tooltip's non-text furniture: no bundle grid, no icon, no nine-slice
sprite frame, because this box paints from the HUD's untextured colour stream.

A **shift-click** is a mode rather than a modifier of the click: with shift held the run's insertion
text goes in at the chat caret and the click action is deliberately *not* run; without it, the reverse.
So a sender name carrying both an insertion and a whisper command never does both, and a shift-click on
a run with no insertion is inert rather than falling through. Insertion goes through the
caret-respecting insert, so a half-typed line survives the gesture, and the chat text filter and length
cap still apply to server-authored text.

Every server-supplied "open URL" click goes through an untrusted-link confirmation prompt before ever
opening a browser; there is no silent-open path. A "run command" click sends exactly as if the player
had typed it and pressed Enter; "suggest command" only fills the input without sending; "copy to
clipboard" and "open file" are supported and explicitly unsupported respectively, with the latter
surfaced as a local message rather than touching the filesystem.

Server-list links and chat `open_url` values become `ServerLinkUrl` wrappers around `url::Url` at their
respective network/UI ingress boundaries. Malformed values and non-web schemes such as `javascript:`
or `file:` never enter the confirmation state, while a confirmed value stays typed until the final
platform handoff calls `ServerLinkUrl::as_str`. The confirmation remains necessary: syntactic URL
validity says nothing about whether a server-authored destination is trustworthy.

## How to change it

- **Add a new editing operation to the shared `EditBox`, not to the chat input directly.** A second,
  chat-specific implementation beside the shared one is worse than not having the feature.
- **A key producer must distinguish "consumed" from "edited."** A key that only moved the caret must
  not fall through to plain text insertion, and must not trigger a fresh suggestion request either —
  folding those two outcomes into one boolean gets one of them wrong.
- **The platform edit-shortcut modifier (Cmd on macOS, Ctrl elsewhere) is resolved once, centrally** —
  never hardcode Ctrl for copy/cut/paste/select-all, and never fold the platform modifier onto the
  generic "control" bit, since that would also turn an ordinary Ctrl+arrow into a word-skip on macOS,
  which vanilla does not do.
- **If you change the horizontal text inset, change it in all four places it appears** — the draw, the
  background plate's own (different) padding, the suggestion popup's anchor, and the click/hover
  hit-test bound — or the surfaces silently drift apart.
- **The scrollback's bottom-edge constant and the input box's top-edge constant are independent** —
  never derive one from the other.
- **Selection highlighting and the caret draw must derive their pixel positions from the same width
  measurement the glyph draw uses**, clamped to both the string and the visible input strip, so a stale
  or out-of-range selection can never emit an invalid fill.
- **Keep command-tree wire indices at the adapter boundary.** Consumers should use
  `CommandTree::root_id`, `effective_children_from`, and `node_for`; only an encoder or diagnostic
  should call `CommandNodeId::index`. Do not retain a `CommandNodeId` after replacing its tree.

## Configuration

Chat display options (scale, width, height while focused/unfocused, line spacing, text/background
opacity, color-code stripping) are persisted alongside other non-default settings, degrading silently to
vanilla defaults when unset. The input length cap (256 characters) is a fixed constant, matching
vanilla's own chat box.

## Dependencies

- `crates/lodestone-shell/src/chat.rs` — the input model, history, tab-completion and suggestion state.
- `crates/lodestone-shell/src/hud.rs` — the chat block of the shared HUD draw (layout, wrap, scrollback,
  caret, selection).
- `lodestone-game::chat` — the received message log this renders from.
- `lodestone-model::text` — the styled-text tree, including the click/hover event fields threaded
  through interactive spans.
- `crate::menu::edit_box::EditBox`, `crate::menu::focus` — the shared text-editing and key-translation
  machinery every other text field in the shell also uses (see [`ui-framework.md`](./ui-framework.md)).
- `crate::platform::clipboard` — copy/cut/paste; degrades gracefully (empty read, fire-and-forget write)
  on platforms without a synchronous clipboard API.
- The 26.2 jar under `.cache/mc/26.2/client-src` — behavioral reference only, never transliterated.
