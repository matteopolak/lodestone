# Server-side command execution

## What it is

The path a `/command` typed by a player takes from a serverbound `chat_command`
frame to a registered plugin command's handler and back as system chat — and,
more importantly, the **seam** that path has to cross, because the crate that
receives the frame is deliberately forbidden from linking the crate that
dispatches it. Issues [#48] (the Brigadier dispatcher server-side) and [#464]
(the wire gap that kept any command from reaching a dispatcher).

## How it works

Five components, three of which already existed and were unreachable:

| component | crate | role |
|---|---|---|
| `lodestone-command` | standalone, **zero dependencies** | the Brigadier argument tree: `CommandTree`, `Node`, `ArgumentType`, `parse_filtered`, `suggest_filtered` |
| `CommandRegistry` + `dispatch` | `lodestone-ecs` | where plugins register, and the `&mut World` dispatcher |
| `Permissions` | `lodestone-ecs` | the five-level 26.2 permission model (`PermissionLevel`, `Permission`, grants, groups) |
| **`CommandSink` / `CommandDispatch`** | `lodestone-server` | **the seam** — this document's subject |
| `ServerBound::ChatCommand` decode | `crates/protocol/v770` | lifting the wire frame into the version-free vocabulary |

The flow, once a host has installed a dispatcher:

```
client types "/beacon light"
  -> ClientAction::SendCommand              (lodestone-model)
  -> V770Adapter::encode_action             -> play::serverbound::CHAT_COMMAND (id 7), body = one string, no leading '/'
  -> V770ServerProtocol::decode             -> ServerBound::ChatCommand { command }
  -> server.rs dispatch_play_packet         -> CommandDispatch::run(&CommandCaller, &command)
  -> host's CommandSink impl                -> lodestone_ecs::commands::dispatch(&mut World, &CommandSource, input)
  -> registered handler runs, mutates the World
  <- CommandResponse::Ran { feedback } | Refused { message }
  <- ServerProtocol::encode_system_chat     -> play::clientbound::SYSTEM_CHAT per line
  <- client decodes ClientEvent::Chat
```

### Why there is a seam at all

`lodestone-server` **must not depend on `lodestone-ecs`**, and that is a
measured decision recorded in its own `Cargo.toml`: linking it would drag the
client vocabulary (`LocalPlayer`, `FrameClock`, `SessionMenus`) plus
`lodestone-physics`/`-game`/`-world` into the graph, and into the browser
bundle, which links `lodestone-server` and links neither today.

But the registry has to live in `lodestone-ecs`, because that is where the
plugin API lives — a registry in `lodestone-server` would be unreachable by
every plugin that can exist, which is why [#118]'s own body was unbuildable as
written.

So the inbound path crosses exactly the boundary the server crate exists to
avoid. [#464] listed three ways out:

| option | verdict |
|---|---|
| 1. `lodestone-server` gains a `lodestone-ecs` dependency | **rejected** — contradicts the boundary, and the cost is already measured |
| 2. a callback/queue seam the host installs | **taken** |
| 3. dispatch moves server-side, registry mirrored across the seam | **rejected** — reintroduces the duplication [#435] declined to create, and leaves plugins registering into a registry dispatch does not read |

Option 2 inverts the dependency: `lodestone-server` declares a trait in
ECS-free vocabulary, and the **host** — a crate that legitimately links both —
implements it. That matches the intent doctrine the rest of this seam already
uses (pre-computed answers handed across, never a query back).

### The vocabulary, and why it is shaped that way

`crates/lodestone-server/src/command.rs`:

```rust
pub struct CommandCaller { pub uuid: Uuid, pub username: String }

pub enum CommandResponse {
    Ran { feedback: Vec<String> },
    Refused { message: String },
}

pub trait CommandSink: Send + Sync {
    fn run(&self, caller: &CommandCaller, command: &str) -> CommandResponse;
}

pub struct CommandDispatch { /* Option<Arc<dyn CommandSink>> */ }
```

Nothing here names a `World`, a `Resource`, a packet id or a protocol number.
That is the whole point: the moment this trait names an ECS type, the boundary
is gone.

`&self`, not `&mut self`, because several connection tasks may call it. The
implementor needs `&mut World` and therefore an interior `Mutex` — that is
deliberately the *implementor's* problem, since making it this crate's problem
would mean this crate knowing what a `World` is.

`Refused` deliberately cannot distinguish a permission denial from a parse
failure from an unknown command. Distinguishing them would require this crate
to know what a permission is.

### The two security properties

**1. No sink installed means nothing runs.** `CommandDispatch::default()` holds
no sink and answers `UNKNOWN_COMMAND` without consulting anything. An absent
dispatcher must never read as blanket permission. This mirrors
`dispatch_refuses_rather_than_ungates_when_permissions_are_missing`
(`crates/lodestone-ecs/tests/plugin_command_registry.rs:492`) one layer out: a
missing *resource*, and now a missing *sink*, both refuse.

**2. The caller identity cannot be influenced by the command text.** The
`CommandCaller` is built in `serve_connection_inner` at the Play handoff from
the uuid `login_success` echoed to this client and the username that passed
`is_valid_player_name`. A sink therefore cannot be aimed at resolving a
different player's permissions.

The wire layer *cannot* itself check a permission — it has no `Permissions`
resource and by the boundary above never will. What it enforces is the identity
and the fail-closed default; the check itself happens in `dispatch`, via
`lodestone-command`'s `parse_filtered`, which fails loudly with
`ParseErrorKind::NoPermission` (whereas `suggest_filtered` prunes the subtree
silently, so tab completion never reveals a command you cannot run).

### Entry points

`serve_connection_with_commands` is the only entry point that takes a
`CommandDispatch`. Every other `serve_connection*` passes the inert
`CommandDispatch::none()`, so their wire bytes are unchanged.

This is a **new entry point rather than a changed signature** on purpose, and it
is the established pattern here (`serve_connection_with_block_ticks`,
`serve_connection_with_mob_events` exist for the same reason): a large number of
`crates/protocol/v770/tests/*` call the older ones directly, and every added
parameter would break all of them.

## How to change it

**Adding a host-dispatched packet kind.** Add a method to `CommandSink` **with a
default body that refuses**, so an existing host impl keeps compiling and an
un-updated host fails closed rather than open. The obvious next candidate is
`custom_payload` → `lodestone_ecs::plugin_message`, which has the same shape:
its consumer also structurally cannot live in `lodestone-server`.

**Do not generalise this to the other stranded serverbound variants.**
`cargo xtask connectedness` reports 43 `v770` serverbound variants that decode
into `ServerBound::Ignored`. Almost all of them — `SWING`, `INTERACT`,
`MOVE_PLAYER_ROT`, `PLAYER_ABILITIES` — are stranded because the *gameplay* is
unimplemented, and their consumer belongs in `lodestone-server`. Routing those
through a host callback would move server behaviour out of the server. The axis
this seam generalises along is narrow and specific: **packets whose consumer
lives in the plugin API.**

### Gotchas

* **`CHAT_COMMAND` was never in the stranded 43.** It was one of the nine that
  did not decode at all. Those are different fixes, and the issue body does not
  distinguish them — check which case a packet is in before assuming.
  Measured delta for this work: `serverbound decoded 60/69, connected 17/69` →
  `61/69` and `18/69`, with `decodes-to-Ignored-only` unchanged at 43.
* **`CHAT_COMMAND_SIGNED` is deliberately not decoded.** Its body carries a
  timestamp, salt, per-argument signatures and a last-seen acknowledgement
  block, none of which we have a session key to verify. A client only sends the
  signed form for arguments the server declared signable in a `COMMANDS` tree we
  do not yet send, so in practice every command from a real client arrives
  unsigned. If a `COMMANDS` encoder ever lands, this becomes reachable and must
  be handled.
* **`ServerProtocol::encode_system_chat` defaults to emitting nothing**, like
  every other optional encoder. Its failure mode is silent rather than loud: the
  command still *runs*, the player just never learns what happened. A family that
  wants commands must implement it.
* **26.2 has five permission levels, not four**, and no longer a bare number:
  `PermissionLevel::{All, Moderators, Gamemasters, Admins, Owners}`. [#127]'s
  body says four and is wrong; `crates/lodestone-ecs/src/permissions.rs` already
  transliterates the five — use it rather than re-deriving.
* **`lodestone_model::command_tree` and `lodestone-command` are not the same
  thing and must not be merged.** One is a flat, wire-shaped *decode target*
  keyed by registry id that tolerates unknown ids; the other is an arena-based
  *construction API* with `dyn ArgumentType` as behaviour. [#435] kept both
  deliberately.
* **`crates/lodestone-shell/src/.../chat.rs` still carries three hand-rolled
  copies of Brigadier reader/parser logic** (`validate_simple`, `read_quoted`,
  `is_unquoted_string`) which should delegate here. Not done — see *Known gaps*.

## Configuration

None. No env vars, no feature flags. Whether commands work at all is decided by
one thing: whether the host called `serve_connection_with_commands` with a
`CommandDispatch::installed(..)` rather than the default.

## Known gaps

**Nothing installs a sink in production yet, so no real player can run a command
today.** The seam, the decode, the dispatch and the gate are all in place and
proven end-to-end; what is missing is host wiring, in three files owned by other
work:

1. `PluginCommandsPlugin` is added in **zero** production code paths (only
   tests), so no production `World` even holds a `CommandRegistry`. The place to
   add it is `crates/lodestone-shell/src/sim/build.rs:127`.
2. `IntegratedServer::open_in_memory_with_mobs`
   (`crates/lodestone-server/src/integrated.rs:358`) — the production
   singleplayer constructor — has no way to accept a `CommandDispatch` and calls
   `serve_connection_with_mob_events_shared` internally.
3. `crates/lodestone-shell/src/net.rs:1531` is the one place a `World` handle and
   the `IntegratedServer` are simultaneously in scope, so it is where the sink
   would be constructed and installed.

Also open: **`/execute` and its subcommand chain** ([#123]) needs a context
object; **command blocks** do not tick; **selectors** (`@a`/`@e` and their
filters) are unimplemented; **functions and datapacks** are unimplemented. Those
are the rest of [#48], and all of them are now unblocked rather than blocked —
they have a wire to arrive on.

## Dependencies

* `lodestone-command` — the argument tree. **Keep its `[dependencies]` empty**;
  that property is what made adding it to `lodestone-ecs` risk-free.
* `lodestone-ecs` — `CommandRegistry`, `dispatch`, `Permissions`,
  `PluginCommandsPlugin`. Reached only through the seam, never linked by
  `lodestone-server`.
* `lodestone-server` — `CommandSink`, `CommandDispatch`, `ServerBound::ChatCommand`,
  `serve_connection_with_commands`.
* `crates/protocol/v770` — the only family implementing `ServerProtocol`, so the
  only one that can host commands at all.

[#48]: https://github.com/matteopolak/lodestone/issues/48
[#118]: https://github.com/matteopolak/lodestone/issues/118
[#123]: https://github.com/matteopolak/lodestone/issues/123
[#127]: https://github.com/matteopolak/lodestone/issues/127
[#435]: https://github.com/matteopolak/lodestone/issues/435
[#464]: https://github.com/matteopolak/lodestone/issues/464
