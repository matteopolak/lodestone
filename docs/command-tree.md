# Command tree

## What it is

`crates/lodestone-command` is a standalone, **ECS-free, version-free** library
for Brigadier-style command trees: root/literal/argument nodes with redirects
and an `executable` flag, parsers and completion-suggesters for Brigadier's
primitive argument types, and a registry for a plugin to add its own. It
depends on nothing else in this workspace and nothing else in this workspace
depends on it yet — see "Why this is a sanctioned island" below.

## How it works

`CommandTree` (`crates/lodestone-command/src/node.rs`) is a flat arena of
`Node`s addressed by an opaque `NodeId`. Each node is `Root`, `Literal { name
}`, or `Argument { name, argument_type }`, plus an `executable: bool`, an
optional `redirect: Option<NodeId>`, and — unconditionally, on every node —
`permission: Option<NodeId>` (see "The unconsumed `permission` field" below).
Build one with `add_literal`/`add_argument`/`set_executable`/`set_redirect`.

`CommandTree::parse` (`src/parse.rs`) and `CommandTree::suggest`
(`src/suggest.rs`) are direct restatements of
`com.mojang.brigadier.CommandDispatcher`'s `parseNodes`/`execute` and
`getCompletionSuggestions`, ported from brigadier 1.3.10 with no
`CommandSource` and no command callback (there is nothing to call — see
below). `crates/lodestone-command/src/reader.rs`'s `StringReader` is the same
port at the character level: `read_int`/`read_long`/`read_float`/`read_double`/
`read_bool`/`read_string`/`read_string_until`, including two easy-to-miss
details that change *where* an error is reported rather than just whether one
fires — an out-of-range number resets the cursor to the **start** of the
token before raising the error, not its end, and an invalid escape inside a
quoted string points at the escaped character itself, one position earlier
than where the reader's cursor actually is when it notices.

`crates/lodestone-command/src/argument.rs` has the six built-in
`ArgumentType`s (`IntegerArgument`, `LongArgument`, `FloatArgument`,
`DoubleArgument`, `BoolArgument`, `StringArgument` with its `Word`/`Quotable`/
`Greedy` kinds) plus `ArgumentTypeRegistry`, a name-keyed lookup table a
plugin populates with its own `ArgumentType` implementations (issue #119's "a
way for a plugin to register a custom `ArgumentType` with the same two
functions" — `parse` and `suggest`). A tree never needs the registry to use a
type; `add_argument` takes an `Arc<dyn ArgumentType>` directly, and the
registry exists purely so a plugin can share one named type across several of
its own command declarations.

### Why this is a sanctioned island

This crate has **zero consumers today**, by design, not by oversight. Three
are expected and none of them exist yet:

- **#48** — the server-side Brigadier dispatcher. This crate has no
  `CommandSource` and no execution semantics; `executable` is a bare flag
  with nothing attached to run.
- **#46** — the client command UX (autocomplete, inline highlighting).
  **As of this writing, #46 already has its own, separate, decode-oriented
  tree model** — `lodestone_model::command_tree` (`CommandTree`,
  `RawCommandNode`, `NodeKind`, `ArgumentParser`) plus
  `lodestone_shell::chat`'s `highlight`/`complete`, documented in
  [`docs/commands.md`](./commands.md). That work targets decoding
  `COMMANDS`/`COMMAND_SUGGESTIONS` off the wire and driving the chat box from
  the result, and it explicitly deferred sharing an argument-type library
  with #48/#118 rather than build one — this crate is that library, landed
  afterward. **The two are not integrated.** Reconciling them (whether
  `chat.rs`'s local argument validation should eventually delegate to
  `lodestone_command::argument`'s parsers, and whether a decoded wire tree
  should build a `lodestone_command::CommandTree` instead of its own) is
  unresolved and is not attempted here — flagged on issue #46 rather than
  decided unilaterally, since `lodestone-shell`/`lodestone-client`/
  `lodestone-model` are all outside this crate's ownership.
- **#118** — plugin command registration. Its own issue text says the plugin
  registry and #48's dispatcher "should share rather than diverge" — this
  crate is that shared argument-tree substrate for both.

### `COMMANDS`/`COMMAND_SUGGESTIONS` decode status (verified, not assumed)

Checked directly rather than taken on the issue text's word:
`grep -rn "clientbound::COMMANDS" crates/protocol/v770/src/adapter.rs` and the
same for `COMMAND_SUGGESTIONS` both return **zero hits**, even though the
packet id constants exist in the generated tables
(`crates/protocol/v770/src/generated/packet_ids.rs:172-173`, ids 15 and 16).
Every protocol family in this workspace has **zero decode** for both packets
today. This crate adds none — `crates/protocol/**` was out of scope here
regardless of that finding, and (per the note above) #46's own work is
already the one building the decode-consuming side.

### The unconsumed `permission` field

Every `Node` carries `pub permission: Option<NodeId>`
(`CommandTree::set_permission`). **Nothing reads it.** It is present from day
one specifically so that issue #122's per-node permission check has somewhere
to land without changing every node constructor's signature when it arrives.
Treat any future PR that "adds" a permission field to `Node` as a sign this
doc has gone stale, not as legitimate new work.

## How to change it

- **Adding a built-in argument type**: implement `ArgumentType` in
  `argument.rs` (object-safe: `parse(&self, &mut StringReader) ->
  Result<ParsedValue, ParseError>` and an optional `suggest`), following the
  existing numeric types' pattern of resetting the reader's cursor to the
  argument's own start before returning a bounds error — that is what makes
  the reported position match the oracle instead of pointing at the end of
  the token.
- **Widening `ParsedValue`**: it is deliberately a plain enum (`Integer`,
  `Long`, `Float`, `Double`, `Bool`, `String`, `Custom(String)`) rather than
  `Box<dyn Any>`, so it stays `PartialEq`/`Debug`/testable without forcing
  every custom type to carry those bounds too. A future consumer needing a
  richer payload (e.g. #48 wanting a resolved player UUID rather than a raw
  name string) should widen this enum rather than reach for `Any` — the
  parser/reader logic and the value representation are deliberately
  decoupled, and `Any` would only benefit one future caller at the cost of
  making every existing match arm fallible.
- **The redirect-cycle guard** (`parse.rs`'s `after_match`): read the doc
  comment there before touching it. The short version: Brigadier's own
  separator-consumption gate (`reader.canRead(redirect == null ? 2 : 1)`,
  collapsed here to one `can_read()` check) already bounds recursion depth by
  the input's length for *any* tree shape — an ordinary redirect cycle cannot
  actually hang. The extra `(node, cursor)` visited-set exists only to catch
  a custom `ArgumentType` (a plugin's, via #119's registry) that moves the
  reader's cursor backward and defeats that bound from outside `parse`'s
  control. `crates/lodestone-command/tests/brigadier_spec.rs` has both a test
  that exercises this with such an adversarial type and a control proving
  the guard doesn't misfire on an ordinary, well-behaved repeated redirect
  (the `/execute ... run <command>` pattern).
- **Known simplifications versus real Brigadier** (documented in
  `src/lib.rs`'s crate doc, not repeated here in full): `parse` tries
  ambiguous argument-child candidates in insertion order and takes the first
  success rather than Brigadier's full "prefer a complete parse among every
  simultaneously-successful candidate" resolution, and the separator-gate
  collapse above is slightly more permissive than Brigadier's own
  `canRead(2)` case for a non-redirect child with exactly one trailing
  separator and nothing after it. Neither has mattered for any tree this
  crate's own tests or its three named future consumers are expected to
  build; widen `parse_nodes` rather than the crate doc's claims if that
  changes.

## Configuration

None — the crate has no feature flags, no environment variables, and (see
`Cargo.toml`) no dependencies at all, including no dependency on any protocol
crate, `lodestone-ecs`, or `lodestone-model`.

## Dependencies

None (deliberately). `crates/lodestone-command/Cargo.toml` has an empty
`[dependencies]` table. The crate is reachable automatically: the root
`Cargo.toml`'s `members = ["crates/lodestone-*", ...]` glob already covers
`crates/lodestone-command` without any manifest edit — checked directly with
`cargo metadata --no-deps` before assuming otherwise.
