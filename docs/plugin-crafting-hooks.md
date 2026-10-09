# Plugin crafting-station hooks — anvil, grindstone, smithing table, loom, stonecutter

## What it is

A server-side seam mirroring Bukkit's `PrepareAnvilEvent`/`PrepareSmithingEvent`/`PrepareItemCraftEvent`: a plugin can allow, deny or replace the result a crafting station is about to show a player. `lodestone_server::plugin_crafting` holds `CraftingStationHooks`, a registry of `CraftingStationHook` implementors, each answering one `StationInputs` with a `StationVerdict` (`Allow`, `Deny`, `Replace(ItemStack)`). `crates/plugins/lodestone-crafting-warden` is the reference plugin.

## How it works

**Not the bevy tier.** Station results are computed in `lodestone-server`'s plain (non-ECS) dispatch functions (`anvil.rs`, `smithing.rs`, `loom.rs`, `grindstone_result`, `stonecutting::result`, joined at `server/container_clicks.rs`'s `workstation_result`). Like the `dyn`-dispatched `ChunkGenerator`/`DimensionRegistry` seam ([plugin worldgen API](plugin-worldgen-api.md)), it avoids inventing a schedule this crate does not run for state (`PlayerInventory`, `OpenContainer`) that is never a bevy component. Verdicts reuse the shared vocabulary (observation in, typed `Allow`/refuse/`Replace` out, first non-`Allow` wins; [plugin API](plugin-api.md), [packet wiring](packet-wiring.md)); two of the intent doctrine's clauses are dropped (there is no human source of a workstation result to outrank, and no lifecycle beyond one question).

**Registry placement.** `CraftingStationHooks` is a cheap-clone, `Arc`-backed registry (like `PluginChannelRegistry`), a sibling field on `WorldStateHandle` beside `scoreboard`/`teams`/`nbt_storage`/`stopwatches`. Because `WorldStateHandle` reaches `crate::server::play_dispatch::dispatch_play_packet`, it reaches every production call site with no new parameter on the `serve_connection*` wrappers; leaf functions get a narrow `&CraftingStationHooks`.

**One choke point.** Opening a station, clicking inside it, choosing a loom or stonecutter offer, renaming an item and the direct take path all pass `world.crafting_hooks()` to `workstation_result`, which builds the normal result and, only when a hook is registered, packages station, input cells and computed result into `StationInputs` for `CraftingStationHooks::evaluate` (an empty registry short-circuits on one `is_empty()` check).

**Verdicts.** `Allow` leaves the result, `Deny` produces nothing, `Replace(ItemStack)` substitutes a stack. Hooks run in ascending priority and the first non-`Allow` wins, so hooks cannot loop rewriting each other. `StationInputs::computed` carries the computed result (`None` if the inputs combine into nothing) so a `Replace` hook can tweak a real result instead of reimplementing recipe rules. Cost stays server-controlled: Bukkit's event replaces only the result stack, and the anvil's XP cost is computed once from the pre-click cells in `apply_workstation_clicked`'s `anvil_cost`, separate from `workstation_result`, so a hook cannot change what a take costs or whether it is allowed.

**Reference plugin.** `lodestone-crafting-warden` ships `AnvilBlessing` (`Replace`: prepends `"[Blessed] "` to any custom-named anvil result; idempotent, inert on unnamed repairs) and `SmithingSwordBan` (`Deny`: refuses `minecraft:diamond_sword` to `minecraft:netherite_sword` only). A host calls `pub fn register(hooks: &CraftingStationHooks)`, the free-function convention shared with `lodestone_void_world::register`.

**Proof.** The warden's own tests call `on_prepare` directly, which proves hook logic, not production reachability. The wiring proof is `crates/lodestone-server/src/server/container_clicks/tests.rs`, which does not take the warden as a dev-dependency: the dispatch functions are module-private and the module compiles twice under `--lib` tests, so a dev-dependency depending on `lodestone-server` would link two incompatible `CraftingStationHooks`. Test-local stand-ins (`WiringProofDenySwordUpgrade`, `WiringProofBlessAnvilName`) reproduce the warden's logic, are registered the way a host registers a hook, and drive the real `apply_container_clicked`/`apply_workstation_clicked`/`apply_rename_item` dispatch: `a_registered_plugin_hook_vetoes_one_smithing_upgrade_and_allows_a_sibling_one` (with a pickaxe-base positive control showing the veto is scoped) and `a_registered_plugin_hook_blesses_a_real_anvil_rename_take`. `plugin_crafting.rs`'s unit tests cover priority ordering and short-circuiting in isolation.

## How to change it, and the gotchas

- **New station:** the `Station` enum has five result-producing variants; route a new station's computation through `workstation_result`'s `match` or it silently never reaches a plugin. Grep this module and `crate::server::container_clicks::workstation_result` together when `Station` grows.
- **A hook must not panic:** it runs inline on the connection resolving the click or redrawing the menu, so a panic takes that player's connection down.
- **`StationInputs` is observation-only:** station, input cells and computed result, never a menu-slot index, raw click or `PlayerInventory` borrow (mutable access would reopen the reentrancy hazard [packet wiring](packet-wiring.md) forecloses).
- **`Deny`/`Replace` never changes cost:** plugin-controlled cost needs a separate seam (a second hook type or verdict field) rather than burdening every hook with a field it never uses.

## Configuration

None beyond `hooks.register(priority, Arc::new(MyHook))` on `WorldStateHandle::crafting_hooks()`; no manifest, feature or environment variable.

## Dependencies

`lodestone_server::plugin_crafting` depends on `lodestone_model::ItemStack` and `crate::container_click::Station` only (a hook sees resolved game state, never a packet). `lodestone-crafting-warden` depends on `lodestone-server` (path) and `lodestone-model`, no `bevy_ecs`/`bevy_app`. See also [container screens](container-screens.md).
