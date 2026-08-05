# The outbound action hook

## What it is

`EgressFilters` (`crates/lodestone-ecs/src/egress.rs`) is the seam where a plugin inspects, replaces or
suppresses a `ClientAction` that another plugin queued, after `GameTick` has finished and before it
reaches the socket. ProtocolLib's outbound side, at the one layer where it is version-free. Issue #157.

## How it works

A plugin registers a callback on the `EgressFilters` resource; the driver's `ActionQueue` drain
(`lodestone_shell::sim::step::Sim::drain_action_queue`) runs every registered filter over the queue
before handing what survives to `NetClient::send_action`.

```rust,ignore
use lodestone_ecs::{EgressFilterPlugin, EgressFilters, Verdict};
use lodestone_model::ClientAction;

fn install(filters: &mut EgressFilters) {
    filters.register("no-swinging", 0, |action: &ClientAction| {
        if matches!(action, ClientAction::SwingArm { .. }) {
            Verdict::Suppress
        } else {
            Verdict::Allow
        }
    });
}
```

`Verdict` is `Allow`, `Suppress`, or `Replace(Box<ClientAction>)`. Lower `priority` runs first; ties keep
registration order. **The first non-`Allow` verdict wins** — a `Suppress` cannot be un-suppressed, and a
`Replace` is not re-offered to later filters, so two filters that each rewrite the other's output cannot
loop.

### Scope: suppression and version-free replacement, never encoded bytes

Issue #157 is explicit that encoded-packet mutation must not be attempted without re-opening #156's
version-leak concern (still open). Nothing here goes near encoded bytes. The hook sees `ClientAction`,
which is `lodestone-model`'s **version-free** vocabulary — no protocol id, no field order, no wire
encoding — which is exactly why `Verdict::Replace` is safe to offer: handing a `ClientAction` back
cannot leak a version into a shared crate. The concern #156 guards lives at the other layer, inside a
version crate's `VersionAdapter`, and this hook cannot reach it.

### Why a callback and not a `Message`

The issue offers "a `Message`/callback". A `Message` **cannot** work: the drain happens *after*
`GameTick` has finished, in the driver, so a plugin system reading a `Message` would not run again until
the next tick — by which time the action has already gone. Suppression has to be synchronous with the
drain.

The callback receives only `&ClientAction`, **never the `World`**. The drain runs while the driver holds
the `World` guard, so a callback handed a `World` would be one `hold_read` away from the reentrant
deadlock `handle.rs`'s rule 1 exists to stop. A filter needing world state captures an `Arc` its own
system keeps current; a veto that genuinely must consult the world is issue #109's problem, not this
one's.

### Cost, as a count

`CLAUDE.md`: prefer a counter over a duration, and two sequential durations are not protected by being a
ratio. So the cost claim is a **count**, and an exact equality rather than a bound:

| situation | `EgressStats::invocations` |
|---|---|
| no filter registered, 64 actions × 100 drains | **0** |
| 3 filters, 10 actions, all `Allow` | **30** (= actions × filters) |
| 1 suppressing filter + 1 that must not run, 3 actions | **3** (short-circuit) |

With nothing registered, `apply` returns after one `Vec::is_empty` check — no virtual dispatch, no
allocation, no per-action work — and the driver pays one `get_resource` lookup per tick. Note this hook
is **not** on the per-packet-per-player encode path the issue warns about; it runs once per tick over
the local client's own handful of queued actions.

### The gap: three verbs bypass this hook entirely

`ActionQueue` is documented as "the one sanctioned egress" (`player.rs:755`), and for anything a
*plugin* queues it is. It is **not** the only path to the socket. Measured, by
`crates/lodestone-ecs/tests/egress_hook_coverage.rs`, five files reach `send_action` directly on
user-visible paths:

| file | what bypasses |
|---|---|
| `lodestone-shell/src/sim/actions.rs` | attack, interact-entity, use-item (#109's verbs 3 and 6) |
| `lodestone-client/src/handle.rs` | container clicks via `ClientHandle::menu_click` (#109's verb 4) |
| `lodestone-shell/src/app/container_input.rs` | container-screen clicks from the app layer |
| `lodestone-shell/src/app/menus.rs` | sign-edit / menu submission |
| `lodestone-shell/src/sim/session.rs` | respawn, container-close, carried-item selection |

Each is deliberate — discrete clicks control their own wire ordering — but the consequence is that **a
filter cannot see an attack, a use-item or an inventory click**. That is a limit of the seam issue #157
specifies, not an implementation shortfall, and it is why #109's veto is a separate mechanism at the
verb level rather than a special case of this one.

### This is the outbound half only — the inbound half is not wired

Worth stating so nobody records plugin egress/ingress as end-to-end when one direction is missing. This
hook covers **outbound** `ClientAction`s (client → server), with the coverage table above bounding even
that. There is no inbound counterpart: the plugin command registry that landed alongside this
(`30e8f1b`, `684b95a`) is **unreachable from the wire**, because nothing decodes a serverbound
`CHAT_COMMAND` into the dispatcher. Closing that gap reportedly requires revisiting `lodestone-server`'s
deliberate refusal to depend on `lodestone-ecs`, which is a larger architectural decision than a hook.

So: outbound actions can be inspected, replaced and suppressed (within the five-file limit above);
inbound commands do not arrive at all yet. Different directions, different states, and the
version-opaque `RawPacket` half of issue #104 is a third thing again, still unbuilt.

## How to change it, and the gotchas

- **Do not give a filter `&World`.** The drain runs inside the driver's write guard; a `hold_read` from
  a filter is the `accb993` hang (no panic, no error, no log line). If a filter needs state, push it
  into an `Arc` from a system.
- **Do not extend this to encoded packets.** Re-open #156 first. The whole reason `Replace` is safe here
  is that `ClientAction` carries no version information.
- **`the_set_of_direct_send_action_sites_has_not_changed` will fail when someone adds a new direct
  send.** That is the gate working. Prefer fixing the *gap* — route the new send through `ActionQueue`,
  and the list shrinks — over appending to the list. If it must bypass, append it with a comment saying
  why, as the existing entries do.
- **That gate caught its own author.** The first version of `KNOWN_DIRECT_SEND_FILES` had four entries,
  derived from reading the call sites; the gate's first run reported nine. Without it, this document
  would have shipped a confident and wrong claim about how much of the client's egress the hook covers.
  Measure the list, never write it from a survey.
- **Its scanner needs its own controls, and has three.** Both scanned crates must exist and contain
  Rust sources (a typo in a crate name would silently scan nothing); the scan must find at least three
  hits (an empty result would only fail while the known-list is non-empty, and the natural "fix" is to
  empty the list, after which it passes forever measuring nothing); and comment-skipping is checked on a
  synthetic input, because both crates document the `ActionQueue` doctrine in prose *at the very call
  sites that matter*, so a scanner counting comments would report nearly every file.
- **`passed` is not bumped when no filter is registered.** It counts what the hook let through, and a
  hook nobody installed did not let anything through — it was not there. Counting them would make
  `invocations == 0 && passed > 0` reachable and confusing.

## Configuration

`app.add_plugins(EgressFilterPlugin)`. Opt-in: the drain uses `get_resource`, so a client with no plugin
never has the resource and pays one lookup per tick.

## Dependencies

`bevy_app`, `bevy_ecs`, `lodestone_model::ClientAction`. The one call site outside this crate is
`crates/lodestone-shell/src/sim/step.rs`'s `drain_action_queue`.

## See also

- [`docs/plugin-api.md`](./plugin-api.md) — `ActionQueue` as the sanctioned egress, and the correction
  section this refines.
- [`docs/plugin-async-tasks.md`](./plugin-async-tasks.md) — the same "never hand plugin code a `World`
  from outside the tick thread" argument, enforced at runtime there.
