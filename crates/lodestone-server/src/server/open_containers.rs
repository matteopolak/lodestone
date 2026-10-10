//! The container a player has open: window state, per-tick synchronisation, and opening each screen kind (chests, crafting, workstations, enchanting, merchants).

use super::*;

/// Cadence of the periodic open-container sync ([`sync_open_container`]) —
/// the piece that answers `docs/block-entities.md`'s own design question,
/// "a furnace mutates its own container without a client click": nothing
/// about that mutation is a response to any inbound packet, so a connection
/// with a window open needs its own timer polling the block entity, exactly
/// like [`VITALS_TICK_INTERVAL`] polls submersion. Matches the background
/// tick loop's own cadence (`block_entities.rs`'s `BLOCK_ENTITY_TICK_INTERVAL`)
/// so a change is visible within one real tick of it happening, not only the
/// next time the client happens to send a packet.
#[cfg(not(target_arch = "wasm32"))]
pub(super) const CONTAINER_SYNC_INTERVAL: Duration = Duration::from_millis(50);

/// Which block-entity container (if any) this connection currently has open:
/// the container id the client will echo back in every `container_click`/
/// `container_close` for it, the world position it targets, and how many of
/// its own slots ([`BlockEntity::container_slots`]) precede the standard
/// player-inventory tail in that menu's slot numbering (see
/// `crate::inventory::container_menu_slot`, the click-side consumer of this
/// same number).
#[derive(Debug, Clone, Copy)]
pub(super) struct OpenContainer {
    pub(super) window_id: i32,
    pub(super) pos: BlockPos,
    /// Which menu shape this window is, which decides the slot layout and the
    /// quick-move routing [`crate::container_click`] runs.
    ///
    /// A crafting table is [`MenuKind::CraftingTable`] and its `pos` is the
    /// table's block position — used for nothing but the "did the player break the
    /// block under the menu" check, because a crafting table is **not** a block
    /// entity and has no slots at `pos` at all (the virtual-menu step). Its grid lives
    /// on [`PlayerInventory::table_crafting`].
    pub(super) shape: MenuKind,
    pub(super) container_size: usize,
    /// Vanilla's own generic container-menu state-id field, wrapping at `32767`
    /// (its own "increment state id" helper). Bumped by every content/
    /// slot send (this struct's own [`next_state_id`](Self::next_state_id)),
    /// never by a `container_set_data` send — vanilla's own "broadcast
    /// changes" step
    /// does not touch `stateId` for a data-only change either. This crate
    /// does not validate a click's echoed value against it (see
    /// `docs/server-inventory.md`'s existing scope note for window `0`,
    /// which applies identically here) — it exists so a real client
    /// observes vanilla's own incrementing behaviour rather than a
    /// suspicious constant.
    pub(super) state_id: i32,
}

impl OpenContainer {
    /// Bumps and returns the next state id, matching
    /// The abstract container menu's increment state id's exact wrap.
    pub(super) fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 32767;
        self.state_id
    }
}

/// Which merchant screen (if any) this connection currently has open — set
/// by [`open_merchant_screen`]'s caller, read by
/// [`ServerBound::SelectTrade`]'s dispatch arm. Not [`OpenContainer`]: a
/// villager is a [`crate::mobs::SimMob`], not a [`crate::block_entities::BlockEntity`],
/// so it has no `BlockPos` for that struct's `pos` field or its slot-sync
/// machinery to key on. Carries no `window_id` — vanilla's own consumer of
/// the one packet this drives (`SelectTrade`) checks only "is a merchant
/// menu open", not which window, and this struct exists for exactly that
/// question.
#[derive(Debug, Clone, Copy)]
pub(super) struct OpenMerchant {
    /// The villager entity id this screen's offers came from.
    pub(super) entity_id: i32,
}

/// Per-connection bookkeeping for [`OpenContainer`]'s periodic sync
/// ([`sync_open_container`]): the container slots and menu-data properties
/// last pushed to the client, so a background mutation (a furnace's own
/// tick, not any click) can be diffed and only the changed entries re-sent —
/// the same changed-entry model used by [`EntityStreamer`] for entity spawn,
/// update, and removal.
#[derive(Debug, Default, Clone)]
pub(super) struct ContainerSync {
    pub(super) slots: Vec<Option<ItemStack>>,
    pub(super) data: Vec<i32>,
}

/// Reads `pos`'s container slots and menu-data properties, or a pair of empty
/// vectors if nothing is registered there — the one read [`open_container_screen`]
/// (opening a menu) and the `container_sync_tick` arm of [`serve_play`]
/// (re-reading a background-ticked entity) both need, against the same
/// [`BlockEntityHandle`].
pub(super) fn container_state(
    block_entities: &BlockEntityHandle,
    pos: BlockPos,
) -> (Vec<Option<ItemStack>>, Vec<i32>) {
    block_entities.with(|reg| match reg.get(pos) {
        Some(entity) => (entity.container_slots(), entity.data_properties()),
        None => (Vec::new(), Vec::new()),
    })
}

/// Diffs `current_slots`/`current_data` (freshly read off the block entity at
/// `open.pos`) against what [`ContainerSync`] last pushed to this
/// connection, returning the directives that bring the client back in sync —
/// only the entries that actually changed, each via
/// [`ServerProtocol::encode_container_slot`]/[`encode_container_data`](ServerProtocol::encode_container_data).
///
/// This is the one piece of Job 1 with no client packet driving it at all:
/// [`open_container_screen`] covers "a player opens a menu" and
/// [`apply_container_clicked`] covers "a player clicks in one," but a
/// furnace's own background tick (`crate::tick::run_tick_loop`, the shared world tick,
/// running independently of any connection) is neither — see
/// `docs/block-entities.md`'s own note on this. A caller (`serve_play`'s
/// `container_sync_tick` arm) is expected to call this on its own timer,
/// passing a fresh read of the entity's current state each time; this
/// function does no I/O and no locking itself; a plain `#[test]` can drive
/// it directly with no `Connection`/tokio runtime at all.
pub(super) fn sync_open_container<P: ServerProtocol>(
    proto: &P,
    open: &mut OpenContainer,
    sync: &mut ContainerSync,
    current_slots: Vec<Option<ItemStack>>,
    current_data: Vec<i32>,
) -> Vec<ServerDirective> {
    let mut directives = Vec::new();
    for (index, (new, old)) in current_slots.iter().zip(sync.slots.iter()).enumerate() {
        if new != old {
            let state_id = open.next_state_id();
            directives.push(proto.encode_container_slot(
                open.window_id,
                state_id,
                index as i32,
                new.as_ref(),
            ));
        }
    }
    for (index, (new, old)) in current_data.iter().zip(sync.data.iter()).enumerate() {
        if new != old {
            directives.push(proto.encode_container_data(open.window_id, index as i32, *new));
        }
    }
    sync.slots = current_slots;
    sync.data = current_data;
    directives
}

pub(super) async fn publish_open_container<T: Transport, P: ServerProtocol>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    block_entities: &BlockEntityHandle,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
) -> Result<(), ServerError> {
    if let Some(open) = open_container.as_mut() {
        let (slots, data) = container_state(block_entities, open.pos);
        for directive in sync_open_container(proto, open, container_sync, slots, data) {
            apply(conn, state, directive).await?;
        }
    }
    Ok(())
}

/// Vanilla's own per-menu display name is a translatable component
/// (`container.furnace`, `container.hopper`, resolved client-side from the
/// current language); [`ServerProtocol::encode_open_screen`] only ever
/// writes a **literal** string component (see that trait method's own doc
/// comment for why), so this is the literal English text substituted in its
/// place — cosmetic only, never read by any gameplay logic on either side.
pub(super) fn container_title(menu: &str) -> &'static str {
    match menu {
        "minecraft:furnace" => "Furnace",
        "minecraft:smoker" => "Smoker",
        "minecraft:blast_furnace" => "Blast Furnace",
        "minecraft:hopper" => "Hopper",
        "minecraft:generic_9x3" => "Chest",
        "minecraft:anvil" => "Repair & Name",
        "minecraft:grindstone" => "Grindstone",
        "minecraft:smithing" => "Smithing Table",
        "minecraft:loom" => "Loom",
        "minecraft:stonecutter" => "Stonecutter",
        "minecraft:enchantment" => "Enchant",
        "minecraft:merchant" => "Villager",
        "minecraft:beacon" => "Beacon",
        _ => "Container",
    }
}

/// Opens a villager's `minecraft:merchant` trade screen (the merchant-offers
/// packet). Unlike [`open_container_screen`]/`open_crafting_table_screen`,
/// this sends no `container_set_content`/`container_set_data` at all: a
/// merchant window's whole state is the `MERCHANT_OFFERS` packet, sent
/// immediately after `open_screen`.
///
/// Trade selection reads the villager's persistent offer state and commits the
/// purchase only when the buyer can pay. Restock, leveling, demand, and uses
/// are handled by [`crate::mobs::MobSim::villager_offers`] and
/// [`crate::mobs::MobSim::try_villager_trade`].
///
/// `offers` is the priced persistent list from
/// [`crate::mobs::MobSim::villager_offers`], so the displayed `uses` and
/// `demand` values match the state charged by trade selection.
#[allow(clippy::too_many_arguments)]
pub(super) async fn open_merchant_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    offers: &[crate::villager_trade::OfferState],
    level: i32,
    xp: i32,
    next_window_id: &mut i32,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, "minecraft:merchant", container_title("minecraft:merchant")),
    )
    .await?;

    let wire_offers: Vec<MerchantOfferOut> = offers
        .iter()
        .filter_map(|offer| {
            Some(MerchantOfferOut {
                wants_a: (
                    offer.record.wants_item.parse::<ResourceKey>().ok()?,
                    offer.modified_cost_a_count(),
                ),
                wants_b: None,
                gives: (
                    offer.record.gives_item.parse::<ResourceKey>().ok()?,
                    offer.record.gives_count,
                ),
                max_uses: offer.record.max_uses,
                xp: offer.record.xp,
            })
        })
        .collect();

    apply(
        conn,
        state,
        proto.encode_merchant_offers(window_id, &wire_offers, level, xp, true, true),
    )
    .await
}

/// Executes a merchant purchase directly against the player's held
/// inventory and the [`TradeRecord`](crate::mobs::villager::trades::TradeRecord)
/// selected by [`ServerBound::SelectTrade`].
///
/// # Payment model
///
/// A full merchant menu places items into two payment slots and returns the
/// result through a third. That would require per-connection scratch storage
/// shaped like
/// [`PlayerInventory::workstation`], **except** a villager is not a
/// [`crate::block_entities::BlockEntity`] the way an anvil or a furnace is
/// (it is a [`crate::mobs::SimMob`]), so it has no `BlockPos` for
/// [`OpenContainer`]'s slot-sync machinery to key on — the sync loop that
/// makes every *other* menu in this crate live reads a block entity by
/// position. This helper therefore executes the trade in one step when a row
/// is selected and leaves the block-entity slot-sync path untouched.
///
/// # What that costs
///
/// The cost items are found and consumed from wherever they sit in the
/// standard 36-slot hotbar+main inventory ([`PlayerInventory::consume`]),
/// not from two manually-filled slots, so the player can trade without moving
/// items into dedicated payment slots.
///
/// `offer` is the caller's read-only priced peek at the villager's *live*,
/// persistent [`crate::villager_trade::VillagerTrades`] entry
/// ([`crate::mobs::MobSim::villager_offers`]). This function checks whether the
/// buyer's inventory can afford it; [`crate::mobs::MobSim::try_villager_trade`]
/// commits uses, demand, and experience only when that check succeeds.
///
/// Returns `None` — inventory untouched — when the player lacks the cost
/// items or has no room for the result; the inventory stays unchanged and no
/// excess item is dropped.
pub(super) fn attempt_villager_trade(
    inventory: &PlayerInventory,
    offer: &crate::villager_trade::OfferState,
) -> Option<PlayerInventory> {
    let trade = &offer.record;
    let mut trial = inventory.clone();
    // `offer.modified_cost_a_count()`, not `trade.wants_count`: the persistent
    // whole remaining scope was that a real reputation/Hero-of-the-Village
    // discount was computed and never reached a price — this is that price.
    trial.consume(
        trade.wants_item,
        u32::try_from(offer.modified_cost_a_count()).ok()?,
    )?;
    if let Some((item, count)) = trade.wants_b {
        trial.consume(item, u32::try_from(count).ok()?)?;
    }
    let gives = ItemStack::new(
        trade.gives_item.parse::<ResourceKey>().ok()?,
        u32::try_from(trade.gives_count).ok()?,
    );
    let (_, leftover) = trial.add(gives);
    if leftover.is_some() {
        return None;
    }
    Some(trial)
}

/// Opens a block-entity's container screen for this connection, mirroring
/// vanilla's own per-player "open menu" step end to end: a fresh container id
/// (its own container-counter field: `1..=100`, wrapping),
/// an `open_screen` send, then an immediate full `container_set_content`
/// plus every `container_set_data` property (its own "init menu" step's own
/// "add slot listener" call
/// triggers its own "broadcast full state" step the instant the menu is constructed).
///
/// `pos` must already hold a [`BlockEntity`] whose [`BlockEntity::menu_name`]
/// is `Some` — the caller ([`apply_use_item_on`]) checks this before calling
/// in. The `container_set_content` item list is this entity's own
/// [`BlockEntity::container_slots`] followed by the player's standard 27
/// main-storage + 9 hotbar slots (never armour/off-hand — see
/// `crate::inventory::ContainerMenuSlot`'s doc comment), with no
/// cursor/carried stack (this crate's [`PlayerInventory`] tracks no cursor
/// field at all — `docs/server-inventory.md`'s existing scope note, which
/// applies identically to any window).
#[allow(clippy::too_many_arguments)]
pub(super) async fn open_container_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    block_entities: &BlockEntityHandle,
    inventory: &PlayerInventory,
    pos: BlockPos,
    menu: &'static str,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;

    let (own_slots, data) = container_state(block_entities, pos);

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, menu, container_title(menu)),
    )
    .await?;

    // A lectern is a one-slot reader, not a generic container with a player
    // inventory tail.  The client-side menu has the same one-slot shape; if
    // the tail were appended the book would be offset and ordinary clicks
    // could reach inventory slots that the reader never displays.
    let mut items = own_slots.clone();
    if menu != "minecraft:lectern" {
        for native in 9..=35 {
            items.push(inventory.native(native).cloned());
        }
        for native in 0..=8 {
            items.push(inventory.native(native).cloned());
        }
    }

    let mut opened = OpenContainer {
        window_id,
        pos,
        // Every menu this function opens is a plain `generic_*` container
        // shape *except* the beacon, whose one payment slot has its own
        // restricted `may_place`/`max_stack_size` (`MenuKind::Beacon`'s own
        // doc) — everything else here keyed on the block entity's own
        // `menu_name()`, matching this function's one caller.
        shape: match menu {
            "minecraft:beacon" => MenuKind::Beacon,
            "minecraft:lectern" => MenuKind::Lectern,
            _ => MenuKind::Container { size: own_slots.len() },
        },
        container_size: own_slots.len(),
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(window_id, state_id, &items, None),
    )
    .await?;

    for (index, value) in data.iter().enumerate() {
        apply(
            conn,
            state,
            proto.encode_container_data(window_id, index as i32, *value),
        )
        .await?;
    }

    *open_container = Some(opened);
    *container_sync = ContainerSync {
        slots: own_slots,
        data,
    };
    Ok(())
}

/// Opens a crafting table's `minecraft:crafting` menu — the virtual-menu step, the
/// **positionless virtual menu**.
///
/// [`open_container_screen`] structurally cannot do this: it is driven entirely by
/// a [`BlockEntity`] at `pos`, and **a crafting table is not a block entity.** Its
/// slots are scratch space owned by the menu (the menu creates a virtual
/// crafting grid and result slot, then discards both on close), which here is
/// [`PlayerInventory::table_crafting`].
///
/// `pos` is still carried on the [`OpenContainer`] — not to find slots, but so
/// breaking the table closes the window, exactly as it already does for a furnace.
///
/// The 46 slots sent are the crafting menu's own order: result `0`, the 3×3 grid
/// `1..=9`, main storage `10..=36`, hotbar `37..=45`.
pub(super) async fn open_crafting_table_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;

    inventory.open_table_crafting();

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, "minecraft:crafting", "Crafting"),
    )
    .await?;

    let layout = MenuLayout::crafting_table();
    let items = read_menu(&layout, inventory, inventory.table_crafting(), &[]);

    let mut opened = OpenContainer {
        window_id,
        pos,
        shape: MenuKind::CraftingTable,
        // Result + the nine grid cells: the menu's own section, before the player
        // tail. Only `container_menu_slot`'s legacy callers read this; the click
        // path uses `shape`.
        container_size: 10,
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(
            window_id,
            state_id,
            &items,
            inventory.click_state().carried.as_ref(),
        ),
    )
    .await?;

    *open_container = Some(opened);
    // No background mutation to poll — a crafting grid changes only on a click —
    // so the periodic sync is left with nothing to diff.
    *container_sync = ContainerSync::default();
    Ok(())
}

/// The wire `menu_type` [`Station`] opens — `lodestone_game::menus::build_menu`'s
/// own dispatch table (`(Some("anvil"), 3)` etc.) is the client-side mirror of
/// this exact string.
pub(super) fn workstation_menu_type(station: Station) -> &'static str {
    match station {
        Station::Anvil => "minecraft:anvil",
        Station::Grindstone => "minecraft:grindstone",
        Station::Smithing => "minecraft:smithing",
        Station::Loom => "minecraft:loom",
        Station::Stonecutter => "minecraft:stonecutter",
    }
}

/// Opens an anvil/grindstone/smithing-table screen (the workstation menu dispatcher) — the
/// same *positionless virtual menu* shape [`open_crafting_table_screen`]
/// established for the crafting table, because none of these three is a block
/// entity either (see [`PlayerInventory::workstation`]'s own doc).
pub(super) async fn open_workstation_screen<T, P>(
    conn: &mut Connection<T>,
    proto: &P,
    state: &mut State,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
    station: Station,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;
    let layout = MenuLayout::item_combiner(station);
    let inputs = layout.len() - 36 - 1;

    inventory.open_workstation(inputs);

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, workstation_menu_type(station), container_title(workstation_menu_type(station))),
    )
    .await?;

    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let items = read_workstation_menu(&layout, inventory, &cells, station, false, hooks);

    let mut opened = OpenContainer {
        window_id,
        pos,
        shape: MenuKind::ItemCombiner { inputs, station },
        container_size: inputs + 1,
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(window_id, state_id, &items, inventory.click_state().carried.as_ref()),
    )
    .await?;

    *open_container = Some(opened);
    *container_sync = ContainerSync::default();
    Ok(())
}

/// Opens the enchanting-table screen: the same positionless
/// shape as [`open_workstation_screen`], but with no result slot — the item
/// slot is enchanted in place — so it carries its own [`MenuLayout`] and no
/// `Station`. Costs are computed once here from the empty menu (both slots
/// start empty, so all three costs are `0`) and then kept live by
/// [`apply_workstation_clicked`]... actually by the click path directly, since
/// `MenuKind::Enchanting` has no result to re-derive: see
/// `apply_enchanting_clicked`'s own doc for where the three
/// `container_set_data` costs are actually recomputed and sent.
pub(super) async fn open_enchanting_screen<T, P, S>(
    conn: &mut Connection<T>,
    proto: &P,
    source: &S,
    state: &mut State,
    inventory: &mut PlayerInventory,
    pos: BlockPos,
    next_window_id: &mut i32,
    open_container: &mut Option<OpenContainer>,
    container_sync: &mut ContainerSync,
    // Draw an enchantment seed from the connection's `[0, i32::MAX)` random
    // stream. `PlayerInventory::open_workstation` stores the value before the
    // first offer is computed, so menu offers receive a per-session seed.
    enchant_seed_roll: i64,
) -> Result<(), ServerError>
where
    T: Transport,
    P: ServerProtocol,
    S: ChunkSource + ?Sized,
{
    *next_window_id = *next_window_id % 100 + 1;
    let window_id = *next_window_id;
    let layout = MenuLayout::enchanting_table();

    inventory.open_workstation(2);
    inventory.set_enchant_seed(enchant_seed_roll);

    apply(
        conn,
        state,
        proto.encode_open_screen(window_id, "minecraft:enchantment", "Enchant"),
    )
    .await?;

    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let items: Vec<Option<ItemStack>> = layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Grid(cell) => cells.get(cell).cloned().flatten(),
            SlotKind::Container(_) | SlotKind::Result => None,
        })
        .collect();

    let mut opened = OpenContainer {
        window_id,
        pos,
        shape: MenuKind::Enchanting,
        container_size: 2,
        state_id: 0,
    };
    let state_id = opened.next_state_id();
    apply(
        conn,
        state,
        proto.encode_container_content(window_id, state_id, &items, inventory.click_state().carried.as_ref()),
    )
    .await?;
    // Vanilla's own enchantment-menu's own data-slot registrations: three costs, all `0` for an empty
    // menu — its own "get enchantment cost" getter is gated on a non-empty, enchantable item 0.
    for index in 0..3i32 {
        apply(conn, state, proto.encode_container_data(window_id, index, 0)).await?;
    }
    let _ = source; // bookshelf power is read on the first item placement, not at open time — see `apply_enchanting_clicked`.

    *open_container = Some(opened);
    *container_sync = ContainerSync::default();
    Ok(())
}
