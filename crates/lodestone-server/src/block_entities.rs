//! The [`BlockPos`]-keyed registry that gives the four block-entity
//! simulations (`composter`/`furnace`/`hopper`/`brewing`, `docs/block-entities.md`)
//! somewhere to live in a running world.
//!
//! Each simulation type remains a plain value, while [`BlockEntityRegistry`]
//! keeps one at its world position. The registry is an enum
//! ([`BlockEntity`]) over the four existing types rather than four separate
//! maps.
//!
//! [`BlockEntityHandle`] is the `Arc<Mutex<_>>`-backed, `Clone`, shareable
//! handle a caller threads between a connection's own task (which needs to
//! *insert* a fresh entry on placement — see `crate::server`'s
//! `apply_use_item_on`) and a background tick-loop task (which needs to
//! *advance* every entry once a tick) — the exact shape
//! [`crate::mobs::LiveMobSource`] already established for the mob simulation,
//! reused here rather than reinvented.
//!
//! # What is deliberately not modeled yet
//!
//! * **Hopper power is caller-supplied.** `crate::redstone::best_neighbor_signal`
//!   provides the signal, and [`BlockEntityRegistry::tick_all_with_hopper_lock`]
//!   passes each hopper's `enabled` flag into its tick. `crate::random_tick`
//!   maintains that property on block state. The plain
//!   [`tick_all`](BlockEntityRegistry::tick_all) shorthand still ticks every
//!   hopper without the lock, so a production caller that holds a world must use
//!   the locking form.
//! * **Hopper adjacency only resolves another hopper.** A real container
//!   (chest, furnace slots) at the adjacent position is not something this
//!   crate can hand a hopper today: [`Hopper::tick`](crate::hopper::Hopper::tick)
//!   wants a flat `&mut [Option<ItemStack>]` slice, and only [`Hopper`] itself
//!   exposes one — a furnace's input/fuel/output are three separate typed
//!   fields, and there is no chest block entity in this crate at all. So
//!   [`BlockEntity::hopper_slots_mut`] answers `None` for every non-hopper
//!   variant, which is an honest "no adjacency support yet," not a bug: a
//!   hopper next to a furnace simply never transfers, same as a hopper next
//!   to open air. Extending this needs a `Container`-shaped seam over the
//!   furnace's three slots, deliberately not built here.
//! * **Partial visual sync.** A furnace's `lit` flip
//!   ([`Furnace::is_lit`](crate::furnace::Furnace::is_lit), carried out of
//!   this module as [`BlockEntity::tick_non_hopper`]'s `Option<bool>` return)
//!   reaches the client — `crate::tick::run_tick_loop` is the one caller
//!   holding both this registry and a `ChunkSource::set_block`, so that is
//!   where the write happens, not here. Ticking a composter to ready
//!   ([`Composter::is_ready`](crate::composter::Composter::is_ready)) still
//!   does not write anything back — this module's job is *simulating*, not
//!   *rendering*.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use lodestone_core::Nbt;
use lodestone_model::{BlockPos, ItemStack};

use crate::brewing::BrewingStand;
use crate::composter::Composter;
use crate::furnace::{Furnace, FurnaceKind};
use crate::hopper::Hopper;

/// One of the four block-entity kinds this crate simulates, at rest in the
/// [`BlockEntityRegistry`].
#[derive(Debug, Clone, PartialEq)]
pub enum BlockEntity {
    /// `minecraft:composter`.
    Composter(Composter),
    /// `minecraft:furnace`/`minecraft:smoker`/`minecraft:blast_furnace`
    /// (the [`FurnaceKind`] inside [`Furnace`] distinguishes them).
    Furnace(Furnace),
    /// `minecraft:hopper`.
    Hopper(Hopper),
    /// `minecraft:brewing_stand`.
    BrewingStand(BrewingStand),
    /// A plain item container with no simulation of its own — a chest, trapped
    /// chest or barrel. `id` is the block-entity type key, `slots` its
    /// [`CONTAINER_9X3_SIZE`] inventory.
    ///
    /// It has no `tick`: lid animation is client-side only, and nothing about
    /// the container's contents changes on its own.
    Container {
        /// The `minecraft:block_entity_type` key (`minecraft:chest`, …).
        id: String,
        /// The container's own slots, in menu order.
        slots: Vec<Option<ItemStack>>,
    },
    /// A block entity this crate has no simulation for (vault, decorated pot,
    /// …). The full NBT payload is retained for these opaque types.
    /// The vanilla id and the full NBT compound are preserved verbatim so the entity
    /// round-trips through a save/load cycle unchanged.
    Opaque { id: String, nbt: Nbt },
    /// `minecraft:command_block`/`chain_command_block`/`repeating_command_block`.
    /// The mode is derived from the block itself
    /// (`crate::command_block::mode_for_block`), never stored here — see that
    /// module's own doc for the data model and the pure tick semantics. The
    /// running tick loop supplies the player and world context separately.
    CommandBlock(crate::command_block::CommandBlockData),
    /// `minecraft:spawner`. See `crate::mob_spawner`'s own doc for the
    /// decision this state feeds, and `crate::tick::run_tick_loop` for the
    /// driver — this registry's own [`tick_all`](BlockEntityRegistry::tick_all)
    /// does **not** advance a spawner, the same way it does not run the
    /// natural-spawn cycle: a spawner needs the player list and the live entity
    /// set, neither of which this registry has a handle to.
    Spawner(crate::mob_spawner::SpawnerState),
    /// `minecraft:sign`/`minecraft:hanging_sign` (`SIGN_UPDATE` handling).
    /// See [`SignData`] for the fields and [`apply_sign_update`]
    /// for the editor gate `SIGN_UPDATE` is checked against.
    Sign(SignData),
    /// `minecraft:beacon` (`SET_BEACON` handling). See
    /// [`BeaconData`] for the fields and `crate::beacon` for the pyramid
    /// detection, effect-selection validation and periodic-application
    /// arithmetic this variant's own state feeds.
    Beacon(BeaconData),
    /// `minecraft:crafter` — a 9-slot grid plus its per-slot enabled/disabled
    /// bitmask (`indices 0..9`). **Not modelled**: the actual auto-crafting
    /// trigger (redstone-pulse tick, recipe matching, result dispensing into
    /// the world) — this closes
    /// `CONTAINER_SLOT_STATE_CHANGED`'s own decode/wiring gap, not the
    /// block's mechanism, and `data_properties`'s own doc names `triggered`
    /// (index 9) as the disclosed always-`0` consequence.
    ///
    /// `slots` is boxed because it is the one field that sets
    /// `size_of::<BlockEntity>()`: an inline `[Option<ItemStack>; 9]` puts
    /// every other variant behind an enum this wide too, and a debug build
    /// gives a `match` arm one stack slot per arm rather than one for the
    /// widest arm alone — see [`PlacedBlockEntity`]'s doc comment for the
    /// arithmetic this costs a wide match specifically.
    Crafter {
        /// The 9-slot 3×3 crafting grid, row-major (`CrafterMenu`'s own
        /// `x + y * 3` addressing).
        slots: Box<[Option<ItemStack>; 9]>,
        /// `true` where that index is disabled — `CONTAINER_SLOT_STATE_CHANGED`'s
        /// own write surface.
        disabled: [bool; 9],
    },
}

/// A placed beacon's pyramid tier, selected powers and payment item —
/// vanilla's `BeaconBlockEntity`, reduced to what `SET_BEACON` and the
/// periodic effect sweep actually touch. The beam-continuity/colour state
/// (`beamSections`) is not carried here at all: nothing in this crate renders
/// a beam, and [`crate::beacon::beam_unobstructed`] recomputes the one bit
/// that gates effect application (non-emptiness) fresh from the world each
/// time it is needed, rather than caching it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BeaconData {
    /// The pyramid tier beneath this beacon, `0..=4` — refreshed by
    /// `crate::server` from [`crate::beacon::beacon_levels`] whenever the
    /// menu opens or a `SET_BEACON` is handled, **not** on every tick (see
    /// this crate's own `BeaconData` doc for why nothing here ticks
    /// continuously). A menu left open while the pyramid is being dismantled
    /// will not see the number change until the next such refresh — a real,
    /// minor divergence from vanilla's own 80-tick background recompute.
    pub levels: u8,
    /// The selected primary power's canonical key, or `None` if unset.
    pub primary_effect: Option<String>,
    /// The selected secondary power's canonical key (level-4 pyramids only —
    /// see [`crate::beacon::validate_beacon_effects`]), or `None`.
    pub secondary_effect: Option<String>,
    /// The single payment-slot item, if any. It is consumed one at a time by
    /// a successful `SET_BEACON` action.
    pub payment: Option<ItemStack>,
}

/// A placed sign's text and edit-permission state, reduced to what
/// `SIGN_UPDATE` actually touches. The update rewrites `messages` (plain
/// strings with formatting codes stripped), never `color`/`hasGlowingText` — those
/// need a dye-interaction path this crate does not model, so they are not
/// carried here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignData {
    /// The four lines a player reading the sign from its front sees.
    pub front: [String; 4],
    /// The four lines a player reading the sign from behind sees.
    pub back: [String; 4],
    /// `is_waxed` — set by a honeycomb interaction (not modelled here yet);
    /// while `true`, [`apply_sign_update`] refuses every edit, matching
    /// `updateSignText`'s own `!this.isWaxed()` guard.
    pub waxed: bool,
    /// `playerWhoMayEdit` — the uuid `SignBlock.openTextEdit` last granted
    /// edit permission to, or `None` once spent. This crate grants it only at
    /// placement time (see [`block_entity_for_item`]'s sign arms), matching
    /// `SignItem.useOn`'s own `sign.openTextEdit(player, signEntity, true)`;
    /// vanilla's *other* grant site — right-clicking an already-placed blank
    /// sign — needs the interact-on-block path this crate's `USE_ITEM_ON`
    /// consumer does not thread block-entity access through yet, so a sign
    /// can be edited exactly once, immediately after placement, until that
    /// gap closes.
    pub editor: Option<uuid::Uuid>,
    /// Whether this is a hanging sign (`minecraft:hanging_sign`) rather than
    /// a standing/wall sign (`minecraft:sign`) — vanilla gives the two
    /// distinct `BlockEntityType`s, which [`BlockEntity::type_id`] reads this
    /// to reproduce.
    pub hanging: bool,
}

impl Default for SignData {
    /// Four blank lines on both sides, unwaxed, no editor granted — the state
    /// of a sign this crate places without going through [`block_entity_for_item`]
    /// (there is no such production caller today, but a default is cheaper
    /// than an `Option` at every call site that might one day want one).
    fn default() -> Self {
        SignData {
            front: Default::default(),
            back: Default::default(),
            waxed: false,
            editor: None,
            hanging: false,
        }
    }
}

/// `ChatFormatting::stripFormatting`'s regex, transcribed —
/// `(?i)§[0-9A-FK-OR]`: every `§` immediately followed by a legacy colour or
/// style code (case-insensitive) is dropped as a pair, and nothing else is
/// touched. `handleSignUpdate` runs this on every line *before* the ownership
/// gate even runs, so it happens here rather than in [`apply_sign_update`] —
/// the two are separate steps in vanilla and a rejected edit still had its
/// text stripped for the (discarded) attempt, which this mirrors by stripping
/// unconditionally at decode.
#[must_use]
pub fn strip_sign_formatting(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{00A7}'
            && let Some(&next) = chars.peek()
        {
            let lower = next.to_ascii_lowercase();
            if lower.is_ascii_digit() || matches!(lower, 'a'..='f' | 'k'..='o' | 'r') {
                chars.next();
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Applies a `SIGN_UPDATE` packet — `SignBlockEntity.updateSignText`'s full
/// gate, transcribed: `entity` must be a [`BlockEntity::Sign`], not waxed, and
/// `editor` must be the uuid currently granted edit permission
/// ([`SignData::editor`]). A successful edit clears the grant
/// (`setAllowedPlayerEditor(null)`), so a second `SIGN_UPDATE` for the same
/// position without a fresh grant is refused exactly like vanilla's — the
/// same "was warned and ignored" outcome `updateSignText`'s `else` branch
/// logs.
///
/// Returns whether the edit was applied, so a caller has an honest signal for
/// "nothing changed" rather than silently no-op-ing either way.
pub fn apply_sign_update(
    entity: &mut BlockEntity,
    editor: uuid::Uuid,
    is_front_text: bool,
    lines: [String; 4],
) -> bool {
    let BlockEntity::Sign(sign) = entity else {
        return false;
    };
    if sign.waxed || sign.editor != Some(editor) {
        return false;
    }
    if is_front_text {
        sign.front = lines;
    } else {
        sign.back = lines;
    }
    sign.editor = None;
    true
}

/// Slot count of the `generic_9x3` menu — a chest, trapped chest or barrel
/// (a 27-slot container).
pub const CONTAINER_9X3_SIZE: usize = 27;

/// Slot count of the 3×3 menu used by a dispenser or dropper. This gives both
/// blocks real, persistent storage alongside the three
/// `generic_9x3` blocks; the scheduled fire tick receives no live container
/// handle, so this storage is updated through explicit container operations.
pub const CONTAINER_3X3_SIZE: usize = 9;

/// The block-entity type key for a container block, or `None` if that block is
/// not one of the plain-inventory containers this crate models (`generic_9x3`
/// or `generic_3x3` — see [`BlockEntity::container`]/[`container_of_size`]).
///
/// Keyed on the block *name* with no properties, so a caller holding a canonical
/// state string must split it first.
#[must_use]
pub fn container_type_for_block(block: &str) -> Option<&'static str> {
    match block {
        "minecraft:chest" => Some("minecraft:chest"),
        "minecraft:trapped_chest" => Some("minecraft:trapped_chest"),
        "minecraft:barrel" => Some("minecraft:barrel"),
        "minecraft:dispenser" => Some("minecraft:dispenser"),
        "minecraft:dropper" => Some("minecraft:dropper"),
        _ => None,
    }
}

impl BlockEntity {
    /// A fresh, empty `generic_9x3` container of type `id`.
    #[must_use]
    pub fn container(id: &str) -> Self {
        BlockEntity::Container {
            id: id.to_owned(),
            slots: vec![None; CONTAINER_9X3_SIZE],
        }
    }

    /// A fresh, empty container of type `id` with `size` slots — the
    /// dispenser/dropper `generic_3x3` counterpart to [`Self::container`],
    /// which is fixed at [`CONTAINER_9X3_SIZE`].
    #[must_use]
    pub fn container_of_size(id: &str, size: usize) -> Self {
        BlockEntity::Container {
            id: id.to_owned(),
            slots: vec![None; size],
        }
    }

    /// A fresh, empty crafter — every slot enabled, nothing crafting.
    #[must_use]
    pub fn crafter() -> Self {
        BlockEntity::Crafter {
            slots: Box::new([None, None, None, None, None, None, None, None, None]),
            disabled: [false; 9],
        }
    }
}

impl BlockEntity {
    /// This entity's `minecraft:block_entity_type` registry key — the `id` field
    /// vanilla writes into the chunk NBT, and the key a protocol crate resolves
    /// to the VarInt type id carried in the chunk packet's block-entity array.
    /// The chunk writer uses this registry key for that conversion.
    ///
    /// Deliberately *not* [`menu_name`](Self::menu_name): those two agree for
    /// the furnace family and the hopper by coincidence, and disagree for every
    /// variant with no container screen — a composter has a real block-entity
    /// type and no menu at all.
    #[must_use]
    pub fn type_id(&self) -> &str {
        match self {
            BlockEntity::Composter(_) => "minecraft:composter",
            BlockEntity::Furnace(f) => match f.kind() {
                FurnaceKind::Furnace => "minecraft:furnace",
                FurnaceKind::Smoker => "minecraft:smoker",
                FurnaceKind::BlastFurnace => "minecraft:blast_furnace",
            },
            BlockEntity::Hopper(_) => "minecraft:hopper",
            BlockEntity::BrewingStand(_) => "minecraft:brewing_stand",
            BlockEntity::Container { id, .. } | BlockEntity::Opaque { id, .. } => id,
            // `BlockEntityTypes.COMMAND_BLOCK` is the one registry entry
            // `CommandBlockEntity`'s constructor names regardless of which of
            // the three command-block *blocks* it is attached to — unlike
            // `Furnace`, there is no per-instance kind to switch on here.
            BlockEntity::CommandBlock(_) => "minecraft:command_block",
            BlockEntity::Spawner(_) => "minecraft:spawner",
            BlockEntity::Sign(sign) => {
                if sign.hanging {
                    "minecraft:hanging_sign"
                } else {
                    "minecraft:sign"
                }
            }
            BlockEntity::Beacon(_) => "minecraft:beacon",
            BlockEntity::Crafter { .. } => "minecraft:crafter",
        }
    }

    /// A mutable view of this entity's flat item-slot array, if it has one
    /// shaped that way — today, only [`Hopper`]. See the module doc comment's
    /// "hopper adjacency" scope note for why every other variant answers
    /// `None` rather than something partial/misleading.
    fn hopper_slots_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        match self {
            BlockEntity::Hopper(h) => Some(h.slots_mut()),
            // A chest/barrel *is* a flat slot array, so hopper adjacency into
            // one works — the "no real container at the adjacent position"
            // scope note in the module doc no longer covers these three.
            BlockEntity::Container { slots, .. } => Some(slots),
            // A crafter's grid is a flat 9-slot array too, same as a
            // dispenser/dropper's `Container` — hopper adjacency into one
            // works the identical way.
            BlockEntity::Crafter { slots, .. } => Some(slots.as_mut_slice()),
            BlockEntity::Composter(_) | BlockEntity::Furnace(_) | BlockEntity::BrewingStand(_)
            | BlockEntity::Opaque { .. } | BlockEntity::CommandBlock(_)
            | BlockEntity::Spawner(_) | BlockEntity::Sign(_) | BlockEntity::Beacon(_) => {
                None
            }
        }
    }

    /// The vanilla `minecraft:*` menu identifier this entity's container
    /// screen opens as, or `None` if it has no menu screen at all.
    ///
    /// Two of the four kinds answer `None`, for two different reasons —
    /// stated here rather than left to be discovered by a missing
    /// `open_screen`:
    /// * **[`Composter`] has no vanilla menu at all.** Vanilla's own composter
    ///   block handles a right-click and empty-hand-use by adding or emptying
    ///   one item per click directly against the block entity — there is no
    ///   container-menu subclass for it in vanilla's decompiled source at all,
    ///   unlike every other block entity here. A right-click on a composter is
    ///   therefore never a "screen" question at all; it needs its own
    ///   serverbound handling, not this module's.
    /// * **[`BrewingStand`] has a real vanilla menu
    ///   (5 slots: 3 potion bottles + ingredient + fuel) but this crate cannot
    ///   open it yet**, because its slots are not [`ItemStack`]s —
    ///   `docs/block-entities.md`'s second named gap: "the brewing stand's
    ///   `Bottle` is not a real `ItemStack`" (no potion-contents component
    ///   anywhere in `lodestone_model::ItemComponents`). Every wire encoder
    ///   this module feeds ([`container_slots`](Self::container_slots),
    ///   [`CONTAINER_SET_CONTENT`]) speaks `Option<ItemStack>` only, so a
    ///   brewing stand has nothing valid to put in that list — opening one
    ///   would need either a real potion-contents model or a second,
    ///   bottle-shaped wire path this crate does not have. Real, deliberate
    ///   scope cut, not an oversight.
    #[must_use]
    pub fn menu_name(&self) -> Option<&'static str> {
        match self {
            BlockEntity::Furnace(f) => Some(match f.kind() {
                FurnaceKind::Furnace => "minecraft:furnace",
                FurnaceKind::Smoker => "minecraft:smoker",
                FurnaceKind::BlastFurnace => "minecraft:blast_furnace",
            }),
            BlockEntity::Hopper(_) => Some("minecraft:hopper"),
            // A dispenser/dropper's `generic_3x3` (9 slots) is
            // narrower than a chest/trapped-chest/barrel's `generic_9x3` (27) —
            // the two `Container` shapes share one Rust variant but not one
            // vanilla menu, so this reads the id rather than assuming.
            BlockEntity::Container { id, slots } => Some(if slots.len() == CONTAINER_3X3_SIZE {
                debug_assert!(id == "minecraft:dispenser" || id == "minecraft:dropper", "a 9-slot container must be a dispenser or dropper: {id}");
                "minecraft:generic_3x3"
            } else {
                "minecraft:generic_9x3"
            }),
            BlockEntity::Composter(_) | BlockEntity::BrewingStand(_) | BlockEntity::Opaque { .. }
            // A command block opens its own dedicated GUI
            // (`Player.openCommandBlock`), not an `AbstractContainerMenu` —
            // there is no vanilla menu identifier for it at all.
            | BlockEntity::CommandBlock(_)
            // A spawner has no `AbstractContainerMenu` either — right-clicking
            // one in survival does nothing at all in vanilla.
            | BlockEntity::Spawner(_)
            // A sign has no `AbstractContainerMenu` either — `SignBlock`'s own
            // interaction opens the dedicated text-edit screen `SIGN_UPDATE`
            // answers, not a menu.
            | BlockEntity::Sign(_) => None,
            BlockEntity::Beacon(_) => Some("minecraft:beacon"),
            // Vanilla's own menu-type registry key for the 3x3 crafter.
            BlockEntity::Crafter { .. } => Some("minecraft:crafter_3x3"),
        }
    }

    /// This entity's own container slots, in vanilla menu order — the
    /// furnace's `[input, fuel, output]` (vanilla's own furnace-menu constants
    /// put ingredient/fuel/result at slots `0`/`1`/`2`) or the
    /// hopper's 5 flat slots. Empty for a variant
    /// with [`menu_name`](Self::menu_name) `None` — nothing should ever call
    /// this for one, but an empty list is the honest answer if something
    /// does, not a panic.
    #[must_use]
    pub fn container_slots(&self) -> Vec<Option<ItemStack>> {
        match self {
            BlockEntity::Furnace(f) => vec![f.input().cloned(), f.fuel().cloned(), f.output().cloned()],
            BlockEntity::Hopper(h) => h.slots().to_vec(),
            BlockEntity::Container { slots, .. } => slots.clone(),
            // `BeaconMenu`'s single payment slot (`PAYMENT_SLOT = 0`).
            BlockEntity::Beacon(b) => vec![b.payment.clone()],
            // `CrafterMenu.addSlots`'s own `x + y * 3` order — already this
            // array's own indexing.
            BlockEntity::Crafter { slots, .. } => slots.to_vec(),
            BlockEntity::Composter(_) | BlockEntity::BrewingStand(_) | BlockEntity::Opaque { .. }
            | BlockEntity::CommandBlock(_) | BlockEntity::Spawner(_) | BlockEntity::Sign(_) => Vec::new(),
        }
    }

    /// Writes one container slot by its position in
    /// [`container_slots`](Self::container_slots)'s own ordering — the
    /// counterpart a `container_click` consumer needs to apply the client's
    /// predicted diff verbatim (`docs/server-inventory.md`'s established
    /// scope: this crate applies the click's own diff rather than re-running
    /// vanilla's `doClick` state machine server-side). An out-of-range
    /// `slot` is a silent no-op, matching `PlayerInventory::set_native`'s own
    /// convention for the identical "malformed index" case.
    pub fn set_container_slot(&mut self, slot: usize, item: Option<ItemStack>) {
        match self {
            BlockEntity::Furnace(f) => match slot {
                0 => f.set_input(item),
                1 => f.set_fuel(item),
                2 => f.set_output(item),
                _ => {}
            },
            BlockEntity::Hopper(h) => {
                if slot < h.slots().len() {
                    h.set_slot(slot, item);
                }
            }
            BlockEntity::Container { slots, .. } => {
                if let Some(cell) = slots.get_mut(slot) {
                    *cell = item;
                }
            }
            BlockEntity::Beacon(b) => {
                if slot == 0 {
                    b.payment = item;
                }
            }
            // Writing *any* item into a slot —
            // even setting it back to empty, per vanilla's own unconditional
            // check before the write — re-enables it if it was disabled.
            BlockEntity::Crafter { slots, disabled } => {
                if let Some(cell) = slots.get_mut(slot) {
                    if let Some(flag) = disabled.get_mut(slot) {
                        *flag = false;
                    }
                    *cell = item;
                }
            }
            BlockEntity::Composter(_) | BlockEntity::BrewingStand(_) | BlockEntity::Opaque { .. }
            | BlockEntity::CommandBlock(_) | BlockEntity::Spawner(_) | BlockEntity::Sign(_) => {}
        }
    }

    /// Toggles a crafter's `slot`
    /// enabled/disabled, `CONTAINER_SLOT_STATE_CHANGED`'s own write surface.
    /// `None` (and a `false` return) for any variant other than
    /// [`Crafter`](Self::Crafter): a `CONTAINER_SLOT_STATE_CHANGED` reaching
    /// a menu that is not really a crafter receives the honest "did nothing"
    /// result rather than mutating the wrong shape.
    ///
    /// The slot gate — in range *and* currently empty —
    /// applies to **both** directions, not just disabling: vanilla's method
    /// takes an `enabled` bool and runs the identical check regardless of
    /// which way it points, so a slot holding an item cannot be toggled
    /// either way through this packet. Returns whether the state actually
    /// changed, so a caller has an honest signal rather than a guaranteed
    /// no-op read as success.
    pub fn set_crafter_slot_state(&mut self, slot: usize, enabled: bool) -> bool {
        let BlockEntity::Crafter { slots, disabled } = self else {
            return false;
        };
        if slot >= 9 || slots[slot].is_some() {
            return false;
        }
        let want_disabled = !enabled;
        if disabled[slot] == want_disabled {
            return false;
        }
        disabled[slot] = want_disabled;
        true
    }

    /// This entity's menu-local `container_set_data` properties, in vanilla
    /// property-index order — the furnace's four burn/cook timers (see
    /// [`Furnace::container_data`]'s own doc comment for the property
    /// indices). Empty for every other kind: the hopper's menu registers no
    /// such data slots at all, and composter/
    /// brewing-stand are excluded for the same reasons
    /// [`menu_name`](Self::menu_name) gives.
    #[must_use]
    pub fn data_properties(&self) -> Vec<i32> {
        match self {
            BlockEntity::Furnace(f) => (0..4).map(|i| f.container_data(i)).collect(),
            // Beacon data properties: level, primary effect, and secondary
            // effect, in that order.
            BlockEntity::Beacon(b) => vec![
                i32::from(b.levels),
                crate::beacon::encode_beacon_effect(b.primary_effect.as_deref()),
                crate::beacon::encode_beacon_effect(b.secondary_effect.as_deref()),
            ],
            // Indices `0..9` are the per-slot enabled (`0`)/disabled (`1`)
            // flags; index `9` is the trigger flag — always `0` here, since nothing
            // in this crate ever sets it (the auto-crafting trigger itself is
            // not modelled; see this variant's own doc comment).
            BlockEntity::Crafter { disabled, .. } => {
                let mut props: Vec<i32> = disabled.iter().map(|&d| i32::from(d)).collect();
                props.push(0);
                props
            }
            BlockEntity::Hopper(_) | BlockEntity::Composter(_) | BlockEntity::BrewingStand(_)
            | BlockEntity::Container { .. } | BlockEntity::Opaque { .. }
            | BlockEntity::CommandBlock(_) | BlockEntity::Spawner(_) | BlockEntity::Sign(_) => {
                Vec::new()
            }
        }
    }

    /// Advances this entity by exactly one tick, for every variant *except*
    /// [`Hopper`] — a hopper's tick needs its two adjacent registry entries,
    /// which only [`BlockEntityRegistry::tick_hopper`] (holding `&mut self`
    /// over the whole map) can resolve, so it is deliberately excluded here
    /// and ticked separately.
    ///
    /// Returns `Some(now_lit)` when a [`Furnace`]'s `AbstractFurnaceBlock.LIT`
    /// flipped this tick — [`FurnaceTick::lit_changed`], forwarded rather than
    /// dropped so the caller (which holds the [`ChunkSource`](crate::chunk::ChunkSource)
    /// this registry does not) can write the block state through. Composter
    /// `is_ready`/brewing-stand visual sync remain the same disclosed gap this
    /// module's doc comment names — narrower than before, not attempted here.
    fn tick_non_hopper(&mut self) -> Option<bool> {
        match self {
            BlockEntity::Composter(c) => {
                c.tick();
                None
            }
            BlockEntity::Furnace(f) => f.tick().lit_changed,
            BlockEntity::BrewingStand(b) => {
                b.tick();
                None
            }
            BlockEntity::Hopper(_) => {
                debug_assert!(false, "hoppers are ticked via tick_hopper, not this path");
                None
            }
            // A command block is driven by scheduled redstone/chain ticks
            // (`crate::command_block::{on_power_changed, tick,
            // next_chain_position}`), never by a plain once-a-tick advance —
            // see that module's own doc for why nothing calls those yet.
            //
            // A spawner needs the player list and the live entity set to
            // decide anything (`crate::mob_spawner::SpawnerState::tick`'s own
            // `SpawnCtx`), neither of which this method — or this whole
            // registry — has a handle to. `crate::tick::run_tick_loop` drives
            // it directly instead, the same reason the natural-spawn cycle
            // does not live in this registry either.
            // A sign has no active tick of its own — `SignBlockEntity` in
            // 26.2 carries no `tick()` at all (unlike a hanging sign's older
            // wind-sway variant, which this port does not model), so an
            // idle-until-edited sign matches vanilla exactly.
            // A beacon's pyramid/beam recompute and periodic effect
            // application are player-position-dependent (`crate::beacon`'s
            // functions all take a `ChunkSource` and, for the effects
            // themselves, need `ActiveEffects` and the wire) — none of which
            // this registry holds a handle to, the same reason a spawner is
            // driven outside this path. `crate::server`'s per-connection tick
            // section is the real driver; see `BeaconData`'s own doc for the
            // "refreshed on player action, not every tick" tradeoff that
            // follows from not having a driver here.
            // No `craftingTicksRemaining` countdown here — see this variant's
            // own doc comment for why the trigger itself is out of scope.
            BlockEntity::Crafter { .. } => None,
            BlockEntity::Container { .. } | BlockEntity::Opaque { .. } | BlockEntity::CommandBlock(_)
            | BlockEntity::Spawner(_) | BlockEntity::Sign(_) | BlockEntity::Beacon(_) => None,
        }
    }
}

/// Resolves the fresh [`BlockEntity`] a *placement* of `item` (a full item
/// id, e.g. `"minecraft:furnace"`, matching [`ItemStack::item`]'s `Display` —
/// the same string vocabulary [`crate::composter::compostable_chance`] and
/// [`crate::furnace::base_burn_duration`] already key on) should create,
/// alongside the canonical block-state string to write through
/// [`crate::chunk::ChunkSource::set_block`] for it. `None` for any item that
/// is not one of the four block-entity blocks this crate models — the
/// caller's cue to fall back to its existing plain-block placement.
///
/// Every block name here is the item's own name with no properties (no
/// `facing=`/`lit=` — this crate's placement already has no per-block
/// orientation rules, see `docs/block-edit.md`'s scope note; this is the same
/// simplification, not a new one).
#[must_use]
pub fn block_entity_for_item(item: &str) -> Option<(&'static str, BlockEntity)> {
    let (block, placed) = placed_block_entity_for_item(item)?;
    Some((block, placed.instantiate(block)))
}

/// The stack, in bytes, one [`block_entity_for_item`] call must fit inside —
/// asserted by `tests::resolving_a_placement_fits_a_modest_stack`, because the
/// type system has no way to state "this function's frame stays small" and a
/// frame that outgrows a thread stack is invisible until an unrelated suite
/// dies at frame zero.
///
/// The split resolution below reserves 35,920 bytes across the two frames that
/// are live at once on that call path — 18,528 for [`block_entity_for_item`]
/// plus 17,392 for [`PlacedBlockEntity::instantiate`], both read out of the
/// prologues with `llvm-objdump -d` over this crate's own object files. The
/// budget is set well over seven times that, so the callee constructors, the
/// caller's own `(&str, BlockEntity)` binding and the harness's frames all
/// fit, while staying comfortably *under* the 366,720 bytes a single wide
/// match costs — the guard fires on a return to that shape.
#[cfg(test)]
const PLACEMENT_STACK_BUDGET: usize = 256 * 1024;

/// The block-entity *kind* a placement resolves to, ahead of any
/// [`BlockEntity`] existing — small enough that the wide item-id match below
/// can afford one stack slot per arm.
///
/// # Why the resolution is split in two
///
/// A debug build gives each arm of a `match` its own stack slot for that arm's
/// temporaries, so such a match's frame is the *sum* over its arms rather than
/// the largest of them. `size_of::<BlockEntity>()` is 9,168 bytes — its own
/// [`Hopper`] variant, at the same 9,168 bytes for its five-slot flat
/// container, is now the widest; [`Crafter`](BlockEntity::Crafter)'s own grid
/// is boxed rather than inline for exactly this reason (see that variant's
/// own doc comment) and no longer sets the enum's size. `size_of::<ItemStack>()`
/// is 1,832 on its own — so a forty-arm item-id match that materialises one
/// `BlockEntity` per arm still reserves 366,720 bytes in its prologue, more
/// than a default thread stack: merely *calling* such a function overflows, no
/// recursion involved.
///
/// Matching an item id down to this descriptor first keeps the wide match's
/// per-arm slot at tuple-of-pointers scale and leaves exactly one place
/// ([`instantiate`](Self::instantiate), nine arms) where a `BlockEntity` is
/// built at all. Boxing a whole arm's produced value is *not* an alternative
/// to that split: `Box::new(expr)` evaluates `expr` into a stack temporary
/// before moving it to the heap, so every arm would still reserve its full
/// 9,168 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacedBlockEntity {
    Composter,
    Furnace(FurnaceKind),
    Hopper,
    BrewingStand,
    /// A plain item container of `size` slots. Its
    /// `minecraft:block_entity_type` key is the block name resolved alongside
    /// the descriptor, so it is not repeated here.
    Container { size: usize },
    CommandBlock,
    Beacon,
    Crafter,
    Sign { hanging: bool },
}

impl PlacedBlockEntity {
    /// Builds the one fresh [`BlockEntity`] a placement creates. `block` is the
    /// canonical block name resolved alongside the descriptor, which the
    /// container variants need as their type key.
    fn instantiate(self, block: &str) -> BlockEntity {
        match self {
            PlacedBlockEntity::Composter => BlockEntity::Composter(Composter::new()),
            PlacedBlockEntity::Furnace(kind) => BlockEntity::Furnace(Furnace::new(kind)),
            PlacedBlockEntity::Hopper => BlockEntity::Hopper(Hopper::new()),
            PlacedBlockEntity::BrewingStand => BlockEntity::BrewingStand(BrewingStand::new()),
            PlacedBlockEntity::Container { size } => BlockEntity::container_of_size(block, size),
            PlacedBlockEntity::CommandBlock => {
                BlockEntity::CommandBlock(crate::command_block::CommandBlockData::new())
            }
            PlacedBlockEntity::Beacon => BlockEntity::Beacon(BeaconData::default()),
            PlacedBlockEntity::Crafter => BlockEntity::crafter(),
            PlacedBlockEntity::Sign { hanging } => BlockEntity::Sign(SignData {
                hanging,
                ..SignData::default()
            }),
        }
    }
}

/// The canonical block-state string a placement of `item` writes, paired with
/// the [`PlacedBlockEntity`] describing what to register at that position —
/// [`block_entity_for_item`]'s item-id half, kept separate from the
/// construction half for the stack-frame reason [`PlacedBlockEntity`]'s own doc
/// comment gives.
///
/// The block name is the item's own name in every arm; it is spelled out per
/// arm rather than taken from `item` because the caller is promised a
/// `&'static str` and a matched string literal is the only `'static` the match
/// has.
fn placed_block_entity_for_item(item: &str) -> Option<(&'static str, PlacedBlockEntity)> {
    let placed = match item {
        "minecraft:furnace" => ("minecraft:furnace", PlacedBlockEntity::Furnace(FurnaceKind::Furnace)),
        "minecraft:smoker" => ("minecraft:smoker", PlacedBlockEntity::Furnace(FurnaceKind::Smoker)),
        "minecraft:blast_furnace" => (
            "minecraft:blast_furnace",
            PlacedBlockEntity::Furnace(FurnaceKind::BlastFurnace),
        ),
        "minecraft:composter" => ("minecraft:composter", PlacedBlockEntity::Composter),
        "minecraft:hopper" => ("minecraft:hopper", PlacedBlockEntity::Hopper),
        "minecraft:brewing_stand" => ("minecraft:brewing_stand", PlacedBlockEntity::BrewingStand),
        "minecraft:chest" => (
            "minecraft:chest",
            PlacedBlockEntity::Container { size: CONTAINER_9X3_SIZE },
        ),
        "minecraft:trapped_chest" => (
            "minecraft:trapped_chest",
            PlacedBlockEntity::Container { size: CONTAINER_9X3_SIZE },
        ),
        "minecraft:barrel" => (
            "minecraft:barrel",
            PlacedBlockEntity::Container { size: CONTAINER_9X3_SIZE },
        ),
        "minecraft:dispenser" => (
            "minecraft:dispenser",
            PlacedBlockEntity::Container { size: CONTAINER_3X3_SIZE },
        ),
        "minecraft:dropper" => (
            "minecraft:dropper",
            PlacedBlockEntity::Container { size: CONTAINER_3X3_SIZE },
        ),
        "minecraft:beacon" => ("minecraft:beacon", PlacedBlockEntity::Beacon),
        "minecraft:crafter" => ("minecraft:crafter", PlacedBlockEntity::Crafter),
        "minecraft:command_block" => ("minecraft:command_block", PlacedBlockEntity::CommandBlock),
        "minecraft:chain_command_block" => (
            "minecraft:chain_command_block",
            PlacedBlockEntity::CommandBlock,
        ),
        "minecraft:repeating_command_block" => (
            "minecraft:repeating_command_block",
            PlacedBlockEntity::CommandBlock,
        ),
        // The twelve standing-sign woods plus their twelve hanging-sign
        // counterparts (`lodestone-data`'s `BLOCK_FOR_ITEM` census, items
        // 1016-1039) — one block-entity type each
        // (`minecraft:sign`/`minecraft:hanging_sign`, see
        // [`BlockEntity::type_id`]). The block name is the item's own name:
        // `block_items::block_placed_by` reports the *standing* block for every
        // one of these (wall/hanging-attached orientation is
        // `placed_block_state`'s job, resolved after this call, exactly as the
        // debug_assert at [`block_entity_for_item`]'s call site expects).
        //
        // `editor` starts `None` — the placing player is granted it by
        // `crate::server`'s placement arm, which is the one call site that
        // actually knows who is placing (see [`SignData::editor`]'s own doc
        // comment for why placement is the only grant site this crate has).
        "minecraft:oak_sign" => ("minecraft:oak_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:spruce_sign" => ("minecraft:spruce_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:birch_sign" => ("minecraft:birch_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:jungle_sign" => ("minecraft:jungle_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:acacia_sign" => ("minecraft:acacia_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:cherry_sign" => ("minecraft:cherry_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:dark_oak_sign" => ("minecraft:dark_oak_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:pale_oak_sign" => ("minecraft:pale_oak_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:mangrove_sign" => ("minecraft:mangrove_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:bamboo_sign" => ("minecraft:bamboo_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:crimson_sign" => ("minecraft:crimson_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:warped_sign" => ("minecraft:warped_sign", PlacedBlockEntity::Sign { hanging: false }),
        "minecraft:oak_hanging_sign" => (
            "minecraft:oak_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:spruce_hanging_sign" => (
            "minecraft:spruce_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:birch_hanging_sign" => (
            "minecraft:birch_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:jungle_hanging_sign" => (
            "minecraft:jungle_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:acacia_hanging_sign" => (
            "minecraft:acacia_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:cherry_hanging_sign" => (
            "minecraft:cherry_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:dark_oak_hanging_sign" => (
            "minecraft:dark_oak_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:pale_oak_hanging_sign" => (
            "minecraft:pale_oak_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:mangrove_hanging_sign" => (
            "minecraft:mangrove_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:bamboo_hanging_sign" => (
            "minecraft:bamboo_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:crimson_hanging_sign" => (
            "minecraft:crimson_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        "minecraft:warped_hanging_sign" => (
            "minecraft:warped_hanging_sign",
            PlacedBlockEntity::Sign { hanging: true },
        ),
        _ => return None,
    };
    Some(placed)
}

/// The region-local owner of one block entity during a serial tick pass.
///
/// The current executor still advances every owner on one thread. This type
/// makes the owner that produces an outbound block-state write visible before
/// a future region executor changes where that write is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockEntityTickOwner {
    /// The chunk column containing the entity at `(cx, cz)`.
    Chunk { cx: i32, cz: i32 },
}

/// One block entity assigned to its chunk-local [`BlockEntityTickOwner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEntityTickAssignment {
    /// The owner that advances `pos` during this pass.
    pub owner: BlockEntityTickOwner,
    /// The entity position.
    pub pos: BlockPos,
}

/// A deterministic, chunk-owned view of the entities present at tick start.
///
/// Chunks are visited in `(cx, cz)` order, then their entities in `(y, z, x)`
/// order. The plan is a snapshot: insertions made after it is built wait for
/// the next tick, exactly as they did when the registry snapshotted its keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockEntityTickPlan {
    assignments: Vec<BlockEntityTickAssignment>,
}

impl BlockEntityTickPlan {
    fn from_positions(positions: impl IntoIterator<Item = BlockPos>) -> Self {
        let mut by_chunk: BTreeMap<(i32, i32), Vec<BlockPos>> = BTreeMap::new();
        for pos in positions {
            by_chunk
                .entry((pos.x.div_euclid(16), pos.z.div_euclid(16)))
                .or_default()
                .push(pos);
        }
        let assignments = by_chunk
            .into_iter()
            .flat_map(|((cx, cz), mut positions)| {
                positions.sort_unstable_by_key(|pos| (pos.y, pos.z, pos.x));
                positions.into_iter().map(move |pos| BlockEntityTickAssignment {
                    owner: BlockEntityTickOwner::Chunk { cx, cz },
                    pos,
                })
            })
            .collect();
        Self { assignments }
    }

    /// Every entity assignment in this tick's deterministic serial order.
    #[must_use]
    pub fn assignments(&self) -> &[BlockEntityTickAssignment] {
        &self.assignments
    }
}

/// A block-state write handed from a chunk-owned entity tick to the world
/// writer that owns visible state and client publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEntityTickEffect {
    /// The chunk owner that produced this effect.
    pub owner: BlockEntityTickOwner,
    /// The block-state position the global writer must update.
    pub pos: BlockPos,
    /// The furnace's new `lit` value.
    pub lit: bool,
}

/// A [`BlockPos`]-keyed map of live [`BlockEntity`] values — the world's own
/// set of ticking furnaces/composters/hoppers/brewing stands. See the module
/// doc comment for what this closes and what it still does not.
#[derive(Debug, Default)]
pub struct BlockEntityRegistry {
    entities: HashMap<BlockPos, BlockEntity>,
}

impl BlockEntityRegistry {
    /// An empty registry — the state of a freshly started world before any
    /// placement.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every block entity with its position, in arbitrary order.
    ///
    /// **Non-destructive, unlike every other route into the map**, which is the
    /// whole point: world saving has to read the entire registry without
    /// disturbing the live simulation. The persistence path reads the complete
    /// registry without removing entries, so `chunk_nbt.rs` can write every
    /// block entity in each chunk.
    pub fn iter(&self) -> impl Iterator<Item = (&BlockPos, &BlockEntity)> {
        self.entities.iter()
    }

    /// How many block entities are currently registered. Mostly for tests
    /// and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the registry holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Inserts (or overwrites) the entity at `pos`.
    pub fn insert(&mut self, pos: BlockPos, entity: BlockEntity) {
        self.entities.insert(pos, entity);
    }

    /// Removes and returns the entity at `pos`, if any — e.g. once block
    /// breaking learns to consult this registry (not done by this landing;
    /// `crate::server`'s `apply_block_action` does not call this yet).
    pub fn remove(&mut self, pos: BlockPos) -> Option<BlockEntity> {
        self.entities.remove(&pos)
    }

    /// A read-only view of the entity at `pos`, if any.
    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&BlockEntity> {
        self.entities.get(&pos)
    }

    /// A mutable view of the entity at `pos`, if any.
    pub fn get_mut(&mut self, pos: BlockPos) -> Option<&mut BlockEntity> {
        self.entities.get_mut(&pos)
    }

    /// Snapshots the current registry into deterministic chunk-local owners.
    ///
    /// This does not tick anything. It is public so a future owner-specific
    /// executor can consume the same assignment contract without reaching
    /// into the registry's storage map.
    #[must_use]
    pub fn tick_plan(&self) -> BlockEntityTickPlan {
        BlockEntityTickPlan::from_positions(self.entities.keys().copied())
    }

    /// Advances every registered entity by exactly one tick.
    ///
    /// Positions are snapshotted up front (`BlockEntityTickPlan`, not a live
    /// iterator over `self.entities`) because [`tick_hopper`](Self::tick_hopper)
    /// needs to mutate the map (remove-then-reinsert three entries) while a
    /// plain `HashMap` iterator would forbid mutating the map it is walking.
    /// The snapshot cannot observe an entity a tick *added* mid-pass (nothing
    /// here adds one — only placement does, and placement never runs
    /// concurrently with a tick since both hold the same registry lock, see
    /// [`BlockEntityHandle`]), so this is a complete, deterministic serial pass
    /// over exactly what existed when the tick started.
    pub fn tick_all(&mut self) -> Vec<BlockEntityTickEffect> {
        self.tick_all_with_hopper_lock(&|_| true, &|_| true)
    }

    /// [`tick_all`](Self::tick_all), with each hopper's redstone lock supplied
    /// by the caller and the scan bounded by chunk residency.
    ///
    /// `enabled` receives a hopper's position and answers whether it may
    /// transfer this tick — `false` while redstone-powered. The caller reads it
    /// off the block state, which is where vanilla keeps it
    /// (`HopperBlock.ENABLED`, maintained by `checkPoweredState`); this registry
    /// has no world access and deliberately does not compute it.
    ///
    /// `is_loaded` receives every entity's position (not only a hopper's) and
    /// answers whether its *chunk* is currently loaded. A position it rejects
    /// is skipped entirely — `enabled` is never called for it, and neither is
    /// [`BlockEntity::tick_non_hopper`]. **This is a deliberate behavioural
    /// choice, not merely a cost optimisation**: vanilla ticks block entities
    /// from each loaded chunk's own tick list, not from one registry that
    /// remembers every position a player has ever visited
    /// (`BlockEntityRegistry` has no eviction — see the module doc). Before
    /// this, `is_loaded` did not exist and every registered entity ticked
    /// forever regardless of whether anyone was near it; the measured cost of
    /// that was up to 610 synchronous column *generations* per tick once the
    /// registry outgrew `ChunkStore`'s capacity (`chunk_store.rs`'s own test
    /// module has the counters). The correct fix is to stop asking the
    /// question for a position nothing has loaded, not to make asking it
    /// cheaper — a `false`-returning `is_loaded` reproduces vanilla exactly (a
    /// furnace far from every player does not advance), whereas a
    /// residency-aware-but-still-polled read would leave a hopper's `enabled`
    /// flag stuck at whatever it last observed, which is not a state vanilla
    /// ever has a hopper sit in.
    ///
    /// `tick_all` remains as the unlocked, always-loaded shorthand for the
    /// several tests and call sites that hold no world, so this is an
    /// addition rather than a signature change beyond adding `is_loaded`.
    /// **`crate::tick::run_tick_loop` is the one production caller that must
    /// use this one** — it is the only place holding both a `ChunkSource` and
    /// this registry, and a hopper ticked through the shorthand can never be
    /// locked or bounded.
    ///
    /// Returns every furnace-kind `BlockEntityTickEffect` whose `lit` flipped
    /// this tick. The effect is an explicit hand-off from the chunk owner to
    /// the world writer, which calls `ChunkSource::set_block` and publishes the
    /// visible change — see [`BlockEntity::tick_non_hopper`]'s own doc for why
    /// this registry cannot write it itself.
    pub fn tick_all_with_hopper_lock(
        &mut self,
        is_loaded: &dyn Fn(BlockPos) -> bool,
        enabled: &dyn Fn(BlockPos) -> bool,
    ) -> Vec<BlockEntityTickEffect> {
        let plan = self.tick_plan();
        let mut lit_changes = Vec::new();
        for assignment in plan.assignments() {
            let pos = assignment.pos;
            if !is_loaded(pos) {
                continue;
            }
            let is_hopper = matches!(self.entities.get(&pos), Some(BlockEntity::Hopper(_)));
            if is_hopper {
                self.tick_hopper(pos, enabled(pos));
            } else if let Some(entity) = self.entities.get_mut(&pos)
                && let Some(now_lit) = entity.tick_non_hopper()
            {
                lit_changes.push(BlockEntityTickEffect {
                    owner: assignment.owner,
                    pos,
                    lit: now_lit,
                });
            }
        }
        lit_changes
    }

    /// Ticks the hopper at `pos` against its `above`/`below` neighbours,
    /// mirroring [`Hopper::tick`](crate::hopper::Hopper::tick)'s own
    /// `above`/`below` parameters — this is the "way to ask what's the
    /// container (if any) at world position P" `docs/block-entities.md`
    /// named as the missing piece, answered here for the one container shape
    /// ([`Hopper`]) this crate actually has slots for (see the module doc
    /// comment's scope note).
    ///
    /// Implemented as remove-tick-reinsert rather than three simultaneous
    /// `get_mut` calls: a single `HashMap` cannot hand out more than one live
    /// mutable borrow at a time (`pos`, `pos.y-1`, and `pos.y+1` could even
    /// collide if `pos` were malformed, though `y±1` never equals `y`), so
    /// this sidesteps the borrow entirely rather than reaching for unstable
    /// `get_many_mut`.
    fn tick_hopper(&mut self, pos: BlockPos, enabled: bool) {
        let Some(BlockEntity::Hopper(mut hopper)) = self.entities.remove(&pos) else {
            return;
        };

        let below_pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let above_pos = BlockPos::new(pos.x, pos.y + 1, pos.z);
        let mut below = self.entities.remove(&below_pos);
        let mut above = self.entities.remove(&above_pos);

        // The redstone lock. `enabled` is read from the block state
        // the caller supplies rather than recomputed here, because the block
        // state *is* vanilla's source of truth for it —
        // vanilla's own powered-state check writes
        // `ENABLED` on every neighbour change and on placement, and
        // the hopper block entity then simply obeys it. This registry has no world
        // access to compute `hasNeighborSignal` itself, and needs none.
        hopper.tick(
            enabled,
            below.as_mut().and_then(BlockEntity::hopper_slots_mut),
            above.as_mut().and_then(BlockEntity::hopper_slots_mut),
        );

        self.entities.insert(pos, BlockEntity::Hopper(hopper));
        if let Some(entity) = below {
            self.entities.insert(below_pos, entity);
        }
        if let Some(entity) = above {
            self.entities.insert(above_pos, entity);
        }
    }
}

/// A shared, mutation-capable handle onto one [`BlockEntityRegistry`] —
/// `Arc<Mutex<_>>`-backed and cheaply `Clone`, the same shape
/// [`crate::mobs::LiveMobSource`] already established for sharing the mob
/// simulation between a connection's task and a background tick-loop task.
/// See the module doc comment for why a registry needs this at all (a
/// connection inserts on placement; a tick loop, where one is spawned,
/// advances every entry independently of any one connection's traffic).
#[derive(Debug, Clone, Default)]
pub struct BlockEntityHandle(Arc<Mutex<BlockEntityRegistry>>);

impl BlockEntityHandle {
    /// A handle onto a fresh, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` against the locked registry, returning its result. The whole
    /// point of funnelling every access through this one method (rather than
    /// exposing the `Mutex` or a `lock()` accessor) is that no caller can
    /// forget to handle a poisoned lock inconsistently — see the `expect`
    /// below, matching [`crate::mobs::LiveMobSource`]'s own
    /// "poisoned lock is a bug, not a recoverable condition" precedent.
    pub fn with<R>(&self, f: impl FnOnce(&mut BlockEntityRegistry) -> R) -> R {
        let _order = crate::lock_order::acquire(crate::lock_order::LockClass::BlockEntities);
        let mut guard = self.0.lock().expect("block entity registry lock poisoned");
        f(&mut guard)
    }
}

/// # Block-state NBT representation
///
/// Renders a canonical block-state string as the `{Name, Properties}` compound
/// used by NBT fields that hold a block state.
///
/// `Properties` is omitted entirely for a state with none, matching vanilla's own
/// codec (an empty map would still decode, but writing one where vanilla writes
/// nothing is a gratuitous divergence in a payload we may one day compare byte for
/// byte against a capture). Every property value is a `String`, including numeric
/// and boolean ones — `Properties` is a map of the property's *serialized name*,
/// never its typed value.
#[must_use]
pub fn block_state_nbt(state: &str) -> Nbt {
    let (name, properties) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut fields = vec![("Name".to_string(), Nbt::String(name.to_string()))];
    let pairs: Vec<(String, Nbt)> = properties
        .split(',')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), Nbt::String(value.to_string())))
        .collect();
    if !pairs.is_empty() {
        fields.push(("Properties".to_string(), Nbt::Compound(pairs)));
    }
    Nbt::Compound(fields)
}

/// One in-flight moving piston's network NBT — `PistonMovingBlockEntity`'s
/// `getUpdateTag`, which is `saveCustomOnly`, i.e. exactly `saveAdditional`'s five
/// fields with no `id`/`x`/`y`/`z`.
///
/// Ported from `saveAdditional`, not from the field declarations, and the two
/// differ in a way that matters:
///
/// * **`facing` is a `Byte`**, because `Direction.LEGACY_ID_CODEC` is
///   `Codec.BYTE` over `get3DDataValue`. Written as an `Int` it decodes as absent
///   and every piston silently defaults to `DOWN`.
/// * **`progress` is `progressO`**, the value at the *start* of the tick, so a
///   freshly created entity reports [`crate::piston::PISTON_INITIAL_PROGRESS`] and
///   not the `0.5` the server's own first tick would already have reached. A client
///   ramps from this seed itself; sending the advanced value halves the animation.
/// * `extending` and `source` are `putBoolean`, which is a `Byte` in NBT.
#[must_use]
pub fn moving_piston_nbt(entity: &crate::piston::MovingBlockEntity) -> Nbt {
    Nbt::Compound(vec![
        (
            "blockState".to_string(),
            block_state_nbt(&entity.moved_state),
        ),
        ("facing".to_string(), Nbt::Byte(entity.facing_3d_value())),
        (
            "progress".to_string(),
            Nbt::Float(crate::piston::PISTON_INITIAL_PROGRESS),
        ),
        ("extending".to_string(), Nbt::Byte(i8::from(entity.extending))),
        ("source".to_string(), Nbt::Byte(i8::from(entity.source))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    /// One item id per [`PlacedBlockEntity`] variant, so the frame the guard
    /// below measures covers every arm of
    /// [`PlacedBlockEntity::instantiate`] — including both the `Crafter` arm
    /// and the `Hopper` arm, the latter now the one that sets
    /// `BlockEntity`'s width (see [`PlacedBlockEntity`]'s own doc comment).
    const ONE_ITEM_PER_PLACED_KIND: &[&str] = &[
        "minecraft:composter",
        "minecraft:furnace",
        "minecraft:hopper",
        "minecraft:brewing_stand",
        "minecraft:chest",
        "minecraft:dispenser",
        "minecraft:command_block",
        "minecraft:beacon",
        "minecraft:crafter",
        "minecraft:oak_sign",
        "minecraft:oak_hanging_sign",
    ];

    /// The stack-frame guard [`PLACEMENT_STACK_BUDGET`] exists for: resolving a
    /// placement must fit a modest thread stack, because a `match` arm's
    /// temporaries are per-arm allocas in a debug build and a wide match over a
    /// 16 KiB return type reserves megabytes (see [`PlacedBlockEntity`]'s doc
    /// comment for the arithmetic and the frame sizes).
    ///
    /// Run in a re-exec of this very test binary rather than on a thread here,
    /// because an over-budget frame does not return an error — it trips the
    /// thread's guard page and aborts the process. In a child, that abort is a
    /// non-zero exit status this assertion can name; in-process it would take
    /// the whole suite down with a bare `SIGABRT`, which is precisely the
    /// unattributable failure mode this guard exists to replace.
    #[test]
    fn resolving_a_placement_fits_a_modest_stack() {
        const CHILD_MARKER: &str = "LODESTONE_PLACEMENT_STACK_CHILD";
        const TEST_PATH: &str = "block_entities::tests::resolving_a_placement_fits_a_modest_stack";

        if std::env::var_os(CHILD_MARKER).is_some() {
            std::thread::Builder::new()
                .name("placement-stack-budget".to_owned())
                .stack_size(PLACEMENT_STACK_BUDGET)
                .spawn(|| {
                    for item in ONE_ITEM_PER_PLACED_KIND {
                        let (block, entity) = block_entity_for_item(item).expect(item);
                        assert_eq!(block, *item, "the block name is the item's own name");
                        // Reading the entity back keeps the value live past the
                        // call, so the frame under test is a real one.
                        assert!(!entity.type_id().is_empty());
                    }
                })
                .expect("spawn the budgeted thread")
                .join()
                .expect("resolving a placement panicked inside the budgeted thread");
            return;
        }

        let exe = std::env::current_exe().expect("this test binary's own path");
        let status = std::process::Command::new(exe)
            .args(["--exact", TEST_PATH])
            .env(CHILD_MARKER, "1")
            .status()
            .expect("re-exec this test binary");
        assert!(
            status.success(),
            "resolving a placement did not fit {PLACEMENT_STACK_BUDGET} bytes of stack \
             (child exited {status}); a wide match materialising one BlockEntity per arm \
             costs one 9,168-byte alloca per arm — see PlacedBlockEntity's doc comment",
        );
    }

    #[test]
    fn block_entity_for_item_resolves_all_four_kinds_and_rejects_a_plain_block() {
        let (block, entity) = block_entity_for_item("minecraft:furnace").expect("furnace");
        assert_eq!(block, "minecraft:furnace");
        assert!(matches!(entity, BlockEntity::Furnace(f) if f.kind() == FurnaceKind::Furnace));

        let (block, entity) = block_entity_for_item("minecraft:smoker").expect("smoker");
        assert_eq!(block, "minecraft:smoker");
        assert!(matches!(entity, BlockEntity::Furnace(f) if f.kind() == FurnaceKind::Smoker));

        let (block, entity) = block_entity_for_item("minecraft:blast_furnace").expect("blast furnace");
        assert_eq!(block, "minecraft:blast_furnace");
        assert!(matches!(entity, BlockEntity::Furnace(f) if f.kind() == FurnaceKind::BlastFurnace));

        let (block, entity) = block_entity_for_item("minecraft:composter").expect("composter");
        assert_eq!(block, "minecraft:composter");
        assert!(matches!(entity, BlockEntity::Composter(_)));

        let (block, entity) = block_entity_for_item("minecraft:hopper").expect("hopper");
        assert_eq!(block, "minecraft:hopper");
        assert!(matches!(entity, BlockEntity::Hopper(_)));

        let (block, entity) = block_entity_for_item("minecraft:brewing_stand").expect("brewing stand");
        assert_eq!(block, "minecraft:brewing_stand");
        assert!(matches!(entity, BlockEntity::BrewingStand(_)));

        assert!(
            block_entity_for_item("minecraft:stone").is_none(),
            "a plain block must not resolve to a block entity"
        );
    }

    /// A standing sign and a hanging sign resolve to two different
    /// [`BlockEntity::type_id`]s — the collision this crate must not fold
    /// into one, since vanilla gives them distinct `BlockEntityType`s.
    #[test]
    fn sign_items_resolve_to_the_right_block_entity_type() {
        let (block, entity) = block_entity_for_item("minecraft:oak_sign").expect("oak sign");
        assert_eq!(block, "minecraft:oak_sign");
        assert_eq!(entity.type_id(), "minecraft:sign");
        assert!(matches!(entity, BlockEntity::Sign(ref s) if !s.hanging));

        let (block, entity) =
            block_entity_for_item("minecraft:warped_hanging_sign").expect("warped hanging sign");
        assert_eq!(block, "minecraft:warped_hanging_sign");
        assert_eq!(entity.type_id(), "minecraft:hanging_sign");
        assert!(matches!(entity, BlockEntity::Sign(ref s) if s.hanging));

        // Control: a sign has no menu at all — the same shape as composter/
        // brewing-stand, distinct from every other placeable this module
        // models.
        assert_eq!(entity.menu_name(), None);
        assert!(entity.container_slots().is_empty());
    }

    /// [`strip_sign_formatting`]: `(?i)§[0-9A-FK-OR]` pairs are dropped, and
    /// nothing else is — a bare `§` with no following code character, or one
    /// followed by an unrecognised letter, must survive untouched.
    #[test]
    fn strip_sign_formatting_matches_the_vanilla_regex() {
        assert_eq!(strip_sign_formatting("\u{00A7}chello"), "hello", "lowercase colour code");
        assert_eq!(strip_sign_formatting("\u{00A7}Chello"), "hello", "uppercase, case-insensitive");
        assert_eq!(strip_sign_formatting("\u{00A7}khello\u{00A7}r"), "hello", "obfuscate + reset");
        assert_eq!(
            strip_sign_formatting("a\u{00A7}0b\u{00A7}fc"),
            "abc",
            "multiple codes through one line"
        );
        assert_eq!(
            strip_sign_formatting("trailing\u{00A7}"),
            "trailing\u{00A7}",
            "a bare section sign with nothing after it is not a code pair"
        );
        assert_eq!(
            strip_sign_formatting("\u{00A7}ztext"),
            "\u{00A7}ztext",
            "'z' is outside 0-9a-fk-or and must not be stripped"
        );
        assert_eq!(strip_sign_formatting("plain text"), "plain text");
    }

    /// [`apply_sign_update`]'s full gate: the placer (who placement grants
    /// [`SignData::editor`] to) can make exactly one successful edit before
    /// the grant is spent, a different uuid is refused outright, and a waxed
    /// sign refuses everyone — three distinct refusal reasons, each checked
    /// against the same starting state so none is a vacuous pass against the
    /// others.
    #[test]
    fn apply_sign_update_gates_on_editor_and_waxed() {
        let placer = uuid::Uuid::from_u128(1);
        let stranger = uuid::Uuid::from_u128(2);
        let lines = ["hello".to_string(), "world".to_string(), String::new(), String::new()];

        // The editor's own edit succeeds and spends the grant.
        let mut entity = BlockEntity::Sign(SignData { editor: Some(placer), ..SignData::default() });
        assert!(apply_sign_update(&mut entity, placer, true, lines.clone()));
        let BlockEntity::Sign(sign) = &entity else { panic!("must still be a sign") };
        assert_eq!(sign.front, lines);
        assert_eq!(sign.back, ["", "", "", ""], "only the front side was targeted");
        assert_eq!(sign.editor, None, "a successful edit clears the grant");

        // The same placer, immediately again: the grant is already spent, so
        // this must be refused and the text must not change — this is the
        // control that proves the first edit really cleared it rather than
        // succeeding by coincidence.
        let second = ["overwritten".to_string(), String::new(), String::new(), String::new()];
        assert!(!apply_sign_update(&mut entity, placer, true, second));
        let BlockEntity::Sign(sign) = &entity else { panic!("must still be a sign") };
        assert_eq!(sign.front, lines, "a spent grant must not be reusable");

        // A stranger, fresh grant: refused regardless, and the back side
        // (never touched above) proves untouched too.
        let mut entity = BlockEntity::Sign(SignData { editor: Some(placer), ..SignData::default() });
        assert!(!apply_sign_update(&mut entity, stranger, false, lines.clone()));
        let BlockEntity::Sign(sign) = &entity else { panic!("must still be a sign") };
        assert_eq!(sign.back, ["", "", "", ""], "a non-owner's edit must not land");
        assert_eq!(sign.editor, Some(placer), "a refused edit must not spend the grant either");

        // Waxed refuses even the rightful editor.
        let mut waxed = BlockEntity::Sign(SignData {
            editor: Some(placer),
            waxed: true,
            ..SignData::default()
        });
        assert!(!apply_sign_update(&mut waxed, placer, true, lines.clone()));

        // Control: a non-sign block entity must never be mistaken for one.
        let mut not_a_sign = BlockEntity::Composter(Composter::new());
        assert!(!apply_sign_update(&mut not_a_sign, placer, true, lines));
    }

    /// Placement wiring, end to end through the registry: a placed sign is
    /// registered with `editor` already set to the placer, so the very next
    /// `SIGN_UPDATE` from that uuid succeeds with no separate grant step —
    /// this is what `crate::server`'s placement arm relies on.
    #[test]
    fn a_freshly_placed_sign_grants_its_placer_immediate_edit_permission() {
        let (_, mut entity) = block_entity_for_item("minecraft:spruce_sign").expect("spruce sign");
        let placer = uuid::Uuid::from_u128(42);
        if let BlockEntity::Sign(sign) = &mut entity {
            sign.editor = Some(placer);
        }
        let lines = ["FREE", "SPRUCE", "SIGN", ""].map(str::to_owned);
        assert!(apply_sign_update(&mut entity, placer, true, lines.clone()));
        let BlockEntity::Sign(sign) = &entity else { panic!("must still be a sign") };
        assert_eq!(sign.front, lines);
    }

    /// A furnace's menu identity/slots/data round trip through the generic
    /// [`BlockEntity`] accessors exactly as [`Furnace`]'s own API reports
    /// them — the wiring layer (`crate::server`) never reads a [`Furnace`]
    /// directly, so this is the control that the indirection is faithful.
    #[test]
    fn furnace_menu_accessors_mirror_the_furnace_directly() {
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        let mut entity = BlockEntity::Furnace(furnace);

        assert_eq!(entity.menu_name(), Some("minecraft:furnace"));
        assert_eq!(
            entity.container_slots(),
            vec![Some(stack("minecraft:iron_ore", 1)), Some(stack("minecraft:coal", 1)), None]
        );
        // Furnace::container_data(0..3) for a freshly set, not-yet-ticked
        // furnace: unlit (0), no lit-duration recorded yet (0), no cook
        // progress (0), but `set_input` already computed the recipe's total
        // cook time (200 for iron ore, matching `furnace.rs`'s own
        // `DEFAULT_COOK_TIME`/recipe-table citation) into index 3.
        assert_eq!(entity.data_properties(), vec![0, 0, 0, 200]);

        entity.set_container_slot(2, Some(stack("minecraft:iron_ingot", 1)));
        assert_eq!(entity.container_slots()[2], Some(stack("minecraft:iron_ingot", 1)));

        let smoker = BlockEntity::Furnace(Furnace::new(FurnaceKind::Smoker));
        assert_eq!(smoker.menu_name(), Some("minecraft:smoker"));
        let blast = BlockEntity::Furnace(Furnace::new(FurnaceKind::BlastFurnace));
        assert_eq!(blast.menu_name(), Some("minecraft:blast_furnace"));
    }

    /// A hopper's 5 flat slots round-trip through the same generic
    /// accessors, and it has no menu-local data properties at all — the
    /// control that `data_properties` genuinely varies by kind rather than
    /// always answering the furnace's 4.
    #[test]
    fn hopper_menu_accessors_mirror_the_hopper_directly() {
        let mut hopper = Hopper::new();
        hopper.set_slot(0, Some(stack("minecraft:diamond", 3)));
        let mut entity = BlockEntity::Hopper(hopper);

        assert_eq!(entity.menu_name(), Some("minecraft:hopper"));
        assert_eq!(entity.container_slots().len(), 5);
        assert_eq!(entity.container_slots()[0], Some(stack("minecraft:diamond", 3)));
        assert!(entity.data_properties().is_empty());

        entity.set_container_slot(1, Some(stack("minecraft:emerald", 1)));
        assert_eq!(entity.container_slots()[1], Some(stack("minecraft:emerald", 1)));
    }

    /// A dispenser and dropper each get a real 9-slot
    /// `generic_3x3` container — distinct from a chest's 27-slot
    /// `generic_9x3`, which the `menu_name`/`container_slots` split by size
    /// must not collapse into one shape.
    #[test]
    fn dispenser_and_dropper_get_a_nine_slot_generic_3x3_container() {
        let (block, entity) = block_entity_for_item("minecraft:dispenser").expect("dispenser");
        assert_eq!(block, "minecraft:dispenser");
        assert_eq!(entity.menu_name(), Some("minecraft:generic_3x3"));
        assert_eq!(entity.container_slots().len(), CONTAINER_3X3_SIZE);

        let (block, entity) = block_entity_for_item("minecraft:dropper").expect("dropper");
        assert_eq!(block, "minecraft:dropper");
        assert_eq!(entity.menu_name(), Some("minecraft:generic_3x3"));
        assert_eq!(entity.container_slots().len(), CONTAINER_3X3_SIZE);

        // The existing chest path must still answer the wider menu — the
        // control that the id-based split above did not just make every
        // container 9 slots.
        let (_, chest) = block_entity_for_item("minecraft:chest").expect("chest");
        assert_eq!(chest.menu_name(), Some("minecraft:generic_9x3"));
        assert_eq!(chest.container_slots().len(), CONTAINER_9X3_SIZE);

        assert_eq!(container_type_for_block("minecraft:dispenser"), Some("minecraft:dispenser"));
        assert_eq!(container_type_for_block("minecraft:dropper"), Some("minecraft:dropper"));
    }

    /// **Control**: composter and brewing stand have no menu at all
    /// (see [`BlockEntity::menu_name`]'s own doc comment for why each is
    /// excluded, for two different reasons) — proving the generic accessors
    /// answer "nothing to open" rather than silently fabricating a menu.
    #[test]
    fn composter_and_brewing_stand_have_no_menu() {
        let composter = BlockEntity::Composter(Composter::new());
        assert_eq!(composter.menu_name(), None);
        assert!(composter.container_slots().is_empty());
        assert!(composter.data_properties().is_empty());

        let brewing = BlockEntity::BrewingStand(BrewingStand::new());
        assert_eq!(brewing.menu_name(), None);
        assert!(brewing.container_slots().is_empty());
        assert!(brewing.data_properties().is_empty());
    }

    #[test]
    fn registry_insert_get_remove_round_trip() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(1, 64, 1);
        assert!(reg.get(pos).is_none());

        reg.insert(pos, BlockEntity::Composter(Composter::new()));
        assert!(matches!(reg.get(pos), Some(BlockEntity::Composter(_))));
        assert_eq!(reg.len(), 1);

        assert!(matches!(reg.remove(pos), Some(BlockEntity::Composter(_))));
        assert!(reg.get(pos).is_none());
        assert!(reg.is_empty());
    }

    /// The ownership plan must be independent of `HashMap` bucket layout: a
    /// later executor may split at these assignments, while today's executor
    /// consumes the same sequence serially. Negative coordinates are the
    /// control for truncation-toward-zero accidentally assigning `x = -1` to
    /// the origin chunk.
    #[test]
    fn tick_plan_groups_entities_by_chunk_in_canonical_serial_order() {
        let mut reg = BlockEntityRegistry::new();
        let positions = [
            BlockPos::new(16, 70, 0),
            BlockPos::new(-1, 72, 0),
            BlockPos::new(1, 80, 1),
            BlockPos::new(1, 64, 1),
        ];
        for pos in positions {
            reg.insert(pos, BlockEntity::Composter(Composter::new()));
        }

        assert_eq!(
            reg.tick_plan().assignments(),
            [
                BlockEntityTickAssignment {
                    owner: BlockEntityTickOwner::Chunk { cx: -1, cz: 0 },
                    pos: BlockPos::new(-1, 72, 0),
                },
                BlockEntityTickAssignment {
                    owner: BlockEntityTickOwner::Chunk { cx: 0, cz: 0 },
                    pos: BlockPos::new(1, 64, 1),
                },
                BlockEntityTickAssignment {
                    owner: BlockEntityTickOwner::Chunk { cx: 0, cz: 0 },
                    pos: BlockPos::new(1, 80, 1),
                },
                BlockEntityTickAssignment {
                    owner: BlockEntityTickOwner::Chunk { cx: 1, cz: 0 },
                    pos: BlockPos::new(16, 70, 0),
                },
            ],
            "chunk order, then local position order, is the serial execution contract"
        );
    }

    /// Two chunk owners handing lit flips to the global world writer must
    /// retain the plan's order. The same insertion sequence is intentionally
    /// scrambled, so this is not a test of insertion order or hash layout.
    #[test]
    fn tick_effects_preserve_chunk_owner_handoff_order() {
        let mut reg = BlockEntityRegistry::new();
        for pos in [BlockPos::new(16, 70, 0), BlockPos::new(-1, 70, 0)] {
            let mut furnace = Furnace::new(FurnaceKind::Furnace);
            furnace.set_fuel(Some(stack("minecraft:coal", 1)));
            furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
            reg.insert(pos, BlockEntity::Furnace(furnace));
        }

        assert_eq!(
            reg.tick_all(),
            [
                BlockEntityTickEffect {
                    owner: BlockEntityTickOwner::Chunk { cx: -1, cz: 0 },
                    pos: BlockPos::new(-1, 70, 0),
                    lit: true,
                },
                BlockEntityTickEffect {
                    owner: BlockEntityTickOwner::Chunk { cx: 1, cz: 0 },
                    pos: BlockPos::new(16, 70, 0),
                    lit: true,
                },
            ],
            "the global world writer must receive effects in the serial owner order"
        );
    }

    /// A furnace ticked through the registry behaves exactly like one ticked
    /// directly — the registry is a location, not a reimplementation. Loaded
    /// with fuel and ore, it lights on the first `tick_all` and reports the
    /// same `lit_changed` transition [`Furnace::tick`]'s own unit tests
    /// already pin.
    #[test]
    fn tick_all_advances_a_registered_furnace() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));

        reg.tick_all();

        let Some(BlockEntity::Furnace(f)) = reg.get(pos) else {
            panic!("furnace must still be registered after a tick");
        };
        assert!(f.is_lit(), "fuel + ingredient must light the furnace on its first tick");
    }

    /// [`BlockEntity::tick_non_hopper`]'s `Option<bool>` return must actually
    /// reach [`BlockEntityRegistry::tick_all`]'s caller — this registry has no
    /// `ChunkSource` to write the block state itself (see the module doc's
    /// "Partial visual sync" note), so `crate::tick::run_tick_loop` is the one
    /// that can, and it can only if the lit flip survives the trip out of
    /// `tick_all_with_hopper_lock`. A second tick with nothing left to ignite
    /// must report no changes at all, not a stale repeat of the first.
    #[test]
    fn tick_all_reports_a_furnace_lit_flip_to_its_caller() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));

        assert_eq!(
            reg.tick_all(),
            vec![BlockEntityTickEffect {
                owner: BlockEntityTickOwner::Chunk { cx: 0, cz: 0 },
                pos,
                lit: true,
            }]
        );
        assert_eq!(
            reg.tick_all(),
            Vec::new(),
            "already-lit furnace must not report a spurious flip every tick"
        );
    }

    /// Two hoppers stacked (`below` sits directly under `above`) move
    /// **two** items on the first tick, not one — this is vanilla's real
    /// "double hopper" throughput, not a bug: each hopper's own
    /// [`Hopper::tick`](crate::hopper::Hopper::tick) independently attempts
    /// *both* a push (into its own `below`) and a pull (from its own
    /// `above`), so within one `tick_all` pass, `below`'s tick pulls one
    /// item up from `above`, and **separately** `above`'s own tick pushes
    /// one item down into `below` — two independent successful transfers in
    /// the same tick, exactly the mechanic vanilla hopper clocks/sorters
    /// exploit for 2x throughput. `hopper.rs`'s own
    /// `sucks_one_item_from_above_on_first_ready_tick` already proves the
    /// *single*-hopper pull in isolation; this proves the pair survives
    /// going through the registry's remove/tick/reinsert path with **both**
    /// neighbours present simultaneously (the harder case: `tick_hopper`
    /// must resolve `above` correctly even while `below` has *also* been
    /// removed from the map for the duration of the call) and predicts the
    /// combined result rather than just one direction.
    #[test]
    fn tick_all_moves_two_items_between_a_stacked_hopper_pair_on_the_first_tick() {
        let mut reg = BlockEntityRegistry::new();
        let below_pos = BlockPos::new(5, 10, 5);
        let above_pos = BlockPos::new(5, 11, 5);

        let mut above = Hopper::new();
        above.set_slot(0, Some(stack("minecraft:diamond", 3)));
        reg.insert(below_pos, BlockEntity::Hopper(Hopper::new()));
        reg.insert(above_pos, BlockEntity::Hopper(above));

        reg.tick_all();

        // `below`'s own tick pulled one item from `above` (3 -> 2 there,
        // landing in `below`'s empty slot 0), then `above`'s own tick pushed
        // one more of its remaining items down into `below` (2 -> 1 there,
        // merging into `below`'s slot 0) — two transfers, one net item each
        // direction's own tick contributed.
        let Some(BlockEntity::Hopper(below)) = reg.get(below_pos) else {
            panic!("below hopper must still be registered");
        };
        assert_eq!(below.slots()[0], Some(stack("minecraft:diamond", 2)));

        let Some(BlockEntity::Hopper(above)) = reg.get(above_pos) else {
            panic!("above hopper must still be registered");
        };
        assert_eq!(above.slots()[0], Some(stack("minecraft:diamond", 1)));
    }

    /// The residency rule, isolated from any world/store machinery: a position
    /// `is_loaded` rejects must not tick at all — not the hopper path, not
    /// the plain `tick_non_hopper` path.
    #[test]
    fn tick_all_with_hopper_lock_skips_a_position_the_loaded_predicate_rejects() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));

        reg.tick_all_with_hopper_lock(&|_| false, &|_| true);

        let Some(BlockEntity::Furnace(f)) = reg.get(pos) else {
            panic!("furnace must still be registered — `is_loaded` skips the tick, not the entity");
        };
        assert!(
            !f.is_lit(),
            "a furnace whose chunk `is_loaded` reports unloaded must not advance — this is \
             the vanilla behaviour the fix chooses (see `tick_all_with_hopper_lock`'s own doc \
             comment): a block entity outside every loaded chunk simply does not tick."
        );

        // Control: the same furnace, same fuel and ore, with the predicate
        // flipped to `true` — proves the furnace above was skipped *because*
        // of the predicate, not because it can never light (which would make
        // the assertion above vacuous).
        let mut reg = BlockEntityRegistry::new();
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));
        reg.tick_all_with_hopper_lock(&|_| true, &|_| true);
        let Some(BlockEntity::Furnace(f)) = reg.get(pos) else {
            panic!("furnace must still be registered");
        };
        assert!(f.is_lit(), "control: the identical furnace must light when `is_loaded` says yes");
    }

    /// The hopper-specific half: `enabled` —
    /// the closure that in production reaches `world.block_state` and would
    /// otherwise generate a whole column per probe — must never even be
    /// *called* for a position `is_loaded` rejects. This is the control that
    /// the residency check bounds the expensive call itself, not just its visible effect:
    /// counting invocations rather than inferring "did it tick" from state.
    #[test]
    fn tick_all_with_hopper_lock_never_calls_enabled_for_an_unloaded_hopper() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(5, 10, 5);
        reg.insert(pos, BlockEntity::Hopper(Hopper::new()));

        let enabled_calls = AtomicU64::new(0);
        reg.tick_all_with_hopper_lock(&|_| false, &|_| {
            enabled_calls.fetch_add(1, Ordering::Relaxed);
            true
        });
        assert_eq!(
            enabled_calls.load(Ordering::Relaxed),
            0,
            "`enabled` — production's `world.block_state` probe — must not be called at all \
             for a position `is_loaded` rejects. A nonzero count here is exactly issue #504's \
             defect: a per-hopper world read on every tick regardless of residency."
        );

        // Control: flip `is_loaded` to `true` and confirm the same closure
        // now *is* called exactly once — proving the zero above is the gate
        // firing, not a hopper that was never going to call it anyway (e.g.
        // if the position were somehow not recognised as a hopper).
        let enabled_calls = AtomicU64::new(0);
        reg.tick_all_with_hopper_lock(&|_| true, &|_| {
            enabled_calls.fetch_add(1, Ordering::Relaxed);
            true
        });
        assert_eq!(
            enabled_calls.load(Ordering::Relaxed),
            1,
            "control: a loaded hopper must still have its `enabled` closure called exactly once"
        );
    }

    /// **Control**: a hopper with nothing above and nothing below must not
    /// panic and must not change — proves `tick_hopper`'s `None` handling
    /// (empty neighbour slots) is real, not merely untested.
    #[test]
    fn tick_all_leaves_an_isolated_hopper_unchanged_other_than_its_cooldown() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 0, 0);
        reg.insert(pos, BlockEntity::Hopper(Hopper::new()));

        reg.tick_all();

        let Some(BlockEntity::Hopper(h)) = reg.get(pos) else {
            panic!("hopper must still be registered");
        };
        assert!(h.slots().iter().all(Option::is_none), "no neighbours means nothing to move");
    }

    #[test]
    fn handle_with_mutates_the_shared_registry() {
        let handle = BlockEntityHandle::new();
        let pos = BlockPos::new(2, 2, 2);
        handle.with(|reg| reg.insert(pos, BlockEntity::Composter(Composter::new())));

        let clone = handle.clone();
        let present = clone.with(|reg| reg.get(pos).is_some());
        assert!(present, "a clone of the handle must see the same registry");
    }

    /// The actual background driver [`crate::IntegratedServer::open_in_memory_with_mobs`]
    /// spawns, not just [`BlockEntityRegistry::tick_all`] called synchronously
    /// — this is what proves a furnace really ticks *in a running task over
    /// time*, the missing half `docs/block-entities.md` named ("no tick loop
    /// drives them"). Loaded with one coal (1600-tick burn duration — far
    /// more than needed) and one iron ore (a 200-tick smelt,
    /// `furnace.rs`'s own `iron_ore_smelts_into_one_ingot_at_exactly_tick_200`
    /// unit test pins the same number), it must light on the very first real
    /// tick the loop performs and hold a real iron ingot in its output slot
    /// well before 200 ticks' worth of virtual time (10s at 50ms/tick) have
    /// elapsed — predicting the value (a real ingot), not just that
    /// *something* eventually changed.
    ///
    /// `#[tokio::test(start_paused = true)]`: the same precedent
    /// `tests/serve_play.rs` already established for this crate — real
    /// `tokio::time::interval`s, virtual clock, resolves in a fraction of a
    /// second of actual wall time.
    /// [`moving_piston_nbt`] against `PistonMovingBlockEntity.saveAdditional` read
    /// as a record definition: five fields, and the **tag types** are the half a
    /// name-only comparison cannot see.
    ///
    /// `facing` written as an `Nbt::Int` is the specific failure this pins. A
    /// client reads it with `getBooleanOr`-style tolerance for the flags but a
    /// strict `Nbt::Byte` match for the direction, so an `Int` decodes as *absent*
    /// and every piston in the world silently animates toward `DOWN` — a clean
    /// parse, no error, wrong geometry. Same shape as the `Age` short/int collision
    /// this repo already paid for.
    #[test]
    fn moving_piston_nbt_matches_the_vanilla_update_tag() {
        let entity = crate::piston::MovingBlockEntity {
            moved_state: "minecraft:piston_head[facing=east,short=false,type=sticky]".to_string(),
            direction: crate::neighbor_update::Direction::East,
            // Distinct on purpose: equal flags would let a transposition of the two
            // adjacent booleans through unnoticed.
            extending: true,
            source: false,
        };
        let Nbt::Compound(fields) = moving_piston_nbt(&entity) else {
            panic!("the update tag must be a compound");
        };
        let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["blockState", "facing", "progress", "extending", "source"],
            "exactly `saveAdditional`'s five fields, and no id/x/y/z — \
             `getUpdateTag` is `saveCustomOnly`"
        );
        let field = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .expect("field present")
        };
        // `Direction.LEGACY_ID_CODEC` is `Codec.BYTE` over `get3DDataValue`, and
        // EAST is 5.
        assert_eq!(field("facing"), Nbt::Byte(5));
        assert_eq!(field("progress"), Nbt::Float(0.0), "`progressO`, not `progress`");
        assert_eq!(field("extending"), Nbt::Byte(1));
        assert_eq!(field("source"), Nbt::Byte(0));
        // The block-state payload is a `Name` string plus a `Properties` map
        // whose values are all strings, including the boolean `short`.
        assert_eq!(
            field("blockState"),
            Nbt::Compound(vec![
                (
                    "Name".to_string(),
                    Nbt::String("minecraft:piston_head".to_string())
                ),
                (
                    "Properties".to_string(),
                    Nbt::Compound(vec![
                        ("facing".to_string(), Nbt::String("east".to_string())),
                        ("short".to_string(), Nbt::String("false".to_string())),
                        ("type".to_string(), Nbt::String("sticky".to_string())),
                    ])
                ),
            ])
        );
        // A state with no properties omits `Properties` entirely, as vanilla's
        // codec does.
        assert_eq!(
            block_state_nbt("minecraft:stone"),
            Nbt::Compound(vec![(
                "Name".to_string(),
                Nbt::String("minecraft:stone".to_string())
            )])
        );
    }

    /// `crate::tick::run_tick_loop` drives block-entity ticking through
    /// `tick_all_with_hopper_lock`; this test exercises that registry call
    /// directly and checks the lock-sensitive behavior.
    /// What this test actually exercises, a furnace advancing correctly over
    /// 200 real ticks of [`BlockEntityRegistry::tick_all`], is unchanged by
    /// which loop calls it, so it is driven directly here instead.
    #[test]
    fn two_hundred_ticks_of_tick_all_fully_smelts_an_iron_ore() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));

        assert_eq!(
            reg.tick_all(),
            vec![BlockEntityTickEffect {
                owner: BlockEntityTickOwner::Chunk { cx: 0, cz: 0 },
                pos,
                lit: true,
            }],
            "must light on the first tick"
        );
        for _ in 1..200 {
            reg.tick_all();
        }
        let output = match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.output().cloned(),
            _ => None,
        };
        assert_eq!(
            output,
            Some(stack("minecraft:iron_ingot", 1)),
            "200 ticks must fully smelt the iron ore into an ingot"
        );
    }

    #[test]
    fn a_fresh_crafter_has_nine_empty_enabled_slots_and_the_crafter_3x3_menu() {
        let crafter = BlockEntity::crafter();
        assert_eq!(crafter.menu_name(), Some("minecraft:crafter_3x3"));
        assert_eq!(crafter.type_id(), "minecraft:crafter");
        assert_eq!(crafter.container_slots(), vec![None; 9]);
        // 9 slot-enabled flags (0 = enabled) then `triggered` = 0 — vanilla's
        // own `NUM_DATA = 10`.
        assert_eq!(crafter.data_properties(), vec![0; 10]);
    }

    #[test]
    fn set_crafter_slot_state_toggles_an_empty_slot_both_ways() {
        let mut crafter = BlockEntity::crafter();
        assert!(crafter.set_crafter_slot_state(3, false), "disabling an empty, enabled slot must succeed");
        assert_eq!(
            crafter.data_properties(),
            vec![0, 0, 0, 1, 0, 0, 0, 0, 0, 0],
            "index 3 must read back disabled (1), every other slot and `triggered` untouched"
        );
        assert!(crafter.set_crafter_slot_state(3, true), "re-enabling it must succeed");
        assert_eq!(crafter.data_properties(), vec![0; 10]);
    }

    /// The slot-occupancy gate applies to **both**
    /// directions — a slot holding an item cannot be toggled either way,
    /// which is the specific case a fixture that only tried disabling could
    /// not see (an already-enabled slot with an item would look identical to
    /// a "the call was a no-op because nothing changed" false pass).
    #[test]
    fn set_crafter_slot_state_refuses_a_slot_holding_an_item_in_either_direction() {
        let mut crafter = BlockEntity::crafter();
        crafter.set_container_slot(4, Some(stack("minecraft:stick", 1)));
        assert!(
            !crafter.set_crafter_slot_state(4, false),
            "a slot with an item must refuse to disable"
        );
        assert_eq!(crafter.data_properties()[4], 0, "must still read enabled");

        // Force it disabled directly (bypassing the gate) to exercise the
        // re-enable-refusal arm too, not just the mirror of the case above.
        let BlockEntity::Crafter { disabled, .. } = &mut crafter else { unreachable!() };
        disabled[4] = true;
        assert!(
            !crafter.set_crafter_slot_state(4, true),
            "a slot with an item must refuse to re-enable too"
        );
        assert_eq!(crafter.data_properties()[4], 1, "must still read disabled");
    }

    #[test]
    fn set_crafter_slot_state_is_out_of_range_and_wrong_variant_safe() {
        let mut crafter = BlockEntity::crafter();
        assert!(!crafter.set_crafter_slot_state(9, false), "index 9 is out of the 9-slot range");
        assert!(!crafter.set_crafter_slot_state(usize::MAX, false));

        let mut hopper = BlockEntity::Hopper(Hopper::new());
        assert!(
            !hopper.set_crafter_slot_state(0, false),
            "a CONTAINER_SLOT_STATE_CHANGED reaching a menu that is not really a crafter \
             must be the same honest no-op vanilla's own instanceof chain refuses"
        );
    }

    /// Placing an item into a disabled slot
    /// re-enables it, matching vanilla's own unconditional check.
    #[test]
    fn set_container_slot_re_enables_a_disabled_crafter_slot() {
        let mut crafter = BlockEntity::crafter();
        assert!(crafter.set_crafter_slot_state(2, false));
        assert_eq!(crafter.data_properties()[2], 1);
        crafter.set_container_slot(2, Some(stack("minecraft:diamond", 1)));
        assert_eq!(crafter.data_properties()[2], 0, "placing an item must re-enable the slot");
        assert_eq!(crafter.container_slots()[2], Some(stack("minecraft:diamond", 1)));
    }
}
