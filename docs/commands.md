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

**This crate cannot decode the packet bytes itself.** `crates/protocol/**` is
owned by a different session (input-verb actions were in flight there while
this landed), so the actual `if packet_id == play::clientbound::COMMANDS`
adapter arm — and the matching arm for `COMMAND_SUGGESTIONS` — is a **named
follow-up**, not built here. What exists is everything the arm needs to
construct: `CommandTree::new(nodes, root)` takes exactly
`ClientboundCommandsPacket`'s own `(entries, rootIndex)` shape, index-for-index.

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

`SuggestionRequests` tracks the one in-flight serverbound round trip: a
monotonically increasing transaction id, and a reply is honoured only when
its id matches the request currently pending (mirroring vanilla's
`ClientSuggestionProvider::completeCustomSuggestions`) — a stale reply, from
a request the input has since outgrown, is dropped rather than applied.

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

- **Wire the actual decode** (the named follow-up): add a `COMMANDS`/
  `COMMAND_SUGGESTIONS` arm to `crates/protocol/v770/src/adapter.rs`'s
  clientbound dispatch, next to `PLAYER_INFO_UPDATE`'s. `COMMANDS`'s wire
  shape (`ClientboundCommandsPacket`) is a `List<Entry>` then a root VarInt;
  each `Entry` is a flags byte, a VarInt array of child indices, an optional
  VarInt redirect (`flags & 8`), and — for literal/argument types — a name
  and (for arguments) a parser id plus that parser's own network template.
  `ArgumentParser::has_network_payload`/`from_registry_id_no_payload` in
  `lodestone-model` tell you which ids need extra bytes read; everything
  else is `SingletonArgumentInfo` (zero payload). `COMMAND_SUGGESTIONS` is
  three VarInts (`id`, `start`, `length`) then a `List<Entry>` of
  (UTF-8 string, optional NBT-component tooltip). Lift both into the two
  `ClientEvent` variants this doc names above.
- **Wire the chat box's Tab key and draw**: `app.rs` (keyboard input) and
  `hud.rs` (the suggestion popup and the grey/red inline highlighting) are
  both out of this session's file ownership — send exact lines plus a few of
  anchor rather than editing them directly. The call shape is:
  `chat::complete(&tree, input.as_str())`, branching on `Completion::Local`
  (show immediately) vs `Completion::NeedsServer` (call
  `SuggestionRequests::request`, send the resulting `ClientAction`, and wait
  for `ClientEvent::CommandSuggestionsReceived` to reach `SuggestionRequests::receive`).
  The tree itself needs a shell-owned cell (matching `net::BiomeNameCell`'s
  shape) fed by `ClientEvent::CommandTreeUpdated` — that cell does not exist
  yet either; see the follow-up issue this landed with.
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
