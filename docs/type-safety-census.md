# Type-safety census

## What it is

This is the checked disposition ledger for public and crate-public primitive APIs whose names imply a narrower semantic domain. It bounds future type-safety work without treating protocol, storage, cache, or user-authored text as accidental debt.

## How it works

The census scans production Rust sources (tests and benches excluded) with two deliberately conservative signatures:

```text
numeric: public functions with state_id, block_state, effect_id, item_id, entity_id, window_id, slot, mode, kind, rotation, or sequence primitive parameters
text: public String fields whose names end in url, dimension, potion, effect, state, kind, mode, key, or id
```

The snapshot contains **74 numeric APIs** and **62 text fields**, **136 sites total**. Every row is assigned either a migration family or an intentional boundary category. The scanner is a discovery guard, not a claim that every integer or string in the repository needs a wrapper.

| disposition | sites |
|---|---:|
| `dimension-resource-url` | 16 |
| `entity-network-id` | 22 |
| `intentional-cache-index` | 6 |
| `intentional-external-identity` | 1 |
| `intentional-observability-label` | 2 |
| `intentional-ring-buffer-index` | 1 |
| `intentional-storage-boundary` | 2 |
| `intentional-user-or-format-text` | 2 |
| `intentional-wire-boundary` | 36 |
| `inventory-menu-slot` | 32 |
| `potion-and-state-value` | 11 |
| `typed-discriminator` | 5 |

Migration families are `entity-network-id`, `inventory-menu-slot`, `potion-and-state-value`, `typed-discriminator`, and `dimension-resource-url`.

Intentional categories retain primitives because the representation is the interface: bytes/integers at wire boundaries, strings in storage/import formats, external identity strings, cache or ring-buffer indices, observability labels, secrets, and user-authored or format-defined text.

## Site ledger

| site | disposition |
|---|---|
| `crates/lodestone-render/src/block_entity.rs: pub fn campfire_item_matrix(pos: [i32; 3], facing_yaw_deg: f32, slot: usize) -> Mat4 {` | `inventory-menu-slot` |
| `crates/lodestone-render/src/block_entity.rs: pub fn shelf_item_offset(slot: usize, align_to_bottom: bool) -> Vec3 {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/sleep.rs:     pub fn lay_down(&self, entity_id: i32) {` | `entity-network-id` |
| `crates/lodestone-server/src/sleep.rs:     pub fn get_up(&self, entity_id: i32) {` | `entity-network-id` |
| `crates/lodestone-server/src/players.rs:     pub fn swing(&self, entity_id: i32, hand: lodestone_model::Hand) {` | `entity-network-id` |
| `crates/lodestone-server/src/players.rs:     pub fn set_position(&self, entity_id: i32, position: Vec3) {` | `entity-network-id` |
| `crates/lodestone-server/src/players.rs:     pub fn set_rotation(&self, entity_id: i32, rotation: Rotation) {` | `entity-network-id` |
| `crates/lodestone-server/src/inventory.rs:     pub fn set_selected_hotbar_slot(&mut self, slot: u8) -> bool {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/inventory.rs:     pub fn apply_menu_slot_change(&mut self, menu_slot: i32, item: Option<ItemStack>) -> bool {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/inventory.rs:     pub fn set_selected_bundle_item(&mut self, slot: i32, selected: i32) {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/inventory.rs:     pub fn selected_bundle_item(&self, slot: usize) -> Option<usize> {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/inventory.rs: pub fn player_craft_grid_cell(menu_slot: i32) -> Option<usize> {` | `inventory-menu-slot` |
| `crates/lodestone-ecs/src/entity_spawn.rs: pub fn is_plugin_entity_id(entity_id: i32) -> bool {` | `entity-network-id` |
| `crates/lodestone-server/src/brewing.rs:     pub fn bottle(&self, slot: usize) -> Option<&Bottle> {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/brewing.rs:     pub fn set_bottle(&mut self, slot: usize, bottle: Option<Bottle>) {` | `inventory-menu-slot` |
| `crates/lodestone-ecs/src/entity.rs:     pub fn get(&self, entity_id: i32) -> Option<Entity> {` | `entity-network-id` |
| `crates/lodestone-ecs/src/entity.rs:     pub fn insert(&mut self, entity_id: i32, entity: Entity) {` | `entity-network-id` |
| `crates/lodestone-ecs/src/entity.rs:     pub fn remove(&mut self, entity_id: i32) -> Option<Entity> {` | `entity-network-id` |
| `crates/lodestone-client/src/state.rs:     pub(crate) fn entity(&self, entity_id: i32) -> Option<EntityView> {` | `entity-network-id` |
| `crates/lodestone-shell/src/sim/session.rs:     pub fn select_slot(&mut self, slot: usize) {` | `inventory-menu-slot` |
| `crates/lodestone-shell/src/sim/session.rs:     pub fn send_container_button_click(&self, window_id: i32, button_id: i32) {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/mobs/vehicles.rs:     pub fn vehicle_ridden_by(&self, player_entity_id: i32) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/vehicles.rs:     pub fn dismount_rider(&mut self, player_entity_id: i32) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/vehicles.rs:     pub fn apply_boat_paddle(&mut self, player_entity_id: i32, left: bool, right: bool) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-client/src/handle.rs:     pub fn entity(&self, entity_id: i32) -> Option<EntityView> {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/mod.rs:     pub fn mob_ridden_by(&self, player_entity_id: i32) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/mod.rs:     pub fn mount_mob(&mut self, id: i32, player_entity_id: i32) -> bool {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/mod.rs:     pub fn dismount_mob(&mut self, player_entity_id: i32) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/mod.rs:     pub fn trigger_camel_dash(&mut self, player_entity_id: i32) -> bool {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/mod.rs:     pub fn apply_mob_move(&mut self, player_entity_id: i32, position: Vec3, yaw: f32) -> bool {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/minecart.rs:     pub fn minecart_ridden_by(&self, player_entity_id: i32) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/minecart.rs:     pub fn mount_minecart(&mut self, id: i32, player_entity_id: i32) -> bool {` | `entity-network-id` |
| `crates/lodestone-server/src/mobs/minecart.rs:     pub fn dismount_minecart_rider(&mut self, player_entity_id: i32) -> Option<i32> {` | `entity-network-id` |
| `crates/lodestone-server/src/block_entities.rs:     pub fn set_container_slot(&mut self, slot: usize, item: Option<ItemStack>) {` | `inventory-menu-slot` |
| `crates/lodestone-server/src/block_entities.rs:     pub fn set_crafter_slot_state(&mut self, slot: usize, enabled: bool) -> bool {` | `inventory-menu-slot` |
| `crates/lodestone-render/src/block_models.rs: pub fn biome_tint_kind_for_slot(slot: u8) -> Option<TintKind> {` | `inventory-menu-slot` |
| `crates/lodestone-render/src/block_models.rs:     pub(crate) fn reserve(&mut self, slot: u8, rgb: u32) {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/menu.rs:     pub fn item_combiner(container_size: usize, result_slot: usize, layout: SpecialLayout) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs: pub fn quick_craft_mask(header: i32, kind: i32) -> i32 {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn quick_craft_remainder(&self, painted: &[usize], kind: i32, source: &ItemStack) -> i32 {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn perform_drag(&mut self, kind: i32, slots: &[usize], ctx: PlayerCtx) -> ClickOutcome {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs: pub fn is_valid_quick_craft_type(kind: i32, infinite_materials: bool) -> bool {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs: pub fn quick_craft_place_count(slots: i32, kind: i32, stack: &ItemStack) -> i32 {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn left(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn right(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn shift(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn hotbar_swap(slot: usize, hotbar: u8) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn offhand_swap(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn clone_slot(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn drop_one(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn drop_stack(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/click.rs:     pub fn double(slot: usize) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-game/src/reconcile.rs:     pub fn to_action(&self, window_id: i32) -> ClientAction {` | `inventory-menu-slot` |
| `crates/versions/26.2/src/packets/metadata.rs: pub fn write_update_attributes(w: &mut Writer, entity_id: i32, attributes: &[EntityAttributeSnapshot]) {` | `intentional-wire-boundary` |
| `crates/lodestone-shell/src/menu/book_view.rs:     pub fn lectern(open: BookViewOpen, window_id: i32, page: i32) -> Self {` | `inventory-menu-slot` |
| `crates/lodestone-shell/src/gpu/distant_terrain.rs:     pub(crate) fn rejects_unpopulated_submission(&self, slot: usize) -> bool {` | `intentional-ring-buffer-index` |
| `crates/lodestone-worldgen-core/src/engine/scratch.rs:     pub(crate) fn cell_get(&self, slot: usize, cx: i32, cy: i32, cz: i32) -> Option<[f64; 8]> {` | `intentional-cache-index` |
| `crates/lodestone-worldgen-core/src/engine/scratch.rs:     pub(crate) fn cell_put(&mut self, slot: usize, cx: i32, cy: i32, cz: i32, v: [f64; 8]) {` | `intentional-cache-index` |
| `crates/lodestone-worldgen-core/src/engine/scratch.rs:     pub(crate) fn slot_get(&self, slot: usize, key: (i32, i32, i32)) -> Option<f64> {` | `intentional-cache-index` |
| `crates/lodestone-worldgen-core/src/engine/scratch.rs:     pub(crate) fn slot_put(&mut self, slot: usize, key: (i32, i32, i32), v: f64) {` | `intentional-cache-index` |
| `crates/lodestone-worldgen-core/src/counters.rs:     pub fn bump_slot_miss(slot: usize) {` | `intentional-cache-index` |
| `crates/lodestone-worldgen-core/src/counters.rs:     pub fn bump_slot_miss(_slot: usize) {}` | `intentional-cache-index` |
| `crates/lodestone-worldgen/src/structure/mod.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-worldgen/src/structure/mod.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-worldgen/src/structure/mod.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-worldgen/src/structure/mod.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-worldgen/src/feature/vegetation/features.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-worldgen/src/feature/vegetation/features.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-worldgen/src/feature/vegetation/features.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-worldgen/src/feature/mod.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-auth/src/store.rs:     pub profile_id: String,` | `intentional-external-identity` |
| `crates/lodestone-command/src/argument.rs:     pub kind: StringKind,` | `intentional-user-or-format-text` |
| `crates/lodestone-render/src/banner_pattern.rs:     pub pattern_asset_id: String,` | `dimension-resource-url` |
| `crates/lodestone-auth/src/flow.rs:     pub url: String,` | `dimension-resource-url` |
| `crates/lodestone-server/src/redstone_target.rs:     pub new_state: String,` | `potion-and-state-value` |
| `crates/lodestone-server/src/scheduled_tick.rs:     pub kind: String,` | `typed-discriminator` |
| `crates/lodestone-server/src/scheduled_tick.rs:     pub kind: String,` | `typed-discriminator` |
| `crates/lodestone-game/src/chat.rs:     pub translation_key: String,` | `intentional-user-or-format-text` |
| `crates/lodestone-server/src/chunk_nbt.rs:     pub kind: String,` | `typed-discriminator` |
| `crates/lodestone-shell/src/remote_skins.rs:     pub url: String,` | `dimension-resource-url` |
| `crates/lodestone-shell/src/resources.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-shell/src/resources.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-worldgen/src/end/podium.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-render/src/block_models.rs:     pub kind: String,` | `typed-discriminator` |
| `crates/lodestone-shell/src/menu/packs.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-server/src/heavy_scene.rs:     pub run_id: String,` | `intentional-observability-label` |
| `crates/lodestone-server/src/heavy_scene.rs:     pub executable_kind: String,` | `intentional-observability-label` |
| `crates/lodestone-server/src/block_placement.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-server/src/random_tick.rs:     pub state: String,` | `potion-and-state-value` |
| `crates/lodestone-server/src/redstone_note_block.rs:     pub new_state: String,` | `potion-and-state-value` |
| `crates/lodestone-assets/src/meta.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-server/src/redstone_dispenser.rs:     pub new_state: String,` | `potion-and-state-value` |
| `crates/lodestone-server/src/plugin_dimension.rs:     pub key: String,` | `dimension-resource-url` |
| `crates/lodestone-assets/src/item_model.rs:     pub kind: String,` | `typed-discriminator` |
| `crates/lodestone-server/src/protocol.rs:     pub url: String,` | `dimension-resource-url` |
| `crates/lodestone-server/src/player_data.rs:     pub dimension: String,` | `dimension-resource-url` |
| `crates/lodestone-server/src/advancements.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-server/src/advancements.rs:     pub id: String,` | `dimension-resource-url` |
| `crates/lodestone-protocol-common/src/packets/login.rs:     pub server_id: String,` | `intentional-wire-boundary` |
| `crates/lodestone-protocol-common/src/packets/login.rs:     pub uuid: String,` | `intentional-wire-boundary` |
| `crates/lodestone-model/src/item.rs:     pub pattern_asset_id: String,` | `dimension-resource-url` |
| `crates/lodestone-anvil/src/level_dat.rs:     pub dimension: String,` | `intentional-storage-boundary` |
| `crates/lodestone-anvil/src/schematic.rs:     pub state: String,` | `intentional-storage-boundary` |
| `crates/versions/26.2/src/packets/common.rs:     pub key: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/login.rs:     pub server_id: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/configuration.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/registry.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/game.rs:     pub dimension: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/game.rs:     pub dimension: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/game.rs:     pub dimension: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/game.rs:     pub key: String,` | `intentional-wire-boundary` |
| `crates/versions/26.2/src/packets/game.rs:     pub final_state: String,` | `intentional-wire-boundary` |
| `crates/versions/1.20.6/src/packets/chunk.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.20.6/src/packets/configuration.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.20.6/src/packets/configuration.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.20.6/src/packets/login.rs:     pub server_id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/packets/position.rs:     pub dimension: String,` | `intentional-wire-boundary` |
| `crates/versions/1.7/src/packets/entity.rs:     pub player_uuid: String,` | `intentional-wire-boundary` |
| `crates/versions/1.7/src/packets/entity.rs:     pub key: String,` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/packets/chunk.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/packets/login.rs:     pub server_id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.7/src/packets/login.rs:     pub server_id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/packets/configuration.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/packets/configuration.rs:     pub id: String,` | `intentional-wire-boundary` |
| `crates/versions/1.13/src/canonical.rs:     pub fn resolve(&self, state_id: u32) -> Option<StateId> {` | `intentional-wire-boundary` |
| `crates/versions/1.13/src/canonical.rs:     pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> StateId {` | `intentional-wire-boundary` |
| `crates/versions/1.14/src/canonical.rs:     pub fn resolve(&self, state_id: u32) -> Option<StateId> {` | `intentional-wire-boundary` |
| `crates/versions/1.14/src/canonical.rs:     pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> StateId {` | `intentional-wire-boundary` |
| `crates/versions/1.17/src/canonical.rs:     pub fn resolve(&self, state_id: u32) -> Option<StateId> {` | `intentional-wire-boundary` |
| `crates/versions/1.17/src/canonical.rs:     pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> StateId {` | `intentional-wire-boundary` |
| `crates/versions/1.19/src/canonical.rs:     pub fn resolve(&self, state_id: u32) -> Option<StateId> {` | `intentional-wire-boundary` |
| `crates/versions/1.19/src/canonical.rs:     pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> StateId {` | `intentional-wire-boundary` |
| `crates/versions/1.20.6/src/canonical.rs:     pub fn resolve(&self, state_id: u32) -> Option<StateId> {` | `intentional-wire-boundary` |
| `crates/versions/1.20.6/src/canonical.rs:     pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> StateId {` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/canonical.rs:     pub fn resolve(&self, state_id: u32) -> Option<StateId> {` | `intentional-wire-boundary` |
| `crates/versions/1.21.11/src/canonical.rs:     pub fn resolve_or_air(&self, state_id: u32, tally: &mut FallbackTally) -> StateId {` | `intentional-wire-boundary` |

## How to change it

When a migration family lands, update all rows in that family together and rerun the two census expressions. Remove rows that no longer match; add newly exposed candidates and classify them in the same commit. A primitive may move to an intentional category only when its boundary role is documented and tested. Canonical block states now cross production APIs as `lodestone_data::block_states::StateId`: raw values are allowed only at decode, chunk-store, or version-adapter boundaries, and are validated before state-specific work.

The render atlas follows the same boundary: `BlockAtlas::state_id_of` parses a
textual state key and returns `Option<StateId>`. The shell's collision adapter
calls `StateId::raw()` only when filling its packed raw-state list; unknown or
partial state strings remain unresolved rather than being assigned a plausible
canonical id.
Other state-aware consumers should use `StateId::from_state_str` directly;
they must not recover a raw id only to validate it again before lookup. An
unknown block name remains unresolved and therefore cannot be treated as a
solid built-in state.

Potion names at built-in consumers follow the corresponding typed path:
`PotionId::from_name` returns a validated `PotionId`, while the raw
`potion_id` compatibility helper remains for model and wire-facing storage.
Unknown names stay unresolved instead of being converted into a guessed registry
number.

Container synchronization state now uses `lodestone_model::ContainerStateId`; keep packet decoding and encoding at its `from_wire`/`as_wire` boundary rather than restoring integer casts in menu consumers.

Recipe synchronization item ids now use `lodestone_model::ItemId`: the 26.2
adapter classifies known ids only after generated-census validation and keeps
unknown non-negative ids protocol-local through the recipe fields. Consumers
must require `ItemId::canonical_raw()` before indexing a generated item table;
the shared `RegistrySet` representation remains a separate session-scoped
boundary for fields belonging to several registries.

Entity identity helpers use `lodestone_model::EntityNetworkId`. Server-owned
ids are classified at packet ingress with `from_wire`; plugin-owned negative
ids are created only by the plugin allocator. Render and shell entity maps keep
the classified value through lookup and deterministic seed calculation, calling
`raw()` only at a wire or GPU POD boundary. The remaining server simulation and
client compatibility APIs in the ledger are the next migration surface.

## Configuration

There is no runtime configuration. The scope is production `*.rs` under `crates/`, excluding `tests/` and `benches/` directories.

## Dependencies

The ledger depends on `rg` for reproducible discovery and on the public API names in the Rust workspace. Follow-up migrations depend on the model types at their relevant protocol, storage, or plugin boundary.
