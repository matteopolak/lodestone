# `/givedebug`: a client-side testing affordance for inventory

## What it is

A **client-only** chat command, `/givedebug <item> <amount>`, that lets the
interactive player give themselves an item while playing a live oracle, to
exercise inventory-dependent features. It is deliberately not vanilla's
`/give`: no NBT, no components, no selectors, no tab completion — just an
item id and a count. The name is `givedebug` rather than `give` so it reads
unmistakably as a testing affordance, never as a half-finished vanilla
command.

It needs the player to be **op** on the server; see
[Opping the interactive player on the live oracles](#opping-the-interactive-player-on-the-live-oracles)
below.

## How it works

`crates/lodestone-shell/src/chat.rs` intercepts the typed line *before* it
reaches the normal command/chat seam:

- `intercept_give_debug(line: &str) -> GiveDebugOutcome` parses
  `/givedebug <item> <amount>`. `NotGiveDebug` means the line isn't
  `/givedebug` at all and must fall through untouched to
  [`compose_chat_action`].
- The item id is validated with `lodestone_assets::ResourceLocation::parse`
  — the same parser used everywhere else an item/block id is read from text
  — so `diamond_pickaxe` defaults to `minecraft:diamond_pickaxe` exactly like
  vanilla, and `not a valid id!!` is rejected before anything is sent.
- The amount is parsed as a positive `u32`; zero, negative or non-numeric
  input is rejected locally.
- On success it composes the server's **real** `/give @s <item> <amount>`
  as an ordinary `ClientAction::SendCommand` — the same action a typed
  `/gamemode creative` produces — plus a `local_echo` string showing exactly
  what will be sent, so the translation is never a surprise.
- On failure it returns `GiveDebugOutcome::Error(message)`: a local-only
  chat line, never sent to the server. **A debug command that fails quietly
  is worse than no command** — silence would look identical to a dropped
  packet.

`Sim::send_chat` in `crates/lodestone-shell/src/sim.rs` is the integration
point: it runs `intercept_give_debug` first, and only falls through to the
existing `compose_chat_action` path on `NotGiveDebug`. Both the `Send` and
`Error` branches push a line into the local `SessionChat` component (via a
small `push_local_chat` helper added alongside it) stamped with the
driver's own clock — mirroring exactly how an inbound server chat line is
stamped in `NetUpdate::Chat` a few dozen lines below. `send_chat` changed
from `&self` to `&mut self` for this (`push_local_chat` needs a write lock),
which is transparent to its one caller (`app.rs`'s `handle_chat_key`,
already inside a `&mut self` method).

### Why client-side composition, not a local inventory mutation

We are a client; the server is the sole authority on inventory. Composing
the server's real `/give` and sending it is the only choice that cannot
desync: the server either grants the item (and the normal inventory-update
packets flow back through the existing path) or refuses it (op check,
unknown item, etc.) and that refusal reaches chat through the **existing**
inbound-chat path — nothing new needed there, it was confirmed working by
inspection of `NetUpdate::Chat`'s handling in `sim.rs`. Mutating a local
`Inventory`/`ContainerMenu` copy directly was considered and rejected: the
next server-authoritative inventory sync would silently overwrite it,
teaching nothing about the feature under test and papering over exactly the
round-trip a *give* is supposed to exercise.

## How to change it

- Parsing and validation: `intercept_give_debug` in
  `crates/lodestone-shell/src/chat.rs`. It is pure (no ECS, no network) and
  unit-tested there — add cases as new `#[test]`s in that file's `tests`
  module rather than reaching for a live oracle.
- Wiring into the outbound seam: `Sim::send_chat` in
  `crates/lodestone-shell/src/sim.rs`, just above the existing
  `compose_chat_action` call.
- If a future command needs the same "parse locally, translate to a real
  server command, echo the translation" shape (e.g. a debug teleport or
  weather command), follow the `GiveDebugOutcome` enum's three-way shape
  (`NotGiveDebug` / `Send { local_echo, action }` / `Error`) rather than
  inventing a new one — it is what keeps the "malformed input never reaches
  the network" property mechanically enforced rather than remembered.
- **Gotcha**: `ClientAction` derives `PartialEq` but not `Eq` (it carries
  floats elsewhere in the enum), so `GiveDebugOutcome` cannot derive `Eq`
  either — tests compare with `assert_eq!`/pattern-match, not `assert!(...
  == ...)` inside a `HashSet` or similar.

## Configuration

Nothing new to configure on the client — no feature flag, no constant. The
translated command always targets `@s` (the connected player), never a
selector or another player's name.

## Dependencies

- `lodestone_assets::ResourceLocation` for item-id parsing (already a
  `lodestone-shell` dependency).
- `lodestone_ecs::session::SessionChat` for the local echo/error line —
  same component the inbound chat path writes to.
- The server's own `/give` command and op check — this command sends real
  bytes and trusts the server to do the actual validation of the item id
  against its registry (an id that parses as a valid `ResourceLocation` but
  names no real item, e.g. `minecraft:not_an_item`, is rejected by the
  *server*, and that rejection surfaces as an ordinary chat line, not a
  local error — `intercept_give_debug` only validates shape, not
  existence).

## Opping the interactive player on the live oracles

`/givedebug` (like anything gated on op) is refused unless the connecting
account is op. `scripts/live-oracles/{creative,survival,terrain}.sh` now op
an account automatically once the server reports ready, via
`scripts/live-oracles/rcon-op.py`.

**Why RCON `op <name>` rather than pre-writing `ops.json`.** Offline mode
derives the account UUID from the *username*, so in principle `ops.json`
could be populated before the player ever joins. That requires reproducing
Mojang's offline-UUID algorithm (an MD5-based UUID v3 over
`"OfflinePlayer:" + name`) correctly in tooling outside the server — exactly
the class of hand-rolled reimplementation this repo's `CLAUDE.md` warns has
burned whole sessions when subtly wrong, with no easy way to verify it
short of a live check. `op <name>` over RCON needs none of that: the
server derives the UUID itself (and has since 1.7.6, long before RCON was
scriptable here), whether or not the named player has ever joined. Letting
the server do its own UUID derivation is strictly more robust.

**The name is configurable.** Set `LODESTONE_OP_NAME` before running a
script to op your actual in-game username; the default, `LodestonePlayer`,
is a deliberately generic placeholder rather than a hardcoded personal
account.

```bash
LODESTONE_OP_NAME=YourMinecraftName ./scripts/live-oracles/survival.sh
```

**Never disturbs the test path.** The op step is best-effort: it retries
for a few seconds, and on failure prints a warning to stderr but always
returns success, so no live gate that starts an oracle can fail because
RCON hiccupped. It also cannot collide with `lodestone-testsupport`'s
`unique_username()` — that generates a fresh per-test name every call
(`E<seq>_<stamp>`), which never matches a fixed interactive name, and no
live gate depends on being op to pass.

**One RCON constraint that bit this repo before, respected here too.**
Vanilla's RCON server performs exactly one `read()` per request and closes
the connection unless that single read contains the whole frame. `rcon-op.py`
builds each frame as one contiguous `bytes` object and sends it with a
single `sock.sendall(...)` — never multiple `send()` calls — for exactly
this reason.

**Verified against a real server**, not just read for plausibility: against
the already-running `lodestone-survival` oracle, `rcon-op.py 127.0.0.1
25566 lodestone "op LodestonePlayer"` returned `Made lodestoneplayer a
server operator` and added a `"lodestoneplayer"` entry to
`.cache/mc/survival/ops.json`; re-running it printed vanilla's own
`Nothing changed. The player already is an operator` (still exit 0); and an
intentionally wrong password produced `RCON authentication failed` with a
non-zero exit, confirming the auth-failure path is real rather than assumed.

**creative.sh and terrain.sh do not manage their own `server.properties`**
(unlike `survival.sh`, which rewrites it every run) — they rely on RCON
already being enabled in the on-disk, gitignored world directory, which was
true for both at the time this was written (`rcon.password=lodestone`,
ports 25571 and 25581 respectively). If either world is ever regenerated
from scratch, RCON needs enabling by hand before the op step can do
anything; it degrades to a harmless warning otherwise.
