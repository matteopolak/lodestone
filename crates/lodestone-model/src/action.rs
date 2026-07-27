use uuid::Uuid;

use crate::{
    common::Hand,
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
    /// Send player movement.
    Move {
        /// Player position.
        pos: Vec3,
        /// Player rotation.
        rotation: Rotation,
        /// Whether the player is on the ground.
        on_ground: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
