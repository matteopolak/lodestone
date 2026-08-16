# Server access control

## What it is

Ops, whitelist, player bans and IP bans — vanilla's four JSON files, read and written
by `crates/lodestone-server/src/access.rs` and enforced at login. Issue #336. Before this,
`grep -rniE 'whitelist|banned.player|ops\.json|permission.level'` over `crates/` returned two
hits, both *test comments* about vanilla's RCON console: there was no operator model at all and a
hosted world had no way to refuse anybody.

| file | entry fields |
|---|---|
| `ops.json` | `uuid`, `name`, `level` (0–4), `bypassesPlayerLimit` |
| `whitelist.json` | `uuid`, `name` |
| `banned-players.json` | `uuid`, `name`, `created`, `source`, `expires`, `reason` |
| `banned-ips.json` | `ip`, `created`, `source`, `expires`, `reason` |

Field names and the `"yyyy-MM-dd HH:mm:ss Z"` / `"forever"` date format are vanilla's, so a file
this server writes is one a real 26.2 server reads and vice versa.

## How it works

`AccessLists` is the store; `AccessHandle` is the cloneable handle every accepted connection and any
admin console share, with the same `with`-funnels-every-access shape `BlockEntityHandle` established.

**`AccessLists::may_join` is `PlayerList.canPlayerLogin`, in vanilla's order:** player ban →
whitelist → IP ban → player limit. **The order is observable.** A player who is both banned and
not whitelisted is told they are *banned*; a test asserting the whitelist message there would be
asserting the wrong precedence, and would pass for the wrong reason.

It returns a `JoinRefusal` carrying vanilla's own translation key
(`multiplayer.disconnect.banned.reason`, `…banned_ip.reason`, `…not_whitelisted`,
`…server_full`), so a real client renders its own localised text.

### Where it is enforced

`server.rs`'s `ServerBound::LoginStart` arm, **after** the username validation and **before**
`login_success` — vanilla's own point in the sequence, so a refused player never reaches
Configuration. The refusal goes out through `encode_disconnect` and the driver returns
`ServerError::AccessDenied`.

Two parameters carry it into `serve_connection_inner`: the `AccessHandle` and the connection's
`peer_ip`. Every pre-existing entry point passes `AccessHandle::default()` (empty lists — admits
everybody, ops nobody) and `None`, the compatibility shape this file uses for every feed, so no
off-limits call site changed behaviour. Two entry points carry real ones:

* `serve_connection_with_access` — public, so the enforcement is drivable from a test. An access
  check nothing outside the crate can call is exactly the island the repo rules are about.
* `serve_connection_with_mob_events_and_commands_shared` — the production LAN path, fed from
  `LanConfig::access` and the accept loop's `peer_addr().ip()`.

`tests/serve_play.rs`'s `a_banned_uuid_is_refused_at_login_and_admitted_once_pardoned` drives both
directions: refused with the key on the wire, then admitted after a pardon. The second arm is the
control — without it the test cannot tell "the ban was enforced" from "this fixture never joins".

## How to change it, and the gotchas

**The name is a label; the uuid is the identity.** Bans, ops and the whitelist are all matched on
uuid. Offline mode derives the uuid from the name (see `CLAUDE.md`'s live-server hazards), so the
two agree there — but matching on the name would let a rename evade a ban.

**A missing file is an empty list; a malformed file is an error.** A world that has never had an op
has no `ops.json`, which is every world's first start. But an `ops.json` with a typo that read as
"no operators" is how an admin loses access to their own server, so `Error::Malformed` is reported
rather than swallowed.

**`AccessLists::default` is permissive, and that is deliberate.** A singleplayer world has no files
and its one player must be able to do everything. `set_owner(Some(uuid))` names a player who is
always level 4, always whitelisted and always past the player limit; a *host* opts into
restriction, the world does not opt in for them. Getting this backwards locks the player out of
their own world.

**An unparseable `expires` keeps the ban.** Refusing to enforce a ban we cannot read is the wrong
direction to fail. A parseable one really does expire — `parse_ban_expiry` reads vanilla's format
into Unix seconds so a timed ban lapses without anyone editing the file.

**The player limit is stored and inert.** `may_join` takes an `online` count and the enforcement
point passes `0`, because this crate has no cross-connection player registry to count from. That is
the honest split rather than a fabricated count: the ban and whitelist checks are live, the limit is
not. Wiring it needs `PlayerRegistry` to become world-scoped.

**Permission levels are stored and fully read.** `permission_level`/`command_permission_level`
answer 0–4 off the op entry, and every built-in command root is gated at its vanilla level through
`crate::commands::registrar::Registrar::require_level`, resolved by `crate::commands::level_filter`
— see `crate::commands`'s own module doc ("Permissions are real now").

**Granting/revoking access has a real command surface, scoped to RCON.** `/op`, `/deop` and
`/whitelist` (`crate::commands::access_commands`) read and write the *shared* `AccessHandle` —
`IntegratedServer::open_to_lan` threads the same handle every accepted connection's join check
reads into `RconConfig::access`, so an op granted over RCON is real for the very next join. In-game
chat (`crate::server::dispatch_play_packet`) has no `AccessHandle` in scope and these commands
refuse there by name ("No access list is configured for this world") rather than silently doing
nothing — see `access_commands`'s own module doc for why chat was deliberately left out. **Nothing
calls `AccessHandle::save` after a mutation yet**, so a grant/revoke is immediate for the running
process but does not survive a restart unless the host separately persists it.

## Configuration

`whitelist.json`'s **presence does not enable the whitelist**; vanilla's `white-list` server
property does, and here it is `AccessLists::whitelist_enabled`, off by default and set through
`AccessHandle::set_whitelist_enabled`. It is not persisted by `save()` for the same reason: it is a
property, not a list.

`LanConfig::access` is how a host supplies real lists:

```rust
let access = lodestone_server::AccessHandle::load(&world_dir)?;
access.set_whitelist_enabled(true);
let config = LanConfig { access, ..LanConfig::default() };
```

The `Default` is empty, so `bind` and every existing caller behave exactly as before.

## Dependencies

`serde_json` and `std::fs`, both already present. Native only, `cfg`-gated off on `wasm32` like
`region_source`: a browser world has no filesystem to hold the lists and no remote player to refuse.
