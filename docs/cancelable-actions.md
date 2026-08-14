# Cancelable interaction verbs

## What it is

`ActionVetoes` (`crates/lodestone-ecs/src/veto.rs`) is the veto point for the interaction verbs every
protection, anti-grief and anti-cheat plugin actually cancels. A plugin registers a predicate per verb;
the engine asks *before* it commits, and a `Deny` stops the action before the predictor runs and before
anything reaches the wire. This is what separates "plugins can read state" from "plugins can be a
protection plugin".

## How it works

```rust,ignore
use lodestone_ecs::veto::{ActionVetoPlugin, ActionVetoes, Verb, VerbContext, Verdict};

fn protect_spawn(vetoes: &mut ActionVetoes) {
    vetoes.register(Verb::BlockBreak, "spawn-protection", 0, |ctx| {
        let VerbContext::BlockBreak { pos, .. } = ctx else { return Verdict::Allow };
        Verdict::deny_if(pos.x.abs() <= 16 && pos.z.abs() <= 16)
    });
}
```

Lower `priority` runs first; ties keep registration order. The **first `Deny` short-circuits** — later
predicates are not consulted and cannot un-deny. `ActionVetoes::names(verb)` answers "which plugin
cancelled this", the first question anyone asks of a protection plugin.

### Coverage: four verbs wired, two deferred

| verb | commitment point | asked? |
|---|---|---|
| `BlockBreak` | `lodestone_shell::interact::drive_mining`, before `Mining::continue_` | **yes** |
| `BlockPlace` | `interact::drive_placement`, before `Placement::use_on` | **yes** |
| `EntityDamage` | `lodestone_shell::sim::actions::Sim::attack_entity` | **yes** |
| `PlayerMove` | `lodestone_controller::ecs::send_player_input` | **yes** |
| `InventoryClick` | `ClientHandle::menu_click` → `SharedState::menu_click` | **not yet** |
| `PlayerInteract` | `Sim::use_item_live`'s three branches | **not yet** |

The three the issue names as making a protection plugin buildable — break, place, damage — are wired,
plus move. `crates/lodestone-ecs/tests/veto_coverage.rs` asserts this table by scanning for each verb's
ask site, so a verb losing its wiring fails a test instead of becoming a plugin author's bug report.

**Why the last two are deferred rather than forced in:**

- `InventoryClick` commits in `SharedState::menu_click` (`state.rs`), which builds the action while
  **holding a write guard** on the `World`. Asking there is legal but wants its own care, and the
  app-layer callers (`app/container_input.rs`) have no `World` access at all.
- `PlayerInteract` commits in three branches of `Sim::use_item_live`, each of which runs the placement
  predictor and **takes a use-sequence number**. Denying after the sequence is taken forks the counter,
  which `docs/baritone-port.md` §3.6 forbids outright, so the ask must go ahead of it in all three
  branches.

A plugin can register for either today; it simply will not be consulted. That is stated here and
asserted by `the_verbs_documented_as_unwired_really_have_no_ask_site`, so the claim cannot quietly
become false in either direction.

### Why a synchronous predicate, and why it gets no `World`

The design constraint itself is: *"a plugin system that cancels must not need to re-enter the
World to do so"*. Both obvious designs fail it:

- **A `Message` a plugin answers.** The commitment happens inside one system (or one `Sim` method), so a
  plugin system reading a message would not run until the *next* tick — after the block is broken.
- **A predicate handed `&World`.** Three verbs commit from plain `impl Sim` methods that reach the
  `World` through `self.read`/`self.write`. A predicate called there is *inside a guard*, and any
  `hold_read` in it is `handle.rs`'s rule 1 — the `accb993` hang, with no panic and no log line.

So `VetoFn` takes only the verb's own `VerbContext` and returns `Verdict`. It **cannot** re-enter the
`World` because it is handed no way to. A plugin needing world state keeps it in an `Arc` its own system
refreshes each tick. That is a real constraint on plugin authors, and it is the price of the deadlock
being unrepresentable rather than merely discouraged.

`allows` takes `&self`, which is what lets a system holding `Res<ActionVetoes>` and a `Sim::read`
closure holding a guard share one entry point.

### Cost

A count, not a duration (`CLAUDE.md`). `ActionVetoes` keeps a `u64` bitset of verbs with at least one
predicate, so `allows` returns after **one bit test** when nothing is registered for that verb:

| situation | `invocations` |
|---|---|
| nothing registered, 10 000 asks | **0** |
| one predicate, 50 asks | **50** |
| a denying predicate ahead of one that panics if reached | **1** per ask (short-circuit) |

That bitset matters because `PlayerMove` is asked on every input change, and the common case is an empty
registry.

### Relationship to the outbound hook

Two different layers, deliberately:

- `ActionVetoes` stops a **verb** *before* the predictor runs, so client state never diverges.
- `EgressFilters` inspects a **`ClientAction`** at the `ActionQueue` drain, after the fact.

`EgressFilters` structurally cannot cover attack, use-item or inventory click, because those bypass
`ActionQueue` and write the socket directly (`docs/outbound-action-hook.md` has the measured list). That
is precisely why the veto is a separate mechanism at the verb level rather than a special case of the
hook.

### Closing out the remaining design questions

The original design question ("event cancellation semantics in an ECS schedule") named four open
questions. This mechanism answers the first two by construction (a pre-check gate, and "cancelled" is
absence-of-effect rather than a `bool` a plugin sets or a `Commands`-deferred undo — see "Why a
synchronous predicate" above). The remaining two:

**(3) Monitor-priority interaction with a pre-check gate.** These are not in tension, because they
operate at different points in the pipeline rather than competing for the same one. `ActionVetoes`
runs *before* a verb's effect commits, inside the engine method that would otherwise commit it, and
sees a typed `VerbContext` — never the `World`. `EventPriority::Monitor`
(`docs/plugin-api.md`'s "plugin event bus" section) is a read-only tier over `GameEvent`, which is
pushed from `SharedState::apply` *after* the effect (or non-effect) already happened, from the
`ClientEvent` the server or the ingest fold actually produced. So a `Monitor` observer never sees "the
veto decision" as an event of its own — it sees whatever the verb's outcome was: no `EntityDamaged`
event exists for damage a veto stopped, because the veto ran before `Sim::attack_entity` produced one.
A `Monitor`-tier plugin logging "what happened" and a veto-tier plugin deciding "should this happen"
are answering different questions at different times by construction, the same separation Bukkit's
own `MONITOR` priority keeps from `LOWEST`..`HIGHEST`'s cancellation-capable tiers — the two were never
going to need to coordinate directly.

**(4) A generic `Cancelable<T>` wrapper, versus a bespoke gate per verb.** Built and shipped as the
*second* option, and it turned out to generalize better than a generic wrapper type would have: one
`Verb` enum, one `VerbContext` (a per-verb payload enum), one `Verdict`, one `ActionVetoes` registry —
uniform across all six verbs despite their commitment sites having genuinely different shapes (some
inside a `System`, some inside a plain `impl Sim` method already holding a guard). A generic
`Cancelable<T>` wrapping each verb's own event/component type would have needed one instantiation per
verb *and* would not by itself have solved the reentrancy constraint that motivated the whole design —
the constraint lives in what the predicate is handed, not in how the wrapper is spelled. Verb-keyed
dispatch was the cheaper generalization for the actual hard part.

**Closed.** Both remaining questions have answers now on record.

## How to change it, and the gotchas

- **Never hand a veto predicate a `World`, an `EcsHandle`, or anything reaching either.** The whole
  soundness argument is that it has no way to re-enter the lock. An overload "just for this one verb"
  deletes the argument.
- **Ask before the predictor, not after.** `drive_mining`'s veto sits ahead of `Mining::continue_`, and
  `drive_placement`'s ahead of `Placement::use_on`, because both advance a state machine and `use_on`
  takes a block-prediction `sequence`. Forking that counter is forbidden outright
  (`docs/baritone-port.md` §3.6). A denial must leave it untouched.
- **A denial mid-dig must abort the dig.** `drive_mining` denies via the same idempotent `mining.0.stop()`
  every other early return uses, so a protection plugin denying during a hold sends one `ABORT` rather
  than stranding the predictor with a dig the server never sees finished.
- **`PlayerMove` is asked *after* the edge check, and that ordering is load-bearing.** The veto sits
  after `if last.0 == Some(next) { continue; }` and before `last.0 = Some(next)`, so a denial does not
  latch `LastPlayerInput`. Latching a value that was never sent is the exact bug `Egress`'s own doc
  describes: the first real change after the veto lifted would be suppressed as a redundant resend.
- **`BreakRejection::Vetoed` and `PlaceRejection::Vetoed` are the one non-legality variant in each enum.**
  Every other variant is "something the shell would have refused from a mouse click too". A veto applies
  to the human path identically, so a plugin seeing `Vetoed` should look for *another plugin*, not for a
  mistake in its own intent.
- **The coverage gate is a source scan, and knows it.** It cannot tell whether an ask is in the *right
  place* (a `VerbContext::BlockBreak` built after the predictor advanced would satisfy it and be
  useless) or whether the code is reachable. It catches the specific regression the island rule warns
  about: wiring silently disappearing while every unit test stays green. Placement correctness is
  argued at the call site and gated by `crates/lodestone-shell/tests/break_intent.rs`.
- **That gate needs both halves.** `every_verb_claimed_wired_has_a_real_engine_ask_site` and
  `the_verbs_documented_as_unwired_really_have_no_ask_site` are a pair: a scanner that returned every
  file for every query would pass the first and fail the second. And
  `every_verb_variant_is_either_wired_or_explicitly_deferred` means adding a `Verb` and forgetting to
  wire *or* document it fails here.
- **Do not guess a file-count floor for a scanner control.** `lodestone-controller` has **four** source
  files; an earlier `> 5` floor failed on it. Measure the tree.

## Configuration

`app.add_plugins(ActionVetoPlugin)`. Opt-in: every ask site uses `Option<Res<ActionVetoes>>` or
`get_resource`, so a client with no plugin never has the resource and every verb is allowed by a `None`
check.

## Dependencies

`bevy_app`, `bevy_ecs`, `lodestone_model::BlockPos`. Ask sites live in `lodestone-shell`
(`interact.rs`, `sim/actions.rs`) and `lodestone-controller` (`ecs.rs`).

## See also

- [`docs/outbound-action-hook.md`](./outbound-action-hook.md) — the `ClientAction` layer, and
  the measured list of paths that bypass it.
- [`docs/plugin-api.md`](./plugin-api.md) — the intent doctrine these verbs sit on top of.
- [`docs/plugin-async-tasks.md`](./plugin-async-tasks.md) — the same "plugin code never gets a `World`
  it could deadlock on" argument, enforced at runtime there.
