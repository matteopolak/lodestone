# Commands: the tree, dispatch, permissions, and access control

## What it is

One argument-tree data model underlies three independent consumers: the server's own built-in
command dispatcher and permission model, a plugin-command seam that keeps `lodestone-server` from
depending on `lodestone-ecs`, and the client's decode of a server-sent command tree for chat
tab-completion and syntax highlighting. Access control (ops/whitelist/bans) and permission levels
gate which of a connection's commands are visible or runnable at all.

## How it works

### The tree substrate: `lodestone-command`

A standalone, zero-dependency, ECS-free and version-free crate: a flat arena of nodes
(root/literal/argument), redirects, an executable flag, and a direct port of Brigadier's own
parse/suggest algorithm down to the character level. It has no execution semantics of its own —
`executable` is a bare flag with nothing attached to run — by design, so that server dispatch,
plugin registration, and client decode can each build their own behavior on the same tree shape
instead of duplicating parsing three times. `lodestone-command-mc` layers Minecraft-specific
argument types (game mode, entity selectors, block/item ids, positions, scoreboard/team types) on
top, since an argument type that knows what an item *is* cannot live in a crate with zero
dependencies.

### The server-side path: wire to dispatch to reply

A `/command` typed by a player travels `ClientAction::SendCommand` → the wire (`CHAT_COMMAND`) →
`ServerBound::ChatCommand` → the server's own handler, which tries the built-in tree
(`ServerCommands`) first and only falls through to a host-installed plugin dispatcher
(`CommandDispatch`) when nothing at the root matched. Built-ins (`/gamerule`, `/gamemode`, `/give`,
`/execute`, `/scoreboard`, `/team`, `/tp`, `/summon`, `/weather`, and others) run directly against
server state; a plugin command runs through the seam below. A response becomes system chat lines
sent back to the caller.

The server's own tree is also projected to a real client over the wire (`COMMANDS`, clientbound),
pruned per-connection by permission level exactly as vanilla prunes an unusable subtree — a denied
node takes its whole subtree with it — so tab completion and highlighting only ever show what that
connection can actually run.

### Why there is a plugin seam at all

`lodestone-server` must not depend on `lodestone-ecs`: linking it would drag client-only vocabulary
and several other crates into the server's dependency graph, including the browser bundle, which
links the server but not the client ECS. But the plugin command registry has to live in
`lodestone-ecs`, since that's where the plugin API itself lives. So `lodestone-server` declares a
small, ECS-free trait (a caller-identity struct plus a command string in, "ran" or "refused" out)
and the **host** — a crate legitimately linking both — implements it. No sink installed means every
command is refused; the wire layer enforces the caller's identity and a fail-closed default, but
never a specific permission, since it has no `Permissions` resource and structurally never will.

### Permission levels and access control

Vanilla's five permission levels (`All`/`Moderators`/`Gamemasters`/`Admins`/`Owners`, numbered 0–4)
gate both built-in command roots and several administrative packets vanilla sends instead of a
slash command (difficulty changes, game-rule changes, command-block edits, game-mode changes). A
connection's level is resolved once, at the Play handoff, from its authenticated identity — never
from the command text itself.

Native hosts can install `access::PermissionLevelProvider` through
`AccessHandle::set_permission_provider(Some(Arc::new(provider)))` before publishing or serving
connections. The provider receives `PermissionLevelContext { uuid, fallback_level }`, an identity
and a snapshot of the existing access-list result. `Some(0..=4)` overrides that result, `None`
defers to it, and an invalid numeric level resolves to zero. The same effective level gates the
advertised command tree, command execution, and administrative packets.

For the local integrated-server bridge, that captured level also flows with
`CommandCaller` into the ECS command sink. Immediately before resolving a
plugin command, the sink writes it to that player's `Permissions` subject.
This is the input that makes a plugin node with the ordinary `Op` default agree
with the server's built-in command gates; plugin grants, groups, declarations,
and installed permission resolvers remain in the ECS resource and are not
replaced by the bridge.

Handle clones share provider installation and removal. Provider callbacks run after both handle
locks are released, so they may inspect stored access through `permission_level` or replace the
provider. They must not block or recursively resolve the effective command level. Provider policy
does not alter persisted ops, join bans, whitelist membership, or player-limit bypass. Removing a
provider restores ordinary access-list resolution for future connections. Existing connections
retain the level captured at their Play handoff; this API does not perform live privilege revocation.
This is delegation of the five server levels, not a node/wildcard permission registry.

Access control is vanilla's four JSON files (`ops.json`, `whitelist.json`, `banned-players.json`,
`banned-ips.json`), read and written through one shared, cloneable handle every connection and the
admin console read from. Join order matches vanilla exactly — player ban, then whitelist, then IP
ban, then player limit — and each refusal carries vanilla's own translation key so a real client
renders localized text. A missing file means an empty list (every world's first start); a malformed
one is a hard error rather than silently read as "no operators", since the latter is how an admin
locks themselves out of their own server. Bans and ops match on UUID, never name — matching by name
would let a rename dodge a ban — and offline mode's UUID-from-username derivation is exactly why
the two coincide there. An unparseable ban expiry keeps the ban rather than silently lapsing it. A
world with no access lists configured at all (a fresh singleplayer world, above all) is maximally
permissive by default, since its one player must be able to do everything; a host opts *into*
restriction rather than every world opting out of it.

### The client side: decoding and using a server's tree

A real server's command tree arrives as a self-describing, variable-length node stream with no
per-node length prefix, decoded into a version-free tree model kept separate from
`lodestone-command`'s own construction API (a decode target keyed by wire registry ids, tolerant of
an unrecognized argument type — marking just that one node unusable rather than corrupting the rest
of the stream, a deliberate and safer divergence from vanilla's own decoder, which corrupts every
following node on the same failure). Tab completion and inline syntax highlighting are pure
functions over that tree and the chat input line: some completions resolve entirely locally
(literal children, a handful of small fixed-domain argument types); everything else — entity
selectors, resource-registry types, anything with a declared suggestion provider — asks the server
and waits for a matching reply, safely over-approximating rather than risking a wrong local answer.

## How to change it

- **Adding a host-dispatched capability**: add a method to the dispatch trait with a default body
  that *refuses*, so an existing host implementation keeps compiling and an unupdated host fails
  closed rather than open.
- **Adding a payload-carrying argument type**: add it to the version-free parser enum, wire its
  decode by reading the real vanilla type's own network-deserialize method (not a summary of it),
  and re-derive any local completion domain from that type's own vanilla suggestion list rather
  than guessing.
- **A new built-in server command**: register it against `ServerCommands` at vanilla's own
  permission level; if it needs to reach state outside the current connection (another player, a
  world-wide store), route it through the existing effect-queue/shared-handle pattern rather than
  reaching into another connection directly.
- **Granting or revoking access at runtime**: the shared access handle is what admin commands
  mutate; nothing persists a runtime grant back to disk automatically, so a host that wants it
  durable must call the save path itself.

### Gotchas

- An **exact** permission deny does not cover its children (denying `myplugin.admin` still leaves
  `myplugin.admin.reload` allowed by a broader wildcard grant) — carving out a whole branch needs
  the wildcard form of the deny.
- An **undeclared** permission node is held by every operator and denied to everyone else by
  default, matching Bukkit's own default rather than denying everyone — the single most surprising
  resolution step for anyone porting a Bukkit plugin's assumptions.
- Resolution order (specificity, then own-subject-over-group, then deny-over-allow, then declared
  default) is not arbitrarily reorderable — some of those steps only ever matter in combination, so
  a wrong ordering can be silently invisible against most trees and wrong against a pathological one.
- A redirect is a same-position jump, not a token-consuming one, so a naive walker can loop forever
  on a redirect cycle; both the tree library and the client's own walker guard this with a
  visited-node set, not merely a bound on remaining input length.
- The tree a client renders is pruned by permission level per-connection — a lower-permission
  player isn't shown a greyed-out command, it simply never receives that subtree at all.
- **Command text is remote input, and command *arguments* can nest.** The string carrying a
  command is read with the protocol's default cap of 32767 characters, so anything in an
  argument's own grammar that recurses per character has 32767 available levels. The SNBT
  parser behind `minecraft:nbt_tag`/`minecraft:nbt_compound_tag` is the case that matters:
  compounds and lists nest, so a command of nothing but open brackets recursed once per
  bracket and aborted the process — a client crashing its own host. It is bounded at 512
  levels (`snbt::MAX_NESTING`), the depth past which the game's own reader refuses a
  serialized structure as too complex; nothing the parser produces is useful past that,
  since the value's purpose is to become a stored or transmitted structure that would be
  refused there anyway. The check sits at the top of `read_value_at`, the one function both
  the compound and the list reader reach a nested value through, so a nesting form added to
  either inherits it. Measured on the platform default 2 MiB stack the parser walks 1024
  levels and not 4096, so the bound is comfortably reachable — which matters, because a
  bound the parser overflows before reaching is a crash behind an accepted input rather
  than a bound. `ParseErrorKind::NestingTooDeep` is the refusal.
- **`CommandTree::parse`'s redirect walk is iterative, not recursive, precisely because of the
  above.** Every redirect hop used to cost one Rust call-stack frame; measured on a 2 MiB stack, a
  self-redirecting literal parsed 1024 hops and overflowed before 2048, well inside a command's
  32767-character cap, so a long enough `/execute run execute run …` aborted the process. Vanilla
  declares no parse-depth limit (a JVM stack overflow is recoverable there), so there was no outside
  source to derive a cap from — the fix removes the failure mode instead of bounding it: the walk
  keeps its own explicit heap stack (a `Vec` of pending redirect fallbacks) in place of the call
  stack, the same shape the neighbor-update propagator uses for its own chained notifications, so
  depth costs heap, not stack frames. The visited-`(node, cursor)` guard is unrelated and still
  needed for the separate case it always covered — an adversarial custom `ArgumentType` that rewinds
  the cursor, defeating the "every hop consumes a character" bound from outside `parse`'s own
  control.

## Configuration

- `ops.json` / `whitelist.json` / `banned-players.json` / `banned-ips.json` at the server root,
  native only — a browser build has no filesystem and no remote player to refuse.
- Whitelist *enforcement* is a separate flag from the file's mere presence, off by default,
  matching vanilla's own `white-list` server property being independent of the file existing.
- No configuration governs the command tree or permission model's shape — a plugin declares its
  own nodes and permission defaults at registration time.
- Native permission providers are installed in code on the `AccessHandle` passed to the host's
  publish/serve configuration. They are transient and must be reinstalled after restarting the host.

## Dependencies

- `lodestone-command` (zero dependencies) — the tree/parse/suggest substrate.
- `lodestone-command-mc` — Minecraft-specific argument types layered on top.
- `lodestone-ecs` — the plugin command registry and the permission-resolution resource, reached
  only through the dispatch seam, never linked directly by `lodestone-server`.
- `lodestone-server` — the dispatch seam, `ServerCommands`, and the access-control store.
- `crates/versions/26.2` — the only family implementing the server-side wire encode/decode for
  commands and access-driven disconnects.
