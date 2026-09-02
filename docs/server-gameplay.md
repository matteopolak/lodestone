# Server-authoritative gameplay: breaking, inventory, crafting, effects, advancements

## What it is

Five gameplay surfaces the server is authoritative over rather than trusting a client's own claim:
whether a block break is legitimate and when it actually completes, what a player's inventory holds
and what a container click actually does to it, whether a crafting grid actually produces the
result a client displays, which sounds/particles/level events a client should see that it cannot
predict on its own, and a player's advancement and statistic progress. The shared idea across all
five: the server independently derives the real outcome and only trusts a client's own prediction
as far as comparing it against that derived truth, correcting it (never silently accepting it) when
the two disagree.

## How it works

### Block-break validation

A held-down dig is validated the same way vanilla times one: a per-tick destroy-progress rate is
computed from the block's hardness and the held tool (using a lower-bound speed estimate multiplied
by a generous headroom, since this server does not yet track enchantments, potion effects, or other
speed modifiers a real client's tool might have — the goal is rejecting an impossible dig, not
exactly reproducing vanilla's own timing math). A start-and-immediately-stop pair that clears the
completion threshold breaks the block; a genuine one-tick block (zero hardness) breaks on the start
action alone, matching a real client's own "instant mine" behavior; and — the fix for the more
disruptive of the two original bugs — a stop that arrives too early is never simply *refused*. It is
deferred: the dig keeps accruing progress on the server's own clock until it reaches full, and the
block breaks a tick or two late with no rollback sent, exactly matching vanilla's own behavior for
this case. Refusing it outright looked more correct and was not: on a local integrated server both
packets typically land on the very same tick, which no non-instant block could ever legitimately
clear that fast, so refusing broke ordinary block breaking entirely. Creative mode is not modeled
anywhere in this server yet, so its instant-break behavior is a named, accepted gap rather than a
partial fix — and it cannot be half-fixed by treating a lone start action as sufficient, since that
is indistinguishable on the wire from an ordinary survival player who taps and moves on, and would
reopen the instant-break exploit this validation exists to close.

### Server-authoritative inventory and container clicks

The server keeps its own model of a player's inventory (the same native slot numbering the client's
own menu code uses, intentionally restated rather than shared, since this crate is deliberately
client- and version-free) and now decodes the two packets that actually change it over the wire: a
hotbar-selection change, and a container click. A join sends the player's actual restored inventory
as an explicit snapshot — without it, a freshly joined client starts from an empty default and only
discovers its real inventory contents on the first click, when a corrective full resync catches the
disagreement; nothing was ever lost, but the client had never been told what it already had.

**A container click's outcome is derived server-side, never taken on faith from what the client
claims changed.** Earlier, trusting the client's own diff of "which slots changed and to what"
seemed harmless because it could not introduce a disagreement that was not already possible — which
missed the actual problem: it let any client mint any item into any slot simply by naming it in that
diff. The server now replays the click itself (the same state machine a real client runs locally) as
a source of truth over the same slots, and compares the result against what the client claims;
where the two agree, nothing extra is sent, and an honest client pays no additional traffic, but a
disagreement produces a full corrective resync rather than accepting the client's version. The
crafting-grid slots specifically route through the crafting model below so that a result slot is
always re-derived rather than copied from a claim.

A couple of gameplay actions reached no effect for a surprisingly mundane reason worth remembering
as a class: the client-side half (a keybind, an encoder) was complete, but the specific wire values
those actions used had never been added to the server's own decode table at all, so the packet was
discarded before any router or dispatcher ever saw it — when a keypress reaches nothing, check the
decode step itself before suspecting anything downstream of it. One such case also had its two
outcomes swapped from what the keybind names would suggest, which is exactly the kind of mistake
that produces a well-formed but backwards packet on both sides and is invisible without an explicit
assertion on which outcome is which.

### Server-side crafting

The server keeps its own crafting grid and resolves a result from the bundled real recipe corpus,
re-deriving the result immediately on every input change so there is no way for a stale or
client-claimed result to exist even momentarily. The result slot itself cannot be *written* by
anything a client sends — but that is not the same as being unclickable, and conflating the two
produced a real, multi-symptom bug: taking the result (a click on it) is precisely how crafting
happens, consuming ingredients from the grid as a side effect of the take rather than the click
being a plain slot write. The actual defect was in how a disagreement was detected: the comparison
used to check only the slots a client's own prediction claimed to have changed, and a client cannot
predict a result it never computed itself, so a real craft happening entirely server-side always
looked like agreement and was never communicated back — the craft was real, but visually the output
looked unclickable, and a shift-click on it required closing and reopening the container before
anything appeared to happen. The fix is the same general rule as the inventory case above: compare
the client's claim against the whole derived menu state, not just the slots the claim itself names.

A crafting table's own grid is not backed by a block entity the way a furnace or hopper's container
is (vanilla itself throws its crafting-table container away on close), so opening one is handled as
its own kind of window with its own transient grid, carried on the player's own per-connection state
rather than looked up from the world — closing it must return both the grid's contents and anything
held on the cursor back to the player (or drop it in the world), since silently discarding either
would delete items on every close. Recipe-book "place recipe" requests reference a recipe by an
opaque, server-assigned index into the recipe list the server itself sends at join — that packet
has to be sent for the feature to be reachable at all, and the index space must be built from
exactly the same ordering the server resolves an index back into a recipe with, or a client's
request silently places a *different* recipe than the one it asked for.

### Server-initiated sounds, particles, and level events

Anything a client cannot predict for itself on a real event (a mob's hurt or death sound, a door
opening from a redstone signal, a block breaking, an item being placed) has to be told to it
explicitly, or it is simply silent — this server went a long time with no encoder for any of these
at all, at which point every one of those moments was inaudible and invisible regardless of how
correct the underlying simulation was. **The double-trigger trap is the one gotcha specific to this
subsystem**: the client already predicts a small set of these itself locally (its own block break
and placement sounds, in particular), so an effect published back to the *same* connection that
caused it plays twice unless the acting player is explicitly excluded from that one broadcast —
every other connection still needs to hear it normally.

### A recurring trap across all three protocol seams above (and advancements)

**Every server-side gameplay encoder here (crafting/recipe-book, world effects, advancements and
statistics) is a defaulted `ServerProtocol` method, and every one of them must also be forwarded
through the generic "boxed protocol" wrapper that singleplayer specifically uses — a forgotten
forward is not a compile error, it silently answers with the trait's own do-nothing default.**
This has shipped more than once, and each time the symptom was identical: every other hosting path
worked, while singleplayer specifically emitted nothing at all for the affected feature, because
singleplayer is the one path in this codebase that reaches a protocol implementation through that
generic wrapper rather than a concrete type. Adding a new server-side encoder anywhere in this
subsystem needs its forward added in the same change, not as an afterthought.

### Advancements and statistics

A version-free model of the advancement tree, per-player criteria progress, and a statistics
counter, tracked server-side and streamed to a client over its own dedicated packets — mirroring
vanilla's own split of "an advancement is complete once every requirement group has at least one
satisfied criterion" and a fixed, shallow visibility window (a node is shown if it, or something a
small fixed number of generations below or above it in the tree, is complete) that deliberately does
not get wider just because a distant ancestor happens to be done. Progress is flushed to a client
incrementally as it changes rather than resent in full every time, with one deliberate exception: a
join always sends the complete tree once, unconditionally, before any incremental update — sending
an incremental delta first, with nothing to compare it against, is meaningless.

## How to change it

- **A new native inventory slot or equipment kind**: extend the server's own inventory model and its
  menu-slot-to-native-slot mapping together; keep the numbering deliberately restated to match the
  client's own equivalent table rather than imported, since this crate has no dependency on the
  client's own code and a numbering drift between the two would silently misroute items.
- **A new container/menu kind (a real chest, a brewing stand, and so on)**: give it a real backing
  model with its own slot layout, rather than special-casing it inside the generic click handler —
  the click-derivation logic is deliberately generic over "the menu currently open," not per-kind.
- **A new server-initiated sound, particle, or level event**: add it as a new case in the shared
  effect vocabulary and its one encoder — never invent a second transport lane for it.
- **A new server-derived encoder of any kind added to this subsystem**: add its forward to the boxed
  protocol wrapper in the same change, and prefer a test that enumerates the trait's own methods
  against the wrapper's implementation over one that repeats a hand-maintained list — a hand-written
  list cannot notice a method that was never added to it in the first place.
- **Loosening the block-break speed check**: only once the server actually tracks the per-player
  inputs (enchantments, effects, tool) that speed genuinely depends on — until then, narrowing the
  headroom risks rejecting a legitimate player rather than only catching a genuine cheat.

## Configuration

None of these five surfaces has a runtime flag or setting; behavior is either a fixed constant
transcribed from vanilla (break-progress thresholds, effect ranges) or driven entirely by the
bundled game data (the recipe corpus, the advancement tree).

## Dependencies

- Generated per-block-state and per-item data (hardness, tool speed, sound/particle registries) for
  block-break timing and world effects.
- The bundled real recipe corpus and the shared, version-free recipe-matching logic also used
  client-side, so there is exactly one implementation of "does this grid match this recipe."
- The `ServerProtocol` seam (see `docs/dedicated-server.md`) for every wire-facing piece described
  here; none of these modules names a packet id or protocol version directly.
