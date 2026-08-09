# Commands

## What it is

The client's half of vanilla's Brigadier command UX (issue #46): a decode
target for the server's whole command tree, and the tab-completion /
syntax-highlighting engine that walks it against the chat box's input line.
This covers **joining** a vanilla server and typing against its tree, not
hosting one — the server-side dispatcher is issue #48, and a plugin's own
command registration is issue #118; both are explicitly out of scope here
and designed to share an argument-type library with this work rather than
duplicate it.

## How it works

### The tree model — `lodestone_model::command_tree`

`lodestone-model` owns the version-free, decode-target shape: [`CommandTree`],
[`RawCommandNode`], [`NodeKind`] (`Root` / `Literal` / `Argument` /
`Unrecognized`), and [`ArgumentParser`] (one variant per 26.2's
`minecraft:command_argument_type` registry id, with payload for the seven
parsers that carry one — numeric bounds, a `StringType` ordinal, entity/
score-holder flags, a `time` minimum, or a registry key). See that module's
own doc for the exact wire citations (`ClientboundCommandsPacket.java`,
`ArgumentTypeInfos.java`, and each parser's own `Info` class, all under
`.cache/mc/26.2/client-src`).

Two [`lodestone_model::event::ClientEvent`] variants carry this crate's
decode: `CommandTreeUpdated { tree: Box<CommandTree> }` for the whole tree
(`minecraft:commands`, clientbound id 16), and
`CommandSuggestionsReceived { id, start, length, suggestions }` for a reply to
a serverbound `command_suggestion` request (`minecraft:command_suggestions`,
clientbound id 15). Both route `SHELL` in `event::route` — the same shape as
`BiomeRegistryNames`: a registry-generation table with one obvious consumer
and no per-entity/per-session scalar to fold.

`lodestone-model` cannot decode the packet bytes itself — it has no protocol
dependency by design. The decode lives in `crates/protocol/v770/src/adapter.rs`
(issue #470), which is the only family that implements it:

- `decode_command_tree` reads `ClientboundCommandsPacket`'s private constructor
  order — **the node list first, the root index last** — and each node as
  `readNode` writes it: a flags byte, a VarInt child-index array, the redirect
  index only when `FLAG_REDIRECT` (`0x08`) is set, then the type-dependent stub.
- `read_argument_parser` reads the thirteen payload-carrying
  `ArgumentTypeInfo`s. Everything else is a `SingletonArgumentInfo` whose
  `deserializeFromNetwork` consumes nothing, so falling through to
  `ArgumentParser::from_registry_id_no_payload` reads zero bytes and is correct
  rather than a guess.
- `decode_command_suggestions` reads three VarInts then a list of
  `(String, Optional<Component>)`.

**The unknown-parser rule is a deliberate divergence from vanilla, and it is
the safer one.** `ClientboundCommandsPacket.read` bails out of the *whole node*
the moment `BuiltInRegistries.COMMAND_ARGUMENT_TYPE.byId` returns `null` — after
consuming the name and id, but without consuming the payload or the
custom-suggestions id — which leaves its own reader mid-node and corrupts every
entry that follows. We instead assume "no payload" (true for 44 of the 57 ids),
still consume the custom-suggestions id when the flag claims one, and mark the
node `NodeKind::Unrecognized { parser_id }`. A datapack or mod argument type we
do not model therefore costs one unusable node instead of the entire tree.

### Why the gates use captured server bytes

The tree is a self-describing, variable-length node stream with **no per-node
length prefix**, so a single wrong payload width does not error — it silently
reinterprets every following node. `decode(encode(x)) == x` is worthless here:
two symmetric misunderstandings satisfy it.

So `crates/protocol/v770/tests/live_command_tree.rs` (feature `live-commands`,
`#[ignore]`d) captures the real thing from the flat creative oracle and checks
it in under `tests/fixtures/`:

| fixture | what it is |
|---|---|
| `command_tree_creative.hex` | a real 26.2 server's `minecraft:commands` payload — 30 248 bytes, 2 017 nodes |
| `command_suggestions_gamemode.hex` | that same server's reply to a real serverbound `command_suggestion` for `/gamemode ` |

`Reader::ensure_empty` landing exactly on the last byte of a 30 kB, 2 000-node
walk is the end-to-end evidence that every payload width is right. Measured
control: making `minecraft:time` read one spurious leading flags byte desyncs
the stream immediately and the decode fails with a half-eaten identifier
(`invalid command suggestions provider key inecraft:ask_server`).

The `command_tree_creative.hex` fixture is **not byte-stable** across captures —
`enumerateNodes` orders nodes by a BFS over a hash map, and the joining player's
permission level decides which nodes are sent at all. The hermetic siblings
therefore assert structure and completion behaviour, never a byte count.

### The completion gate, and why it is not in the protocol crate

`crates/lodestone-shell/tests/command_tree_completion.rs` is the gate that
matters: a tree can decode perfectly, every wire link green, and still yield no
suggestions — the connected-wire-carrying-a-wrong-value failure
`cargo xtask connectedness` structurally cannot see. It lives in the shell
because that is the only crate linking both `lodestone_registry::adapter_for_protocol`
(the same call the live client makes) and `chat::complete`.

Its expected values come from **outside this tree**: the same live session that
captured the tree also asked the server for `/gamemode ` suggestions and got
`start=10 length=0 texts=["adventure", "creative", "spectator", "survival"]`.
`complete()` must independently produce that exact list, in that order, by
walking the tree and applying `GameType`'s own value set — two different
mechanisms landing on the same four strings.

### The chat-box engine — `lodestone_shell::chat`

`highlight(tree, line)` and `complete(tree, line)` are pure functions (no
window, no client handle) sharing one internal walker (`parse_line`) that
consumes the input left to right: a literal child matches by exact text; an
argument child is read per its `ArgumentParser` (a greedy phrase or `message`
argument swallows the rest of the line; a quoted phrase reads to its closing
`"`; everything else reads to the next space) and validated where the
grammar is simple enough to check locally — numeric bounds, `bool`, and the
small fixed-domain parsers in `local_domain` (`bool`, `operation`,
`entity_anchor`, `gamemode`, `team_color`, `scoreboard_slot`, each sourced
from that type's own vanilla `listSuggestions`). The first token that
matches nothing ends the walk: everything from there to the end of the line
is `HighlightKind::Unparsed`, and `complete` offers nothing past that point.

`complete` returns one of three things:

- `Completion::Local` — computed entirely from the tree (literal children by
  prefix, plus any `local_domain` argument).
- `Completion::NeedsServer` — the position needs the round trip: any argument
  node carrying a `suggestions` provider id, or any argument parser outside
  `local_domain`'s small fixed set (entity selectors, resource-registry
  types, score holders, block/item predicates, NBT, …). See the module doc
  in `chat.rs` for exactly why this is a **safe over-approximation**: vanilla
  answers some of these locally from session/world state this crate
  deliberately doesn't hold (`chat.rs` stays pure), and asking the server
  instead is slower than vanilla but never wrong — the server's own
  Brigadier dispatcher computes the same merged suggestion set.
- `Completion::None` — not a command, a prior token already failed, or the
  current position has no reachable children.

**The trailing space is data, not whitespace.** A token that runs all the way
to the cursor with no space after it is *still being typed*, so completion is
offered by its **parent**, filtered by the token as a prefix — `parse_line`
returns via `still_typing` rather than advancing into the matched node. This is
vanilla's behaviour, not a simplification:
`CommandContextBuilder.findSuggestionContext(cursor)` only takes its
`range.getEnd() < cursor` branch when the cursor is strictly *past* the parsed
range; sitting exactly on the end it returns
`SuggestionContext(prev, nodeRange.getStart())`. So `/gamemode` suggests
`gamemode`, and only `/gamemode ` suggests the four game modes.

Getting this wrong is silent, and it was: the walker advanced into the matched
node, which made every fully-typed command name complete to its own arguments
and stopped any half-typed name completing to itself. It was invisible to the
hermetic unit tests because every one of them completes a line ending in a
space; the real-server gate found it on the first run. It is the same class of
defect as a `canonicalize` that `.trim()`s the line. Gated by
`command_tree_completion.rs`'s
`a_trailing_space_decides_between_finishing_a_token_and_starting_the_next`,
which asserts **both** halves.

`SuggestionRequests` tracks the one in-flight serverbound round trip: a
monotonically increasing transaction id, and a reply is honoured only when
its id matches the request currently pending (mirroring vanilla's
`ClientSuggestionProvider::completeCustomSuggestions`) — a stale reply, from
a request the input has since outgrown, is dropped rather than applied.

### The keystroke path — what actually presses Tab

Everything above was once an island: `complete` and `SuggestionRequests` had
**no production caller**, and `menu/render/screens.rs`'s
`command_block_frame(state, tree)` was called only with `tree: None`. The chain
now runs end to end:

| link | where |
|---|---|
| decode | `crates/protocol/v770` → `ClientEvent::CommandTreeUpdated` / `CommandSuggestionsReceived` |
| fold | `net::forward`'s two arms → `net::CommandTreeCell` |
| **every edit** | `app::menus::handle_chat_key` → `WindowApp::refresh_command_suggestions` → `ChatInput::update_command_info(tree)` |
| Tab | `handle_chat_key`'s `KeyCode::Tab` → `ChatInput::tab(tree, shift)` |
| arrows / Escape | `handle_chat_history_key`, `handle_chat_key` → `ChatInput::suggestion_up`/`_down`/`_escape` |
| pointer | `app::lifecycle`'s three `is_chat_open` arms → `WindowApp::suggestion_row_under_cursor` → `ChatInput::suggestion_hover`/`_click`/`_scroll` |
| round trip | either seam returns the `ClientAction::CommandSuggestion`; `app::menus::pump_command_suggestions` polls the cell and calls `ChatInput::apply_suggestions` |
| draw | `app::redraw` fills `HudFrame::chat_suggestions` → `hud::suggestion_layout` → `hud::draw_command_suggestions` |
| command block | `try_use` → `MenuNav::set_command_tree` → `key_command_block`'s Tab → `CommandBlockState::apply_completion` |

### The dropdown — `hud::draw_command_suggestions` and `chat::SuggestionsList`

A port of vanilla's `CommandSuggestions.SuggestionsList`, not a design. The
parts that are easy to get subtly wrong, all of them transcribed:

- **The list appears while typing, not only on Tab.** The seam is
  `ChatInput::update_command_info` — vanilla's `EditBox` responder
  (`ChatScreen.onEdited` → `CommandSuggestions.updateCommandInfo`). Every edit
  path in `handle_chat_key` must call it; a fourth edit site added without it
  silently reverts to Tab-only and no `cargo check` can see that.
- **A line the player has not edited shows nothing.** `allow_suggestions`
  (vanilla's `allowSuggestions`) is false after `ChatInput::set` and after a
  history recall, so opening chat with a seeded `/` does not dump the root
  command list on screen.
- **Tab does two different things.** With the popup up it commits the
  highlighted row; with it down it *shows* the list and edits nothing (reachable
  because `ChatScreen.init` calls `setAllowHiding(false)`). `tab_cycles` — set
  only by a commit, cleared by an arrow — is what makes the *first* Tab commit
  without moving and every later one cycle first. Shift reverses the cycle.
- **The arrows browse without editing.** They move the highlight and the grey
  ghost preview (`EditBox.setSuggestion`); the line is untouched until a commit.
- **The window is capped at `SUGGESTION_LINE_LIMIT` (10) rows** with a separate
  scroll `offset`. `cycle`'s two scroll branches are asymmetric on purpose:
  wrapping from row 0 to the last row jumps `offset` straight to its ceiling.
  The wheel moves `offset` and **not** the selection.
- **Escape hides the popup and consumes the key**, so a second Escape is what
  closes the box — `CommandSuggestions.keyPressed` runs before `ChatScreen`'s
  own handling.
- **Placement is `anchorToBottom`, always above the input line.** It is a
  constructor flag, not a "fits above?" test: `ChatScreen` passes `true` and
  `AbstractCommandBlockEditScreen` passes `false` (a fixed `y = 72`). There is no
  fallback direction to implement.

Deliberately left: vanilla's grey **usage box** (`extractUsage`, the
`commandUsage` lines shown when there is no list), which needs Brigadier's
`getSmartUsage` over a whole subtree — a second walker, not a draw. The tooltip
panel is flat rather than `TooltipRenderUtil`'s gradient. And *dynamic*
mid-typing suggestions are unreachable rather than unbuilt: a client only asks
the server when a node declares a suggestion provider, and this project's server
declares none, so against our own server the popup shows static tree suggestions
only. Against a real vanilla server (126 provider ids in its own tree) the round
trip runs.

Geometry lives in `hud.rs`, not `chat.rs`, because it needs glyph advances —
`chat.rs` deliberately owns no font metrics. `hud::suggestion_layout` is the one
expression, and `HudRenderer::suggestion_layout` is how the pointer hit-test
reaches it with the same font, so a click cannot land on a row the player is not
looking at.

Two properties are worth keeping when this changes:

- **The reply's own `start` decides where the text lands**, not a re-derived
  local offset — a correct list at the wrong offset overwrites the wrong span.
  A `start` outside the line it answers is rejected, never clamped.
- **`apply_suggestions` is safe to poll every frame**: the id match consumes
  the pending request, so the second poll of the same response is stale by
  construction. That is what lets the frame loop read the cell like every other
  `net` cell rather than needing a queue. It **raises the dropdown and does not
  edit the line** — vanilla's async reply ends in `showSuggestions(false)`, and a
  reply that rewrote the line under a player still typing is exactly what the
  popup exists to avoid.

Tab reaches `handle_chat_key` at all because `input::resolve_key`
short-circuits on `gate.chat_open` before any gameplay binding — the
player-list binding is on the same physical key.

**Known gap, measured while wiring this**: `Screen::CommandBlockEdit` is drawn
**nowhere**. `render::frame_for` has no arm for it (it is an overlay screen,
like `Paused`/`Death`), `nav::on_screen_frame` has no arm either, and
`app/redraw.rs` has overlay blocks for pause, death and in-world settings but
not for this one — so `command_block_frame` still has no production caller and
the screen's clicks never hit-test. The tree, the Tab key and the suggestion
popup rows on that screen are therefore correct-but-dark until that fourth
overlay block exists; the chat box is the half of #471 that reaches pixels
today.

### The redirect-cycle guard

A redirect is a **same-position jump**, not a token-consuming one, so a
server-sent redirect cycle (`execute run` redirecting back toward the root
is a real vanilla shape) could hang naive tree code with no tokens left to
bound the recursion. The one guard is
`CommandTree::effective_children` — a visited-node set — and both `chat.rs`
functions call it instead of reading `children`/`redirect` directly. See
that function's own doc and both crates' `effective_children_terminates_on_a_redirect_cycle`
/ `complete_and_highlight_terminate_on_a_redirect_cycle` tests for the
control proving the guard actually fires (a genuine two-node cycle, not a
hypothetical one).

## How to change it

- **Adding a payload-carrying argument type**: add the variant in
  `lodestone_model::command_tree::ArgumentParser`, the id to
  `ArgumentParser::has_network_payload`, and a branch to
  `read_argument_parser` in `crates/protocol/v770/src/adapter.rs`. Read the
  type's own `deserializeFromNetwork` — not `serializeToNetwork`, and not a
  summary of it. Then **re-capture the live fixture**, because a width that is
  wrong in both directions still round-trips.
- **The one remaining gap: the tree never reaches the shell.** The decode is
  done and gated, but `net::forward` has **no arm** for either
  `CommandTreeUpdated` or `CommandSuggestionsReceived`, and `event::route`
  sends both to `SHELL`. So `forward`'s catch-all `debug_assert!` — which reads
  `route(other).must_forward()` — **will fire on a debug-build join to a real
  server**. Release builds are unaffected. What is needed, in order:
  1. A `net::CommandTreeCell` / `SharedCommandTree` matching `BiomeNameCell`'s
     shape exactly (that pattern is what `ClientEvent::CommandTreeUpdated`'s
     own doc points at), threaded into `forward` alongside `biome_names`, plus
     the two arms folding into it and returning `Ok(())`.
  2. `menu/render/screens.rs`'s `command_block_frame` already takes
     `tree: Option<&CommandTree>` and threads it into `state.completions(tree)`
     — it just needs the cell's contents instead of the `None` every caller
     passes today. That is the shortest path to real pixels, now that #47
     landed and the command-block edit screen actually opens.
  3. The chat box's Tab key: `app/menus.rs`'s `handle_chat_key` swallows Tab in
     its `_ => {}` arm, and `menu/nav.rs`'s command-block `MenuKey::Tab` arm is
     a documented no-op "with no command tree ever reaching this client yet".
     Both comments are now the only thing that is stale. The call shape is
     `chat::complete(&tree, input.as_str())`, branching on `Completion::Local`
     (show immediately) vs `Completion::NeedsServer` (call
     `SuggestionRequests::request`, send the resulting `ClientAction`, and route
     `ClientEvent::CommandSuggestionsReceived` into `SuggestionRequests::receive`).
- **Add an argument type's local domain**: `local_domain` in `chat.rs`. Only
  add an entry when the *entire* vanilla suggestion set is a small fixed
  list independent of world/session state — check the type's own
  `listSuggestions` in `.cache/mc/26.2/client-src/net/minecraft/commands/arguments/`
  first. Getting this wrong in either direction is not silently unnoticed:
  too broad and highlighting/completion tests fail (they assert exact
  lists); too narrow and the position over-defers to the server, which is
  the correct fail-safe, not a bug.
- **Gotcha**: `StringKind`'s enum-ordinal order (`SingleWord`,
  `QuotablePhrase`, `GreedyPhrase`) and `is_unquoted_string`'s allowed
  charset are Brigadier-library knowledge, not sourced from this session's
  `.cache/mc/26.2` decompile (that tree has no `com.mojang.brigadier`
  sources) — flagged in both modules' doc comments. If a future session
  finds Brigadier's own source, that is the place to re-verify against.
- **Gotcha**: at most one argument child per node is tried, with no
  backtracking across several viable ones. Every real vanilla tree checked
  so far has at most one argument alternative per literal branch, so this
  has no observed effect — but a tree that genuinely branches into two
  argument types at the same position would only ever see the
  first-registered one.

## Configuration

None. The tree and its suggestions are pushed by the server; there is no
client-side option that changes how either is interpreted.

## Dependencies

- `lodestone_model::command_tree` — the decode-target types this module
  walks; no protocol or shell dependency in either direction.
- `lodestone_client::ClientAction::CommandSuggestion` — already defined and
  encoded by the v770 adapter (`crates/protocol/v770/src/adapter.rs`); this
  work is the first thing that constructs it outside the protocol crate,
  closing the "constructed nowhere" island the original issue named.
- `crate::chat::ChatInput` — the input line `complete`/`highlight` are called
  against. Both take the current line as a plain `&str` rather than owning
  it, matching `ChatInput`'s "cursor is always at the end" invariant
  (`docs/chat.md`).
