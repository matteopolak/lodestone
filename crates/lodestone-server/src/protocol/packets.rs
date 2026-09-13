//! Version-free inbound packets and outbound connection directives.
//!
//! Packet ids and byte layouts stay in the version crates; these enums carry only the
//! values the shared server loop can consume or emit.

use lodestone_core::State;
use lodestone_model::{
    BlockActionKind, BlockFace, BlockPos, CommandBlockMode, Difficulty, GameMode, Hand,
    ItemStack, RecipeBookType, ResourceKey, ResourcePackResponseKind, Rotation, Vec3, Vec3f,
};
use uuid::Uuid;

/// A server-bound packet, lifted into the version-free vocabulary the server
/// loop understands.
///
/// The variants mirror the capability-gated login state machine: protocols with
/// a Configuration phase wait for the client's own
/// [`LoginAcknowledged`](Self::LoginAcknowledged) and
/// [`ConfigurationFinished`](Self::ConfigurationFinished) packets, while older
/// protocols transition directly to Play after login success because those
/// packets do not exist on their wire.
// `PartialEq` only, not `Eq`: `PlayerMoved`'s `f64` fields have no total
// order, so `f64: Eq` does not exist and a derived `Eq` cannot be added here.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ServerBound {
    /// The handshake selected a next connection state (Status or Login).
    Handshake {
        /// The state the client asked to move into.
        next_state: State,
    },
    /// Login start, carrying the requested username and the profile id the
    /// client presented.
    LoginStart {
        /// The username the client presented.
        username: String,
        /// The profile id the client presented (echoed back in the login
        /// success reply; a real auth-mode server would instead resolve this
        /// from the session server).
        uuid: Uuid,
    },
    /// The client asked for the server-list status (mirrors
    /// `ServerboundStatusRequestPacket`, whose body is *empty* —
    /// vanilla's own empty stream codec).
    ///
    /// This is the very first thing a real client sends after a handshake whose
    /// `next_state` was Status, i.e. when a player adds our server to their
    /// multiplayer list. The loop answers with
    /// [`ServerProtocol::encode_status_response`].
    StatusRequest,
    /// The client asked us to echo a clock reading so it can compute latency
    /// (mirrors `ServerboundPingRequestPacket`: a single big-endian `long`).
    ///
    /// Sent in the Status phase immediately after
    /// [`ServerBound::StatusRequest`]; the loop answers with
    /// [`ServerProtocol::encode_pong_response`] carrying `time` unchanged and
    /// then terminates the connection, exactly as vanilla's own
    /// status-phase ping-request handler does.
    PingRequest {
        /// The client's local clock reading, echoed back verbatim. Vanilla
        /// treats this as opaque — it is the *client* that subtracts it from
        /// its own clock — so the server must not reinterpret or clamp it.
        time: i64,
    },
    /// The reply to a server-originated `ping` control packet.
    ///
    /// Its fixed-width id is separate from the keep-alive challenge and has
    /// no matching server-side request state. The connection consumes it to
    /// preserve the protocol's no-output acknowledgement behaviour.
    Pong {
        /// The client-echoed control-ping id.
        id: i32,
    },
    /// The client acknowledged login success. This is the server-side signal
    /// to move the connection into [`State::Configuration`] and start sending
    /// configuration-phase directives, mirroring
    /// `ServerboundLoginAcknowledgedPacket`.
    LoginAcknowledged,
    /// The client's answer to a [`ServerDirective`]-carried encryption
    /// request: the RSA-encrypted shared secret and the
    /// RSA-encrypted echo of the server's verify token, mirroring
    /// `ServerboundKeyPacket`. Both fields are still ciphertext here — this
    /// variant is produced by [`ServerProtocol::decode`] with no crypto of
    /// its own, exactly like every other lift in this enum; the connection
    /// loop is what owns a private key and can do anything with these bytes.
    /// Only reachable in [`State::Login`], after the loop itself sent an
    /// encryption request — see `crate::server`'s handling for the ordering
    /// this depends on (the request and this response travel in the clear;
    /// everything after this must not).
    EncryptionResponse {
        /// RSA-encrypted (PKCS#1 v1.5) 16-byte AES shared secret.
        shared_secret: Vec<u8>,
        /// RSA-encrypted echo of the verify token the server sent.
        verify_token: Vec<u8>,
    },
    /// The client acknowledged the end of configuration. This is the
    /// server-side signal to move the connection into [`State::Play`] and
    /// begin the join sequence, mirroring
    /// `ServerboundFinishConfigurationPacket`.
    ConfigurationFinished,
    /// The client echoed a previously-sent keep-alive challenge (mirrors
    /// `ServerboundKeepAlivePacket`). `id` is the value that was echoed; the
    /// loop compares it against the challenge it is waiting on before
    /// treating the connection as alive again.
    KeepAlive {
        /// The challenge id the client echoed back.
        id: i64,
    },
    /// The client completed one local play tick.
    ///
    /// This empty marker closes the movement sample for the connection. The
    /// server uses it to clear inherited launch momentum after a tick that
    /// carried no player-position packet; otherwise a projectile fired later
    /// while standing still would retain the previous tick's velocity.
    ClientTickEnded,
    /// The client's absolute position changed (`move_player_pos` /
    /// `move_player_pos_rot` — the only two serverbound movement packets that
    /// carry a position). This drives chunk-cache-center/view-streaming
    /// updates (needs only `x`/`z`) and [`crate::fall::FallTracker`]
    /// (needs `y`/`on_ground`).
    ///
    /// `rotation` is `Some` only for `move_player_pos_rot`, which is the
    /// packet a client sends whenever position *and* look both changed in a
    /// tick — i.e. the overwhelmingly common case of a player walking while
    /// turning. The rotation is retained here because:
    /// a player who walks and turns never sends `move_player_rot` at all
    /// (vanilla's own client-side send-position routine picks exactly one of the four
    /// movement packets per tick), so handling only the rotation-*only*
    /// sibling would have left the common case frozen at yaw 0. `None` for
    /// `move_player_pos`, whose wire body genuinely has no angles — a
    /// distinction the consumer must keep, since "no angles in this sample"
    /// is not the same as "facing due south".
    PlayerMoved {
        /// New absolute x position, in blocks.
        x: f64,
        /// New absolute y position (feet), in blocks.
        y: f64,
        /// New absolute z position, in blocks.
        z: f64,
        /// New body/head rotation, when this sample carried one.
        rotation: Option<Rotation>,
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
    },
    /// The client's look changed but its position did not
    /// (`move_player_rot`), the packet a player standing still and turning on
    /// the spot sends every tick.
    ///
    /// This distinct variant lets a stationary avatar track where its player
    /// is looking instead of re-aiming only when the player also walks, so it
    /// is not redundant with [`PlayerMoved`](Self::PlayerMoved)'s `rotation`.
    PlayerRotated {
        /// New body/head yaw, in degrees.
        yaw: f32,
        /// New pitch, in degrees.
        pitch: f32,
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
    },
    /// Neither position nor look changed enough to be dirty, but the client's
    /// grounded/collision status flipped (`move_player_status_only`).
    ///
    /// Carries no pose data at all — the flags byte is the whole body. Its
    /// one consumer is [`crate::fall::FallTracker`]: this is the packet that
    /// reports the landing of a fall whose final sample had no net position
    /// change, the exact gap that type's own doc comment used to disclose.
    PlayerStatusOnly {
        /// Whether the client reports itself as grounded in this sample.
        on_ground: bool,
    },
    /// The player threw an item out of their hand — `Q` / `Ctrl+Q`, vanilla's
    /// `ServerboundPlayerActionPacket` ordinals `DROP_ITEM` (4) and
    /// `DROP_ALL_ITEMS` (3).
    ///
    /// **These used to decode to [`Ignored`](Self::Ignored)**, and the note on
    /// [`BlockAction`](Self::BlockAction) below said so — item handling was out of
    /// this crate's scope when that note was written. It no longer is (this crate
    /// owns [`PlayerInventory`](crate::PlayerInventory) and spawns item entities
    /// for block drops), so the ordinals now lift to their own variant. Pressing
    /// `Q` did nothing at all before, and no `_ =>` arm was to blame: the
    /// information was thrown away one layer earlier, at the decode.
    ItemDropped {
        /// `true` for `DROP_ALL_ITEMS` (`Ctrl+Q`, the whole selected stack),
        /// `false` for `DROP_ITEM` (one item) — vanilla's `all` argument to
        /// its own remove-from-selected routine.
        whole_stack: bool,
    },
    /// A block-breaking phase (`ServerboundPlayerActionPacket`'s
    /// `START_DESTROY_BLOCK`/`ABORT_DESTROY_BLOCK`/`STOP_DESTROY_BLOCK`
    /// ordinals). The two drop ordinals share the same wire packet and lift to
    /// [`ItemDropped`](Self::ItemDropped); release-use, swap-with-offhand and
    /// stab still decode to [`Ignored`](Self::Ignored).
    BlockAction {
        /// Which phase of the break this is.
        action: BlockActionKind,
        /// Target block position.
        pos: BlockPos,
        /// Face being mined. Decoded for parity with the wire packet; the
        /// current break handling does not use it (no per-face behaviour is
        /// modelled).
        face: BlockFace,
        /// Client block-prediction sequence number. Decoded but not yet
        /// acted on — this crate does not send
        /// `ClientboundBlockChangedAckPacket`; see `docs/block-edit.md`'s
        /// scope note.
        sequence: i32,
    },
    /// Right-click placement against a block face
    /// (`ServerboundUseItemOnPacket`).
    ///
    /// The clicked block and face determine the placement cell (see
    /// `crate::server`'s handling); `cursor` is vanilla's own
    /// block-hit-result location getter reduced to block-local coordinates, and
    /// is what decides a stair/slab/trapdoor's `half`.
    UseItemOn {
        /// The block face the client clicked.
        pos: BlockPos,
        /// Which face of `pos` was clicked.
        face: BlockFace,
        /// Block-local hit position within `pos`, each component `0.0`–`1.0`.
        /// `crate::block_placement` reads its `y` for the upper/lower-half
        /// decision every `Half`-bearing block makes.
        cursor: Vec3f,
        /// Client block-prediction sequence number (see
        /// [`BlockAction::sequence`](Self::BlockAction) for why it is
        /// decoded but not yet acted on).
        sequence: i32,
        /// `0` main hand, `1` off hand — vanilla's own interaction-hand enum ordinal.
        /// `crate::server`'s `apply_use_item_on` reads this to resolve which
        /// native inventory slot the spawn-egg/flint-and-steel/placement
        /// branches act on; same convention as [`UseItem::hand`](Self::UseItem).
        hand: u8,
    },
    /// The client asked to change its own game mode
    /// (`ServerboundChangeGameModePacket` — the F4 switcher a
    /// singleplayer/LAN host with cheats sends).
    ///
    /// The server stays authoritative: this is a *request*, and
    /// `crate::server` answers it by echoing the mode it actually applied
    /// through [`ServerProtocol::encode_game_mode`] plus
    /// [`encode_player_abilities`](ServerProtocol::encode_player_abilities), so
    /// a client that guessed wrong is corrected rather than trusted.
    ChangeGameMode {
        /// The requested mode.
        mode: GameMode,
    },
    /// The client sent a player-command packet
    /// (`ServerboundPlayerCommandPacket`). The packet's action
    /// ordinal is carried raw — the same shape `BlockAction` uses for its
    /// consumed ordinals — and only the one this crate has a consumer for,
    /// `STOP_SLEEPING` (`0`, the "wake up" a client sends when the player
    /// climbs out of bed or dies), is surfaced as a variant by the version
    /// crate's decoder; the other ordinals (sprinting/riding/jump states)
    /// decode to [`Ignored`](Self::Ignored).
    ///
    /// Note this packet deliberately carries **no** player identity: the wire
    /// `entityId` is always the sender's own local-player id (`1`), so the
    /// consumer must resolve who is waking up from the connection's own player
    /// id — see `crate::sleep::SleepVote` for why the key cannot come from the
    /// wire.
    PlayerCommand {
        /// Vanilla's own player-command-packet action ordinal sent by the
        /// client.
        action: i32,
    },
    /// The client requested a difficulty change
    /// (`ServerboundChangeDifficultyPacket`). This crate has no
    /// permission/operator model, so `crate::server`'s consumer always
    /// accepts it — see that consumer's own doc comment for the vanilla
    /// permission check this replaces and why.
    DifficultyChanged {
        /// The requested difficulty.
        difficulty: Difficulty,
    },
    /// The client requested locking/unlocking difficulty
    /// (`ServerboundLockDifficultyPacket`).
    DifficultyLockChanged {
        /// Whether difficulty should now be locked (further
        /// [`DifficultyChanged`](Self::DifficultyChanged) requests still
        /// decode and update the tracked value — vanilla does not reject a
        /// change while locked at the packet layer either; the lock is a UI
        /// affordance in the vanilla client, not a server-side veto).
        locked: bool,
    },
    /// The client requested one or more game-rule value changes
    /// (`ServerboundSetGameRulePacket`). Each entry is `(rule
    /// key, raw string value)`, exactly as sent — this crate has no
    /// `GameRules` registry to validate a key or parse a value's real type
    /// against, so nothing here rejects an unknown key or a malformed value
    /// (vanilla itself just logs a warning and skips the entry; see
    /// `crate::server`'s consumer).
    GameRuleChanged {
        /// `(rule key, raw value)` pairs, in wire order.
        entries: Vec<(String, String)>,
    },
    /// The client selected a new hotbar slot
    /// (`ServerboundSetCarriedItemPacket`). Mirrors vanilla's own
    /// set-carried-item handler, which writes
    /// straight into vanilla's own per-player inventory's selected-slot setter, with
    /// **no confirmation packet** — see `crate::inventory::PlayerInventory
    /// ::set_selected_hotbar_slot`'s consumer in `crate::server` for why
    /// nothing is sent back here either.
    CarriedItemChanged {
        /// The newly selected hotbar slot. The protocol decoder validates
        /// `0..HOTBAR_SIZE` before producing this variant (mirroring
        /// vanilla's own is-hotbar-slot guard); an out-of-range wire value decodes to
        /// [`Ignored`](Self::Ignored) instead.
        slot: u8,
    },
    /// A container click (`ServerboundContainerClickPacket`).
    ///
    /// **The button input is what the consumer acts on.** `slot`, `button` and
    /// `click_type` are the raw click; `crate::container_click::do_click`
    /// re-derives the whole menu state from them, exactly as vanilla's own
    /// container-menu do-click routine does. That replaces the earlier scope cut
    /// in which the client's own `changed_slots` prediction was applied verbatim
    /// — a hole through which any client could name any item in any slot.
    ///
    /// `changed_slots` and `carried_item` are still carried, because the wire
    /// packet has them and they are the client's post-click prediction (see
    /// `docs/container-clicks.md`): the consumer compares them against what it
    /// derived, purely to decide whether a correcting `container_set_content` is
    /// worth sending. Nothing is ever *stored* from them.
    ContainerClicked {
        /// The window the click targeted — `0` for the player's own inventory
        /// screen, otherwise the id the server handed out in `open_screen`.
        window_id: i32,
        /// The client's menu state id at the time of the click. Decoded for
        /// parity with the wire packet; not yet validated against the server's own
        /// (`OpenContainer::state_id`), which would let the server *reject* a click
        /// raced against a correction rather than merely overwrite its result.
        state_id: i32,
        /// The clicked menu slot. `-999`
        /// ([`SLOT_OUTSIDE`](crate::container_click::SLOT_OUTSIDE)) is vanilla's
        /// "outside the window", which drops the cursor into the world.
        slot: i32,
        /// `buttonNum`: the mouse button for a pickup/quick-move, the hotbar index
        /// (or `40` for the off-hand) for a swap, or the drag header/type mask for
        /// a quick-craft.
        button: i8,
        /// `ContainerInput`'s ordinal — `0` pickup, `1` quick-move, `2` swap,
        /// `3` clone, `4` throw, `5` quick-craft, `6` pickup-all.
        click_type: i32,
        /// The client's predicted per-slot result. **Never stored** — see this
        /// variant's own doc comment.
        changed_slots: Vec<(i32, Option<ItemStack>)>,
        /// The client's predicted cursor stack. **Never stored**, for the same
        /// reason; the server tracks its own cursor in
        /// [`ClickState`](crate::container_click::ClickState).
        carried_item: Option<ItemStack>,
    },
    /// The client clicked a recipe in the recipe book, asking the server to lay it
    /// out in the open crafting grid (`ServerboundPlaceRecipePacket`).
    ///
    /// **`recipe_index` is an opaque id the *server* assigns**, not a name: vanilla
    /// sends the whole book with `ClientboundRecipeBookAddPacket` and the client
    /// echoes back a position in that list. See
    /// [`crate::crafting::recipe_at_index`] for the id space this crate defines and
    /// for the consequence — nothing sends this packet until that clientbound half
    /// exists.
    RecipePlaced {
        /// The window the recipe should be laid into.
        window_id: i32,
        /// Vanilla's own recipe-display-id index.
        recipe_index: i32,
        /// `useMaxItems` — shift-clicking the recipe, which fills as many rounds as
        /// the inventory allows.
        use_max_items: bool,
    },
    /// The player changed one recipe-book tab's open/filter state. The server
    /// keeps this per connection so the setting is available to the player's
    /// recipe-book state without confusing it with the crafting grid.
    RecipeBookSettingsChanged {
        /// Which recipe-book tab changed.
        book_type: RecipeBookType,
        /// Whether the tab is open.
        open: bool,
        /// Whether the tab shows only craftable recipes.
        filtering: bool,
    },
    /// The client has displayed one recipe-book entry and cleared its "new"
    /// highlight. The display id is validated against the book this connection
    /// received before the server folds it into
    /// [`crate::inventory::PlayerInventory`].
    RecipeBookRecipeSeen {
        /// The per-session display id from the server's recipe-book add packet.
        recipe_index: i32,
    },
    /// The client opened an advancement tab, or closed the advancements
    /// screen. The connection keeps this selection and republishes it through
    /// [`ServerProtocol::encode_select_advancements_tab`] so the client-side
    /// screen and the server-owned session state stay in agreement.
    SeenAdvancements {
        /// The selected tab's resource location, or `None` after the screen
        /// closed.
        tab: Option<String>,
    },
    /// The client reported the outcome of a server-pushed resource pack.
    /// The connection loop records this in the shared resource-pack feed so a
    /// host can decide what the report means without coupling the protocol
    /// decoder to a policy such as requiring acceptance.
    ResourcePackResponse {
        /// Id of the resource pack this response concerns.
        id: Uuid,
        /// Outcome reported by the client.
        response: ResourcePackResponseKind,
    },
    /// The client completed its initial placement and is ready for movement
    /// dependent simulation.
    PlayerLoaded,
    /// The client applied a server-issued position correction and echoed its
    /// teleport id. The connection accepts movement again only when this id
    /// matches its latest outstanding correction.
    TeleportationAccepted {
        /// The id copied from the clientbound player-position packet.
        id: i32,
    },
    /// The client changed its flight toggle; permission remains server-owned.
    PlayerAbilitiesChanged {
        /// Requested current flight state.
        flying: bool,
    },
    /// Requests the authoritative block-entity data for operator inspection.
    BlockEntityTagQuery {
        /// Correlation id echoed in the response.
        transaction_id: i32,
        /// Position in the player's current dimension.
        pos: BlockPos,
    },
    /// Requests the authoritative save data for a live entity.
    EntityTagQuery {
        /// Correlation id echoed in the response.
        transaction_id: i32,
        /// Connection-visible entity id, not an entity-type registry id.
        entity_id: i32,
    },
    /// The client closed a container screen (`ServerboundContainerClosePacket`).
    /// `window_id` is the id the client had open — vanilla's
    /// `ServerPlayer::doCloseContainer` compares this against nothing at all
    /// (it just closes whatever `containerMenu` currently is); this crate's
    /// consumer instead compares it against the connection's own tracked open
    /// window before clearing it, so a stale close for an already-replaced
    /// window cannot clobber a newer one. See `crate::server`'s consumer.
    ContainerClosed {
        /// The window id the client reports closing.
        window_id: i32,
    },
    /// The client attacked an entity with its currently held item
    /// (`ServerboundAttackPacket`). 26.2 split this out of the old
    /// combined interact packet — the wire body carries only the target
    /// entity id, no hand/location/secondary-action data (see this variant's
    /// consumer, `crate::server::apply_attack`, for the damage/knockback
    /// pipeline this drives). The generic `minecraft:interact` packet
    /// (`ServerboundInteractPacket`) is the *other* half and has its own
    /// variant, [`InteractEntity`](Self::InteractEntity): 26.2 split the old
    /// combined packet in two, and the two halves reach different consumers
    /// here — attack goes to the damage pipeline, interact to
    /// `crate::mobs::MobSim::interact`.
    Attack {
        /// Target entity id.
        entity_id: i32,
    },
    /// A player right-clicked an entity (`ServerboundInteractPacket`) — the
    /// taming, feeding, sitting and breeding trigger.
    ///
    /// `using_secondary_action` is the packet's trailing boolean (the shift
    /// modifier), carried rather than dropped because vanilla's own
    /// `mobInteract` chain consults it — vanilla's own abstract-horse mob-interact routine's
    /// `isTamed() && player.isSecondaryUseActive()` opens the inventory instead
    /// of mounting. Nothing reads it yet; it is on the wire and dropping it
    /// would have to be undone.
    ///
    /// The low-precision `Vec3` location the packet also carries is **not**
    /// here: vanilla uses it only for the `INTERACT_AT` sub-action (clicking a
    /// specific part of an armour stand), which this crate has no model for.
    InteractEntity {
        /// Target entity id.
        entity_id: i32,
        /// `InteractionHand` ordinal: `0` = main hand, `1` = off hand.
        hand: i32,
        /// Whether the client was sneaking.
        using_secondary_action: bool,
    },
    /// The player began using the item in `hand` in mid-air
    /// (`ServerboundUseItemPacket`).
    ///
    /// This is the *start* of a use, not a completed action, and the difference
    /// matters: an instant throwable (snowball, egg, ender pearl) is released by
    /// vanilla's `use` the moment the packet arrives, while a bow starts a draw
    /// whose length the **server** counts and which ends with a separate
    /// [`ReleaseUseItem`](Self::ReleaseUseItem). One packet, two behaviours,
    /// decided by what is in the hand — see `crate::server`'s `apply_use_item`.
    ///
    /// `ServerboundUseItemPacket` also carries the client's yaw/pitch, which is
    /// what makes a launch direction available without this crate tracking
    /// rotation for every connection: a throw needs the facing *at the instant of
    /// the throw*, and the last `PlayerRotated` packet is not necessarily that.
    UseItem {
        /// `0` main hand, `1` off hand.
        hand: u8,
        /// Yaw in degrees, as the client reported it with the use.
        yaw: f32,
        /// Pitch in degrees.
        pitch: f32,
    },
    /// The player let go of a right-click they had been holding
    /// (`ServerboundPlayerActionPacket`'s `RELEASE_USE_ITEM` ordinal, `5`).
    ///
    /// Vanilla's bow fires from here, not from the `USE_ITEM` that started the
    /// draw, and the arrow's power comes from how long the two were apart —
    /// vanilla's own bow-item power-for-time routine. That interval is counted in **server ticks**, so
    /// the consumer reads `MobSim::tick_count` rather than a wall clock: this crate
    /// links into a wasm32 bundle where `Instant::now()` compiles and then panics
    /// at runtime with no log line.
    ReleaseUseItem,
    /// The `F`-key hand swap (`ServerboundPlayerActionPacket`'s
    /// `SWAP_ITEM_WITH_OFFHAND` ordinal, `6`) — see `crate::server`'s own
    /// `ServerBound::SwapItemInHand` dispatch arm for the consumer.
    ///
    /// Vanilla's own player-action handler's `SWAP_ITEM_WITH_OFFHAND`
    /// arm swaps `getItemInHand(MAIN_HAND)` with `getItemInHand(OFF_HAND)` and calls
    /// `stopUsingItem()`; the stop-using half is not modelled here (this crate has
    /// no in-progress "using item" state to cancel — see [`ReleaseUseItem`](Self::ReleaseUseItem)'s
    /// own doc comment for the one piece of that state this crate does track, which
    /// a hand swap does not touch). Unlike `CreativeModeSlotSet`/`RenameItem`, the
    /// client applies **no local prediction** for this swap (`crates/lodestone-shell`'s
    /// `SwapItemWithOffhand` action only ever encodes the packet), so the corrected
    /// `container_set_slot` pair the consumer sends is not a *correction* — it is
    /// the only place either slot's new contents ever reaches the client at all.
    SwapItemInHand,
    /// The client reporting where the vehicle it rides has got to
    /// (`ServerboundMoveVehiclePacket`), once per tick while mounted.
    ///
    /// **This is not a request — it is authoritative.**
    /// Vanilla's own base-entity is-client-authoritative check delegates to the controlling passenger and
    /// its own player-level is-client-authoritative check returns `true`, so the server's own
    /// `travelRidden` takes the `setDeltaMovement(Vec3.ZERO)` branch and its only
    /// job is to accept this and relay it. A server that also simulated the boat
    /// would fight the player.
    ///
    /// The packet carries no entity id: vanilla resolves the target as
    /// `player.getRootVehicle()` and rejects the packet outright when that is the
    /// player themselves. [`crate::mobs::MobSim::apply_vehicle_move`] is the
    /// consumer and applies the same rule, which is what stops a connection moving
    /// a boat it is not sitting in.
    ///
    /// The two rejections vanilla *can* answer with (moved too quickly, moved
    /// wrongly — both followed by `vehicle.absSnapTo(old…)` and a clientbound
    /// `MOVE_VEHICLE`) are not implemented, so no correction is ever sent. Stated
    /// because the client already handles one if it arrives
    /// (`lodestone_ecs::vehicle::apply_vehicle_moved`).
    VehicleMoved {
        /// The vehicle's position as the client simulated it.
        position: Vec3,
        /// Its yaw in degrees.
        yaw: f32,
        /// Its pitch in degrees. A boat never changes it; a land mount takes half
        /// its rider's, so it is carried rather than dropped.
        pitch: f32,
    },
    /// A spectator clicking a player's name in the tab list
    /// (`ServerboundTeleportToEntityPacket`), asking to be moved to that
    /// player's position. Vanilla's own teleport-to-entity handler
    /// resolves the uuid against every
    /// loaded level's entities and teleports on the first hit, gated on
    /// `player.isSpectator()`.
    ///
    /// This crate's consumer (`crate::server`'s dispatch arm) narrows the
    /// search to connected players only — `PlayerRegistry::candidates` is the
    /// one uuid-keyed, position-carrying source this crate has; there is no
    /// uuid index for non-player entities (`MobSim` resolves mobs by integer
    /// id), so a spectator targeting a mob's uuid is a disclosed gap rather
    /// than a silent one. The target's own facing is not carried through
    /// either — `PlayerCandidate` has no rotation field — so the teleport
    /// keeps the spectator's current yaw/pitch rather than matching vanilla's
    /// `entity.getYRot()/getXRot()`.
    TeleportToEntity {
        /// The uuid of the entity to teleport to.
        uuid: Uuid,
    },
    /// The client swung its arm (`ServerboundSwingPacket`). Vanilla's own
    /// entity swing routine (called from `handleAnimate`) broadcasts a
    /// `ClientboundAnimatePacket` to every player *tracking* the swinger —
    /// **not** back to the swinger itself, which already plays the animation
    /// locally the instant it sends this packet.
    ///
    /// This crate has no per-connection tracking-radius model, so the
    /// consumer (`crate::server`'s dispatch arm, via
    /// [`crate::players::PlayerRegistry::swing`]) narrows "tracking players"
    /// to "every other connected player", the same narrowing chat and
    /// position already make. Singleplayer has nobody else to broadcast to,
    /// so it is silently a no-op there, matching the `Chat` variant's own
    /// documented posture.
    Swing {
        /// Hand whose swing the server should broadcast.
        hand: Hand,
    },
    /// A spectator clicking a nearby entity to attach their camera to it
    /// (`ServerboundSpectatorActionPacket`). Vanilla's `handleSpectatorAction`
    /// gates on `player.isSpectator()`, resolves the wire's network entity id
    /// against the current level, checks the world border and a 3-block
    /// interaction range, and — if `target.isPickable()` — calls
    /// `this.player.setCamera(target)`, which sends
    /// `ClientboundSetCameraPacket`.
    ///
    /// This crate's consumer (`crate::server`'s dispatch arm) resolves the id
    /// against both id-keyed sources it has — `MobHandle::position` for a mob,
    /// `PlayerRegistry::candidates` for a player — and applies the same
    /// gating this crate can express: spectator mode, and a plain
    /// centre-to-centre distance check (no per-entity bounding box exists
    /// here, so this narrows vanilla's box-aware range check) rather than
    /// `isPickable`, which this crate has no model for. There is no
    /// server-side "current camera" state and therefore no reset path: a real
    /// client resets its own rendered viewpoint locally (sneaking out of
    /// spectator-camera mode sends no packet in vanilla either), so a single
    /// one-shot `SET_CAMERA` is the whole of vanilla's own server-side
    /// contribution to this feature.
    SpectatorAction {
        /// The network entity id the client wants to attach its camera to, or
        /// `None` for the wire's "no target" encoding. Vanilla's own handler
        /// does nothing at all for `None` (there is no reset-to-self branch),
        /// so this crate's consumer mirrors that rather than inventing one.
        target_entity_id: Option<i32>,
    },
    /// The client's movement-input flags for the current tick
    /// (`ServerboundPlayerInputPacket`). Three of the seven flags are
    /// threaded through: `sprint` is half of vanilla's melee knockback-bonus
    /// gate (vanilla's own attack routine's `isSprinting() && fullStrengthAttack` — see
    /// `crate::server::apply_attack`'s own doc comment for the other half,
    /// which this crate cannot track), `shift` drives vanilla's own
    /// per-player ride-tick dismount check (`wantsToStopRiding()` is
    /// `isShiftKeyDown()`, tested every tick a passenger is aboard — see
    /// `crate::server`'s `PlayerInput` consumer for why reacting to each
    /// received packet already reproduces that edge, given this packet's own
    /// producer only sends on change), and `jump` is vanilla's own camel
    /// on-player-jump routine's
    /// whole trigger — see `crate::server`'s `PlayerInput` consumer for the
    /// same "a received packet already is the edge" reasoning `shift`
    /// documents, applied to a mounted camel's dash instead of a dismount.
    /// The other four flags (forward/backward/left/right) are decoded off
    /// the wire by [`ServerProtocol::decode`] but not threaded through here
    /// — nothing in this crate's server-authoritative model needs them yet,
    /// the same "decode what the loop needs, not the whole packet"
    /// convention [`PlayerMoved`](Self::PlayerMoved)'s own doc comment
    /// already establishes for its two fields.
    PlayerInput {
        /// Whether the client reports itself as sprinting this tick.
        sprint: bool,
        /// Whether the client reports itself as sneaking this tick —
        /// vanilla's `wantsToStopRiding()` input.
        shift: bool,
        /// Whether the client reports itself as jumping this tick —
        /// vanilla's own camel on-player-jump routine's trigger for a mounted camel's dash.
        jump: bool,
    },
    /// A creative-mode inventory slot write predicted locally by the client
    /// (`ServerboundSetCreativeModeSlotPacket`). Uses the exact
    /// same menu-slot numbering [`ContainerClicked`](Self::ContainerClicked)
    /// does — see
    /// [`PlayerInventory::apply_menu_slot_change`](crate::inventory::PlayerInventory::apply_menu_slot_change)'s
    /// own doc comment for the table — because vanilla's
    /// `handleSetCreativeModeSlot` writes through the identical
    /// `player.inventoryMenu.getSlot(slotNum)` indexing. This crate has no
    /// creative-mode/game-mode model to gate on (`hasInfiniteMaterials()` in
    /// vanilla), matching the permission-check omission
    /// [`DifficultyChanged`](Self::DifficultyChanged)'s own doc comment
    /// already documents for this crate's singleplayer-only shape.
    CreativeModeSlotSet {
        /// Wire slot index. Vanilla only ever writes for `1..=45`
        /// (its own `validSlot` check); `0`
        /// (crafting output) and negative values (vanilla's "drop into the
        /// world" case, `packet.slotNum() < 0`) are decoded but never
        /// recognised by
        /// [`apply_menu_slot_change`](crate::inventory::PlayerInventory::apply_menu_slot_change) —
        /// this crate has no world-drop model, the same scope cut
        /// [`BlockAction`](Self::BlockAction)'s own doc comment already
        /// makes for the item-drop action ordinals.
        slot: i16,
        /// The item now in that slot, or `None` to clear it.
        item: Option<ItemStack>,
    },
    /// The client sent a `client_command`
    /// (`ServerboundClientCommandPacket`). `action` is vanilla's
    /// `Action` ordinal, straight off the wire: `0` = perform respawn, `1` =
    /// request stats (no stats model exists in this crate — see
    /// `crate::server`'s consumer), `2` = request current game-rule values
    /// (mirrors `sendGameRuleValues`, answered from the same
    /// [`WorldAdminState`](crate::server) already built).
    ClientCommand {
        /// Action ordinal, straight off the wire.
        action: i32,
    },
    /// The client changed a setting after joining
    /// (`ServerboundClientInformationPacket`). Most fields are
    /// cosmetic (locale, chat visibility, skin parts, main hand) and this
    /// crate has nothing that reads any of them; `view_distance` is the one
    /// exception — the server uses `view_distance` to honour the client's
    /// requested tracking radius, subject to its own configured cap. Matches
    /// [`PlayerInput`](Self::PlayerInput)'s "decode what the loop needs, not
    /// the whole packet" convention.
    ClientInformationChanged {
        /// Requested render distance in chunks. Vanilla only ever sends
        /// `2..=32`; `crate::server`'s consumer clamps against the server's
        /// own configured view radius either way, so an out-of-range value
        /// degrades rather than misbehaves.
        view_distance: i8,
    },
    /// The client acknowledged one chunk batch
    /// (`ServerboundChunkBatchReceivedPacket`) — vanilla's
    /// `PlayerChunkSender` flow control, which allows at most one
    /// unacknowledged batch in flight at a time
    /// (`ServerProtocol`'s own trait doc comment already states this
    /// contract for the *initial* join batch; this variant lets
    /// `crate::server` honour it for every later view-streaming batch too,
    /// so each batch acknowledgement gates the next batch.
    ChunkBatchAcknowledged {
        /// The client's requested chunks-per-tick delivery rate. Decoded for
        /// parity with the wire packet but not yet used to pace *within* a
        /// batch — see `crate::server`'s consumer for the one invariant this
        /// crate does enforce (never starting a second batch before the
        /// first is acked) and this field's own future scope.
        desired_chunks_per_tick: f32,
    },
    /// The client ran a command (`ServerboundChatCommandPacket`).
    ///
    /// `command` is the text **without** its leading `/` — that is the wire
    /// format, not a normalisation we apply: vanilla's own packet carries it
    /// stripped (`crates/protocol/v770/src/packets/game.rs`'s `ChatCommand`
    /// documents the same layout from the client-encode side).
    ///
    /// This crate cannot execute it. The Brigadier registry plugins register
    /// into lives in `lodestone-ecs`, which this crate deliberately does not
    /// depend on, so `crate::server` hands this to the host through
    /// [`CommandDispatch`](crate::CommandDispatch) and turns the answer into
    /// [`encode_system_chat`](ServerProtocol::encode_system_chat) directives.
    /// See `crate::command`'s module doc for the whole argument, including
    /// why the two rejected alternatives were rejected.
    ///
    /// Both the unsigned `chat_command` and the signed `chat_command_signed`
    /// produce this — the latter's per-argument `ArgumentSignatures` are
    /// decoded and then dropped rather than verified, since a client only
    /// sends the signed form for arguments the server's `COMMANDS` tree
    /// declared **signable**, and this server declares none (sending a tree
    /// via [`encode_commands`](ServerProtocol::encode_commands) does not
    /// change that: the tree carries no signability). So there is nothing for
    /// per-argument verification to gate here — unlike
    /// [`Chat`](Self::Chat)'s whole-message signature, which
    /// `crate::chat_session::decide` does verify against the sender's
    /// announced session, `chat_command_signed`'s signatures have no
    /// declared-signable argument to be *for*, and the command text itself is
    /// executed identically either way.
    ChatCommand {
        /// Command text without the leading `/`.
        command: String,
    },
    /// A player typed an ordinary chat message (`minecraft:chat`).
    ///
    /// The sibling of [`ChatCommand`](Self::ChatCommand), and the half that
    /// was missing entirely: the outbound direction
    /// ([`encode_system_chat`](ServerProtocol::encode_system_chat),
    /// clientbound `system_chat`) has always been complete and well-tested,
    /// so grepping for "chat" found a finished feature and hid the fact that
    /// a player could not say anything to us at all.
    ///
    /// # Signature and timestamp/salt now survive decoding; the acknowledgement does not
    ///
    /// The wire packet also carries a last-seen acknowledgement block
    /// (`ServerboundChatPacket`, 26.2) — a varint offset, a fixed 20-bit bit
    /// set and a checksum byte. That part is decoded (the layout has to be
    /// read to find the end of the frame) and then still **dropped**, for the
    /// same reason it always was: the sequence counter belongs to whoever
    /// drives the connection, and a second writer forks it (see
    /// `ChatAckInfo`'s unreachability from the WASM plugin ABI,
    /// `lodestone-wasm-host`'s `abi.rs`, for the same shape). This crate also
    /// still never sends a signed `player_chat` (see "Chat is therefore
    /// broadcast unsigned" below), so a real client's own outgoing
    /// last-seen window stays permanently empty regardless — there is
    /// nothing yet for the acknowledgement to carry.
    ///
    /// `timestamp`/`salt`/`signature` **do** now survive, because
    /// `crate::chat_session::decide` needs them to verify a message against
    /// the sender's announced [`ChatSessionAnnounced`](Self::ChatSessionAnnounced)
    /// session. See that module's own doc for exactly what this verifies and
    /// what it does not (in particular: no Mojang-provenance check on the
    /// announced key itself).
    ///
    /// Chat is still **broadcast unsigned to every peer**, as a `system_chat`
    /// component rendered in vanilla's own `chat.type.text` (`"<%s> %s"`)
    /// form, rather than as a real `player_chat` packet — verification only
    /// gates whether *this server* accepts the message, it does not let any
    /// other client verify it too. Emitting real `player_chat` is a separate,
    /// larger piece of work; see `docs/player-chat.md`.
    Chat {
        /// The message text exactly as the player typed it, capped at 256
        /// characters by the wire format.
        message: String,
        /// Client-reported timestamp, epoch milliseconds
        /// (vanilla's own chat-packet timestamp field). Part of the signed payload —
        /// see [`crate::chat_session::decide`].
        timestamp_millis: i64,
        /// Random salt used for signing (`0` for unsigned chat).
        salt: i64,
        /// The 256-byte signature, when the client sent one. `None` for
        /// unsigned chat, or when this player has never announced a session
        /// at all.
        signature: Option<[u8; 256]>,
    },
    /// A client announced (or re-announced) its chat-signing session
    /// (`minecraft:chat_session_update`) —
    /// `ServerboundChatSessionUpdatePacket` → vanilla's own remote-chat-session data record.
    ///
    /// `crate::chat_session::ServerChatSession::new` is this variant's one
    /// consumer: it replaces whatever session this connection had announced
    /// before (resetting the verification chain to index 0, mirroring
    /// vanilla's own `resetPlayerChatState` swapping the whole
    /// `signedMessageDecoder` rather than repairing one) — see that type's
    /// own doc for what is and is not checked about it.
    ChatSessionAnnounced {
        /// The client-generated session UUID every signed message's chain is
        /// rooted at (vanilla's own signed-message-link session-id field).
        session_id: Uuid,
        /// Profile public key expiry, epoch milliseconds.
        expires_at_millis: i64,
        /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key, verbatim
        /// off the wire.
        public_key: Vec<u8>,
        /// Mojang's signature over `public_key` (`publicKeySignatureV2`),
        /// carried but never checked against Mojang's own Services key — see
        /// `crate::chat_session`'s module doc for why.
        key_signature: Vec<u8>,
    },
    /// A tab-completion request (`ServerboundCommandSuggestionPacket`, the
    /// wire half that `ChatCommand` alone does not cover).
    ///
    /// `command` is the **whole input line, including the leading `/`** — that
    /// is the wire format (`crates/protocol/v770/src/packets/game.rs`'s
    /// `CommandSuggestion` struct documents the same layout from the
    /// client-encode side), unlike [`ChatCommand`](Self::ChatCommand), which
    /// has the slash stripped by the *different* packet that carries it.
    /// `crate::server`'s consumer strips it before consulting
    /// [`crate::ServerCommands::suggest`], mirroring vanilla's own
    /// custom-command-suggestions handler, which
    /// skips exactly one leading `/` off its `StringReader` before parsing.
    CommandSuggestion {
        /// Transaction id, echoed back verbatim in the response so the client
        /// can discard a reply to a request it has since abandoned.
        id: i32,
        /// The full input line as typed, including the leading `/`.
        command: String,
    },
    /// A custom plugin-message payload from the client
    /// (`ServerboundCustomPayloadPacket`) — the version-free
    /// lowering of the packet, exactly as it crossed the wire: a namespaced
    /// channel identifier plus the channel's raw bytes.
    ///
    /// Two channels are *interpreted* by the loop rather than dispatched —
    /// `minecraft:register` / `minecraft:unregister` update which channels this
    /// connection supports (see [`crate::ClientChannels`]) — and every other
    /// channel is looked up in the server's
    /// [`PluginChannelRegistry`](crate::PluginChannelRegistry) and delivered to
    /// whatever registered interest owns it, or silently dropped when none
    /// does, exactly vanilla's `DiscardedPayload` fallback.
    CustomPayload {
        /// The namespaced channel identifier (e.g. `minecraft:brand`).
        channel: ResourceKey,
        /// The channel-specific payload bytes, verbatim.
        data: Vec<u8>,
    },
    /// The client typed a new name into an open anvil's name field
    /// (`ServerboundRenameItemPacket`). Vanilla's own handler
    /// (its own rename-item handler) reads this only when
    /// `player.containerMenu instanceof AnvilMenu` — see `crate::server`'s
    /// consumer for that same gate. The text is carried raw; filtering and the
    /// 50-character cap are vanilla's own anvil-menu item-name setter's own `validateName`,
    /// ported to [`crate::anvil::validate_rename`] rather than done here, so a
    /// rejected rename is indistinguishable from one this crate chose not to
    /// decode.
    RenameItem {
        /// The client-typed text, unfiltered.
        name: String,
    },
    /// Middle-click (`keyPickItem`) aimed at a block
    /// (`ServerboundPickItemFromBlockPacket`). Vanilla's client sends this
    /// unconditionally on every pick against a block hit — there is no
    /// client-side prediction of which of the three pick-block outcomes
    /// applies (vanilla's own client-side pick-block-or-entity routine →
    /// its own multiplayer-game-mode pick-item-from-block handler does nothing but forward the packet); the
    /// whole hotbar-select/swap/create decision is server-authoritative, in
    /// vanilla's own pick-item-from-block handler →
    /// `tryPickItem`. See `crate::server`'s consumer for that three-way split.
    PickItemFromBlock {
        /// The targeted block position.
        pos: BlockPos,
        /// `hasControlDown()` at the time of the pick — copy the block's data
        /// (block-entity contents/NBT) onto the resulting stack. Gated
        /// server-side on infinite materials, not decoded away here: `packet
        /// .includeData() && this.player.hasInfiniteMaterials()` is
        /// vanilla's own AND, and only the consumer has the game-mode half.
        include_data: bool,
    },
    /// Middle-click aimed at an entity
    /// (`ServerboundPickItemFromEntityPacket`) — same shape as
    /// [`PickItemFromBlock`](Self::PickItemFromBlock), aimed at
    /// `entity.getPickResult()` instead of a block's clone stack.
    PickItemFromEntity {
        /// The targeted entity's network id.
        entity_id: i32,
        /// `hasControlDown()`. For an entity this also gates
        /// vanilla's own fetch-profile-command print-for-avatar routine on a game-master avatar in
        /// vanilla — not modelled here (no game-master command channel), so
        /// this crate's consumer uses it only for the block-entity NBT case,
        /// which does not apply to an entity target at all; carried for wire
        /// fidelity and symmetry with the block variant.
        include_data: bool,
    },
    /// The client pressed a data-driven button in an open menu
    /// (`ServerboundContainerButtonClickPacket`). Only `EnchantmentMenu` reads
    /// this in vanilla (its own container-button-click handler
    /// → its own container-menu click-menu-button routine) — every other menu's
    /// `clickMenuButton` override is the default `false`. `button_id` is
    /// vanilla's own enchantment-menu click-menu-button routine's **slot index** (`0..3`), not a cost;
    /// the cost is that slot's own entry in the table's three
    /// `container_set_data` properties, re-derived server-side rather than
    /// trusted from the client.
    ContainerButtonClick {
        /// The window the client believes is open — vanilla compares this
        /// against `player.containerMenu.containerId` before doing anything.
        window_id: i32,
        /// Which of the three enchantment offers was chosen.
        button_id: i32,
    },
    /// The client toggled a crafter slot's enabled/disabled state
    /// (`ServerboundContainerSlotStateChangedPacket`).
    /// Vanilla's own container-slot-state-changed handler:
    /// `this.player.containerMenu instanceof CrafterMenu crafterMenu &&
    /// crafterMenu.getContainer() instanceof CrafterBlockEntity
    /// crafterBlockEntity` — refused for every other open menu, which this
    /// crate's consumer reproduces by checking `open_container`'s own tracked
    /// menu shape (`crate::container_click::MenuKind`) rather than trusting
    /// `window_id` alone.
    ContainerSlotStateChanged {
        /// The window the client believes is open, matching
        /// [`ContainerButtonClick`](Self::ContainerButtonClick)'s own
        /// `window_id`.
        window_id: i32,
        /// Which of the crafter's 9 grid slots (`CrafterMenu`'s own `x + y *
        /// 3` addressing — the same order `BlockEntity::Crafter`'s own
        /// `slots` field uses).
        slot_id: i32,
        /// `true` to enable, `false` to disable.
        new_state: bool,
    },
    /// The command-block GUI's "Done" button
    /// (`ServerboundSetCommandBlockPacket`) — the command-block GUI update.
    /// `crate::server`'s consumer is `ServerGamePacketListenerImpl
    /// .handleSetCommandBlock`: swap the block to the requested mode
    /// (preserving `FACING`), write `conditional`, then update the entity's
    /// command/track-output/"Always Active" fields — see
    /// `crate::command_block`'s own module doc for exactly how far that
    /// consumer goes and what still needs a real redstone signal instead of
    /// this packet.
    SetCommandBlock {
        /// The target command block's position.
        pos: BlockPos,
        /// The command text to store, unfiltered.
        command: String,
        /// Validated command-block execution mode.
        mode: CommandBlockMode,
        /// `COMMAND_BLOCK_FLAG_TRACK_OUTPUT`.
        track_output: bool,
        /// `COMMAND_BLOCK_FLAG_CONDITIONAL`.
        conditional: bool,
        /// `COMMAND_BLOCK_FLAG_AUTOMATIC` — the "Always Active" toggle.
        automatic: bool,
    },
    /// A sign's text-edit submission (`ServerboundSignUpdatePacket`).
    /// `crate::block_entities::apply_sign_update` is the
    /// consumer: it re-checks vanilla's own sign-block-entity update-sign-text gate
    /// (not waxed, and `editor` is the uuid `openTextEdit` granted) before
    /// writing either side's four lines.
    SignUpdate {
        /// The target sign's block position.
        pos: BlockPos,
        /// Whether the front (vs. back) text is being edited.
        is_front_text: bool,
        /// The sign's four text lines, wire-shape only (raw, unstripped) —
        /// matching every other packet-shaped variant's convention of doing
        /// no interpretation in the protocol crate. `crate::server`'s
        /// consumer runs `crate::block_entities::strip_sign_formatting` on
        /// each line before anything else, matching `handleSignUpdate`'s own
        /// `ChatFormatting::stripFormatting` map.
        lines: [String; 4],
    },
    /// A beacon's power-selection submission (`ServerboundSetBeaconPacket`,
    /// update). `crate::server`'s consumer is
    /// vanilla's own beacon-menu update-effects routine: re-derive the pyramid tier, validate the
    /// pair with `crate::beacon::validate_beacon_effects`, and on success
    /// consume one payment item and resync the menu's data slots.
    SetBeacon {
        /// The chosen primary power's canonical key, or `None` to clear it.
        primary: Option<String>,
        /// The chosen secondary power's canonical key (level-4 pyramids
        /// only), or `None`.
        secondary: Option<String>,
    },
    /// A book-and-quill draft save or signing submission
    /// (`ServerboundEditBookPacket`). Carries no
    /// `ItemStack` — the packet only names a slot and the new text;
    /// `crate::server`'s consumer looks the carried book up in the tracked
    /// `PlayerInventory` itself, mirroring vanilla's own edit-book handler's own
    /// `this.player.getInventory().getItem(slot)`.
    EditBook {
        /// The native inventory slot holding the book — a hotbar index
        /// (`0..9`) or the off-hand (`40`); every other value is refused the
        /// same way vanilla's own `isHotbarSlot(slot) || slot ==
        /// 40` gate refuses it.
        slot: i32,
        /// Draft or final page text, wire-shape only (raw, unfiltered) —
        /// this crate runs no chat-filtering service, matching every other
        /// text-carrying variant's convention.
        pages: Vec<String>,
        /// `Some(title)` when this is a signing submission (transmute to
        /// `minecraft:written_book`); `None` for an ordinary draft save.
        title: Option<String>,
    },
    /// A merchant trade-row selection (`ServerboundSelectTradePacket`).
    /// Carries no window id — vanilla's own consumer,
    /// its own select-trade handler, checks only that
    /// `player.containerMenu instanceof MerchantMenu`, so `crate::server`'s
    /// consumer resolves the villager from this connection's own tracked
    /// open-merchant state rather than from anything on the wire.
    SelectTrade {
        /// Index into the open merchant's accumulated offer list
        /// (`crate::mobs::villager::trades::offers_up_to`'s own order).
        index: i32,
    },
    /// A rider's paddle input (`ServerboundPaddleBoatPacket`). `crate::server`'s
    /// consumer resolves the
    /// vehicle from the reporting connection's own `player_entity_id`
    /// (`MobSim::apply_boat_paddle`), the same "the wire carries no window/
    /// vehicle id, the connection supplies it" shape [`SelectTrade`] already
    /// uses.
    PaddleBoat {
        /// Left paddle in use.
        left: bool,
        /// Right paddle in use.
        right: bool,
    },
    /// A bundle-tooltip highlight claim sent by the client.
    /// The ordinary bundle-contents decoder has no selected-item value and
    /// reconstructs `-1`, so this is the *only* place the index appears. The
    /// server-side extraction path reads it to decide which nested item a
    /// right-click extracts. `crate::server`'s consumer
    /// stores it in the tracked `PlayerInventory` (`set_selected_bundle_item`)
    /// for `container_click::pickup`'s next click to read.
    SelectBundleItem {
        /// The menu slot holding the bundle, matching `container_click`'s own
        /// slot indices — the packet carries no window id, so this is
        /// interpreted against whichever menu is currently open, exactly as
        /// `SelectTrade` above is.
        slot_id: i32,
        /// Highlighted stack's index within the bundle, or `-1` for none.
        selected_item_index: i32,
    },
    /// A packet the loop does not need to act on (teleport confirmations,
    /// look-only or status-only movement, and several other decoded-but-
    /// unmodelled families — see `crates/protocol/v770/src/server_protocol.rs`'s
    /// own arm comments for exactly which). The loop ignores these but stays
    /// connected.
    Ignored,
}

/// A side effect the [`ServerProtocol`] asks the connection layer to perform,
/// mirroring the client-side `Directive`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerDirective {
    /// Write a client-bound packet with this protocol-specific id and body.
    Send {
        /// Protocol-specific packet id.
        packet_id: i32,
        /// Encoded packet body.
        payload: Vec<u8>,
    },
    /// Move the connection to a new state (applied after preceding sends).
    SetState(State),
    /// Enable or reconfigure zlib compression (negative disables).
    SetCompression(i32),
    /// Enable the AES-128-CFB8 stream cipher on this connection using the
    /// given 16-byte shared secret, mirroring
    /// `Connection::enable_encryption` on the client side of the same
    /// handshake. **Ordering is load-bearing, the same hazard
    /// `SetCompression` documents for itself**: the connection layer applies
    /// directives strictly in order and reads the codec's encryption state at
    /// the moment it writes, so this must be applied *after* any cleartext
    /// send it follows and *before* anything meant to travel encrypted — in
    /// practice, always immediately after nothing (there is nothing to send
    /// in reply to `EncryptionResponse` itself; the very next directive is
    /// whatever finishes login).
    EnableEncryption(Vec<u8>),
    /// No side effect — the scalar analog of returning an empty directive list.
    /// Used by the default entity encoders so a protocol without entity support
    /// emits nothing rather than a bogus packet; the connection layer skips it.
    None,
}
