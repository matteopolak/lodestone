use uuid::Uuid;

use crate::{
    common::{Difficulty, GameMode, Hand},
    ids::ResourceKey,
    item::ItemStack,
    math::{BlockPos, Rotation, Vec3, Vec3f},
};

/// Things the client wants to do before a version adapter lowers them into a
/// concrete packet.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum ClientAction {
    /// Send a chat message.
    SendChat {
        /// Chat text without a leading command slash.
        text: String,
    },
    /// Send a command.
    SendCommand {
        /// Command text without a leading slash.
        command: String,
    },
    /// Acknowledge signed chat messages without sending a chat body.
    ChatAck {
        /// Number of signed messages added to the last-seen window since the
        /// previous acknowledgement.
        offset: i32,
    },
    /// Send a cryptographically signed chat message (vanilla's secure-chat
    /// `chat` packet, carrying a real `SHA256withRSA` signature and the
    /// client's last-seen acknowledgement window) rather than the always-
    /// unsigned [`ClientAction::SendChat`].
    ///
    /// A driver holding a live signing session produces this itself from a
    /// plain [`ClientAction::SendChat`] it receives — the signature must
    /// cover the *current* last-seen chain, which only the driver tracks, so
    /// this is not meant to be constructed by an application directly.
    SendSignedChat {
        /// Chat text without a leading command slash.
        text: String,
        /// Client timestamp, epoch **milliseconds** — the wire's own unit
        /// (`ChatMessage.timestamp`/`writeInstant`). Deliberately not the
        /// epoch-**seconds** value the signature payload itself is computed
        /// over (`SignedMessageBody.updateSignature`'s
        /// `timeStamp.getEpochSecond()`): carrying only the millisecond form
        /// here and deriving seconds from it at the signing call site
        /// removes the chance of the two drifting apart.
        timestamp_millis: i64,
        /// Random per-message salt, part of the signed payload.
        salt: i64,
        /// The 256-byte `SHA256withRSA` signature over this message
        /// (`lodestone_auth::build_signature_payload`'s output, signed).
        signature: Vec<u8>,
        /// Offset of the last-seen acknowledgement window this same packet
        /// carries — vanilla piggybacks the ack update on every chat send,
        /// signed or not.
        last_seen_offset: i32,
        /// Fixed 20-bit acknowledged bit set, packed into 3 bytes.
        acknowledged: [u8; 3],
        /// Acknowledgement checksum (`0` means "ignore checksum").
        checksum: i8,
    },
    /// Announce (or re-announce) this client's chat-signing session to the
    /// server (`chat_session_update`) — required before the server accepts
    /// any [`ClientAction::SendSignedChat`] this session sends.
    AnnounceChatSession {
        /// This client's session UUID (client-generated once per session).
        session_id: Uuid,
        /// Profile public key expiry, epoch milliseconds.
        expires_at_millis: i64,
        /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key,
        /// verbatim from the key-issuing service.
        public_key: Vec<u8>,
        /// The issuing service's signature over `public_key`, forwarded
        /// verbatim — never verified by this client, only by servers.
        key_signature: Vec<u8>,
    },
    /// Send player movement.
    ///
    /// Carries the same two boolean status bits vanilla's
    /// `ServerboundMovePlayerPacket` family packs into its flags byte:
    /// `on_ground` and `horizontal_collision`. Both are simulation outputs —
    /// the caller's physics step decides them — never something a version
    /// adapter should derive on its own. A version adapter that sends
    /// movement at vanilla's own cadence chooses *which* concrete packet
    /// (position-only, rotation-only, status-only, or both) to emit from the
    /// deltas between successive `Move` actions; it does not change what this
    /// variant carries.
    Move {
        /// Player position.
        pos: Vec3,
        /// Player rotation.
        rotation: Rotation,
        /// Whether the player is on the ground.
        on_ground: bool,
        /// Whether the player collided horizontally this tick.
        horizontal_collision: bool,
    },
    /// Respond to a keep-alive challenge.
    KeepAliveResponse {
        /// Keep-alive id.
        id: i64,
    },
    /// Request respawn.
    Respawn,
    /// Swing an arm.
    SwingArm {
        /// Arm to swing.
        hand: Hand,
    },
    /// Start, abort, or finish breaking a block.
    ///
    /// `sequence` is the client's block-prediction sequence number. Modern
    /// servers echo it when acknowledging or rolling back a predicted block
    /// change, so adapters must not synthesize or drop it.
    BlockAction {
        /// Break action.
        action: BlockActionKind,
        /// Target block position.
        pos: BlockPos,
        /// Face being mined.
        face: BlockFace,
        /// Client prediction sequence number.
        sequence: i32,
    },
    /// Drop one item from the selected stack.
    DropSelectedItem,
    /// Drop the entire selected stack.
    DropSelectedItemStack,
    /// Swap the main-hand and off-hand stacks.
    SwapItemWithOffhand,
    /// Release a charged/held item use, such as a bow draw.
    ReleaseUseItem,
    /// Perform the modern piercing-weapon stab action with the selected item.
    Stab,
    /// Use the held item on a block face.
    ///
    /// `cursor` is the hit location relative to the target block, in block-local
    /// coordinates. `sequence` is the block-prediction sequence number.
    UseItemOn {
        /// Hand containing the item.
        hand: Hand,
        /// Block being targeted.
        pos: BlockPos,
        /// Face being targeted.
        face: BlockFace,
        /// Block-local hit position.
        cursor: Vec3f,
        /// Whether the hit starts inside a block.
        inside_block: bool,
        /// Client prediction sequence number.
        sequence: i32,
    },
    /// Use the held item in air.
    UseItem {
        /// Hand containing the item.
        hand: Hand,
        /// Player rotation at the time of use.
        rotation: Rotation,
        /// Client prediction sequence number.
        sequence: i32,
    },
    /// Interact with an entity.
    InteractEntity {
        /// Target entity id.
        entity_id: i32,
        /// Interaction kind.
        interaction: EntityInteraction,
        /// Whether the player is using the secondary action modifier.
        sneaking: bool,
    },
    /// Click inside an open container.
    ContainerClick {
        /// Open container/window id.
        window_id: i32,
        /// Server-synchronised menu state id.
        state_id: i32,
        /// Menu slot index, or the outside-slot sentinel used by the menu model.
        slot: i32,
        /// Button value whose meaning depends on `click_type`.
        button: i32,
        /// Container click mode.
        click_type: ContainerClickType,
        /// Client-predicted slot contents after the click.
        changed_slots: Vec<ContainerSlotChange>,
        /// Client-predicted carried cursor stack after the click.
        carried_item: Option<ItemStack>,
    },
    /// Close an open container.
    ContainerClose {
        /// Open container/window id.
        window_id: i32,
    },
    /// Change the selected hotbar slot.
    SetCarriedItem {
        /// Hotbar slot index.
        slot: i32,
    },
    /// Set or drop a creative-mode inventory slot.
    SetCreativeModeSlot {
        /// Menu slot index, or a negative value to drop the stack.
        slot: i32,
        /// New stack for the slot. `None` clears it.
        item: Option<ItemStack>,
    },
    /// Send the current movement-input bitset.
    SetPlayerInput(PlayerInput),
    /// Send a player command that is not expressible as continuous input.
    PlayerCommand {
        /// Player entity id the command applies to.
        entity_id: i32,
        /// Command payload.
        command: PlayerCommand,
    },
    /// Disconnect from the server.
    Disconnect,
    /// Send or update client display/locale settings.
    ///
    /// Vanilla clients send this once at join (with the connecting client's
    /// live settings) and again whenever the player changes an option.
    SetClientSettings(ClientSettings),
    /// Announce the client's brand on the `minecraft:brand` plugin channel.
    ///
    /// Vanilla clients send this once, right after entering the configuration
    /// or play state.
    SendBrand {
        /// Free-form client brand string, such as `vanilla`.
        brand: String,
    },
    /// Send an arbitrary plugin message on `channel`.
    ///
    /// [`ClientAction::SendBrand`] is vanilla's one built-in use of
    /// `custom_payload`; this is the general case for a mod/plugin-aware
    /// client that wants to talk on a channel of its own. Valid in the
    /// Configuration and Play states, matching where `custom_payload` itself
    /// exists on the wire.
    SendCustomPayload {
        /// Plugin channel identifier.
        channel: ResourceKey,
        /// Raw payload bytes, opaque to this crate.
        data: Vec<u8>,
    },
    /// Reply to a server-initiated ping challenge (distinct from keep-alive).
    PongResponse {
        /// Id echoed back from the corresponding ping challenge.
        id: i32,
    },
    /// Respond to a server-pushed resource pack.
    ResourcePackResponse {
        /// Id of the resource pack this response concerns.
        id: Uuid,
        /// Outcome being reported.
        response: ResourcePackResponseKind,
    },
    /// Mark the end of the client's local tick.
    ///
    /// Vanilla clients send this once per tick after their movement packet, so
    /// the server can align world ticking with the client's tick boundary.
    EndClientTick,
    /// Click a button in an open container (such as an enchanting-table slot
    /// or lectern page turn) that is not a slot click.
    ContainerButtonClick {
        /// Open container/window id.
        window_id: i32,
        /// Button id defined by the open menu type.
        button_id: i32,
    },
    /// Toggle the client's flight state (creative/spectator double-jump).
    SetFlying {
        /// Whether the client is now flying.
        flying: bool,
    },
    /// Rename an item in an open anvil.
    RenameItem {
        /// New item name.
        name: String,
    },
    /// Select a villager/wandering-trader trade offer by index.
    SelectTrade {
        /// Index into the open merchant's offer list.
        index: i32,
    },
    /// Request the item that would be picked from a targeted block (middle
    /// click), to be placed in the hotbar.
    PickItemFromBlock {
        /// Targeted block position.
        pos: BlockPos,
        /// Whether to include the block entity's data in the picked stack.
        include_data: bool,
    },
    /// Request the item that would be picked from a targeted entity (middle
    /// click), to be placed in the hotbar.
    PickItemFromEntity {
        /// Targeted entity id.
        entity_id: i32,
        /// Whether to include the entity's data in the picked stack.
        include_data: bool,
    },
    /// Confirm the primary/secondary power selection in an open beacon.
    SetBeaconEffects {
        /// Chosen primary effect, or `None` to clear it.
        primary: Option<ResourceKey>,
        /// Chosen secondary effect, or `None` to clear it.
        secondary: Option<ResourceKey>,
    },
    /// Submit the pages (and, if publishing, the title) of a written book being
    /// edited in a lectern or writable-book slot.
    EditBook {
        /// Slot holding the book being edited.
        slot: i32,
        /// Page contents, in order.
        pages: Vec<String>,
        /// Title to publish under, if the player is signing (not just saving a
        /// draft).
        title: Option<String>,
    },
    /// Submit the text of a sign being edited.
    SignUpdate {
        /// Target sign's block position.
        pos: BlockPos,
        /// Whether the front (vs. back) text is being edited.
        is_front_text: bool,
        /// The sign's four text lines.
        lines: [String; 4],
    },
    /// Program an open command block.
    SetCommandBlock {
        /// Target command-block position.
        pos: BlockPos,
        /// Command text to run.
        command: String,
        /// Execution mode.
        mode: CommandBlockMode,
        /// Whether the block's last-output line is tracked.
        track_output: bool,
        /// Whether the block is conditional on the block behind it.
        conditional: bool,
        /// Whether the block runs automatically every tick rather than on
        /// redstone power.
        automatic: bool,
    },
    /// Tell the server this client has finished loading the world and is
    /// ready to have its movement validated.
    ///
    /// Vanilla's server seeds a ~60-tick (~3 s) `clientLoadedTimeoutTimer`
    /// after join/respawn and silently ignores movement packets until it
    /// elapses, *unless* the client sends this to zero it early
    /// (`ServerGamePacketListenerImpl.hasClientLoaded()`). Sent once per
    /// join/respawn, as soon as the client is actually ready to be moved.
    PlayerLoaded,
    /// Report which advancement tab is open, or that the advancements screen
    /// was closed.
    SeenAdvancements {
        /// The opened tab's id, or `None` if the advancements screen was
        /// closed.
        tab: Option<ResourceKey>,
    },
    /// Request tab-completion suggestions for a partially typed command.
    CommandSuggestion {
        /// Transaction id echoed back in the server's suggestions response.
        id: i32,
        /// The command text typed so far, including the leading slash.
        command: String,
    },
    /// Apply paddle input to a boat the player is riding.
    PaddleBoat {
        /// Whether the left paddle is being used.
        left: bool,
        /// Whether the right paddle is being used.
        right: bool,
    },
    /// Report the locally authoritative position of the vehicle the player
    /// is riding, once per tick while mounted (vanilla's
    /// `LocalPlayer.tick()` passenger branch).
    MoveVehicle {
        /// Vehicle's absolute position.
        pos: Vec3,
        /// Vehicle's rotation.
        rotation: Rotation,
        /// Whether the vehicle is on the ground.
        on_ground: bool,
    },
    /// Select which stack inside a bundle tooltip is highlighted.
    SelectBundleItem {
        /// Slot id holding the bundle.
        slot_id: i32,
        /// Highlighted stack's index within the bundle, or `-1` for none.
        selected_item_index: i32,
    },
    /// Toggle a container slot's on/off state (e.g. a crafter's disabled
    /// slots).
    SetContainerSlotState {
        /// Slot index within the container.
        slot_id: i32,
        /// Open container id.
        container_id: i32,
        /// New enabled/disabled state.
        new_state: bool,
    },
    /// Change a recipe book's open/filtering settings.
    SetRecipeBookSettings {
        /// Which recipe book this applies to.
        book_type: RecipeBookType,
        /// Whether the book is open.
        open: bool,
        /// Whether the "only craftable" filter is active.
        filtering: bool,
    },
    /// Mark a recipe as seen, clearing its "new" highlight in the recipe
    /// book.
    RecipeBookSeenRecipe {
        /// The recipe's display index.
        recipe: i32,
    },
    /// Click a recipe book entry to auto-place its ingredients into an open
    /// crafting container.
    PlaceRecipe {
        /// Open container id.
        container_id: i32,
        /// The recipe's display index.
        recipe: i32,
        /// Whether to place the maximum possible quantity rather than one
        /// set of ingredients.
        use_max_items: bool,
    },
    /// Client-initiated round-trip latency probe, sent periodically during
    /// play by the F3 debug overlay's network graph (vanilla's
    /// `PingDebugMonitor`), independent of the server-initiated
    /// [`ClientAction::PongResponse`] reply.
    PingRequest {
        /// Client's local clock reading in milliseconds, echoed back by the
        /// server so round-trip time can be computed.
        time: i64,
    },
    /// While spectating, select which entity (if any) the spectator's
    /// third-person view should follow.
    SpectatorAction {
        /// Id of the entity to spectate, or `None` to stop following one.
        target_entity_id: Option<i32>,
    },
    /// While spectating, teleport to an entity's position by uuid (e.g.
    /// clicking a player in the tab list or team overlay).
    TeleportToEntity {
        /// Uuid of the entity to teleport to.
        target: Uuid,
    },
    /// Request a game-mode change, e.g. via the singleplayer/LAN F4
    /// cheats-enabled game-mode switcher. The server is authoritative and
    /// may ignore this if the requester lacks permission.
    ChangeGameMode {
        /// Requested game mode.
        mode: GameMode,
    },
    /// Reply to a [`crate::event::ClientEvent::CookieRequested`].
    ///
    /// Vanilla's own client (`ClientCommonPacketListenerImpl.handleRequestCookie`)
    /// answers immediately from its local `serverCookies` map with no UI and no
    /// player input — `payload` is `None` when the client has never received a
    /// [`crate::event::ClientEvent::CookieStored`] for this `key`, which the wire
    /// carries as a nullable byte array rather than an error. Present in the
    /// Login, Configuration and Play states alike (`minecraft:cookie_response`
    /// is a `ServerCookiePacketListener` packet, shared by all three).
    CookieResponse {
        /// Cookie key, echoed from the matching
        /// [`crate::event::ClientEvent::CookieRequested`].
        key: ResourceKey,
        /// The previously stored cookie payload, or `None` if this client has
        /// none for `key`.
        payload: Option<Vec<u8>>,
    },

    // ---- the operator/debug serverbound set ---------------------------------
    //
    // Thirteen packets a vanilla client can send that we could not encode at
    // all. They divide by *producer*, and the division is the useful part of
    // this block:
    //
    // * three have a real producer in the tree today — `SubscribeDebug` (paired
    //   with the `debug_*` clientbound values it turns on),
    //   `CustomClickAction` (the reply to `show_dialog`), and the two tag
    //   queries (vanilla's F3+I "copy NBT" debug verb, driven from
    //   `lodestone_ecs::debug_query`);
    // * `ChangeDifficulty` / `LockDifficulty` are the singleplayer difficulty
    //   control, wired through `lodestone_ecs::session`;
    // * the rest — structure block, jigsaw block, test block, test instance,
    //   command minecart, game rules — belong to **creative-mode editor screens
    //   that do not exist yet**. Their encoders are here so the screen is the
    //   only thing missing when someone builds it; `docs/serverbound-coverage.md`
    //   names each one and its screen. Adding an encoder without saying which
    //   of these three buckets it is in is how `SetFlying` shipped with four
    //   encoders and zero producers.
    /// Ask the server for a block entity's NBT (`block_entity_tag_query`).
    ///
    /// Operator-only server-side. Vanilla's producer is the F3+I debug verb.
    QueryBlockEntityTag {
        /// Transaction id echoed back on `tag_query`.
        transaction_id: i32,
        /// The block entity's position.
        pos: BlockPos,
    },
    /// Ask the server for an entity's NBT (`entity_tag_query`).
    QueryEntityTag {
        /// Transaction id echoed back on `tag_query`.
        transaction_id: i32,
        /// The entity's network id.
        entity_id: i32,
    },
    /// Request a world difficulty change (`change_difficulty`).
    ///
    /// Only honoured in singleplayer / on a LAN world the requester hosts;
    /// a dedicated server ignores it unless the difficulty is unlocked.
    ChangeDifficulty {
        /// Requested difficulty.
        difficulty: Difficulty,
    },
    /// Lock or unlock the world difficulty (`lock_difficulty`).
    ///
    /// Vanilla's own button is one-way — locking is permanent — but the wire
    /// carries a boolean, so this does too.
    LockDifficulty {
        /// Whether the difficulty should be locked.
        locked: bool,
    },
    /// Set one or more game rules (`set_game_rule`).
    ///
    /// Values are the rule's **string** form on the wire regardless of the
    /// rule's type: `"true"` for a boolean, `"7"` for an integer. The server
    /// parses against its own typed registry, so an unparseable value is
    /// ignored server-side rather than rejected here.
    SetGameRules {
        /// `(rule key, value)` pairs, applied in order.
        entries: Vec<(ResourceKey, String)>,
    },
    /// Set a command minecart's command (`set_command_minecart`).
    SetCommandMinecart {
        /// Network id of the minecart.
        entity_id: i32,
        /// Command text.
        command: String,
        /// Whether the last output should be tracked for display.
        track_output: bool,
    },
    /// Submit a structure block's configuration (`set_structure_block`).
    ///
    /// `offset` and `size` are **signed bytes** on the wire, not a `Vec3i`:
    /// vanilla clamps `offset` to `-48..=48` and `size` to `0..=48` on read, so
    /// a value outside that is silently narrowed rather than refused.
    SetStructureBlock {
        /// The structure block's position.
        pos: BlockPos,
        /// Which button was pressed.
        update_type: StructureBlockUpdateType,
        /// The block's mode.
        mode: StructureBlockMode,
        /// Structure name.
        name: String,
        /// Relative offset of the captured region, `-48..=48` per axis.
        offset: (i8, i8, i8),
        /// Size of the captured region, `0..=48` per axis.
        size: (i8, i8, i8),
        /// Mirroring applied on load.
        mirror: StructureMirror,
        /// Rotation applied on load.
        rotation: StructureRotation,
        /// Free-form data-mode marker string.
        data: String,
        /// Load integrity, `0.0..=1.0`.
        integrity: f32,
        /// Integrity RNG seed.
        seed: i64,
        /// Whether entities in the region are skipped.
        ignore_entities: bool,
        /// Whether air blocks are rendered in the preview.
        show_air: bool,
        /// Whether the bounding box is rendered.
        show_bounding_box: bool,
        /// Whether loading is strict about block-entity data.
        strict: bool,
    },
    /// Submit a jigsaw block's configuration (`set_jigsaw_block`).
    SetJigsawBlock {
        /// The jigsaw block's position.
        pos: BlockPos,
        /// This jigsaw's own name.
        name: ResourceKey,
        /// The name this jigsaw wants to connect to.
        target: ResourceKey,
        /// The template pool to draw from.
        pool: ResourceKey,
        /// Block state string applied when the jigsaw is consumed.
        final_state: String,
        /// Joint type. Serialized as a **string**, not an ordinal — see
        /// [`JigsawJoint`].
        joint: JigsawJoint,
        /// Selection priority.
        selection_priority: i32,
        /// Placement priority.
        placement_priority: i32,
    },
    /// Press a jigsaw block's "Generate" button (`jigsaw_generate`).
    GenerateJigsawStructure {
        /// The jigsaw block's position.
        pos: BlockPos,
        /// How many levels of pieces to place.
        levels: i32,
        /// Whether the jigsaw blocks themselves are kept.
        keep_jigsaws: bool,
    },
    /// Submit a test block's configuration (`set_test_block`).
    SetTestBlock {
        /// The test block's position.
        pos: BlockPos,
        /// The block's mode.
        mode: TestBlockMode,
        /// Message shown on log/fail.
        message: String,
    },
    /// Act on a test instance block (`test_instance_block_action`).
    TestInstanceBlockAction {
        /// The test instance block's position.
        pos: BlockPos,
        /// Which button was pressed.
        action: TestInstanceAction,
        /// The block's current configuration, echoed back with the action.
        data: TestInstanceData,
    },
    /// Subscribe to a set of server debug feeds (`debug_subscription_request`).
    ///
    /// This is the **producer half** of the `debug_block_value`,
    /// `debug_chunk_value`, `debug_entity_value` and `debug_event` clientbound
    /// packets: the server sends none of them until a client asks. Sending an
    /// empty list unsubscribes from everything, which is how vanilla's debug
    /// renderer turns a feed off. Capped at 32 entries by the wire.
    ///
    /// Keys are `minecraft:debug_subscription` registry identifiers such as
    /// `minecraft:entity_paths`; the adapter resolves them to network ids and
    /// **drops any it does not recognise** rather than failing the whole
    /// subscription.
    SubscribeDebug {
        /// Feeds to subscribe to, replacing the previous set.
        subscriptions: Vec<ResourceKey>,
    },
    /// Report a dialog button press back to the server (`custom_click_action`).
    ///
    /// The reply half of `show_dialog`: a `custom` dialog action carries an id
    /// and an optional NBT payload, and this is how the client returns them.
    /// `payload` is the already-encoded network-NBT body (including the
    /// leading present/absent byte), opaque to this crate.
    CustomClickAction {
        /// The action id declared by the dialog.
        id: ResourceKey,
        /// Encoded optional-NBT payload, at most 65536 bytes.
        payload: Vec<u8>,
    },
}

/// A test instance block's configuration, as `set_test_instance_block`'s
/// `TestInstanceBlockEntity.Data` carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestInstanceData {
    /// The test to run, or `None` for an unconfigured block.
    pub test: Option<ResourceKey>,
    /// Region size in blocks.
    pub size: (i32, i32, i32),
    /// Rotation applied to the placed structure.
    pub rotation: StructureRotation,
    /// Whether entities are ignored.
    pub ignore_entities: bool,
    /// Last run status.
    pub status: TestInstanceStatus,
    /// Failure message, if the last run failed. Carried as the already-encoded
    /// network-NBT `Component` body, opaque to this crate for the same reason
    /// every other `Text`-on-the-wire field is.
    pub error_message: Option<Vec<u8>>,
}

/// Which button of the structure block screen was pressed
/// (`StructureBlockEntity.UpdateType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructureBlockUpdateType {
    /// Just save the settings.
    UpdateData,
    /// Save the region to a structure file.
    SaveArea,
    /// Load the named structure.
    LoadArea,
    /// Detect the region bounds from corner blocks.
    ScanArea,
}

/// A structure block's mode (`StructureMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructureBlockMode {
    /// Save mode.
    Save,
    /// Load mode.
    Load,
    /// Corner marker.
    Corner,
    /// Data marker.
    Data,
}

/// Mirroring applied when a structure is placed (`Mirror`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructureMirror {
    /// No mirroring.
    None,
    /// Mirror across the left-right axis.
    LeftRight,
    /// Mirror across the front-back axis.
    FrontBack,
}

/// Rotation applied when a structure is placed (`Rotation`).
///
/// Named `Structure*` because [`crate::math::Rotation`] already means a
/// yaw/pitch pair; this is the four-way block rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructureRotation {
    /// No rotation.
    None,
    /// 90 degrees clockwise.
    Clockwise90,
    /// 180 degrees.
    Clockwise180,
    /// 90 degrees counter-clockwise.
    CounterClockwise90,
}

/// A jigsaw block's joint type (`JigsawBlockEntity.JointType`).
///
/// **Serialized as its lowercase name, not as an ordinal** —
/// `ServerboundSetJigsawBlockPacket` writes `joint.getSerializedName()` and the
/// server falls back to [`Self::Aligned`] for anything it does not recognise.
/// That is the one field of this packet a transliterated "everything is a
/// VarInt enum" encoder would get wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JigsawJoint {
    /// `"aligned"`.
    Aligned,
    /// `"rollable"`.
    Rollable,
}

impl JigsawJoint {
    /// The wire name.
    #[must_use]
    pub const fn serialized_name(self) -> &'static str {
        match self {
            Self::Aligned => "aligned",
            Self::Rollable => "rollable",
        }
    }
}

/// A test block's mode (`TestBlockMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestBlockMode {
    /// Starts the test.
    Start,
    /// Logs a message.
    Log,
    /// Fails the test.
    Fail,
    /// Accepts (passes) the test.
    Accept,
}

/// Which button of the test instance block screen was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestInstanceAction {
    /// Initialise the region.
    Init,
    /// Query the current status.
    Query,
    /// Apply the submitted settings.
    Set,
    /// Reset the region.
    Reset,
    /// Save the region.
    Save,
    /// Export the region.
    Export,
    /// Run the test.
    Run,
}

/// A test instance block's last-run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TestInstanceStatus {
    /// Never run, or cleared.
    Cleared,
    /// Currently running.
    Running,
    /// Finished, successfully or not.
    Finished,
}

/// Which recipe book a recipe-book action applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecipeBookType {
    /// The crafting-table recipe book.
    Crafting,
    /// The furnace recipe book.
    Furnace,
    /// The blast furnace recipe book.
    BlastFurnace,
    /// The smoker recipe book.
    Smoker,
}

/// Client display and locale settings, sent at join and whenever changed.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientSettings {
    /// Client locale, such as `en_us`.
    pub locale: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility preference.
    pub chat_mode: ChatMode,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Which skin layers/parts are displayed.
    pub skin_parts: DisplayedSkinParts,
    /// Dominant hand used for held-item rendering.
    pub main_hand: MainHand,
    /// Whether the client filters text via a partner service.
    pub text_filtering: bool,
    /// Whether the client allows appearing in server player-list samples.
    pub allow_server_listing: bool,
    /// Particle rendering level.
    pub particle_status: ParticleStatus,
}

/// Chat message visibility preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatMode {
    /// Show all chat.
    Full,
    /// Show commands only (system messages), not player chat.
    CommandsOnly,
    /// Show nothing.
    Hidden,
}

/// Which skin layers/parts the client renders on its player model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DisplayedSkinParts {
    /// Cape layer.
    pub cape: bool,
    /// Jacket overlay layer.
    pub jacket: bool,
    /// Left sleeve overlay layer.
    pub left_sleeve: bool,
    /// Right sleeve overlay layer.
    pub right_sleeve: bool,
    /// Left pants-leg overlay layer.
    pub left_pants_leg: bool,
    /// Right pants-leg overlay layer.
    pub right_pants_leg: bool,
    /// Hat overlay layer.
    pub hat: bool,
}

/// Dominant hand used for held-item rendering and default interaction side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainHand {
    /// Left-handed.
    Left,
    /// Right-handed.
    Right,
}

/// Particle rendering detail level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParticleStatus {
    /// Render all particles.
    All,
    /// Render a reduced set of particles.
    Decreased,
    /// Render minimal particles.
    Minimal,
}

/// Outcome reported for a server-pushed resource pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourcePackResponseKind {
    /// The pack downloaded and applied successfully.
    SuccessfullyLoaded,
    /// The player declined the pack.
    Declined,
    /// The pack failed to download.
    FailedDownload,
    /// The player accepted the prompt and download is starting.
    Accepted,
    /// The pack finished downloading.
    Downloaded,
    /// The pack URL was invalid.
    InvalidUrl,
    /// Reloading the pack failed.
    FailedReload,
    /// The pack was discarded (superseded or connection ended).
    Discarded,
}

/// Command-block execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandBlockMode {
    /// Runs the tick after the block preceding it in the chain runs.
    Sequence,
    /// Runs every tick regardless of redstone power.
    Auto,
    /// Runs only when powered by redstone.
    Redstone,
}

/// A cardinal block face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockFace {
    /// Negative Y.
    Down,
    /// Positive Y.
    Up,
    /// Negative Z.
    North,
    /// Positive Z.
    South,
    /// Negative X.
    West,
    /// Positive X.
    East,
}

/// Block-breaking actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockActionKind {
    /// Begin breaking the target block.
    StartDestroy,
    /// Abort breaking the target block.
    AbortDestroy,
    /// Finish breaking the target block.
    StopDestroy,
}

/// Entity interaction intent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntityInteraction {
    /// Attack the target entity with the currently selected item.
    Attack,
    /// Use a hand on the target entity.
    Interact {
        /// Hand used for the interaction.
        hand: Hand,
    },
    /// Use a hand at a precise entity-local hit position.
    InteractAt {
        /// Hand used for the interaction.
        hand: Hand,
        /// Entity-local hit position.
        target: Vec3,
    },
}

/// A container click mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContainerClickType {
    /// Left/right pickup and place.
    Pickup,
    /// Shift-click quick transfer.
    QuickMove,
    /// Number-key or off-hand swap.
    Swap,
    /// Creative middle-click clone.
    Clone,
    /// Drop from a slot.
    Throw,
    /// Multi-packet drag distribution.
    QuickCraft,
    /// Double-click gather.
    PickupAll,
}

/// One predicted slot update included with a container click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSlotChange {
    /// Menu slot index.
    pub slot: i32,
    /// Predicted stack in that slot. `None` means empty.
    pub item: Option<ItemStack>,
}

/// Continuous player movement input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PlayerInput {
    /// Forward key is pressed.
    pub forward: bool,
    /// Backward key is pressed.
    pub backward: bool,
    /// Left strafe key is pressed.
    pub left: bool,
    /// Right strafe key is pressed.
    pub right: bool,
    /// Jump key is pressed.
    pub jump: bool,
    /// Sneak/secondary-action key is pressed.
    pub shift: bool,
    /// Sprint key is pressed.
    pub sprint: bool,
}

impl PlayerInput {
    /// No movement input.
    pub const EMPTY: Self = Self {
        forward: false,
        backward: false,
        left: false,
        right: false,
        jump: false,
        shift: false,
        sprint: false,
    };
}

/// Discrete player commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlayerCommand {
    /// Leave bed.
    StopSleeping,
    /// Begin sprinting.
    StartSprinting,
    /// Stop sprinting.
    StopSprinting,
    /// Begin a rideable jump with the given boost strength.
    StartRidingJump {
        /// Jump boost strength.
        boost: i32,
    },
    /// Stop a rideable jump.
    StopRidingJump,
    /// Open the controlled vehicle's inventory.
    OpenInventory,
    /// Start elytra/fall flying.
    StartFallFlying,
}
