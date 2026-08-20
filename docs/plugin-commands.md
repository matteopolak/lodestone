# Plugin commands

## What it is

The extension point a third-party plugin uses to register its own commands: a
`CommandRegistry` resource it populates in `Plugin::build`, an argument tree per command with
built-in and custom argument types, a permission node on any node gating it and its whole
subtree, tab completion, and a dispatcher that resolves an input string and runs a handler
with `&mut World`.

Closes the registration API, argument types and tab completion, and per-command-node
permission checks. Lives at `crates/lodestone-ecs/src/commands.rs`, built
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

### Permission gating

Any node can carry a permission. A denied node is invisible **together with its entire
subtree**, which is vanilla's actual semantics, not a shortcut. From
`Commands.fillUsableCommands`:

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

### Argument types

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

## Current wire boundary

v770 decodes serverbound `CHAT_COMMAND`, and the server sends its built-in command tree. A host
that installs `CommandDispatch::installed(..)` can therefore drive a plugin root from a real
client frame; `crates/protocol/v770/tests/command_wire_path.rs` proves that complete wire path,
including permission refusal and a no-sink refusal.

Integrated singleplayer installs the shell's ECS-backed `CommandSink` on its local duplex
connection on both native and browser builds, so direct plugin roots and terminal
`/execute ... run <plugin>` reach this registry in the shipped local path. The server remains
free of an `lodestone-ecs` dependency; the browser uses the portable items-plus-commands
constructor because it has no server tick loop.
Open-to-LAN peers, RCON, console and command blocks do not receive that client registry.

## Why the registry is here and not in `lodestone-server`

The original design note said the registry should be "server-side, since that is where
command *execution* semantics live". Right about semantics, wrong crate, for a reason that
note could not have known:

- **`lodestone-server` deliberately does not depend on `lodestone-ecs`** and says so in its
  own manifest. It links neither bevy nor this crate.
- **There is no plugin API on the server at all.** Every plugin seam in this workspace is a
  `bevy_app::Plugin` added to `lodestone_app::client_app()` (see
  [plugin-registration.md](plugin-registration.md)). A registry inside `lodestone-server`
  would be unreachable by every plugin that can currently exist.

So it lives where the plugin API lives. It is a plain `Resource` with no client-specific
state, so a future server `App` — or the server-side Brigadier dispatcher — inserts the same
resource and calls the same `dispatch`. Nothing here knows which side it is on.

## Why the two command-tree representations stay separate

An earlier design question asked whether `crates/lodestone-command` and
`lodestone_model::command_tree` should be
reconciled, explicitly allowing "duplication is the right call, in which case this
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

**The real duplication was narrower than first supposed, and it was not the node model.** It
was `crates/lodestone-shell/src/chat.rs`'s hand-rolled argument semantics: `validate_simple`
duplicating `lodestone_command::argument`'s
`IntegerArgument`/`LongArgument`/`FloatArgument`/`DoubleArgument` numeric-bounds checks, and
`read_quoted` duplicating `StringReader.readQuotedString`'s `\"`/`\\` escape handling and a
separate charset check duplicating `StringReader.isAllowedInUnquotedString`'s
`[0-9A-Za-z_\-.+]` set.

**That fix has since landed.** `crates/lodestone-shell/src/chat.rs` now depends on
`lodestone-command`; `validate_simple` delegates per `ArgumentParser` variant to the matching
`ArgumentType::parse` through a local `parse_ok` helper, and `read_quoted` delegates to
`lodestone_command::StringReader::read_string` instead of reimplementing the escape handling.
The standalone charset-checking function this section used to name is gone — the delegation
made it unnecessary, confirmed directly against the current source rather than assumed. The
behaviour the fix had to preserve — `chat.rs` returns a validity bool plus span offsets and
must tolerate a half-typed last token, where `lodestone_command` returns
`Result<ParsedValue, ParseError>` — is what `parse_ok` and `read_argument_token` exist to
bridge; see their own doc comments in `chat.rs`.

**One thing changed since this question was first raised, and it matters:** at the time, both
representations were test-only islands. `lodestone-command` now has a real consumer (this
module). `lodestone_model::command_tree` was long the one still waiting on a producer — no
protocol family decoded `COMMANDS` and `ClientEvent::CommandTreeUpdated` was constructed
nowhere — but that has since changed too: `v770`'s adapter now decodes `COMMANDS` and
`COMMAND_SUGGESTIONS` (see [commands.md](commands.md)), so both representations are now
load-bearing rather than one waiting on the other. Re-verify before citing either as an
island — confirmed directly against the current source rather than assumed here.

## How to change it, and the gotchas

- **A handler is stored per `NodeId` in a side table on `RegisteredCommand`, not on the tree
  node.** `lodestone-command` has no execution model on purpose (the server-side Brigadier
  dispatcher will want to define one differently). Keep handlers out of it. The cost is that `on_execute` must set `executable`
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

## `/execute` interop

`CommandSource` still represents a direct plugin root as just its original
`PermissionSubject` and display name. It now also has an optional
`CommandExecutionContext` for a terminal server `/execute ... run <plugin>`:
entity identity, position, rotation, dimension, anchor and permission level.
The registry continues to gate permissions through `source.subject`; a host maps the rewritten
executor entity to that subject, so `execute as bob` is checked as Bob rather than as the
connection that typed the command.

The server boundary is value-only (`ContextualCommandRequest` and
`ContextualCommandResponse`), never an ECS `World`. The response carries the plugin handler's
integer result so the server dispatcher can pass it unchanged to `/execute store result` and
convert a refusal to the normal `store success = 0` outcome. `CommandSink::run_contextual` has a
default `UNKNOWN_COMMAND` refusal; an older host therefore remains safe and direct roots continue
to use `run` unchanged.

The focused adapter in `crates/protocol/v770/tests/command_wire_path.rs` is the reference shape:
lock the host-owned `World`, map the request into `CommandSource::contextual`, and call
`dispatch`. It proves the actual plugin handler sees rewritten context and that `store result`/
`store success` preserve its integer success/failure outcomes. The production shell uses the
same mapping against its `EcsHandle`, but only for the local integrated duplex.

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
- [commands.md](commands.md) — the *client's* Brigadier UX, the other representation.
- [plugin-registration.md](plugin-registration.md) — how a plugin gets into the `App`.
- [roadmap/plugin-framework.md](roadmap/plugin-framework.md) — the capability audit.
