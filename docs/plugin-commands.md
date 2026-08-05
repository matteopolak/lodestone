# Plugin commands

## What it is

The extension point a third-party plugin uses to register its own commands: a
`CommandRegistry` resource it populates in `Plugin::build`, an argument tree per command with
built-in and custom argument types, a permission node on any node gating it and its whole
subtree, tab completion, and a dispatcher that resolves an input string and runs a handler
with `&mut World`.

Closes issues #118 (registration API), #119 (argument types and tab completion) and #122
(per-command-node permission checks). Lives at `crates/lodestone-ecs/src/commands.rs`, built
on `crates/lodestone-command` (see [command-tree.md](command-tree.md)) and
`lodestone_ecs::permissions` (see [permissions.md](permissions.md)).

```rust
impl Plugin for MyPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PluginCommandsPlugin);

        let mut command = PluginCommand::new("warp");
        command.description("teleport around");
        command.alias("w");

        let root = command.root();
        let admin = command.literal(root, "admin");
        command.require_permission(admin, "warp.admin");

        let amount = command.argument(admin, "amount", Arc::new(IntegerArgument::bounded(1, 64)));
        command.on_execute(amount, |invocation| {
            let n = invocation.integer("amount").unwrap_or(0);
            invocation.world.resource_mut::<MyState>().value = n;
            CommandOutcome::Success(n)
        });

        app.world_mut().resource_mut::<CommandRegistry>().register(command).unwrap();
    }
}
```

## How it works

### Registration

`PluginCommand::new(name)` creates a tree whose synthetic root has exactly one child: the
command's root literal. Subcommands and arguments hang off `NodeId`s the builder returns. The
builder is **arena-shaped rather than fluent**: Brigadier's `then(literal(..).then(..))` does
not translate to Rust without a macro or pervasive `Box<dyn>` gymnastics, and the arena form
is what lets a plugin build a tree in a loop.

`CommandRegistry::register` freezes it. It refuses two things at registration rather than
letting them fail later:

- **`NameTaken`** — a duplicate root literal or alias. Two plugins claiming `/warp` and the
  second silently winning presents as "my plugin stopped working" with nothing in any log.
- **`NoHandlers`** — no node has a handler, so the command could never do anything.

### Dispatch

`dispatch(world, source, input)`:

1. strips a leading `/`, trims, and **rewrites an alias to the canonical root** (the tree
   contains only the canonical literal — Bukkit's own behaviour);
2. parses against the command's tree with a permission filter built from `Permissions`;
3. walks the parsed path **backwards** for the nearest node with a handler;
4. runs it with `&mut World`.

Everything that reads the world happens in a block that ends before the handler runs, so a
handler is free to mutate anything — including the registry.

### Permission gating (#122)

Any node can carry a permission. A denied node is invisible **together with its entire
subtree**, which is vanilla's actual semantics, not a shortcut. From
`Commands.fillUsableCommands` (`.cache/mc/26.2/src/net/minecraft/commands/Commands.java:421`):

```java
for (CommandNode<S> child : source.getChildren()) {
   if (child.canUse(commandFilter)) {
      ...
      target.addChild(node);
      if (!child.getChildren().isEmpty()) {
         fillUsableCommands(child, node, commandFilter, converted);
      }
   }
}
```

The recursion sits **inside** the `canUse` branch, so a permitted node under a denied parent
is never reached and never sent — which is why a gated branch simply does not appear in tab
completion.

**The two halves are gated differently, on purpose:**

| operation | a denied node is… | why |
|---|---|---|
| `dispatch` | an explicit `ParseErrorKind::NoPermission` naming the node | Bukkit answers with "you do not have permission", not "unknown command" — the player needs to know the command exists and is not theirs |
| `suggest` | silently absent | vanilla never sent the node, so leaking its existence through a suggestion would defeat the gate |

Getting these the same way round is the easy mistake: a silent `dispatch` is
indistinguishable from a typo, and a loud `suggest` leaks the tree to everyone.

### Argument types (#119)

The primitives come from `lodestone-command`: `IntegerArgument`, `LongArgument`,
`FloatArgument`, `DoubleArgument`, `BoolArgument`, `StringArgument` (word/quotable/greedy).
Custom types implement `ArgumentType` and can be shared through `ArgumentTypeRegistry`.

Two Minecraft-flavoured helpers live here, because they need live state:

- **`player_argument(&PlayerDirectory)`** — parses a name, suggests whoever is in the tab
  list. **Lenient**: vanilla accepts an offline player's name in most commands, and the
  suggestion list is only who happens to be online, so a strict version would reject valid
  input the moment someone logged out mid-typing.
- **`choice_argument(["stone", "dirt"])`** — a closed set. **Strict**: a value outside the set
  fails at parse rather than reaching the handler, because a typo'd block id arriving as a
  `String` would look like a handler bug.

Both are built on `lodestone_command::ChoicesArgument`, which takes a `SuggestionProvider`
closure. `ArgumentType::suggest` receives only the partial token, so a type whose candidates
are *live state* has nowhere to read them from; a provider is a closure the caller built while
it still had access to whatever it needed, so the dependency points the right way rather than
`lodestone-command` growing an ECS-shaped context parameter.

`PlayerDirectory` is an `Arc<RwLock<Vec<String>>>` rather than a `Vec` because the provider is
built once, at plugin-build time, and must keep seeing fresh data. `sync_player_directory`
refreshes it once per `GameTick` from `SessionTabList`.

## What is NOT wired — read this before believing the gate

**No player's typed `/command` reaches `dispatch` yet, on either side.** This is the honest
boundary, and it is two separate gaps in crates outside this work's ownership:

- **Serverbound.** `lodestone-server` never decodes `CHAT_COMMAND` (serverbound id 7).
  `crates/protocol/v770/src/server_protocol.rs` has no arm for it, so it falls to
  `_ => ServerBound::Ignored` and then to an empty match arm at
  `crates/lodestone-server/src/server.rs`'s `dispatch_play_packet`. A player's command is
  dropped **twice over**, before any registry could see it.
- **Clientbound.** No protocol family encodes `COMMANDS` (clientbound id 16), so no client is
  ever sent the tree. `command_tree_for` exists so that arm has something to serialise, with
  the pruning already correct.

`crates/lodestone-ecs/tests/plugin_command_registry.rs` therefore drives the **registry** —
registering through the public plugin API exactly as a third-party plugin does, then
dispatching a real input string and asserting the world changed. What it cannot assert is the
wire hop, and no test in this crate can.

### The brokered patch shape for the serverbound gap

For whoever owns `crates/protocol/**` and `crates/lodestone-server/**`:

1. In `crates/protocol/v770/src/server_protocol.rs`, add an arm for
   `play::serverbound::CHAT_COMMAND` (id 7) decoding the packet's single `command: String`
   field into a new `ServerBound::ChatCommand { command }`.
2. In `crates/lodestone-server/src/protocol.rs`, add that variant.
3. In `crates/lodestone-server/src/server.rs`'s `dispatch_play_packet`, call
   `lodestone_ecs::commands::dispatch` with a `CommandSource::player(uuid, name)` built from
   the connection's profile, and send `CommandOutcome::Failure`/`CommandDispatchError::message`
   back as a system chat message.

Note step 3 requires `lodestone-server` to depend on `lodestone-ecs`, which its manifest
currently and deliberately refuses ("Deliberately NOT `lodestone-ecs`, despite
`docs/server-ecs.md`'s title" — it links neither bevy nor this crate). That decision has to be
revisited before the serverbound half can land, and it is a bigger architectural call than a
decode arm. **Do not treat it as a one-line wire-up.**

## Why the registry is here and not in `lodestone-server`

Issue #118 says the registry should be "server-side, since that is where command *execution*
semantics live". Right about semantics, wrong crate, for a reason the issue could not have
known:

- **`lodestone-server` deliberately does not depend on `lodestone-ecs`** and says so in its
  own manifest. It links neither bevy nor this crate.
- **There is no plugin API on the server at all.** Every plugin seam in this workspace is a
  `bevy_app::Plugin` added to `lodestone_app::client_app()` (see
  [plugin-registration.md](plugin-registration.md)). A registry inside `lodestone-server`
  would be unreachable by every plugin that can currently exist.

So it lives where the plugin API lives. It is a plain `Resource` with no client-specific
state, so a future server `App` — or #48's dispatcher — inserts the same resource and calls
the same `dispatch`. Nothing here knows which side it is on.

## Issue #435: why the two command-tree representations stay separate

#435 asked whether `crates/lodestone-command` and `lodestone_model::command_tree` should be
reconciled, explicitly allowing "duplication is the right call, in which case this issue
should close with that reasoning recorded". **That is the verdict, with one real duplication
identified and its fix specified.**

**They answer different questions, and merging would make one bad at its job:**

| | `lodestone_model::command_tree` | `crates/lodestone-command` |
|---|---|---|
| purpose | decode target | construction API |
| shape | flat `Vec<RawCommandNode>` + root index, matching `ClientboundCommandsPacket`'s `(entries, rootIndex)` index-for-index | arena with `add_literal`/`add_argument` |
| arguments | `ArgumentParser`, a ~56-variant **data** enum keyed by registry protocol id, carrying each parser's `serializeToNetwork` template | `Arc<dyn ArgumentType>` with `parse`/`suggest` **behaviour** |
| unknowns | `Unknown(i32)` / `Unrecognized { parser_id }` — a wire format must tolerate ids it does not know | no such concept; a tree is built from types that exist |
| derives | `Debug, Clone, PartialEq`, relied on for exact-list test assertions | holds trait objects, so cannot |

Making `lodestone-command` the decode target would mean adding wire payloads and unknown-id
tolerance to it, and giving it a `lodestone-model` dependency for `ResourceKey` — destroying
the zero-dependency property that lets it be a graph sink. Making
`lodestone_model::command_tree` the construction API would mean putting `dyn ArgumentType`
inside `ArgumentParser` and losing its `PartialEq`.

**The real duplication is narrower than #435 supposed, and it is not the node model.** It is
`crates/lodestone-shell/src/chat.rs`'s hand-rolled argument semantics:

- `validate_simple` (`chat.rs:390`) — four numeric-bounds checks duplicating
  `lodestone_command::argument`'s `IntegerArgument`/`LongArgument`/`FloatArgument`/`DoubleArgument`.
- `read_quoted` (`chat.rs:370`) — a second copy of `StringReader.readQuotedString`'s
  `\"`/`\\` escape handling.
- `is_unquoted_string` (`chat.rs:383`) — a second copy of
  `StringReader.isAllowedInUnquotedString`'s `[0-9A-Za-z_\-.+]` charset.

**The fix**, for whoever owns `crates/lodestone-shell/src/chat.rs`: add a
`lodestone-shell → lodestone-command` dependency (acyclic — `lodestone-command` is a sink) and
have `validate_simple` delegate per `ArgumentParser` variant to the matching
`ArgumentType::parse`, and `read_quoted`/`is_unquoted_string` to `StringReader`. **One
behaviour must be preserved:** `chat.rs` returns a validity bool plus span offsets and must
tolerate a half-typed last token (see its own comment at `chat.rs:527-551`), where
`lodestone_command` returns `Result<ParsedValue, ParseError>`. A naive delegation would make
the last token un-completable.

**One thing changed since #435 was filed, and it matters:** at the time, both
representations were test-only islands. `lodestone-command` now has a real consumer (this
module), while `lodestone_model::command_tree` still has **zero producers** — no protocol
family decodes `COMMANDS`, and `ClientEvent::CommandTreeUpdated` is constructed nowhere. They
are no longer symmetric candidates for a merge: one is load-bearing, the other is still
waiting for its decoder.

## How to change it, and the gotchas

- **A handler is stored per `NodeId` in a side table on `RegisteredCommand`, not on the tree
  node.** `lodestone-command` has no execution model on purpose (#48 will want to define one
  differently). Keep handlers out of it. The cost is that `on_execute` must set `executable`
  *and* insert into the table — it does both, and is the only way to do either.
- **Dispatch resolves the handler by walking the parsed path backwards.** `CommandTree::parse`
  already rejects a path ending on a non-`executable` node, so the walk only ever skips nodes
  that were executable-but-handlerless — a registration bug, reported as
  `CommandDispatchError::NoHandler` rather than silently succeeding.
- **Aliases are rewritten in exactly one place** (`canonicalize`). A second rewriting site is
  how alias and permission resolution start disagreeing about which command is running.
- **A missing `Permissions` resource is a hard error, never an ungated fallback.**
  `CommandDispatchError::NotInstalled`. The alternative failure mode is silent,
  security-shaped, and would only be noticed by someone who did not have the permission they
  just used. `dispatch_refuses_rather_than_ungates_when_permissions_are_missing` is the
  control.
- **`PluginCommand::new` panics on a name containing a space.** A literal with a space can
  never match — `lodestone-command` tokenizes on exactly `' '` — so it is a programming error,
  and failing at construction is far cheaper to diagnose than a command that silently never
  matches.
- **Adding a resolution step to permission gating means editing
  `lodestone_ecs::permissions`, not here.** This module only supplies the filter closure.

## Issue #123 (`/execute` interop) is **not** closed, and needs a context object

`CommandSource` is deliberately only an identity — a subject and a display name. It is **not**
a vanilla `CommandSourceStack`.

Vanilla's `/execute as <selector> at <selector> run <command>` rewrites a *context*: executor
entity, position, rotation, dimension, anchor, plus `store`/`if`/`unless` result propagation.
None of those fields exist here, and there is nothing to consume them: `/execute` itself does
not exist (issue #48, Tier 4), and the server has no command dispatch at all. Inventing the
context object now would be a context-rewriter with nothing to rewrite and nobody to call
it — an island of precisely the shape `CLAUDE.md` warns about, and #123's own body says it
"should be filed as a reminder rather than started now".

What this work gives #123 is the **seam**: `dispatch` takes the source by reference and never
reads it except to resolve permissions, so a future `/execute` can substitute a different
subject without touching the registry or the tree. Widening `CommandSource` is additive when
#48 lands.

## Configuration

None. `PluginCommandsPlugin` inserts `CommandRegistry`, `Permissions` and `PlayerDirectory`,
and registers `sync_player_directory` in `GameTick`/`TickSet::Send`.

## Dependencies

- `crates/lodestone-command` — the tree, parser and suggester. **This module is that crate's
  first consumer**; its crate doc used to declare itself an island.
- `lodestone_ecs::permissions` — resolution. See [permissions.md](permissions.md).
- `lodestone-game`'s tab list, read through `SessionTabList`, for live player-name
  suggestions.

## Related

- [command-tree.md](command-tree.md) — the substrate.
- [permissions.md](permissions.md) — the gate.
- [commands.md](commands.md) — the *client's* Brigadier UX (#46), the other representation.
- [plugin-registration.md](plugin-registration.md) — how a plugin gets into the `App`.
- [roadmap/plugin-framework.md](roadmap/plugin-framework.md) — the capability audit.
