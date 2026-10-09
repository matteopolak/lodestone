# Commands: the tree, dispatch, permissions, and access control

## What it is

One argument-tree data model serves three consumers: the server's built-in command dispatcher and permission model, a plugin-command seam that keeps `lodestone-server` off `lodestone-ecs`, and the client's decode of a server-sent tree for tab-completion and highlighting. Permission levels and access control (ops, whitelist, bans) gate which commands are visible or runnable.

## How it works

### The tree substrate

`lodestone-command` is a zero-dependency, ECS-free, version-free crate: a flat arena of root, literal and argument nodes, redirects, an executable flag, and a character-level port of Brigadier's parse and suggest algorithm. `executable` is a bare flag so that server dispatch, plugin registration and client decode each build behaviour on one tree shape. `lodestone-command-mc` layers Minecraft argument types (game mode, entity selectors, block and item ids, positions, scoreboard and team types) on top.

### Wire to dispatch to reply

A typed `/command` goes `ClientAction::SendCommand` to `CHAT_COMMAND` to `ServerBound::ChatCommand` to the server handler, which tries the built-in tree (`ServerCommands`: `/gamerule`, `/gamemode`, `/give`, `/execute`, `/scoreboard`, `/team`, `/tp`, `/summon`, `/weather`, and others) and only falls through to a host-installed plugin dispatcher (`CommandDispatch`) when nothing at the root matched. Replies are system chat lines.

Syntax refusals keep the parser's character cursor via `CommandResponse::refused_syntax`. `CommandResponse::chat_lines` renders a red explanation, then at most ten preceding characters in gray, the unparsed suffix in red underline, and a red italic `<--[HERE]`; longer prefixes start with `...` and clicking suggests the original command. The cursor counts characters, not bytes. `CommandResponse::lines` stays plain for RCON and console. Argument errors use `ParseErrorKind::InvalidArgument` (`InvalidBool` only for real booleans), with English messages independent of the diagnostic `Display`. Plugin registries remap cursors back to the caller's input after alias canonicalisation. The 26.2 hosted encoder preserves the components; other families use the plain-text default until they implement `ServerProtocol::encode_system_chat_component`.

The built-in tree is sent to clients (`COMMANDS`) pruned per connection by permission level, where a denied node takes its subtree, so completion shows only runnable commands. For a line whose root is not built in, the built-in suggester returns nothing and the connection can ask the plugin dispatcher for that root's permission-filtered branch.

### The plugin seam

`lodestone-server` must not depend on `lodestone-ecs` (client-only vocabulary, and the browser bundle links the server but not the client ECS), yet the plugin registry lives in `lodestone-ecs`. So the server declares a small ECS-free trait (caller identity plus command string in, ran or refused out) that the host implements. No sink means every command is refused; the wire layer enforces identity and fail-closed behaviour, never a specific permission.

[`plugin_commands`](../crates/lodestone-server/src/plugin_commands.rs) keeps a server-owned tree and permission policy independent of the ECS, with a bounded `ServerCommandQueue` handing requests to the tick owner. Value-only handlers install via `ServerCommandRegistry::into_dispatch`; world-mutating handlers stay behind the queue until the world-effect API settles. `ServerCommandQueue::drain_async` can run a value-only dispatch through the bounded scheduler, returning only from the tick-owned hand-back.

### Permission levels and access control

The five levels (`All`, `Moderators`, `Gamemasters`, `Admins`, `Owners`, 0 to 4) gate built-in roots and several administrative packets (difficulty, game rules, command-block edits, game mode). A connection's level is resolved once at the Play handoff from its authenticated identity, never from command text.

Native hosts install `access::PermissionLevelProvider` with `AccessHandle::set_permission_provider(Some(Arc::new(provider)))` before publishing or serving. It receives `PermissionLevelContext { uuid, fallback_level }`; `Some(0..=4)` overrides, `None` defers, invalid values resolve to zero. The same level gates the advertised tree, execution and administrative packets. In the integrated bridge the level travels with `CommandCaller` into the ECS sink, which writes it to the player's `Permissions` subject before resolving a plugin command, so a plugin node with the `Op` default agrees with built-in gates; grants, groups, declarations and resolvers stay in the ECS resource.

Handle clones share installation. Callbacks run after both handle locks are released, so they may call `permission_level` or replace the provider, but must not block or recurse. The provider does not change persisted ops, join bans, whitelist or player-limit bypass; removing it restores access-list resolution for future connections. A host-plugin command re-resolves the provider for its connected caller right before dispatch, so node-gated plugin commands see grants without reconnecting. Built-in gates, tree visibility and administrative packets keep their handoff level until reconnect. This delegates the five levels; it is not a node/wildcard registry.

Access control is four JSON files (`ops.json`, `whitelist.json`, `banned-players.json`, `banned-ips.json`) behind one shared cloneable handle. Join order is player ban, whitelist, IP ban, player limit, each refusal carrying its translation key. A missing file is an empty list; a malformed one is a hard error (silently reading no operators locks admins out). Bans and ops match on UUID, not name. An unparseable ban expiry keeps the ban. A world with no access lists is maximally permissive; a host opts into restriction. Files decode through closed-schema Serde records: missing fields keep historical defaults, unknown fields or wrong types are hard errors, invalid UUID entries are ignored for compatibility.

### Client decode

A server tree is a variable-length node stream with no per-node length, decoded into a version-free model keyed by wire registry ids. An unrecognised argument type marks only that node unusable (the reference decoder corrupts every following node). Tab completion and highlighting are pure functions of the tree and chat line: literal children and small fixed-domain types resolve locally; entity selectors, registry types and anything with a suggestion provider ask the server and wait.

## How to change it

- **Host-dispatched capability**: add a trait method whose default body refuses, so unupdated hosts fail closed.
- **Payload-carrying argument type**: add it to the version-free parser enum, decode it from the reference type's real network reader, and derive local completion domains from its real suggestion list.
- **Built-in command**: register on `ServerCommands` at the reference permission level; state outside the connection goes through the effect queue or shared handle.
- **Argument refusal**: use `ParseErrorKind::InvalidArgument` (scalar kinds only for that scalar), preserve the cursor, and pass the original command to `refused_syntax`.
- **Runtime access changes**: admin commands mutate the shared handle; nothing persists them, so a host wanting durability calls the save path.

### Gotchas

- An exact permission deny does not cover children; carve out a branch with the wildcard form.
- An undeclared node is held by every operator and denied to others (Bukkit's default).
- Resolution order (specificity, own subject over group, deny over allow, declared default) is not reorderable; a wrong order hides against most trees.
- A redirect is a same-position jump, so walkers guard cycles with a visited set, not only an input-length bound.
- A lower-permission player never receives a denied subtree.
- Command text is remote input and arguments can nest. Strings are capped at 32767 characters, so per-character recursion has 32767 levels. The SNBT parser behind `minecraft:nbt_tag`/`nbt_compound_tag` once aborted the process on a run of open brackets; it is now bounded at 512 levels (`snbt::MAX_NESTING`, the depth where the game refuses a structure as too complex), checked at the top of `read_value_at`, which both compound and list readers pass through. On a 2 MiB stack it walks 1024 levels but not 4096, so the bound is reachable. The refusal is `ParseErrorKind::NestingTooDeep`.
- `CommandTree::parse`'s redirect walk is iterative with an explicit heap stack (a `Vec` of pending redirect fallbacks) because each hop used to cost a call frame and a long `/execute run execute run ...` overflowed before 2048 hops. The visited `(node, cursor)` guard is still needed for custom `ArgumentType`s that rewind the cursor.

## Configuration

- The four JSON files at the server root, native only (a browser build has no filesystem).
- Whitelist enforcement is a separate flag from the file existing, off by default.
- Tree and permission shape are not configured; plugins declare nodes and defaults at registration.
- Permission providers are installed in code on the `AccessHandle`, are transient, and must be reinstalled after a restart.

## Dependencies

- `lodestone-command` (zero dependencies) and `lodestone-command-mc`.
- `lodestone-ecs` for the plugin registry and permission resource, reached only through the seam.
- `lodestone-server` for the seam, `ServerCommands` and the access store.
- The 26.2 family (hosted as 777 through the 26.3 family) for command wire encode and decode and access-driven disconnects.
