# Plugin crafting-station hooks — anvil, grindstone, smithing table, loom, stonecutter

## What it is

The plugin-facing seam issue #150 asked for: a server-side mirror of Bukkit's `PrepareAnvilEvent`/
`PrepareSmithingEvent`/`PrepareItemCraftEvent`, letting a plugin allow, deny, or replace the result a
crafting station is about to show a player, before it ever reaches their screen.

The issue's own body said not to start until the underlying stations existed client- and server-side.
They now do: `crates/lodestone-server/src/anvil.rs`, `smithing.rs` and `loom.rs` (plus the already-shipped
`grindstone_result`/`stonecutting::result`) compute real, jar-verified results for all five stations, and
`crates/lodestone-server/src/server.rs`'s `workstation_result` is the one function every one of them
already passes through. This work adds no new station logic — it hooks the existing one.

[`lodestone_server::plugin_crafting`] is the seam: [`CraftingStationHooks`], a registry of
[`CraftingStationHook`] implementors, each answering one [`StationInputs`] with a [`StationVerdict`]
(`Allow`, `Deny`, or `Replace(ItemStack)`). `crates/plugins/lodestone-crafting-warden` is the reference
plugin — `AnvilBlessing` (a `Replace` example: tweaks a real anvil rename) and `SmithingSwordBan` (a `Deny`
example: vetoes one specific netherite upgrade).

## Why this is not the client-side bevy plugin API

`docs/plugin-api.md` describes a *client-side*, `bevy_ecs`-scheduled plugin tier (`lodestone-app`,
`add_plugins`). Crafting-station results are computed entirely server-side, inside `lodestone-server`'s
own plain (non-ECS) dispatch functions — the same reason `docs/plugin-worldgen-api.md`'s `ChunkGenerator`/
`DimensionRegistry` seam is a `dyn`-dispatched trait invoked from plain function calls rather than a bevy
`System`. This module follows that established precedent, not the client-side one: adopting the bevy shape
here would mean inventing a schedule this crate does not run, for state (`PlayerInventory`, `OpenContainer`)
that is never a bevy component.

Verdicts do borrow the client-side seam's *vocabulary* rather than inventing a third one:
`docs/plugin-api.md`'s intent doctrine and `docs/packet-wiring.md`'s `EgressFilters`/`ActionVetoes` both
settle on the same shape — an observation struct in, a typed `Allow`/refuse/`Replace` verdict out, first
non-`Allow` wins. `StationVerdict` is that shape, reused rather than reinvented. Two of the intent
doctrine's five clauses do not apply here and are dropped rather than faked: there is no second, *human*
source of a workstation result to arbitrate against ("human outranks a plugin" has nothing to outrank), and
a station evaluation has no lifecycle beyond answering the one question it was asked.

## How it works

### The registry rides `WorldStateHandle`

[`CraftingStationHooks`] is a `Clone`-able, `Arc`-backed registry — the same "cheap clone, one store" shape
[`PluginChannelRegistry`] already established for wire-level plugin messaging. It is a new sibling field on
[`WorldStateHandle`], for the identical reason `scoreboard`/`teams`/`nbt_storage`/`stopwatches` already ride
there: `WorldStateHandle` is already threaded to `crate::server::dispatch_play_packet`, so riding here
reaches every production call site with **no new parameter added to the `serve_connection*` wrappers**.
Only the handful of leaf functions that actually compute a station's result gained one new parameter each —
a narrow `&CraftingStationHooks`, not the whole handle, matching this crate's existing precedent
(`apply_use_item_on`'s own `difficulty` parameter comment: pass the scalar/handle a function actually needs,
never a handle that "would invite a second, unrelated read").

### `workstation_result` is the one choke point

```
apply_use_item_on ─────────► open_workstation_screen ─┐
apply_container_clicked ───► apply_workstation_clicked ├─► read_workstation_menu ─┐
apply_container_button_click ► apply_workstation_button_click ┘                   ├─► workstation_result ─► CraftingStationHooks::evaluate
apply_rename_item ─────────────────────────────────────────────────────────────┘
```

Every one of those five production entry points — opening the station, clicking inside it, picking a
loom/stonecutter offer, renaming an item, and the direct take path — already called `workstation_result`
before this work; none of them changed shape, each just gained one `&CraftingStationHooks` argument sourced
from `world.crafting_hooks()`. `workstation_result` builds the vanilla result exactly as before, then —
only when at least one hook is registered — packages it into a [`StationInputs`] (the station, its own
input cells, and the vanilla-computed result) and asks [`CraftingStationHooks::evaluate`]. An empty registry
(the default, and every pre-existing caller before this work) short-circuits before even building that
struct, so a client with no crafting plugin installed pays one `is_empty()` check.

### Verdicts

```rust
pub enum StationVerdict {
    Allow,                 // leave the vanilla result unchanged
    Deny,                  // produce nothing, regardless of what vanilla computed
    Replace(ItemStack),    // substitute a plugin-supplied stack
}
```

Hooks are asked in ascending priority order and **the first non-`Allow` verdict wins** — a later hook is
never asked once one has denied or replaced, so two hooks cannot loop rewriting each other's output, exactly
`EgressFilters`'/`ActionVetoes`' own rule. `StationInputs::computed` carries the vanilla-computed result
(`None` when the current inputs do not combine into anything), so a `Replace`-ing hook can *tweak* a real
result — append a lore line, force a name — rather than reimplementing the station's own recipe rules from
scratch; that is what makes `AnvilBlessing` (below) a few lines instead of a second anvil implementation.

### Cost is untouched

Vanilla's own `PrepareAnvilEvent` only ever lets a plugin replace the *result* stack, never the anvil's
XP-level cost. `AnvilMenu`'s `cost` `DataSlot` is computed once, from the pre-click cells alone, by
`apply_workstation_clicked`'s own `anvil_cost` binding — entirely separate from `workstation_result`. This
module follows that: a hook that replaces or denies a result does not, and cannot, change what a take costs
or whether `mayPickup` allows it.

### The reference plugin: `lodestone-crafting-warden`

`crates/plugins/lodestone-crafting-warden` ships two hooks:

* **`AnvilBlessing`** (`Replace`) — any anvil operation that already produces a custom-named result gets
  `"[Blessed] "` prepended to that name. Idempotent (does not compound on repeated reads of an
  already-blessed menu) and inert for a plain, unnamed repair.
* **`SmithingSwordBan`** (`Deny`) — refuses one specific netherite upgrade
  (`minecraft:diamond_sword` → `minecraft:netherite_sword`) while leaving every other netherite upgrade and
  every armour trim untouched.

`pub fn register(hooks: &CraftingStationHooks)` is the one function a host calls, mirroring
`lodestone_void_world::register`'s own free-function convention for a seam that is a plain registry rather
than a `bevy_app::Plugin`.

## What consumes this

* `crates/plugins/lodestone-crafting-warden` — the reference plugin, above. Its own unit tests call
  `AnvilBlessing`/`SmithingSwordBan`'s `on_prepare` directly, but that only proves the hooks' *logic* is
  correct (the same way `crate::anvil::compute`'s own unit tests are direct calls) — **not** that production
  ever reaches them.
* `crates/lodestone-server/src/server.rs`'s own test module is the wiring proof, and it does **not** take
  `lodestone-crafting-warden` as a dev-dependency: `apply_container_clicked`/`apply_workstation_clicked`/
  `apply_container_button_click`/`apply_rename_item` are module-private, so this proof can only live inside
  this module — and this module is compiled twice when its own `--lib` unit tests build (once as the unit
  under test, once as an ordinary dependency for anything that depends on it normally), so a dev-dependency
  that itself depends on `lodestone-server` normally would link two incompatible copies of
  `CraftingStationHooks` into the same test binary. `WiringProofDenySwordUpgrade`/`WiringProofBlessAnvilName`
  are test-local stand-ins reproducing `SmithingSwordBan`'s/`AnvilBlessing`'s exact logic, registered exactly
  the way a host registers a plugin's hook and never called directly, driving the real
  `apply_container_clicked`/`apply_workstation_clicked`/`apply_rename_item` dispatch — never
  `CraftingStationHooks::evaluate` or a hook's `on_prepare` called directly, which would be the closed loop
  this repo already knows to avoid:
  - `a_registered_plugin_hook_vetoes_one_smithing_upgrade_and_allows_a_sibling_one` — a real smithing-table
    take is silently refused for a banned sword upgrade, and a positive control (the identical dispatch with
    a pickaxe base) proves the veto is scoped to the one named item rather than blocking every take.
  - `a_registered_plugin_hook_blesses_a_real_anvil_rename_take` — a real `apply_rename_item` call followed
    by a real take produces an item whose name carries the hook's prefix.
* `crates/lodestone-server/src/plugin_crafting.rs`'s own unit tests cover `CraftingStationHooks::evaluate`'s
  priority ordering and short-circuiting in isolation, one layer below the end-to-end proof above.

## How to change it, and the gotchas

* **Adding a station**: `docs/backlog.md`/the anvil-family issues name the five real stations this crate
  simulates. If a sixth one's own result computation is ever added, route it through `workstation_result`'s
  existing `match` — a station whose compute function is *not* called from there would silently never reach
  a plugin, the exact island shape this repo already knows about. Grep this module and
  `crate::server::workstation_result` together whenever `Station` gains a variant.
* **A hook must not panic.** It runs inline on the connection resolving the click or redrawing the menu; a
  panic takes that player's connection down with it.
* **`StationInputs` is observation-only, deliberately.** It carries the station, its own input cells, and
  the vanilla-computed result — never a menu-slot index, a raw click, or a `PlayerInventory` borrow. Adding
  a mutable reference to either would reopen the reentrancy hazard `docs/packet-wiring.md` already forecloses
  for `EgressFilters`/`ActionVetoes`.
* **A `Deny`/`Replace` never changes cost.** If a future issue wants a plugin-controlled cost too, that is a
  new, separate seam (a second hook type, or a second field on the verdict) — folding it into `StationVerdict`
  would make the common case (most hooks only care about the result) carry a field it never uses.

## Configuration

None. A host constructs nothing beyond calling `hooks.register(priority, Arc::new(MyHook))` on the world's
own `WorldStateHandle::crafting_hooks()` — there is no manifest, feature flag, or environment variable.

## Dependencies

`lodestone_server::plugin_crafting` depends on `lodestone_model::ItemStack` and
`crate::container_click::Station` only — no protocol crate, since a hook sees already-resolved game state,
never a packet. `lodestone-crafting-warden` depends on `lodestone-server` (path) and `lodestone-model`
(workspace) — no `bevy_ecs`/`bevy_app`, since this seam is not a bevy plugin.

## See also

- [`plugin-api.md`](plugin-api.md) — the client-side bevy plugin tier and its intent doctrine, whose
  verdict vocabulary this module reuses.
- [`plugin-worldgen-api.md`](plugin-worldgen-api.md) — the closest sibling in shape: a plain, `dyn`-dispatched
  server-side seam rather than a bevy plugin, for the identical reason.
- [`packet-wiring.md`](packet-wiring.md) — `EgressFilters`/`ActionVetoes`, the client-side hooks whose
  `Allow`/refuse/`Replace` shape this module's `StationVerdict` mirrors.
- [`container-screens.md`](container-screens.md) — the crafting-table result slot's own "defers to the
  server" shape this issue's body pointed at as the right precedent to extend.
