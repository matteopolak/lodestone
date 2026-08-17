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
every plugin that can exist, which is why a design proposing that was
unbuildable as written.

So the inbound path crosses exactly the boundary the server crate exists to
avoid. Three ways out were considered:

| option | verdict |
|---|---|
| 1. `lodestone-server` gains a `lodestone-ecs` dependency | **rejected** — contradicts the boundary, and the cost is already measured |
| 2. a callback/queue seam the host installs | **taken** |
| 3. dispatch moves server-side, registry mirrored across the seam | **rejected** — reintroduces duplication the design deliberately declined to create, and leaves plugins registering into a registry dispatch does not read |

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
(`crates/lodestone-ecs/tests/plugin_command_registry.rs`) one layer out: a
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
the wire projection (`ServerCommands::wire_tree_for`) all read one `CommandTree`.
`Registrar::arg` records the node's `ArgumentParser` **in the same call that
installs its parser**, from one `McArg` value, so there is no second place a
node's wire identity could be stated differently. The failure that guards against
is specific: a client that autocompletes something the server then rejects.

#### Sending it: `COMMANDS` (clientbound id 16)

The projection now reaches a client. `ServerProtocol::encode_commands` takes the
version-free tree and `V770ServerProtocol` writes
`ClientboundCommandsPacket`'s own layout: `writeCollection(entries)` — flags byte,
`writeVarIntArray(children)`, the redirect index only under `FLAG_REDIRECT`, then
the type-dependent stub — followed by the root index. For an argument the stub is
`writeUtf(name)`, the `minecraft:command_argument_type` registry id, that parser's
`serializeToNetwork` payload, and **then** the custom-suggestions identifier; the
suggestions id coming last is the one field order no field name hints at.

All 57 registry ids are encoded — there is no stub or fallback for a parser we
declare. `ArgumentParser::Unknown(id)` writes the bare id with no payload, which
is unreachable from a decode (an unmodeled id becomes `NodeKind::Unrecognized`)
and unreachable from this crate's projection.

**The tree is per-player.** `wire_tree_for(level)` prunes exactly as
`Commands.sendCommands` → `Commands.fillUsableCommands` does: the recursion into a
child's children sits *inside* the `canUse` branch, so a denied node takes its
whole subtree with it, and every surviving child/redirect index is renumbered
against the pruned list. A level-0 player is sent a bare root today, because all
four built-ins are `Commands.LEVEL_GAMEMASTERS`.

**Where it goes in the join sequence.** `PlayerList.placeNewPlayer` reaches
`sendPlayerPermissionLevel` — which is what calls `sendCommands` — after the
abilities packet and before `sendLevelInfo`. `server.rs` sends it in that
position: after `encode_player_abilities`, before the join clock sync, and before
any chunk. The connection's permission level and its `ServerCommands` are built
there and reused by the `CommandSession` further down, rather than constructed
twice, so the tree the client is sent and the tree the session dispatches against
are the same object.

**Evidence: vanilla's own 30,248 bytes, re-encoded byte-identically.**
`crates/protocol/v770/tests/command_tree_encode.rs` decodes
`fixtures/command_tree_creative.hex` and encodes it again, requiring byte
equality with the capture. That covers 2,017 nodes, 55 of the 57 parser ids, 126
custom-suggestion ids, 85 restricted nodes and 74 redirects — none of which our
own four commands contain, and none of which we authored. Its control fires:
writing the suggestions id before the parser payload instead of after makes it
fail (measured). `brigadier:long` and `minecraft:angle` are the two ids no
vanilla command uses, so they carry their own id-and-payload-length assertions.

#### Selectors

`@s`/`@p`/`@a`/`@r`/`@e`/`@n`, a bare player name, and a bare uuid, all through
`lodestone_command_mc::EntityArg`/`EntitySelector` (`crates/lodestone-command-mc/src/entity.rs`)
and resolved against the roster by `lodestone_server::commands::source::resolve_players`.
The v1 filter set is `type`, `name`, `distance`, `limit`, `sort`, `gamemode`,
`x`/`y`/`z`, `dx`/`dy`/`dz`, `scores`, `team` — ported from
`EntitySelectorParser`/`EntitySelector` in the 26.2 decompile, each with
vanilla's `!` inversion where vanilla has one. `scores={obj=range,...}` reuses
`IntRangeArg`'s own range reader (so the selector map syntax and `/execute if
score`'s range syntax cannot drift apart) and resolves against the real
`ScoreboardHandle` via a `&dyn Fn(&str, &str) -> Option<i32>` lookup threaded
through `resolve_players`; an unknown objective or a holder with no recorded
score both refuse the match. `team=`/`team=!` resolves the identical way,
through a second `&dyn Fn(&str) -> String` closure over
`crate::commands::team_store::TeamHandle` (`""` for a holder on no team, the
same string vanilla itself compares against — no `Option` three-way). `nbt`,
`advancements`, `predicate`, `tag`, `level` and the two `*_rotation` options
are refused **by name** rather than silently ignored (see that module's own
doc for why each needs a subsystem this server does not have). None of it is
visible on the wire — `minecraft:entity` carries one flags byte and no option
list — so the deferred set cannot desync tree parity.

**Resolution only ever reaches players.** `CommandWorld` carries
`&[PlayerCandidate]`, never a general entity list, because entity resolution
would need a world this crate is structurally forbidden from depending on (see
this document's "Why there is a seam at all"). `@e`/`@n` therefore parse the
full grammar but resolve to nothing beyond the player roster — `/kill @e[type=cow]`
is legal syntax that correctly matches zero candidates, the same narrowing
`/gamemode`'s and `/give`'s `<target>` already made.

#### The commands, and how they are gated

`/gamerule` (one literal per rule, so the value's type comes from the rule's own
spec), `/gamemode`, `/give` and `/effect` were the original four; `/time`,
`/difficulty`, `/seed`, `/setworldspawn`, `/spawnpoint` (self-only), `/kill`,
`/experience` (`/xp`), `/clear`, `/setblock`, `/fill`, `/say`, `/me`, `/msg`
(`/tell`/`/w`) and `/help` (root listing only) followed next, then `/tp`
(`/teleport`), `/summon`, `/weather` and `/defaultgamemode` — the four this
document's own "Known gaps" section named as blocked on a missing mechanism,
each now built (see "The four mechanisms" below). Each reads off the
decompiled 26.2 source rather than from memory where a real tree exists to
check against, and each has one execution test in
`crates/lodestone-server/tests/builtin_commands.rs` per the registrar's own
stated bar (its own doc names the three residual runtime panics that fire on a
command's *first* execution, which is exactly what that bar catches). New
argument types back them: `lodestone_command_mc::{TimeArg, BlockArg,
EntityTypeArg}`, plus the pre-existing `Vec3Arg`/`BlockPosArg` for `~`/`^`
coordinates, reused by `/tp` and `/summon`. `/worldborder` (issue #580)
followed, and `/scoreboard` most recently.

#### `/scoreboard`

`crate::commands::scoreboard` plus the store it reads and writes,
`crate::commands::scoreboard_store::ScoreboardHandle`. `objectives
add`/`remove`/`list`, and `players set`/`add`/`remove`/`get`/`reset`/`list`/
`operation` — see that store's own module doc for what "criteria" means here
(nothing: every score changes only because a command asked it to, since no
criteria are simulated).

The store rides *inside* `crate::world_state::WorldStateHandle`, as a sibling
field next to the tick-anchor set already there, rather than as a fourth field
on `CommandWorld` threaded independently through every entry point. That is
what makes it reachable from `/execute if score` (see above), from RCON (the
same `WorldStateHandle` `RconConfig::with_world` already substitutes), and
from a command block's own tick (the same handle `run_command_block_command`
already receives) with **no new parameter anywhere** — the identical island
`/gamerule`'s own history in this document warns a *second*, disconnected
store would be.

New argument types back it, all in `lodestone_command_mc::scoreboard`:
`ObjectiveArg`, `ObjectiveCriteriaArg`, `OperationArg` (vanilla's nine
`+=`/`-=`/…/`><` tokens), `IntRangeArg` (`minecraft:int_range`, also used by
`execute if score … matches`), and `ScoreHolderArg` (`minecraft:score_holder`
— `*`, a selector, or a bare "fake player" word, the dominant real use of a
scoreboard in redstone/adventure-map contexts and the reason this is not
simply `EntityArg` reused).

**Not built:** `objectives setdisplay` and any display-slot concept (nothing
in this crate renders a sidebar, so a stored slot would be write-only), and
`players enable` (meaningless with no criteria semantics modelled at all).

#### `/team`

`crate::commands::team` plus `crate::commands::team_store::TeamHandle` — a
**separate** store from the scoreboard, matching vanilla's own `Scoreboard`
keeping objectives/scores and teams as two tables; landing `/scoreboard` did
not unlock this. `list [<team>]`, `add <team> [<displayName>]`, `remove
<team>`, `empty <team>`, `join <team> [<members>]`, `leave <members>`, and
`modify <team> <option> <value>` for every option vanilla's own
`TeamCommand.java` registers: `displayName`, `color`, `friendlyfire`,
`seeFriendlyInvisibles`, `nametagVisibility`, `deathMessageVisibility`,
`collisionRule`, `prefix`, `suffix`. `<members>` reuses
`lodestone_command_mc::ScoreHolderArg`/`crate::commands::scoreboard::resolve_many`
rather than `EntityArg`, matching vanilla's own `TeamCommand`
(`ScoreHolderArgument.greedyScoreHolder()`), so a selector, `*`, or a bare
"fake player" name work here exactly as they do for a score.

A holder is on at most one team (`TeamHandle::join` removes it from whatever
team it was already on first, matching `Scoreboard.addPlayerToTeam`). The
store rides inside `WorldStateHandle` as a sibling of `scoreboard`, the
identical reachability shape that module's own section above already
explains.

New argument types: `lodestone_command_mc::{TeamArg, TeamColorArg}`
(`minecraft:team`/`minecraft:team_color`); `nametagVisibility`/
`deathMessageVisibility`/`collisionRule` are registered as literal-token
children rather than a generic argument type, matching
`TeamCommand.addTeamOptions`'s own shape for those three.

`team=`/`team=!` is also a new selector predicate
(`lodestone_command_mc::SelectorPredicate::Team`), resolved through
`crate::commands::source::resolve_players`'s new `team` closure parameter —
the identical closure-over-handle shape `scores=` already uses, so this crate
stays ignorant of the store's actual layout. The bare `team=` form (empty
name) matches a holder on **no** team, matching vanilla's own comparison
against `""` rather than a three-way `Option`.

**Not built:** `displayName`/`prefix`/`suffix` accept plain text
(`StringArgument::greedy`), not vanilla's JSON text component — the same
honest omission `/scoreboard objectives add`'s `displayName` already makes,
since this crate has no textual component parser anywhere.
`friendlyfire`/`seeFriendlyInvisibles`/`collisionRule` are stored and
reported back but not yet *enforced* by the mob/combat simulation — "stored
and broadcast is not enforced" is the same shape difficulty was in before its
first real consumer landed (see `crate::world_state`'s own module doc).

#### `/data storage`, and `/execute if`/`unless data storage`

`crate::commands::nbt_data` plus the engine it reads and writes,
`crate::commands::nbt_storage::NbtStorageHandle` — a free-standing per-id NBT
compound with no owner in the world, matching vanilla's own `CommandStorage`
(`Map<ResourceLocation, CompoundTag>`, not attached to anything). Only the
`storage` target of vanilla's three (`storage`/`entity`/`block`) is built:
`entity`/`block` need a live, command-reachable, mutable NBT view of a real
entity or block entity, which this crate has nowhere — the same gap
`crate::commands::execute`'s module doc already named for `if items`.
`get storage <id> [<path>]`, `merge storage <id> <nbt>`, `remove storage <id>
<path>`, and the matching `/execute if`/`unless data storage <id> <path>`
numeric conditional, reached through `ctx.world.state.nbt_storage()` exactly
like `/data storage` itself, so a value written by one and read by the other
agree by construction.

The store's compound representation (`Vec<(String, SnbtValue)>`) is exactly
what `lodestone_command_mc::NbtCompoundArg` already parses `/data merge`'s
`<nbt>` argument into, so there is no conversion at that seam. Two new
argument types back the rest: `lodestone_command_mc::StorageIdArg`
(`minecraft:resource_location`, no census — a storage id is created by use,
not registered ahead of time) and `NbtPathArg` (`minecraft:nbt_path`), whose
own module doc names it as a **v1 reduction**: a dot-separated chain of
compound keys only, refusing (not silently truncating) an array index or a
filter compound. That is exactly why `if data`'s `storage` form can exist
while `if items` (array-indexed paths into an inventory) still cannot.
`SnbtValue` also gained a `Display` impl for `/data get`'s feedback text,
guaranteed by its own test to round-trip through `parse_value`.

**Not built:** the `entity`/`block` targets everywhere they appear
(`/data get`/`merge`/`remove`, and `if data entity`/`if data block`), array
indices and filter-compound predicates in `<path>`.

#### The four mechanisms

Each of `/tp`, `/summon`, `/weather` and `/defaultgamemode` was blocked on one
named missing mechanism, not on tree-building work. All four are now built and
the commands registered on top of them.

* **`/tp`/`/teleport` — a generic post-join teleport encoder.**
  `ServerProtocol::encode_teleport` is a new trait method (default: emit
  nothing), implemented in `crates/protocol/v770/src/server_protocol.rs` by
  reusing the same `encode_player_position_teleport` free function the join
  sequence and `ServerProtocol::encode_respawn` already call — all three stay
  byte-identical for the same inputs by construction. Delivery is an ordinary
  directed `Effect::Teleport`, exactly like `/kill`'s `Effect::Kill`: a
  self-teleport is applied inline by the `ChatCommand` arm, a teleport aimed at
  another player is queued on the `PlayerRegistry` and applied by that
  player's own connection loop. `yaw`/`pitch` are `Option<f32>` — `None` means
  "keep the target's current facing", resolved from that connection's own
  `player_rot` at *application* time (in `apply_own_effect`, now threaded
  `player_pos`/`player_rot` for exactly this), because a command executor has
  no way to read a target's rotation: `PlayerCandidate` carries a position but
  not one. `<location>` resolves `~`/`^` against the **command source**'s own
  position, never a target's — vanilla's `Vec3Argument.getCoordinates(source)`,
  confirmed against `TeleportCommand.java`. `<rotation>` is two plain
  `brigadier:float` nodes rather than `minecraft:rotation` — no `RotationArg`
  parser existed at the time this was written; one now does
  (`lodestone_command_mc::RotationArg`, built for `/execute rotated`), but
  `/tp` itself has not been reworked to use it — the same documented
  approximation `world_spawn_commands`'s `/spawnpoint` angle already makes.
  `/tp` and `/teleport` are two independently-built trees rather than a
  `redirect` — `Registrar::redirect` had no production caller at the time and
  this was not the place to be the first; `crate::commands::execute` is that
  caller now.

  **There is no bare, `@s`-free `/tp <entity>` self-form.** Vanilla's tree has
  `<location>`, `<destination>` and `<targets>` as three *simultaneous*
  argument children of `teleport`, which needs the ambiguity-preserving
  backtracking `lodestone_command::CommandTree::parse`'s own doc comment says
  it deliberately does not implement — "argument children are tried in
  insertion order and the **first** success wins", no retry across siblings
  when that branch turns out incomplete. A bare name is valid syntax for both
  the single-entity `<destination>` and the multi-entity `<targets>`, so
  whichever is registered first always wins outright regardless of what
  follows, and no ordering satisfies both `/tp Steve` (self) and
  `/tp Steve ~5 ~ ~` (move Steve) — measured live: an isolated worktree run
  of the vanilla-tree-shaped first draft failed exactly the two tests
  exercising a `<targets>`-prefixed bare name, both with zero effects
  produced. The shipped tree drops the top-level bare `<destination>` node so
  `<targets>` always wins that position; self-to-entity is reached with an
  explicit `@s` (`/tp @s Steve`) instead, which cannot collide with
  `<location>`'s numeric/`~`/`^` grammar. A disclosed reduction from
  vanilla's own tree, with its own control test asserting the bare form stays
  refused.
* **`/summon` — no new mob-sim capability, an API-shape gap.**
  `crate::mobs::MobHandle::with` and `crate::mobs::MobSim::spawn_species`
  were already `pub`; what was missing was a way for a command executor to
  reach the handle at all. `CommandWorld` gained an `Option<&MobHandle>`
  field (`Option` because RCON has none — see `rcon.rs`'s own doc), and
  `crate::server`'s `ChatCommand` arm passes the same shared handle
  `dispatch_play_packet` already holds — the same one the world tick loop's
  `run_mob_tick_loop` republishes into `LiveMobSource`, so a summoned mob is
  picked up by the very next publish with no second wire built. `<entity>` is
  validated at *parse* time by a new `lodestone_command_mc::EntityTypeArg`
  against `lodestone_data::entity_types::entity_type_id` (protocol 776's real
  census), wired as `minecraft:resource` (registry `minecraft:entity_type`).
  No SNBT (`<nbt>`) and no build-height bounds check — documented gaps, not
  silent ones.
* **`/weather` — a request queue on `WorldStateHandle`, not a new lock.**
  `crate::weather::WeatherState` is still owned by the world tick loop with no
  lock — that has not changed. What changed is
  `WorldStateHandle::request_weather`/`take_weather_request`, a
  `WeatherRequest` slot mirroring `crate::sleep::SleepVote`'s split exactly:
  a caller-side request the loop consults once per pass
  (`run_tick_loop_with_weather`'s own hunk, right before its existing
  `weather.tick(...)` call) and applies **directly** to `WeatherState`'s
  `pub(crate)` fields — mirroring vanilla's own
  `MinecraftServer.setWeatherParameters`, which also writes the booleans and
  timers immediately rather than waiting for a countdown. Applying it before
  `weather.tick` runs is what makes the transition (and its
  `StartRaining`/`StopRaining` broadcast) land on the very next tick instead of
  never — `weather.tick`'s own flip-detection compares against whatever the
  booleans already are when it starts, so a value already set going in would
  never register as a change. `<duration>` uses vanilla's own `TimeArgument.
  time(1)` (no sampled default; a documented fixed stand-in — see
  `crate::commands::weather`'s module doc).
* **`/defaultgamemode` — a store, nothing more.**
  `WorldStateHandle::default_game_mode`/`set_default_game_mode`, defaulting
  to `Survival` (`LevelSettings.DEFAULT`). Wired at the one read site that
  needed it: `crate::server::serve_connection_inner`'s
  `let mut game_mode = GameMode::Survival;` (a brand-new player's starting
  mode) now reads `world.default_game_mode()` instead — a **returning**
  player's saved mode still wins over it a few lines later, unchanged, exactly
  matching vanilla's own "this only affects future joins" semantics. No
  `forceGameMode` enforcement of already-connected players — this crate models
  no such rule and has no cross-connection game-mode push wired to this
  command.

#### `/execute`

`crate::commands::execute` (`ExecuteCommand.java`). The parser needed no
changes at all: every branch point in vanilla's own tree offers at most one
*argument* child alongside its literal children, so the one ambiguity
`lodestone_command::CommandTree::parse` cannot resolve (multiple simultaneous
argument children — `/tp`'s own gap, see that command's module doc) never
engages here.

Built: `as`, `at` (position **and** rotation), `positioned` (+ `as`), `rotated`
(+ `as`), `facing` (`<pos>` and `entity <targets> <anchor>`), `align`,
`anchored`, `in` (single-hosted-dimension census), `run`, and `if`/`unless
entity`/`dimension`/`score`/`data storage`. `at`'s rotation transfer and `rotated as` needed
`PlayerCandidate` to carry a rotation, which it now does —
`crate::players::PlayerRegistry` was already tracking a live per-connection
`Rotation` and simply never threading it through. Each subcommand is one
[`Registrar::modifier`] rewriting the one
[`CommandSource`] flowing through it, redirected back to `execute`'s own
children — the modifier/fork substrate this document already named as "built
before `/execute` needs it" now has its first production caller.

`if`/`unless score` — both of vanilla's two shapes, `matches <range>`
(`lodestone_command_mc::IntRangeArg`) and `<op> <source> <sourceObjective>` as
five literal comparison tokens (`<`, `<=`, `=`, `>=`, `>`) — needed a real
scoreboard to exist first; see "`/scoreboard`" below. Both read through
`ctx.world.state.scoreboard()`, the same store `/scoreboard` itself writes, so
a score set by one command and read by a chained `execute if score` agree by
construction with no new plumbing between them.

`if`/`unless data storage` needed a real NBT command-storage engine first;
see "`/data storage`" below. It is `DataCommand`'s own numeric-conditional
shape (a count, matching `if entity` rather than `if score`'s boolean),
because real vanilla paths can carry wildcards — `NbtPathArg`'s v1 grammar
can only ever produce `0` or `1` here, but the shape is kept so nothing needs
to change if the path grammar is ever widened.

`run <command>` carries **no modifier at all**: `registrar.redirect(run_node,
registrar.root())`, matching vanilla's own `literal("run")
.redirect(dispatcher.getRoot())`. With nothing to apply, the current (possibly
forked) source set passes straight through into a full re-parse of the whole
tree, which is what makes `execute as Steve run kill` affect Steve and not the
caller, and what makes nesting a second `execute` inside `run` ordinary syntax
rather than a special case.

`if`/`unless` needed one small change to `Dispatcher::dispatch`: vanilla's
`addConditional` attaches **both** a fork modifier (`execute if entity @a run
…`) and an executor (`execute if entity @a` alone) to the same condition node,
and real Brigadier's `ContextChain` only ever runs the fork when there is a
further stage. The dispatch walk now skips a node's own modifier when that
node is *also* the parsed path's terminal node and carries its own executor —
see that function's own doc comment for the failure mode it closes (a fork
that empties the source set before the bare form's pass/fail message ever
gets to run).

Not built, each naming its own missing subsystem: `store` (a scoreboard *and*
NBT storage now both exist to write *into* — see "`/scoreboard`"/"`/data
storage`" below — but nothing in the dispatcher yet wraps a chained command's
own return value the way `store` needs to capture it), `if data`'s
`entity`/`block` targets and `if items` (no live, mutable NBT view of an
entity, block entity, or container slot reachable from a command — `storage`
needed none of that, which is why it alone is built; see "`/data storage`"
below), `if predicate` (no loot-predicate engine), `stopwatch` (no
stopwatch registry), `if block`/`biome`/`blocks`/`loaded` (no read-only
block/biome/chunk-residency
query on `CommandWorld`, which today only ever *writes* blocks), `on
<relation>` (no entity-relationship query on the mob simulation), the
`execute summon` modifier form (unnecessary as its own subtree — `/summon` is
already a root command reachable through `run`), and `positioned over
<heightmap>` (no heightmap query). `crate::commands::execute`'s own module
doc is the up-to-date source for this list.

Tested in `crates/lodestone-server/tests/builtin_commands.rs`, each assertion
predicting a rewritten answer a caller-position/caller-entity reading of the
same text would get wrong — `execute as bob run kill` targets bob and not the
caller; `execute at bob positioned ~1 ~1 ~1` lands somewhere none of `at`
alone, `positioned` alone, or the literal offset would; `execute facing 5 64 0
run tp @s ^0 ^0 ^5` only lands on the aimed-at point because `facing` actually
rewrote the rotation `^` resolves against.

#### Command blocks

`crate::command_block` (`CommandBlockEntity.java`/`BaseCommandBlock.java`/
`CommandBlock.java`) and `BlockEntity::CommandBlock` in
`crate::block_entities`. **Before this, there was no command-block block
entity at all** — placing one wrote nothing but a plain block. Now placing
`minecraft:command_block`/`chain_command_block`/`repeating_command_block`
creates a real, persisted `CommandBlockData` (command text, mode derived from
the block itself rather than stored, conditional/"Always Active" flags,
output tracking, success count), and it round-trips through chunk NBT
(`chunk_nbt.rs`'s `Command`/`SuccessCount`/`TrackOutput`/`LastOutput`/
`powered`/`conditionMet`/`auto`/`UpdateLastExecution`/`LastExecution` fields,
field-for-field against `BaseCommandBlock.save`/`CommandBlockEntity
.saveAdditional`).

The pure tick/redstone-edge math is ported and unit-tested against the
decompiled source: `on_power_changed`/`on_automatic_changed` reproduce
`CommandBlock.setPoweredAndUpdate`/`CommandBlockEntity.setAutomatic`'s
rising/falling-edge rules. The one easy-to-guess-wrong rule, caught by this
unit's own tests rather than assumed: `setPoweredAndUpdate` excludes only the
**"Always Active" toggle** and `Mode.SEQUENCE`, not `Mode.AUTO` itself — a
repeating command block that is *not* "Always Active" still schedules off its
own redstone rising edge exactly like an impulse block, and only the toggle
(handled separately by `on_automatic_changed`) makes it self-sufficient. `tick`
reproduces `CommandBlock.tick`'s three-mode branch, including the one-cycle-
stale condition a repeating block's own polling produces, and
`next_chain_position`/`chain_link_present`/`chain_link_should_run` are
`CommandBlock.executeChain`'s walk, one hop at a time.

**Both hops named in an earlier draft of this document are now wired, and a
third, narrower gap remains — none of it in this module:**

1. **The wire decode.** `SET_COMMAND_BLOCK` decodes into
   `ServerBound::SetCommandBlock` (`crates/protocol/v770/src/server_protocol.rs`)
   and `crate::server`'s handler applies it — a real client's command-block
   GUI can write a command now. `SET_COMMAND_MINECART` is still decode-only;
   no command-block-minecart entity exists to write into.
2. **Scheduling into the tick loop.** `tick.rs`'s due-tick drain has a
   `TICK_COMMAND_BLOCK` arm (the same shape as `crate::redstone_dispenser`'s
   `TICK_DISPENSER_FIRE` arm it was written to precede) that calls
   `crate::command_block::tick`, runs the command through a fresh
   `ServerCommands` when the decision says to, and walks any chain behind it.
3. **What is still open:** nothing yet calls `crate::command_block::on_power_changed`
   from a real redstone signal — `CommandBlock.neighborChanged` would need to
   reach into `block_entities` from `crate::random_tick::propagate_and_react`,
   which today only rewrites the block-*state* string and has no block-entity
   handle in scope. So today a command block runs from **"Always Active"**
   (wired end to end) or from a `ServerCommands`/RCON caller setting it up
   directly; an ordinary redstone pulse into an impulse or repeating-but-not-
   automatic block does nothing yet.

`crate::command_block`'s own module doc is the up-to-date source for exactly
what is left.

Per-command wire parity for the original four is asserted against
`crates/protocol/v770/tests/fixtures/command_tree_creative.hex` — 30,248 bytes and
2,017 nodes captured from a real vanilla 26.2 server — comparing node kinds,
names, parser variants *including payload flags*, executable bits, restricted
bits, redirect topology and suggestion ids, recursively and in child order. The
gate carries its own control: pointing the comparison at two different subtrees
must panic.

**The newer commands have no such fixture and therefore no wire-parity gate.**
The captured tree was taken before any of them existed, so their execution
tests are the only gate on record — a real risk this doc states rather than
hides: a node shape one of them gets wrong (an argument order, a literal vs.
argument choice) would ship undetected until a real client's autocompletion
disagreed with what the server accepts. Recapturing the fixture against a
26.2 server that has these commands too would close it.

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
* **`CHAT_COMMAND_SIGNED` is now decoded, corrected from an earlier "deliberately not
  decoded" note here.** It routes through the same `ServerBound::ChatCommand` consumer
  as the plain form — the per-argument signatures are decoded (to find the end of the
  frame) and then dropped rather than verified, since a client only sends this form for
  arguments the server's `COMMANDS` tree declared **signable**, and this server declares
  none. So every command still executes identically regardless of which wire form
  carried it; see `ServerBound::ChatCommand`'s own doc comment.
* **The serverbound suggestion request now decodes, dispatches and answers —
  but is still unreachable from a real client, for a *different* reason than
  before.** `minecraft:command_suggestion` (serverbound id 15) decodes to
  `ServerBound::CommandSuggestion { id, command }`
  (`crates/protocol/v770/src/server_protocol.rs`), `crate::server`'s
  `ServerBound::CommandSuggestion` arm answers it via the new
  `ServerCommands::suggest_response` (byte-range arithmetic unit-tested in
  `crates/lodestone-server/src/commands/mod.rs`'s `suggest_response_tests`),
  and `ServerProtocol::encode_command_suggestions` sends a real
  `minecraft:command_suggestions` (clientbound id 15) reply. The decode half
  is covered by `crates/protocol/v770/tests/serverbound_wiring.rs`'s
  `every_serverbound_variant_is_constructed_by_decode`.
  **What is still missing is the trigger, not the answer**: a client only
  *sends* this request when a tree node carries `FLAG_CUSTOM_SUGGESTIONS`
  (a `minecraft:ask_server` provider id), and this server's tree still
  declares **zero** — every `McArg::suggestion_provider` returns `None`.
  Measured against the vanilla capture, that is right for `minecraft:entity`
  (118 of them, all with no provider) and wrong for the parsers vanilla does
  mark `minecraft:ask_server`: `resource_location` (58), `score_holder` (27),
  `function` (5), `game_profile` (5), `brigadier:string` (9), `objective` (2),
  `resource` (2), `time` (2), `brigadier:float` (1). So today the answering
  half works end to end against a hand-crafted frame, but no real client
  (ours or vanilla) will ever produce that frame against our own tree —
  declaring a provider on one of our matching argument types
  (`lodestone-command-mc`'s `McArg::suggestion_provider`, checked for parity
  against the captured fixture the same way every other wire field is) is
  the remaining, separately-scoped work that closes the loop.
* **`ServerProtocol::encode_system_chat` defaults to emitting nothing**, like
  every other optional encoder. Its failure mode is silent rather than loud: the
  command still *runs*, the player just never learns what happened. A family that
  wants commands must implement it.
* **26.2 has five permission levels, not four**, and no longer a bare number:
  `PermissionLevel::{All, Moderators, Gamemasters, Admins, Owners}`. An earlier
  design's body said four and was wrong; `crates/lodestone-ecs/src/permissions.rs`
  already transliterates the five — use it rather than re-deriving.
* **`lodestone_model::command_tree` and `lodestone-command` are not the same
  thing and must not be merged.** One is a flat, wire-shaped *decode target*
  keyed by registry id that tolerates unknown ids; the other is an arena-based
  *construction API* with `dyn ArgumentType` as behaviour. Both were kept
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
   add it is `client_app`, `crates/lodestone-shell/src/sim/build.rs`.
2. `IntegratedServer::open_in_memory_with_mobs`
   (`crates/lodestone-server/src/integrated.rs`) — the production
   singleplayer constructor — has no way to accept a `CommandDispatch` and calls
   `serve_connection_with_mob_events_shared` internally.
3. `connect_impl`, `crates/lodestone-shell/src/net.rs`, is the one place a `World` handle and
   the `IntegratedServer` are simultaneously in scope, so it is where the sink
   would be constructed and installed.

**The `COMMANDS` (id 16) encoder now sends.** `server.rs` transmits the
per-player-pruned projection at the join sequence's vanilla position, so a
real client's tab completion and command highlighting work against every
built-in listed above, not only the original four.

Still open, and each is now additive rather than blocked:

* **A real scoreboard now exists** (`crate::commands::scoreboard`,
  `crate::commands::scoreboard_store::ScoreboardHandle` — see the
  `#### /scoreboard` section above), and `/execute if`/`unless score` is built
  on it. **A real team store now exists too** (`crate::commands::team`,
  `crate::commands::team_store::TeamHandle` — see `#### /team` above),
  separately from the scoreboard as vanilla itself keeps them, and `team=` is
  a real selector filter. **`at`'s rotation transfer and `rotated as` are also
  built now** — `PlayerCandidate` carries a live rotation (`crate::players`
  already tracked one per connection and simply never threaded it through).
  **A real NBT command-storage engine now exists too**
  (`crate::commands::nbt_storage::NbtStorageHandle` — see `#### /data
  storage` above), so `/data storage` and `/execute if`/`unless data
  storage` are both built; only the `entity`/`block` targets remain missing.
  What is still open: **`store`** (a scoreboard and NBT storage both exist
  to write *into*, but the dispatcher does not yet wrap a chained command's
  return value the way `store` needs to capture it), **`if data`'s
  `entity`/`block` targets and `if items`** (no live, mutable NBT view of an
  entity, block entity, or container slot reachable from a command),
  **`if predicate`** (no loot-predicate engine), **`stopwatch`** (no
  stopwatch registry), **`if block`/`biome`/`blocks`/`loaded`** (no read-only
  block/biome/chunk-residency query on `CommandWorld`, which today only ever
  *writes* blocks), and **`on <relation>`** (no entity-relationship query on
  the mob simulation). Functions/datapacks remain entirely unattempted, as
  scoped. **Command blocks now run end to end from "Always Active"**:
  `SET_COMMAND_BLOCK` decodes and writes the block entity, and `tick.rs`'s
  `TICK_COMMAND_BLOCK` drain runs them. What remains is narrower — an ordinary
  redstone pulse into an impulse or repeating-but-not-automatic command block
  does nothing yet, because nothing calls
  `crate::command_block::on_power_changed` (still a real `never used`
  function, confirmed by `cargo check`'s own dead-code warning) — see this
  document's own `#### Command blocks` section for exactly what is left, and
  why closing it needs `crate::random_tick::propagate_and_react` to gain a
  block-entity handle it does not have in scope today.
* **`/publish`** (the Open-to-LAN port command) is a separate, LAN-specific
  issue and was not attempted here.
* **`/xp query`** parses and resolves its target, then refuses rather than
  answering. A live player's experience is a connection-local
  `PlayerExperience` this crate cannot read cross-connection — `PlayerCandidate`
  (the roster snapshot selectors resolve against) carries a position and a game
  mode, the same two scalars `/gamemode`'s `gamemode=` filter needs, but no
  experience field. Answering the query for real needs that snapshot widened,
  the same way game mode already was — which needs `crate::players`'
  `TrackedPlayer`/`PlayerCandidate::experience` plus a republish call at every
  site that changes a player's XP, mirroring `PlayerRegistry::set_game_mode`'s
  own producer/mirror split. Not done here: those sites are scattered across
  `crate::server`, off limits to this pass.
* **`/setblock`/`/fill` are unreachable from RCON**, unlike every other new
  command — **`/say`/`/me` already are reachable** (`rcon.rs`'s own
  `run_command_as` already special-cases `Effect::Broadcast` through
  `PlayerRegistry::say`; an earlier draft of this document said otherwise and
  was stale). `Effect::SetBlock`/`Fill` need the chunk source only a live
  connection's own `ChatCommand` arm has in scope; RCON's `run_command` has no
  such source at all, and giving it one would mean a shared, always-resident
  chunk-write path RCON could hold outside any connection — a real feature,
  not a routing fix, and out of scope here. `/summon` and `/worldborder` are
  RCON-unreachable for the identical shape of reason (`CommandWorld::mobs`/
  `::border` are `None` there): each needs a handle
  (`crate::mobs::MobHandle`/`crate::border::BorderFeed`) that today is a local
  variable inside `IntegratedServer::open_in_memory_with_mobs` rather than a
  field the server keeps around for `start_rcon` to read back — see `rcon.rs`'s
  own module doc for the up-to-date roster of what RCON can and cannot run.
  `/scoreboard` (and `/execute if score`) **are** reachable from RCON, with no
  new wiring: both ride `crate::world_state::WorldStateHandle`, which
  `RconConfig::with_world` already substitutes for the shared production
  store.
* **`/spawnpoint` has no `<targets>` form**, only the caller-implicit one.
  `RespawnPoint` is a connection-local variable
  (`dispatch_play_packet`'s own `respawn: &mut Option<RespawnPoint>`); reaching
  a *different* connection's copy would need a directed effect this crate does
  not have yet (`Effect::SetRespawnPoint` is deliberately self-targeted-only —
  see its own doc comment).
* **A textual SNBT parser now exists**
  (`lodestone_command_mc::snbt::{parse_value, parse_compound, NbtTagArg,
  NbtCompoundArg, SnbtValue}`), ported clause by clause from `TagParser`. It
  is a grammar-only building block with **no production caller yet**:
  `ItemArg` v1 still refuses a `[…]` component patch by name rather than
  parsing it, because `lodestone_model::ItemStack` has nowhere to put a parsed
  component patch even once one is parsed, and widening it is outside
  `lodestone-command`/`lodestone-command-mc`/`lodestone-server`'s combined
  remit. Since `minecraft:item_stack` carries no wire payload, the node, the
  autocompletion and `/give minecraft:diamond_sword 3` are all complete now
  regardless; the later unit that wires the parser in replaces exactly one
  match arm in `ItemArg::parse`.
* **Deferred selector options.** `nbt`, `advancements`, `predicate`,
  `tag`, `team`, `level` and the two `*_rotation` options are refused **by name**
  rather than ignored — a silently widened selector is the worst available
  failure. `scores` is no longer on this list at all: it parses
  (`entity.rs`'s `read_scores_map`) and resolves against the real
  `ScoreboardHandle` (`source.rs`'s `matches_predicates`), alongside
  `/execute if score` and `/scoreboard`, which read the same store directly.
  `team` is not unlocked by the scoreboard existing — teams are a separate,
  still-unbuilt subsystem (name→team membership, colour, friendly fire), not a
  scoreboard feature. Each remaining option needs a subsystem that does not
  fully exist yet (entity NBT, the advancement predicate engine, entity tags,
  experience levels, per-entity rotation tracking). **None of it is visible on
  the wire**: `minecraft:entity` carries one flags byte and no option list, so
  deferring options cannot break tree parity.
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
* **Command blocks** have a data model and tick math (`crate::command_block`)
  but nothing schedules them yet — see `#### Command blocks` above.
  **Functions and datapacks** are unimplemented.

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
[#464]: https://github.com/matteopolak/lodestone/issues/464
