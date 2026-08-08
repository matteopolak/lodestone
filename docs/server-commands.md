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

### The built-in tree, and the island it used to be

**The seam above is the *plugin* path. It is no longer the only path, and it is
no longer the first one consulted.** `lodestone_server::commands::ServerCommands`
holds the server's own Brigadier tree, and `crate::server`'s `ChatCommand` arm
asks it before it asks the host sink.

That ordering is the whole fix for a defect worth recording precisely, because it
is this repo's dominant class in its purest form. `ServerCommands` existed with
`/gamerule` fully built and tested — and `grep ServerCommands` outside its own
file returned **zero hits**. Its module doc asserted that this arm consulted it.
The assertion was stale: the arm ran a hand-rolled `parse_gamemode_command` string
split and handed everything else to a sink that every real constructor leaves
empty. So **`/gamerule` typed by a player did nothing at all**, its own tests were
green, and nothing in the tree was red. `rcon.rs` was worse: it called the host
sink *only*, so RCON bypassed the built-ins entirely.

Two call sites close it — the `ChatCommand` arm and `rcon.rs::run_command` — and
the precedence at both is:

| outcome | meaning | the caller does |
|---|---|---|
| `Some(outcome)` | a built-in root matched | send its lines, apply its effects; **do not** consult the host |
| `None` | nothing at the root matched | fall through to `CommandDispatch` |

`None` is keyed on `ParseErrorKind::UnknownCommand` specifically, which the tree
produces only when *no token matched at the root at all*. `/gamerule nonsense`
therefore reports its parse error rather than becoming a plugin's problem — the
alternative tells a player the command does not exist when only their argument was
wrong.

#### The three pieces underneath

**Typed argument keys.** `Registrar::arg` returns `(NodeId, ArgKey<A::Value>)`
together, and an `ArgKey<T>` exists *only* as the return value of the call that
created its node. There is no string to typo and no class to get wrong, which is
strictly stronger than Brigadier's `getArgument(name, Class)`. Three runtime
panics remain, all registration bugs that fire on the **first execution** of a
command: a key from another tree, a key naming a node deeper than the executing
one, and an `McArg` whose `Value` disagrees with what its parser produced. The
third is the only real seam and is documented on `McArg` itself.

**Modifiers and forks, built before `/execute` needs them.** A `NodeId`-keyed
modifier table plus a fork set, and a dispatch walk that threads the source set
through every modifier on the path and then runs the deepest executor **once per
surviving source**. A failure aborts an unforked path but only its own branch when
forked, which is what stops `execute as @a run give @s …` at the first player whose
inventory is full. Nothing in production drives this yet — that is why
`crates/lodestone-server/tests/builtin_commands.rs` does, through
`ServerCommands::from_registrar`. Building it now is the reason a port was chosen
over a signature-driven macro: a function signature is a list, and the vanilla
command set is a graph.

**Effects, which are forced rather than stylistic.** `game_mode` and `inventory`
are `dispatch_play_packet`'s own `&mut` parameters. An executor is a shared `Arc`
closure inside a process-wide tree and physically cannot write either, nor reach
`proto` or `conn`. So an executor emits typed `Effect`s and exactly one place
applies them:

| target | path |
|---|---|
| the caller's own connection | `server.rs::apply_own_effect`, inline through `proto` |
| any other player | a **directed, drained** per-uuid queue on `PlayerRegistry` |

The second is the genuinely new mechanism. Chat is the precedent but chat is a
*broadcast*: every connection reads every line through its own cursor. An effect
must reach one player, once, so it is keyed by uuid and **taken** rather than
cursored — a cursor over a shared log would hand Steve's game-mode change to
everybody.

#### One tree, three consumers

Execution (`parse_filtered` → executor table), suggestion (`suggest_filtered`) and
the wire projection (`ServerCommands::wire_tree`) all read one `CommandTree`.
`Registrar::arg` records the node's `ArgumentParser` **in the same call that
installs its parser**, from one `McArg` value, so there is no second place a
node's wire identity could be stated differently. The failure that guards against
is specific: a client that autocompletes something the server then rejects.

**Nothing sends the projection yet.** No protocol family here has a `COMMANDS`
(id 16) *encode* arm. Tab completion against the server's own commands does not
work end to end; what works is that the projection is gated, per command, against
a real vanilla server's own tree.

#### The commands, and how they are gated

`/gamerule` (one literal per rule, so the value's type comes from the rule's own
spec), `/gamemode` and `/give`, each read off the decompiled 26.2 source rather
than from memory. Per-command parity is asserted against
`crates/protocol/v770/tests/fixtures/command_tree_creative.hex` — 30,248 bytes and
2,017 nodes captured from a real vanilla 26.2 server — comparing node kinds,
names, parser variants *including payload flags*, executable bits, restricted
bits, redirect topology and suggestion ids, recursively and in child order. The
gate carries its own control: pointing the comparison at two different subtrees
must panic.

Three shapes the fixture settled that a reconstruction gets wrong:

* `/gamemode`'s mode slot is **one `minecraft:gamemode` parser node, not four
  literals**. The four-literal shape is from a much older version.
* `/give`'s `<targets>` comes **before** `<item>` — the opposite of the English
  reading.
* An optional trailing argument is **two executable nodes on one path**, not one
  node with an `Option<T>` parameter. Both `<item>` and `<count>` are executable
  on the wire; an `Option`-shaped design transmits one.

#### Permission levels

Every built-in root is gated at its vanilla level (2 for all three today) through
`Registrar::require_level`. `lodestone-command`'s permission seam is a dotted
*string*, because that crate cannot know what a permission is, so a level is
encoded as `lodestone.level.N` and read back by `commands::level_filter`. An
unrecognised permission string fails **closed**.

The level itself is resolved **once**, at the Play handoff, from the connection's
own authenticated uuid — never from anything in the command text, the same property
`CommandCaller` exists for. RCON's caller is level 4, matching vanilla's
`RconConsoleSource` (`Commands.LEVEL_OWNERS`).

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
* **Nothing in production calls `AccessLists::set_owner`.** This document's own
  neighbourhood and `access.rs`'s module doc both claim `server.rs` passes an owner
  for the in-memory constructors; that claim is **stale**, measured by
  `grep set_owner crates/lodestone-server/src`. Every connection therefore resolves
  to `permission_level == 0`, so gating a command at its vanilla level 2 makes it
  unreachable in singleplayer. `AccessLists::command_permission_level` is the
  answer: level 4 when *no* operator model is configured at all (no owner **and**
  no ops), collapsing to `permission_level` the moment a host ops anybody — which
  is vanilla's LAN behaviour and the posture the empty default already documented.
  Discovered by the wire gate going red on seven commands at once, not by review.
* **The built-in tree was pointed at the wrong game-rule store.** The old
  `CommandEffects` took a `&GameRulesHandle`, but the production rules live inside
  `WorldStateHandle`. Even fully wired, `/gamerule` would have written a store
  nothing else reads — an island *behind* an island. Hence the `RuleStore` trait
  with both implementors, and `IntegratedServer::start_rcon` substituting its own
  shared handle over whatever the caller put in the config, so a host cannot get it
  wrong.
* **26.2's `/gamemode` accepts the four full names and nothing else.**
  `GameType.byName` is an exact match against `getSerializedName`. The deleted
  `parse_gamemode_command` accepted `c`/`1`/`sp` as well, i.e. it was *more*
  permissive than vanilla — and its own test asserted that permissiveness, so
  nothing was ever red. A faithfulness bug in this direction is invisible to
  testing: it only ever makes a command work that should have failed.
* **`CommandWorld::rules` is `&(dyn RuleStore + Sync)`, not `&dyn RuleStore`.** A
  `CommandWorld` is held across an `await` on a spawned connection task, and a
  `&dyn Trait` is only `Send` when the trait is `Sync`. Three `integrated.rs` spawn
  sites report this, not the module under test.
* **A selector resolves in the order filter → sort → truncate.** Any other order
  makes `sort=nearest,limit=2` return two arbitrary players sorted among
  themselves, which looks plausible in any test with fewer than three candidates.
* **`@s` is exempt from `EntityArgument`'s players-only check.** Vanilla's
  condition is `includesEntities() && playersOnly && !isSelfSelector()`, and `@s`
  sets `includesEntities = true` because the caller might not be a player. Without
  the exemption `/gamemode creative @s` is refused — the single most-used form of
  the command.

## Configuration

None. No env vars, no feature flags. Whether commands work at all is decided by
one thing: whether the host called `serve_connection_with_commands` with a
`CommandDispatch::installed(..)` rather than the default.

## Known gaps

**Built-in commands work in production now.** `/gamerule`, `/gamemode` and
`/give` are reachable by a real player over a real wire with **no host sink
installed**, which is the shipping configuration —
`crates/protocol/v770/tests/builtin_commands_wire_path.rs` is the gate, and it
reads the effect back as a real `game_event`/`container_set_slot` the real client
decoded rather than as a chat line.

**Plugin commands still have no production sink**, so the paragraph below is about
the *plugin* half only. What is missing is host wiring, in three files owned by
other work:

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

Still open, and each is now additive rather than blocked:

* **The `COMMANDS` (id 16) encoder.** The projection exists and is gated; nothing
  transmits it. Until it lands, tab completion against the server's own commands
  does not work, and `CHAT_COMMAND_SIGNED` stays unreachable (see *Gotchas*).
* **`/execute`** ([#123]). The modifier/fork substrate it needs is built and
  gated; what is left is the subcommand tree itself.
* **A textual SNBT parser.** `ItemArg` v1 refuses a `[…]` component patch by name
  rather than dropping it, because no textual SNBT parser exists anywhere in this
  tree (`read_component_patch` is *wire* decode, a different problem). Since
  `minecraft:item_stack` carries no wire payload, the node, the autocompletion and
  `/give minecraft:diamond_sword 3` are all complete now, and the later unit
  replaces exactly one match arm.
* **Deferred selector options.** `scores`, `nbt`, `advancements`, `predicate`,
  `tag`, `team`, `level` and the two `*_rotation` options are refused **by name**
  rather than ignored — a silently widened selector is the worst available
  failure. Each needs a subsystem that does not exist (a scoreboard, entity NBT,
  the advancement predicate engine, entity tags, experience levels, per-entity
  rotation tracking). **None of it is visible on the wire**: `minecraft:entity`
  carries one flags byte and no option list, so deferring options cannot break
  tree parity.
* **`/gamerule` does not have full subtree parity.** Vanilla registers two
  literals per rule (`keep_inventory` *and* `minecraft:keep_inventory`); we
  register one. Closing it is one extra `literal` call. Separately, our
  `GAME_RULES` offers `max_minecart_speed`, which vanilla's tree omits because it
  is behind `FeatureFlags.MINECART_IMPROVEMENTS` — our table carries no
  feature-flag concept. Both are pinned by the parity gate.
* **`PlayerInventory::add` caps every stack at 64** regardless of item, as its own
  doc comment says. `/give` splits by the item's *real* max stack size before
  handing stacks over, so `/give @s diamond_sword 3` produces three
  single-item stacks — but `add` may then merge them into one slot of 3. That is a
  pre-existing inventory limitation, not a command one.
* **Command blocks** do not tick; **functions and datapacks** are unimplemented.

## Dependencies

* `lodestone-command` — the argument tree. **Keep its `[dependencies]` empty**;
  that property is what made adding it to `lodestone-ecs` risk-free. It now also
  carries `ParsedValue::Dyn(Arc<dyn AnyValue>)`, the structured-payload variant the
  typed-key API needs.
* `lodestone-command-mc` — `McArg` plus `GameModeArg`, `EntityArg` (the selector
  grammar and its AST), `Vec3Arg`/`BlockPosArg` and `ItemArg`. Separate from
  `lodestone-command` because an argument type that knows what an item *is* cannot
  live in a crate that depends on nothing. Names no protocol number: `McArg::wire`
  returns the *symbolic* `lodestone_model::command_tree::ArgumentParser`, and the
  numeric registry ids stay in `lodestone-data` and the version crates — which is
  what keeps the version seam (`cargo check -p lodestone-shell
  --no-default-features`) intact.
* `lodestone_server::commands` — `ServerCommands`, `Registrar`, `ArgKey`, `Ctx`,
  `CommandSource`, `Effect`, and the wire projection.
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
