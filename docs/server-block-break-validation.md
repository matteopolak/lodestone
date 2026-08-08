# Server-side block-break validation

## What it is

The server's answer to "may this player really break that block, now?" — vanilla's
`ServerPlayerGameMode.handleBlockBreakAction` destroy-progress arithmetic, ported to
`crates/lodestone-server/src/block_breaking.rs` and consulted by `server.rs`'s
`apply_block_action`. It fixes two defects that were opposite ends of one missing
computation: a zero-hardness block (grass, flowers, sugar cane) could not be broken
*at all*, and any block — bedrock included — could be broken *instantly*.

## How it works

### The two defects, and why they are the same bug

Before this, `apply_block_action` was a three-line state machine: `StartDestroy`
recorded a position in `pending_break`, `StopDestroy` set that position to air.

* **One-shot blocks were unbreakable.** Vanilla computes destroy progress on the
  *start* action and, when it reaches `1.0F` in that first tick, calls
  `destroyAndAck` immediately — the `"insta mine"` branch, which is every
  zero-hardness block. A client that knows the block is instant therefore sends
  `START_DESTROY_BLOCK` **and nothing else**. `pending_break` was set and never
  consumed.
* **Everything was instantly breakable.** With no timing check, a `StartDestroy`
  followed immediately by a `StopDestroy` broke obsidian, or bedrock, from any
  distance (issue #531).

### The arithmetic

`block_breaking::progress_per_tick(block_state, held)` is vanilla's
`BlockBehaviour.getDestroyProgress`:

```
dig_speed / hardness / divider        divider = 30 with the correct tool, 100 without
```

over `lodestone_data::hardness` (jar-derived) and `lodestone_data::tool::mining` —
the same censuses the *client's* mining predictor reads (see
[`block-break-timing.md`](./block-break-timing.md)). Note `ToolMining::correct_tool`
is already vanilla's `hasCorrectToolForDrops`, i.e. **true for a bare hand on a block
that requires no tool**, so dirt divides by 30 and stone by 100.

Then, in `apply_block_action`:

| phase | behaviour |
|---|---|
| out of range, any phase | dropped, before the per-ordinal fork (vanilla's first guard) |
| `StartDestroy`, `progress_per_tick >= 1.0` | broken immediately — the one-shot fix |
| `StartDestroy`, otherwise | recorded as a `PendingBreak { pos, progress_per_tick, start_tick, deferred: false }` |
| `AbortDestroy` | clears `pending_break` if the position matches |
| `StopDestroy`, progress `>= 0.7` | broken |
| `StopDestroy`, progress `< 0.7` | **deferred, not refused** — `PendingBreak::defer` flips `deferred`, and the dig keeps accruing progress on the server's clock until it reaches `1.0` |
| every 50 ms, `deferred` dig at `1.0` | broken by `serve_play`'s `vitals_tick` arm |

An unbreakable block (`hardness == -1.0` — bedrock, barrier) prices at `0.0` progress
per tick, so it satisfies none of the tests at any tick count, and `defer` returns
`None` for it so it cannot park a doomed dig in the slot either. That is a property of
the arithmetic, not a special case.

### The deferred continuation, and why a refusal was wrong

The first cut of this validation *refused* a short `StopDestroy` and re-sent the
block's real state for the client to roll back on. **That broke ordinary block
breaking outright** — hold the mouse on stone, release, nothing happens — and it is
the worse of the two bugs, because a `StopDestroy` is the normal end of a dig for
every block the client does not treat as instant, and on a local integrated server
both packets are read off one buffer and land on **one server tick**, where no
non-instant block can possibly have reached `0.7`.

Vanilla does not refuse either. `ServerPlayerGameMode.handleBlockBreakAction`'s
shortfall branch (`:229-234`) arms `hasDelayedDestroy` / `delayedDestroyPos` with
`delayedTickStart = destroyProgressStart`, and `tick()` → `incrementDestroyProgress`
keeps accruing progress until it reaches `1.0`, at which point the block is destroyed
a tick or two late. It sends no rollback at all on this path. `PendingBreak::deferred`
is that state, and `serve_play`'s `vitals_tick` arm — already a 50 ms timer, one
server tick — is `tick()`. Like vanilla, that pass re-reads the block first and
abandons the dig if something else already removed it, so a deferred break cannot
roll a second set of drops into air.

The deferred target is the whole block (`DELAYED_DESTROY_PROGRESS = 1.0`), not `0.7`:
the `0.7` shortcut is a concession to the client having released the button, and a dig
that did not earn it pays full price on the server's own clock instead.

One deliberate divergence: this crate keeps **one** `pending_break` slot where vanilla
keeps `isDestroyingBlock` and `hasDelayedDestroy` side by side (and prefers the
delayed one in `tick()`). A fresh `StartDestroy` therefore replaces a deferred dig
rather than coexisting with it. The client only ever has one dig in flight, so the
second slot would only model a quirk.

The actual breaking — loot roll, item-entity spawn, block-entity removal, the
`block_update` packet — moved into `server.rs`'s `destroy_block`, vanilla's
`ServerPlayerGameMode.destroyBlock` funnel, because there are now two call sites.

### Where the tick comes from

`dispatch_play_packet` takes a `game_tick: Option<u64>`. The native `serve_play`
passes `ticks_since(play_start)` — the same monotonic counter the time-of-day
broadcast already uses, so a dig's start and stop are priced on one clock. On
`wasm32` it is `None`, because that loop has no `tokio::time`; a `None` on either
side of the comparison skips the **timing** test only, and the hardness and range
tests still apply.

### Resolving the block name

`progress_per_tick` resolves its state through `mobs::block_state_id_or_default`, not
`block_state_id`. The exact index only contains a *bare* name for a block with no
properties, so `"minecraft:stone"` resolves and `"minecraft:sugar_cane"` does not —
every sugar cane state carries `age`. A miss produced `None`, which the caller reads
as "unknown block, do not validate", and that path still waits for a `StopDestroy`
an instant block never sends. Both censuses read here are per-*block*, so the
block's lowest state id is an equivalent key.

## How to change it, and the gotchas

**This is deliberately a plausible check, not an exact port, and
`UNTRACKED_SPEED_HEADROOM = 8.0` is why.** Vanilla's `getDestroyProgress` reads the
whole player: Efficiency, Haste, Mining Fatigue, Aqua Affinity, the
`block_break_speed` attribute, eyes-in-water, feet-on-ground. `lodestone-server`
tracks none of those — no attribute map, no effect list, no game mode. A strict port
would reject legitimate breaks by a player with an enchanted tool, which is worse
than the bug being fixed. So the server's estimate is a lower bound, multiplied by a
headroom that comfortably clears the realistic worst case (Efficiency V ≈ 4.3× ×
Haste II 1.6× ≈ 6.8×) before comparison. Bare-handed obsidian is still rejected by
three orders of magnitude.

If the server grows real per-player attributes and effects, feed them in and drop the
headroom. `lodestone-game`'s `BreakInputs` is the full-fidelity client-side twin and
is the thing to mirror; this crate deliberately does not depend on it.

**Creative mode is not modelled**, here or anywhere in this crate: vanilla's
`instabuild` branch destroys any block on `StartDestroy`, and a creative client that
sends no `StopDestroy` for stone will find it does not break. Nothing in
`lodestone-server` tracks a game mode, so this is a named pre-existing gap rather
than a half-fix.

**The deferred continuation does not fix it, and cannot.** A deferred dig is armed
by a `StopDestroy`; a creative client sends none, so nothing arms. And the wire is
indistinguishable: a lone `StartDestroy` on stone is *also* what an ordinary survival
player sends when they tap-and-move-on, so breaking on it unconditionally would
reintroduce exactly the cheat this module exists to close. The real fix is game-mode
state — honour the serverbound `CHANGE_GAME_MODE` this crate currently decodes into
`ServerBound::Ignored` (`crates/protocol/v770/src/server_protocol.rs`), and stop
hardcoding `game_type: 0` in `begin_play` / `JOIN_GAME_MODE` — then take vanilla's
`instabuild` branch. Until then, note that **no client can legitimately be in
creative against this server anyway**, because survival is all it ever advertises.

**Interaction range is measured eye-to-block-*centre***, where vanilla's
`isWithinBlockInteractionRange` measures to the closest point of the block's box.
That is up to ~0.87 shorter, so `MAX_INTERACTION_DISTANCE` is rounded up to `6.0`
rather than reproducing vanilla's `5.5`. The point is to reject a break from across
the world; per-block AABB geometry is not worth the last half-block here.

**A test that wants the break to land *on the `StopDestroy`* must hold the dig.** A
back-to-back pair lands on one server tick, which now takes the *deferred* path — the
block still breaks, but a few ticks later, so a gate that drains the stream
immediately after the pair sees nothing. See `BARE_HANDED_DIG` in
`tests/serve_play.rs`, and `a_same_tick_stop_breaks_the_block_a_few_ticks_later`,
which turns that lag into the thing under test. Use `tokio::time::sleep`, **not**
`tokio::time::advance`: `advance` jumps the clock before yielding, so the server has
not yet read the `StartDestroy` and stamps it with the already-advanced tick, putting
both packets on one tick anyway. A paused-clock `sleep` lets the start packet drain
first. A diamond pickaxe on stone clears the threshold in a single tick, which is why
`break_with_a_pickaxe` needs none of this.

## Configuration

Four constants in `block_breaking.rs`, each documented on itself:

| constant | value | what it is |
|---|---|---|
| `STOP_DESTROY_PROGRESS` | `0.7` | vanilla's `destroyProgress >= 0.7F` acceptance threshold for a `StopDestroy` |
| `DELAYED_DESTROY_PROGRESS` | `1.0` | vanilla's `tick()` threshold for a *deferred* dig — the whole block |
| `UNTRACKED_SPEED_HEADROOM` | `8.0` | multiplier absorbing every speed input this crate does not track |
| `MAX_INTERACTION_DISTANCE` | `6.0` | eye-to-block-centre reach limit |

## Dependencies

`lodestone_data::hardness` and `lodestone_data::tool` for the censuses,
`crate::mobs::block_state_id_or_default` to bridge state strings to global state ids,
`lodestone_model` for the vocabulary. Names no packet and no protocol version.
`crate::server`'s `apply_block_action` and `destroy_block` are the only callers; see
[`block-edit.md`](./block-edit.md) for the packet path around them and
[`block-drops.md`](./block-drops.md) for what happens after a block does break.
