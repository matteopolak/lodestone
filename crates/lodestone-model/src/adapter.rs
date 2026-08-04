use thiserror::Error;
use uuid::Uuid;

use crate::{
    action::ClientAction,
    event::{ClientEvent, EquipmentSlot},
    ids::ResourceKey,
    item::ItemStack,
    text::Text,
};

/// Packet direction relative to an endpoint, re-exported from `lodestone-core`
/// so adapter id tables can use the same direction type as packet metadata.
pub use lodestone_core::Bound;
/// Version-free connection state, re-exported from `lodestone-core` so protocol
/// packets and canonical model adapters share one type identity.
pub use lodestone_core::State as ConnectionState;

/// The world write seam handed to [`VersionAdapter::handle_packet`], re-exported
/// so protocol crates and consumers share one trait identity. Adapters apply
/// decoded chunks through this rather than surfacing them as events.
pub use lodestone_world::{LoadedChunk, WorldSink};

/// A side effect an adapter asks the connection layer to perform.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum Directive {
    /// Write a packet with this protocol-specific id and body.
    Send {
        /// Protocol-specific packet id.
        packet_id: i32,
        /// Encoded packet body.
        payload: Vec<u8>,
    },
    /// Move the connection to a new state.
    ///
    /// Applies after any [`Directive::Send`] values emitted before it in the
    /// same batch.
    SetState(ConnectionState),
    /// Enable or reconfigure zlib compression.
    ///
    /// Negative thresholds disable compression.
    SetCompression(i32),
    /// Surface a canonical event to the library user.
    Emit(ClientEvent),
    /// Begin the online-mode encryption handshake.
    ///
    /// Carries only the *protocol-shaped* inputs the server sent in its
    /// encryption request: the ASCII server id used in the auth hash, the
    /// server's DER RSA public key, the verify token to echo back, and whether
    /// the server expects a Mojang session-server call. No crypto or I/O is
    /// implied here — the driver generates the shared secret, RSA-wraps it and
    /// the token, optionally authenticates, then asks the adapter to frame the
    /// reply via [`VersionAdapter::build_encryption_response`] and enables its
    /// cipher. Keeping the *framing* (packet id + byte-array layout, which
    /// differs across versions) in the adapter and the *crypto* in the driver is
    /// the whole point of this split.
    BeginEncryption {
        /// ASCII server id used when computing the authentication hash.
        server_id: String,
        /// The server's DER-encoded RSA public key.
        public_key: Vec<u8>,
        /// Verify token the client must echo back encrypted.
        verify_token: Vec<u8>,
        /// Whether the server expects a Mojang session-server join call.
        should_authenticate: bool,
    },
    /// The connection should be closed.
    Disconnect(Text),
    /// A `minecraft:bundle_delimiter` boundary (issue #299).
    ///
    /// Vanilla's own pipeline (`BundlerInfo.java`) never gives this packet a
    /// body — it exists purely as a toggle: the first delimiter after a run of
    /// ordinary packets opens a bundle, the next one closes it, and everything
    /// decoded in between is meant to apply to the client in one atomic step
    /// rather than packet-by-packet. The adapter only recognises the
    /// boundary and hands it up unpaired; pairing two of these into one
    /// bundle and deferring the directives between them is the connection
    /// layer's job (see `lodestone-client`'s driver), since the adapter's
    /// `handle_packet` is called once per packet and has no memory of "am I
    /// inside a bundle" across calls.
    BundleDelimiter,
}

/// Identity the client presents during login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginProfile {
    /// Player username.
    pub username: String,
    /// Player profile UUID.
    pub uuid: Uuid,
}

/// Where the client is connecting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAddress {
    /// Server hostname or IP literal.
    pub host: String,
    /// Server port.
    pub port: u16,
}

/// Error returned by a [`VersionAdapter`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdapterError {
    /// The packet payload could not be decoded.
    #[error("failed to decode packet: {0}")]
    Decode(String),
    /// The action could not be encoded.
    #[error("failed to encode action: {0}")]
    Encode(String),
    /// The server requires a protocol feature this adapter does not implement.
    #[error("unsupported protocol feature: {0}")]
    Unsupported(String),
    /// The state is not supported by the adapter for this inbound packet.
    #[error("unsupported packet state {state:?}")]
    UnsupportedPacketState {
        /// Connection state.
        state: ConnectionState,
    },
    /// The action is not supported by the adapter in the current state.
    #[error("unsupported client action {action:?} in state {state:?}")]
    UnsupportedAction {
        /// Connection state.
        state: ConnectionState,
        /// Unsupported action.
        action: ClientActionKind,
    },
}

/// Compact action kind used in adapter errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientActionKind {
    /// [`ClientAction::SendChat`].
    SendChat,
    /// [`ClientAction::SendCommand`].
    SendCommand,
    /// [`ClientAction::ChatAck`].
    ChatAck,
    /// [`ClientAction::Move`].
    Move,
    /// [`ClientAction::KeepAliveResponse`].
    KeepAliveResponse,
    /// [`ClientAction::Respawn`].
    Respawn,
    /// [`ClientAction::SwingArm`].
    SwingArm,
    /// [`ClientAction::BlockAction`].
    BlockAction,
    /// [`ClientAction::DropSelectedItem`].
    DropSelectedItem,
    /// [`ClientAction::DropSelectedItemStack`].
    DropSelectedItemStack,
    /// [`ClientAction::SwapItemWithOffhand`].
    SwapItemWithOffhand,
    /// [`ClientAction::ReleaseUseItem`].
    ReleaseUseItem,
    /// [`ClientAction::Stab`].
    Stab,
    /// [`ClientAction::UseItemOn`].
    UseItemOn,
    /// [`ClientAction::UseItem`].
    UseItem,
    /// [`ClientAction::InteractEntity`].
    InteractEntity,
    /// [`ClientAction::ContainerClick`].
    ContainerClick,
    /// [`ClientAction::ContainerClose`].
    ContainerClose,
    /// [`ClientAction::SetCarriedItem`].
    SetCarriedItem,
    /// [`ClientAction::SetCreativeModeSlot`].
    SetCreativeModeSlot,
    /// [`ClientAction::SetPlayerInput`].
    SetPlayerInput,
    /// [`ClientAction::PlayerCommand`].
    PlayerCommand,
    /// [`ClientAction::Disconnect`].
    Disconnect,
    /// [`ClientAction::SetClientSettings`].
    SetClientSettings,
    /// [`ClientAction::SendBrand`].
    SendBrand,
    /// [`ClientAction::PongResponse`].
    PongResponse,
    /// [`ClientAction::ResourcePackResponse`].
    ResourcePackResponse,
    /// [`ClientAction::EndClientTick`].
    EndClientTick,
    /// [`ClientAction::ContainerButtonClick`].
    ContainerButtonClick,
    /// [`ClientAction::SetFlying`].
    SetFlying,
    /// [`ClientAction::RenameItem`].
    RenameItem,
    /// [`ClientAction::SelectTrade`].
    SelectTrade,
    /// [`ClientAction::PickItemFromBlock`].
    PickItemFromBlock,
    /// [`ClientAction::PickItemFromEntity`].
    PickItemFromEntity,
    /// [`ClientAction::SetBeaconEffects`].
    SetBeaconEffects,
    /// [`ClientAction::EditBook`].
    EditBook,
    /// [`ClientAction::SignUpdate`].
    SignUpdate,
    /// [`ClientAction::SetCommandBlock`].
    SetCommandBlock,
    /// [`ClientAction::PlayerLoaded`].
    PlayerLoaded,
    /// [`ClientAction::SeenAdvancements`].
    SeenAdvancements,
    /// [`ClientAction::CommandSuggestion`].
    CommandSuggestion,
    /// [`ClientAction::PaddleBoat`].
    PaddleBoat,
    /// [`ClientAction::MoveVehicle`].
    MoveVehicle,
    /// [`ClientAction::SelectBundleItem`].
    SelectBundleItem,
    /// [`ClientAction::SetContainerSlotState`].
    SetContainerSlotState,
    /// [`ClientAction::SetRecipeBookSettings`].
    SetRecipeBookSettings,
    /// [`ClientAction::RecipeBookSeenRecipe`].
    RecipeBookSeenRecipe,
    /// [`ClientAction::PlaceRecipe`].
    PlaceRecipe,
    /// [`ClientAction::PingRequest`].
    PingRequest,
    /// [`ClientAction::SpectatorAction`].
    SpectatorAction,
    /// [`ClientAction::TeleportToEntity`].
    TeleportToEntity,
    /// [`ClientAction::ChangeGameMode`].
    ChangeGameMode,
}

impl From<&ClientAction> for ClientActionKind {
    fn from(value: &ClientAction) -> Self {
        match value {
            ClientAction::SendChat { .. } => Self::SendChat,
            ClientAction::SendCommand { .. } => Self::SendCommand,
            ClientAction::ChatAck { .. } => Self::ChatAck,
            ClientAction::Move { .. } => Self::Move,
            ClientAction::KeepAliveResponse { .. } => Self::KeepAliveResponse,
            ClientAction::Respawn => Self::Respawn,
            ClientAction::SwingArm { .. } => Self::SwingArm,
            ClientAction::BlockAction { .. } => Self::BlockAction,
            ClientAction::DropSelectedItem => Self::DropSelectedItem,
            ClientAction::DropSelectedItemStack => Self::DropSelectedItemStack,
            ClientAction::SwapItemWithOffhand => Self::SwapItemWithOffhand,
            ClientAction::ReleaseUseItem => Self::ReleaseUseItem,
            ClientAction::Stab => Self::Stab,
            ClientAction::UseItemOn { .. } => Self::UseItemOn,
            ClientAction::UseItem { .. } => Self::UseItem,
            ClientAction::InteractEntity { .. } => Self::InteractEntity,
            ClientAction::ContainerClick { .. } => Self::ContainerClick,
            ClientAction::ContainerClose { .. } => Self::ContainerClose,
            ClientAction::SetCarriedItem { .. } => Self::SetCarriedItem,
            ClientAction::SetCreativeModeSlot { .. } => Self::SetCreativeModeSlot,
            ClientAction::SetPlayerInput(_) => Self::SetPlayerInput,
            ClientAction::PlayerCommand { .. } => Self::PlayerCommand,
            ClientAction::Disconnect => Self::Disconnect,
            ClientAction::SetClientSettings(_) => Self::SetClientSettings,
            ClientAction::SendBrand { .. } => Self::SendBrand,
            ClientAction::PongResponse { .. } => Self::PongResponse,
            ClientAction::ResourcePackResponse { .. } => Self::ResourcePackResponse,
            ClientAction::EndClientTick => Self::EndClientTick,
            ClientAction::ContainerButtonClick { .. } => Self::ContainerButtonClick,
            ClientAction::SetFlying { .. } => Self::SetFlying,
            ClientAction::RenameItem { .. } => Self::RenameItem,
            ClientAction::SelectTrade { .. } => Self::SelectTrade,
            ClientAction::PickItemFromBlock { .. } => Self::PickItemFromBlock,
            ClientAction::PickItemFromEntity { .. } => Self::PickItemFromEntity,
            ClientAction::SetBeaconEffects { .. } => Self::SetBeaconEffects,
            ClientAction::EditBook { .. } => Self::EditBook,
            ClientAction::SignUpdate { .. } => Self::SignUpdate,
            ClientAction::SetCommandBlock { .. } => Self::SetCommandBlock,
            ClientAction::PlayerLoaded => Self::PlayerLoaded,
            ClientAction::SeenAdvancements { .. } => Self::SeenAdvancements,
            ClientAction::CommandSuggestion { .. } => Self::CommandSuggestion,
            ClientAction::PaddleBoat { .. } => Self::PaddleBoat,
            ClientAction::MoveVehicle { .. } => Self::MoveVehicle,
            ClientAction::SelectBundleItem { .. } => Self::SelectBundleItem,
            ClientAction::SetContainerSlotState { .. } => Self::SetContainerSlotState,
            ClientAction::SetRecipeBookSettings { .. } => Self::SetRecipeBookSettings,
            ClientAction::RecipeBookSeenRecipe { .. } => Self::RecipeBookSeenRecipe,
            ClientAction::PlaceRecipe { .. } => Self::PlaceRecipe,
            ClientAction::PingRequest { .. } => Self::PingRequest,
            ClientAction::SpectatorAction { .. } => Self::SpectatorAction,
            ClientAction::TeleportToEntity { .. } => Self::TeleportToEntity,
            ClientAction::ChangeGameMode { .. } => Self::ChangeGameMode,
        }
    }
}

/// The version-free base hitbox of an entity type: the standing bounding-box
/// `width` and `height` in blocks, at scale 1.
///
/// This is the [`VersionAdapter::entity_dimensions`] seam's return type. It
/// carries *base* geometry only — deliberately **not** `step_height` (the
/// resolved `STEP_HEIGHT` attribute, folded from the entity's attribute map)
/// nor any `SCALE`-attribute fold. A consumer combines this base box with those
/// attribute-sourced values at spawn; baking either in here would silently
/// disagree with the attribute pipeline the moment a modifier exists.
///
/// The numbers are version data (pose boxes shifted across the 1.9/1.14
/// refactors), so they originate in a version crate's generated table and reach
/// version-free consumers only through this seam — never a direct edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityBaseDimensions {
    /// Standing bounding-box width, in blocks (the box is `width` on both
    /// horizontal axes). Vanilla `EntityDimensions.width()` at scale 1.
    pub width: f32,
    /// Standing bounding-box height, in blocks. Vanilla
    /// `EntityDimensions.height()` at scale 1.
    pub height: f32,
}

/// The version-free per-entity-type facts a physics consumer needs about a
/// *neighbouring* entity: its base hitbox, and whether it can shove the local
/// player.
///
/// This is the [`VersionAdapter::entity_facts`] seam's return type. It is keyed
/// by [`ResourceKey`] rather than by network id, which is what makes it usable
/// past ingest: the raw `add_entity` varint does not survive into the ECS (only
/// the resolved key does), and a resource key is the version-independent
/// identity anyway. Use [`VersionAdapter::entity_dimensions`] instead when you
/// still hold the wire id — on the decode side, or in the integrated server.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityFacts {
    /// The type's base hitbox — exactly what [`VersionAdapter::entity_dimensions`]
    /// returns for the same type. Base geometry only; see
    /// [`EntityBaseDimensions`] on the `SCALE`/`STEP_HEIGHT` folds being the
    /// caller's.
    pub dimensions: EntityBaseDimensions,
    /// Whether an entity of this type can shove the local player through
    /// vanilla `LivingEntity.pushEntities()`.
    ///
    /// # This is a *type*-level capability, and default-**deny**
    ///
    /// `Entity.isPushable()` is `false` and only `LivingEntity` overrides it to
    /// `true`; more to the point, `LivingEntity.aiStep` is the only caller of
    /// `pushEntities()` in the whole vanilla tree, so a non-living entity never
    /// runs the crowd pass at all. A version that does not know a type therefore
    /// reports "no", never "probably". The inverse — assuming an unknown type
    /// can push — makes dropped items nudge the player around the floor.
    ///
    /// `true` means "can, in some runtime state", not "always does". The
    /// per-instance refinements vanilla layers on top (`isAlive()`,
    /// `isSpectator()`, `onClimbable()`, a horse's `!isVehicle()`, a warden's
    /// `!isDiggingOrEmerging()`) are entity *state*, not per-type data, and stay
    /// the consumer's to apply where it has them.
    ///
    /// # What this does **not** cover
    ///
    /// Only the `LivingEntity.pushEntities()` mechanism, because that is the one
    /// `lodestone-physics`'s crowd pass models. Vanilla boats and rideable
    /// minecarts also push players, but from their own tick through
    /// `AbstractBoat.push(Entity)` and `NewMinecartBehavior.pushEntities(AABB)`,
    /// which use a differently-inflated query box and extra conditions. A
    /// version reports `false` for those families rather than approximating them
    /// into the wrong pass.
    pub pushes_players: bool,
}

/// One axis-aligned collision box of a block state, in **block-local**
/// coordinates: `min`/`max` are `[x, y, z]` offsets from the block's own corner.
///
/// This is the element type of the [`VersionAdapter::block_collision`] seam. A
/// full cube is `min = [0.0; 3]`, `max = [1.0; 3]`; a block with no collision
/// (air, water, cobweb, most plants) has *no* boxes at all — an empty slice, not
/// a zero-volume box.
///
/// # `max` is **not** capped at 1.0
///
/// Fences, walls and fence gates reach `y = 1.5` in vanilla, which is why the
/// 0.6-block auto-step cannot mount them. Clamping a box to the unit cube would
/// make a fence look step-able; the uncapped value is load-bearing (see
/// `CollisionView::collision_top` in `lodestone-physics`).
///
/// # Why `f32`
///
/// Vanilla's shapes are `double`, but every distinct coordinate value a real
/// block state uses is exactly representable in `f32` (asserted by the version
/// crate's drift test against the JVM oracle dump), so the narrow form is
/// lossless and halves the rodata a 32k-state table costs. `f32 -> f64` widening
/// at the physics seam is exact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockAabb {
    /// Minimum corner `[x, y, z]`, block-local.
    pub min: [f32; 3],
    /// Maximum corner `[x, y, z]`, block-local. Uncapped — see the type docs.
    pub max: [f32; 3],
}

/// A block state's break-time inputs: vanilla `destroySpeed` (hardness) and
/// whether the correct tool is required for drops.
///
/// This is the [`VersionAdapter::block_hardness`] seam's return type. Both
/// fields are read straight off the version's `BlockState` census, so they mean
/// exactly what vanilla means by them — see the warning below before feeding
/// either one into break-time math.
///
/// # Trap: `requires_correct_tool` is **not** "the player has the right tool"
///
/// `requires_correct_tool` mirrors `BlockState.requiresCorrectToolForDrops` — a
/// property of the *block*, answering "does this block drop nothing unless mined
/// with a suitable tool?". A break-time calculation instead needs
/// `Player.hasCorrectToolForDrops` — a property of the *player's held item vs.
/// the block* — which is what `lodestone-game`'s `BreakInputs.correct_tool`
/// means, and which selects vanilla's `30` vs `100` speed divider.
///
/// The two are near-opposites bare-handed: with an empty hand,
/// `correct_tool == !requires_correct_tool`. Assigning this field straight into
/// `BreakInputs.correct_tool` therefore tells the math that stone is being mined
/// *correctly* by a bare hand, breaking it in **45 ticks instead of 151** — 3.4x
/// too fast — while looking for all the world like faithful data wiring. Wire
/// `hardness` through directly; derive `correct_tool` from the held item (and,
/// bare-handed, from `!requires_correct_tool`), never from this field as-is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockHardness {
    /// `BlockState.getDestroySpeed` (vanilla's field name for hardness).
    /// `-1.0` marks an unbreakable block (bedrock, barrier, ...).
    pub hardness: f32,
    /// `BlockState.requiresCorrectToolForDrops`: whether the *block* demands a
    /// suitable tool for drops. **Not** the player's tool-match flag — see the
    /// type-level warning above.
    pub requires_correct_tool: bool,
}

/// The held item's contribution to the break-time formula for one block state:
/// the two fields that `lodestone-game`'s `BreakInputs` needs and that
/// [`BlockHardness`] deliberately does *not* provide.
///
/// This is the [`VersionAdapter::tool_mining`] seam's return type. It exists so
/// that the caller never has to derive `correct_tool` itself — see the field
/// docs, and the warning on [`BlockHardness`] for what deriving it wrong costs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToolMining {
    /// `ItemStack.getDestroySpeed(state)` → `BreakInputs::tool_speed`. `1.0`
    /// bare-handed, and `1.0` for a tool whose rules do not match this block.
    pub speed: f32,
    /// `Player.hasCorrectToolForDrops(state)` → `BreakInputs::correct_tool`,
    /// which selects vanilla's `30` vs `100` divider.
    ///
    /// **Already folded, do not re-derive.** This is
    /// `!state.requiresCorrectToolForDrops() || item.isCorrectToolForDrops(state)`,
    /// i.e. it *includes* the bare-hand inversion of
    /// [`BlockHardness::requires_correct_tool`]. Assign it straight into
    /// `BreakInputs::correct_tool`; combining it with `requires_correct_tool`
    /// again re-introduces the 3.4x-too-fast bug that field warns about.
    pub correct_tool: bool,
    /// `Tool.damagePerBlock`: durability the held item spends per block broken.
    /// `0` when the held item has no tool component (a bare hand, or a
    /// non-tool), matching vanilla's "no `minecraft:tool`, no durability cost".
    pub damage_per_block: u32,
}

/// An item's built-in **prototype** component values: the ones a clientbound
/// stack never carries, because a stack on the wire is only the *patch* against
/// this.
///
/// This is the [`VersionAdapter::item_prototype`] seam's return type. Everything
/// here is version data (a census of the real registry), which is why it cannot
/// be recovered from a packet capture and why guessing is worse than reporting
/// nothing — see the individual fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemPrototype {
    /// Effective `minecraft:max_stack_size`, `1..=99`.
    ///
    /// Vanilla's `COMMON_ITEM_COMPONENTS` sets `64` and individual items
    /// override it, so this is present for every registered item; the census
    /// records the resolved value rather than the override. `64` is *not* a safe
    /// default: every shulker box, bucket and tool is `1`, and eggs are `16`.
    pub max_stack_size: u32,
    /// Effective `minecraft:max_damage`, or `None` for an item with no
    /// durability at all.
    ///
    /// `Some(_)` is what makes vanilla `ItemStack.isDamageableItem` true and
    /// therefore `ItemStack.isStackable` false for a damaged tool.
    pub max_damage: Option<u32>,
    /// The slot `minecraft:equippable` names, or `None` for an item that is not
    /// equippable.
    ///
    /// [`EquipmentSlot::Body`] (animal armour) and [`EquipmentSlot::Saddle`] are
    /// *not* player armour: vanilla's humanoid-armour gate is
    /// `EquipmentSlot.Type.HUMANOID_ARMOR`, covering only
    /// [`Feet`](EquipmentSlot::Feet)/[`Legs`](EquipmentSlot::Legs)/[`Chest`](EquipmentSlot::Chest)/[`Head`](EquipmentSlot::Head).
    pub equip_slot: Option<EquipmentSlot>,
    /// Whether `minecraft:equippable`'s `allowedEntities` is empty, i.e. any
    /// entity may wear it (vanilla `Equippable.canBeEquippedBy` returns `true`
    /// unconditionally). `false` means the item is restricted to a specific
    /// entity set — `minecraft:wolf_armor` to wolves, `minecraft:saddle` to
    /// `#minecraft:can_equip_saddle` — which this seam deliberately does not
    /// enumerate, because in 26.2 every restricted item is already in a
    /// non-humanoid [`equip_slot`](Self::equip_slot) and so is excluded by the
    /// slot check alone.
    ///
    /// Meaningless (and always `true`) when [`equip_slot`](Self::equip_slot) is
    /// `None`.
    pub equippable_by_any_entity: bool,
}

/// The per-block **movement constants** vanilla stores as
/// `BlockBehaviour.Properties` fields and tag memberships rather than as
/// geometry, keyed by the block's canonical `minecraft:*` name.
///
/// Returned by [`block_physics`]. Every field is what the physics integrator
/// (`lodestone_physics::CollisionView`) asks for by the same name, and every one
/// of them is a *cost of moving through or over a cell* — which is why a
/// pathfinder needs the whole set and why it lives here, in a version-free crate
/// a plugin already depends on, rather than privately inside the client shell.
///
/// # Why this is not behind [`VersionAdapter`]
///
/// The rest of the block data in this module ([`BlockAabb`], [`BlockHardness`])
/// is keyed by **block-state id**, which vanilla renumbers every version — so it
/// has to be reached through a version adapter. These six are keyed by block
/// *name*, which is stable across versions: `minecraft:ice` has been `0.98`
/// friction since 1.0. Putting a name-keyed table behind the version seam would
/// mean re-homing an identical copy in every protocol crate.
///
/// That is a claim about *stability*, not about *correctness by construction*: the
/// values here are anchored to a dump of the real 26.2 server
/// (`crates/protocol/v770/tests/block_physics.rs` replays all 1,196 registered
/// blocks through [`block_physics`] and demands agreement, bit-exactly, on every
/// field). If a future version *does* change one, that gate fails, and the fix at
/// that point is a per-version override — not a silent edit here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockPhysics {
    /// `Block.getFriction` — vanilla `Properties.friction`, default `0.6`.
    ///
    /// Only five blocks differ in 26.2: ice, packed ice and frosted ice `0.98`,
    /// blue ice `0.989`, slime block `0.8`.
    pub friction: f32,
    /// `Block.getSpeedFactor` — default `1.0`. Soul sand and honey block `0.4`.
    pub speed_factor: f32,
    /// `Block.getJumpFactor` — default `1.0`. Honey block `0.5`, alone.
    pub jump_factor: f32,
    /// `Block.getBounceRestitution`, **already net of**
    /// `BlockTags.SUPPRESSES_BOUNCE` — default `0.0`. Slime block `1.0`, every
    /// dyed bed `0.75`.
    ///
    /// Do not subtract the tag again. In 26.2 its only member is
    /// `minecraft:honey_block`, which sets no restitution, so tag-aware and
    /// tag-blind agree on every block — the subtraction is currently a no-op and
    /// a future bouncy suppressor would break it *silently*, which is why the
    /// version crate's gate pins the tag's membership.
    pub bounce_restitution: f32,
    /// `Block.entityInside` → `Entity.makeStuckInBlock`: the per-axis speed
    /// multiplier of the three blocks that grab you, or `None` for everything
    /// else.
    ///
    /// **The one field here that is not a dumped property.** The vector is
    /// constructed in imperative code inside each block's `entityInside`
    /// override, so there is nothing on a `Properties` object to read. What the
    /// version crate's gate *can* establish is completeness: the JVM oracle
    /// enumerates every block whose class overrides `entityInside` (61 of them in
    /// 26.2) and asserts that all three rows below are in that set, so no fourth
    /// grabbing block can appear without the gate noticing the candidate set
    /// changed.
    ///
    /// Per-entity overrides are deliberately not modelled: cobweb gives a
    /// `WEAVING` mob `(0.5, 0.25, 0.5)` instead, and sweet berry bush exempts
    /// foxes and bees.
    pub stuck_multiplier: Option<[f64; 3]>,
    /// Membership of `BlockTags.CLIMBABLE` — ladder, vine, scaffolding, the four
    /// nether vine blocks and both cave-vine blocks. Nine in 26.2.
    ///
    /// Scaffolding is in the tag but holds differently when sneaking, a
    /// distinction this flag does not carry.
    pub climbable: bool,
}

/// What [`block_physics`] answers for a block it has no row for, and for a name
/// it cannot resolve at all: vanilla's own defaults, which the overwhelming
/// majority of the 1,196 registered blocks have verbatim.
pub const DEFAULT_BLOCK_PHYSICS: BlockPhysics = BlockPhysics {
    friction: 0.6,
    speed_factor: 1.0,
    jump_factor: 1.0,
    bounce_restitution: 0.0,
    stuck_multiplier: None,
    climbable: false,
};

/// The movement constants of a block, by its canonical `minecraft:*` name — the
/// name a [`VersionAdapter::block_name`] call already returns for a block-state
/// id.
///
/// An unrecognised name (including one from another namespace, or one this
/// version does not have) yields [`DEFAULT_BLOCK_PHYSICS`] rather than `None`:
/// unlike a shape or a hardness, "no row" is not a data gap here — it is the
/// answer for 1,166 of 1,196 blocks, and a caller that had to handle `None`
/// would end up re-inventing these same defaults at every call site.
///
/// O(1)-ish and allocation-free: a `match` over string literals, which rustc
/// compiles to a length-bucketed comparison chain. The physics tick calls this
/// for the block under the player's feet.
///
/// # Provenance
///
/// Every row is a value read out of the real 26.2 server, not out of the
/// decompiled source by hand — see [`BlockPhysics`] for where the gate lives and
/// what it covers. The `Blocks.java` line numbers below are cross-references for
/// a reader, not the source of the numbers.
#[must_use]
pub fn block_physics(block_name: &str) -> BlockPhysics {
    BlockPhysics {
        friction: match block_name {
            // `Blocks.java:1950` (ice), `:3021` (packed ice), `:3732` (frosted ice).
            "minecraft:ice" | "minecraft:packed_ice" | "minecraft:frosted_ice" => 0.98,
            // `Blocks.java:4227`.
            "minecraft:blue_ice" => 0.989,
            // `Blocks.java:2926`.
            "minecraft:slime_block" => 0.8,
            _ => DEFAULT_BLOCK_PHYSICS.friction,
        },
        // `Blocks.java:2024` (soul sand), `:4843` (honey block).
        speed_factor: match block_name {
            "minecraft:soul_sand" | "minecraft:honey_block" => 0.4,
            _ => DEFAULT_BLOCK_PHYSICS.speed_factor,
        },
        // `Blocks.java:4843` — honey block is the only block in 26.2 that sets it.
        jump_factor: match block_name {
            "minecraft:honey_block" => 0.5,
            _ => DEFAULT_BLOCK_PHYSICS.jump_factor,
        },
        bounce_restitution: match block_name {
            // `Blocks.java:2926`.
            "minecraft:slime_block" => 1.0,
            // `Blocks.java:684`, via the `BED` `ColorCollection` — all 16 dyed
            // beds share one `Properties` builder. Matched by suffix *within the
            // vanilla namespace only*, so a modded `_bed` does not inherit it.
            name if name.starts_with("minecraft:") && name.ends_with("_bed") => 0.75,
            _ => DEFAULT_BLOCK_PHYSICS.bounce_restitution,
        },
        stuck_multiplier: match block_name {
            // `WebBlock.java:30-35`.
            "minecraft:cobweb" => Some([0.25, 0.05, 0.25]),
            // `PowderSnowBlock.java:66`.
            "minecraft:powder_snow" => Some([0.9, 1.5, 0.9]),
            // `SweetBerryBushBlock.java:86`.
            "minecraft:sweet_berry_bush" => Some([0.8, 0.75, 0.8]),
            _ => DEFAULT_BLOCK_PHYSICS.stuck_multiplier,
        },
        // `data/minecraft/tags/block/climbable.json`, all nine entries.
        climbable: matches!(
            block_name,
            "minecraft:ladder"
                | "minecraft:vine"
                | "minecraft:scaffolding"
                | "minecraft:weeping_vines"
                | "minecraft:weeping_vines_plant"
                | "minecraft:twisting_vines"
                | "minecraft:twisting_vines_plant"
                | "minecraft:cave_vines"
                | "minecraft:cave_vines_plant"
        ),
    }
}

/// Adapter implemented by protocol crates to lift packets into this canonical
/// model and lower canonical actions back into packets.
///
/// This trait is the only intended coupling point between a protocol-specific
/// crate and the version-free model:
///
/// - [`VersionAdapter::begin_login`] emits the initial protocol-owned packets
///   required to begin a connection.
/// - [`VersionAdapter::handle_packet`] receives the already-decompressed
///   inbound packet body as raw bytes, plus the version-free connection state
///   and numeric packet id from the protocol layer. It returns an empty vector
///   when a packet is intentionally ignored by the model.
/// - [`VersionAdapter::encode_action`] receives a canonical client action and
///   returns `Ok(None)` only when no packet should be sent. If the protocol
///   cannot faithfully express the requested capability, it should return
///   [`AdapterError::Unsupported`] or [`AdapterError::UnsupportedAction`].
///
/// Directives are executed in returned order, so adapters can request a packet
/// write before a state transition. The trait intentionally does not expose
/// wire codecs, registries, NBT, JSON chat serialization, compression, or
/// protocol crate types, nor does it perform any encryption itself: the
/// encryption *crypto* (key generation, RSA, session auth, cipher state) lives
/// in the driver, and only the protocol-shaped *framing* of the response
/// crosses this seam via [`VersionAdapter::build_encryption_response`]. Those
/// details remain in version adapters.
pub trait VersionAdapter: Send + Sync + std::fmt::Debug {
    /// Returns the adapter's primary protocol number.
    fn protocol_version(&self) -> i32;

    /// Returns human-readable Minecraft release names supported by this adapter.
    fn minecraft_versions(&self) -> &'static [&'static str];

    /// Returns whether this adapter supports `protocol`.
    fn supports(&self, protocol: i32) -> bool;

    /// Returns directives to execute immediately on connect.
    ///
    /// Typical adapters use this to send the handshake and login start packets.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the initial login packets cannot be
    /// encoded.
    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError>;

    /// Handles one inbound protocol packet and returns connection directives.
    ///
    /// `packet_id` is protocol-specific and must not escape this boundary.
    /// Inbound packets are implicitly [`Bound::Client`]; [`Bound`] remains
    /// public for adapter-side packet id tables.
    ///
    /// `world` is the client-owned world write sink. Packets that carry world
    /// data (chunks, and later block updates) apply it here directly, so the
    /// heavy decoded state never travels the bounded event channel; the adapter
    /// then emits only a lightweight [`ClientEvent::ChunkLoaded`] notification.
    /// Packets that do not touch world state simply ignore it.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the packet context or bytes cannot be
    /// handled by the adapter.
    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError>;

    /// Encodes one canonical action into a protocol packet id and payload.
    ///
    /// The returned packet id is protocol-specific and must be interpreted only
    /// by the calling protocol layer.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the action cannot be represented in the
    /// given state. Capability gaps in older protocols should be reported here
    /// instead of being hidden behind lossy defaults.
    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError>;

    /// Frames the serverbound encryption-response packet for this protocol.
    ///
    /// Called by the driver during the online-mode handshake after it has
    /// generated the shared secret and RSA-encrypted both it and the verify
    /// token (see [`Directive::BeginEncryption`]). The adapter owns only the
    /// protocol-specific packet id and byte-array framing, which differs across
    /// versions (pre-1.19 shapes carry an optional salt/signature) — so this
    /// deliberately does not live in shared code. Both arguments are already
    /// ciphertext; no crypto happens here.
    ///
    /// The default returns [`AdapterError::Unsupported`]: a version that has not
    /// implemented online-mode encryption simply never emits
    /// [`Directive::BeginEncryption`], so this is never reached for it. Versions
    /// that do must override it with their version-specific framing.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError`] when the response cannot be framed, or
    /// [`AdapterError::Unsupported`] when the version does not implement
    /// encryption.
    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        let _ = (encrypted_secret, encrypted_token);
        Err(AdapterError::Unsupported(
            "online-mode encryption is not implemented for this protocol version".to_owned(),
        ))
    }

    /// Resolves an entity type's **base** hitbox from its network registry id
    /// (the varint `add_entity` carries), if this version knows the type.
    ///
    /// The id space is version-specific — ids reshuffle as the registry grows —
    /// so this is the sanctioned route for a version-free consumer (the
    /// integrated server, entity navigation) to read per-type geometry without
    /// naming a version crate: it asks the registry for an adapter and calls
    /// this. Returns [`EntityBaseDimensions`] (base `width`/`height` only); the
    /// caller folds in the `SCALE` and `STEP_HEIGHT` attributes from the entity's
    /// attribute map at spawn.
    ///
    /// The default returns `None`: a version that has not homed a dimension
    /// census simply reports "unknown", never a guessed box.
    fn entity_dimensions(&self, entity_type_id: i32) -> Option<EntityBaseDimensions> {
        let _ = entity_type_id;
        None
    }

    /// Resolves an entity type's physics-relevant facts — its base hitbox and
    /// whether it can shove the local player — from its **resource key**, if this
    /// version knows the type.
    ///
    /// # Why keyed by [`ResourceKey`] and not the network id
    ///
    /// [`Self::entity_dimensions`] keys on the `add_entity` varint, which is the
    /// right identity on the decode side but is *gone* by the time physics runs:
    /// `ClientEvent::EntitySpawned` carries the already-resolved key and never the
    /// raw id, so a consumer downstream of ingest has no id to pass. Keying this
    /// seam by the key it actually holds is what lets the entity-push producer
    /// call it at all — without widening a spawn event, and without pushing a raw
    /// wire id back into the ECS, where it would be a second, version-shaped
    /// identity for something the ECS already names version-independently.
    ///
    /// The two are the same census read two ways; a version implementing both must
    /// return the same `dimensions` for the same type through either.
    ///
    /// The default returns `None`: a version that has homed no census reports
    /// "unknown", never a guessed box and never a permissive `pushes_players`. A
    /// caller that cannot distinguish "no adapter" from "unknown type" should
    /// treat both as *not* a pusher — see [`EntityFacts::pushes_players`].
    fn entity_facts(&self, entity_type: &ResourceKey) -> Option<EntityFacts> {
        let _ = entity_type;
        None
    }

    /// Resolves a block state's break-time inputs — vanilla `destroySpeed` and
    /// the block's correct-tool-for-drops requirement — from its **block-state
    /// id** (the id chunk sections and `block_update` carry), if this version
    /// knows the state.
    ///
    /// The block-state id space is version-specific (states are renumbered every
    /// time a block gains or loses a property), so this is the sanctioned route
    /// for a version-free consumer (mining/break-progress) to read per-state
    /// hardness without naming a version crate: ask the registry for an adapter
    /// and call this. A direct dependency on a version crate would mint a second,
    /// divergent version-data seam beside this one.
    ///
    /// The default returns `None`: a version that has not homed a hardness census
    /// reports "unknown", never a guessed number.
    ///
    /// **Before wiring this into break-time math, read the warning on
    /// [`BlockHardness`]:** `requires_correct_tool` is the *block's* requirement,
    /// not `Player.hasCorrectToolForDrops`. Passing it straight through as
    /// `BreakInputs.correct_tool` makes stone break 3.4x too fast.
    fn block_hardness(&self, state_id: u32) -> Option<BlockHardness> {
        let _ = state_id;
        None
    }

    /// Resolves the held item's break-time contribution for a block state —
    /// vanilla `ItemStack.getDestroySpeed` and `Player.hasCorrectToolForDrops`
    /// — from the stack and the **block-state id**, if this version knows the
    /// state.
    ///
    /// `held` is the item in the main hand; `None` (or a stack with no tool
    /// component) is the bare hand.
    ///
    /// # Why this cannot be computed by the caller
    ///
    /// The `minecraft:tool` component is only *sometimes* on the wire. A
    /// clientbound stack carries a `DataComponentPatch` — the delta from the
    /// item's built-in prototype — and vanilla puts a pickaxe's
    /// `minecraft:tool` in that prototype, so an ordinary pickaxe arrives with
    /// an empty patch and [`ItemComponents::tool`] is
    /// [`ToolPatch::Inherited`](crate::ToolPatch::Inherited). The prototype is
    /// version data. Even when a tool *is* on the wire, its rules name blocks by
    /// tag or by version-scoped registry id, and block-state ids are renumbered
    /// every version. All three are version-owned, so this is the sanctioned
    /// route for a version-free consumer to ask "how fast does this item mine
    /// this block, and does it drop?" — exactly as [`block_hardness`] is for
    /// hardness. A direct dependency on a version crate would mint a second,
    /// divergent version-data seam beside this one.
    ///
    /// [`block_hardness`]: VersionAdapter::block_hardness
    ///
    /// The default returns `None`: a version that has not homed a tool census
    /// reports "unknown", never a guessed speed.
    fn tool_mining(&self, held: Option<&ItemStack>, state_id: u32) -> Option<ToolMining> {
        let _ = (held, state_id);
        None
    }

    /// The **collision** geometry of a block state — vanilla
    /// `BlockState.getCollisionShape(...).toAabbs()` — as block-local
    /// [`BlockAabb`]s, or `None` if this version does not know the state.
    ///
    /// An empty slice is a *meaningful* answer, distinct from `None`: the state
    /// exists and has no collision (air, water, lava, cobweb, kelp, most plants).
    ///
    /// # Why this must be a version seam and not derived
    ///
    /// Collision geometry is **code**-defined in vanilla, not property-derived:
    /// `blocks.json` carries no shapes at all, and `Block.getCollisionShape` is
    /// neighbour-state-dependent for stairs, fences, walls and panes. The only
    /// authoritative source is the game itself (boot the server, walk
    /// `Block.BLOCK_STATE_REGISTRY`), and the resulting table is keyed by
    /// block-state ids that are renumbered every version — so a version-free
    /// consumer (the shell's `CollisionView`, a pathfinder) reaches it here, the
    /// same way it reaches [`block_hardness`](VersionAdapter::block_hardness).
    ///
    /// # This is collision, **not** the outline or interaction shape
    ///
    /// Three different vanilla shapes answer three different questions, and they
    /// genuinely disagree: a fluid has a full collision-less cell *and* an empty
    /// outline; kelp has an outline (so it can be targeted and broken) and **no**
    /// collision; a soul-sand block collides to `y = 0.875` but outlines to `1.0`.
    /// Do not wire this into block picking — that needs the outline shape.
    ///
    /// The default returns `None`: a version that has not homed a collision
    /// census reports "unknown", never a guessed cube. A consumer that falls back
    /// to a unit cube on `None` should say so loudly — silently cubing every
    /// solid block is exactly the defect this seam exists to remove (slabs,
    /// stairs, fences and ice all become full blocks, and the player stands half
    /// a block too high on every one of them).
    fn block_collision(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        let _ = state_id;
        None
    }

    /// The block identifier of a block state — for example
    /// `"minecraft:oak_slab"` for every one of that block's states — or `None` if
    /// this version does not know the state.
    ///
    /// Deliberately the *block* name with no property values: this is the key for
    /// the handful of per-block physics constants that vanilla stores as
    /// `BlockBehaviour.Properties` rather than as geometry, and which therefore
    /// cannot be recovered from [`block_collision`](VersionAdapter::block_collision):
    /// `friction` (ice 0.98, slime 0.8), `speedFactor` (soul sand and honey 0.4),
    /// `jumpFactor` (honey 0.5), `bounceRestitution` (slime 1.0, bed 0.75),
    /// `makeStuckInBlock` (cobweb, powder snow, sweet berry bush) and membership
    /// of `BlockTags.CLIMBABLE`. A consumer keys those off *names*, which are
    /// stable across versions in a way state ids are not.
    ///
    /// A consumer wanting properties too should not reach for this — that is
    /// [`BlockStateRegistry`](crate::BlockStateRegistry), whose borrowing shape
    /// requires an owned instance. This returns `&'static str` straight from
    /// rodata and needs no instance.
    ///
    /// The default returns `None`.
    fn block_name(&self, state_id: u32) -> Option<&'static str> {
        let _ = state_id;
        None
    }

    /// The **outline** geometry of a block state — vanilla
    /// `BlockStateBase.getShape(...).toAabbs()` — as block-local [`BlockAabb`]s,
    /// or `None` if this version does not know the state.
    ///
    /// This is the shape **block selection** uses, and it is a third thing,
    /// neither collision nor fluid presence. `Entity.pick` clips with
    /// `ClipContext.Block.OUTLINE` and `ClipContext.Fluid.NONE`, and
    /// `ClipContext.Block.OUTLINE` *is* `BlockStateBase::getShape`. So:
    ///
    /// * `LiquidBlock.getShape` is `Shapes.empty()` → open water and lava are
    ///   never targeted, which is why picking cannot be "the cell is not empty";
    /// * kelp's is `Block.column(16, 0, 9)` and seagrass's `Block.column(12, 0, 12)`
    ///   → **non-empty**, so both are targetable and breakable, even though their
    ///   *collision* shape is empty and their `getFluidState` is water. Picking
    ///   is therefore not `!is_water` either;
    /// * cobweb's outline is a full unit cube while its collision is empty.
    ///
    /// Half of all block states in 26.2 have an outline that differs from their
    /// collision shape, so [`block_collision`](VersionAdapter::block_collision)
    /// is not a usable stand-in. An empty slice is a meaningful answer (the state
    /// exists and cannot be targeted), distinct from `None`.
    ///
    /// # Two shapes here are context-dependent and resolve to their default form
    ///
    /// Vanilla's `getShape` takes a `CollisionContext`; the census dumps it with
    /// `CollisionContext.empty()`, the same thing vanilla's own shape cache does.
    /// Two consequences worth knowing: `minecraft:light` outlines to
    /// `Shapes.empty()` here because its shape is
    /// `context.isHoldingItem(Items.LIGHT) ? Shapes.block() : Shapes.empty()`, and
    /// `minecraft:scaffolding` reports its standing rather than its descending
    /// shape.
    ///
    /// The default returns `None`: a version with no outline census reports
    /// "unknown", never a guessed cube. A consumer falling back to a unit cube
    /// over-selects at the edges of every slab, stair, torch and kelp stalk.
    fn block_outline(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        let _ = state_id;
        None
    }

    /// The **interaction** geometry of a block state — vanilla
    /// `BlockStateBase.getInteractionShape(...).toAabbs()` — as block-local
    /// [`BlockAabb`]s, or `None` if this version does not know the state.
    ///
    /// Almost always an *empty* slice: `BlockBehaviour.getInteractionShape`
    /// defaults to `Shapes.empty()` and only the cauldron family, hoppers,
    /// scaffolding and composters override it in 26.2.
    ///
    /// # It refines the hit *face*, it does not add a hit
    ///
    /// The one caller is `BlockGetter.clipWithInteractionOverride`, which clips
    /// the outline first and only then, **if the outline already hit**, clips the
    /// interaction shape and — when that hit is nearer — replaces the resulting
    /// hit's `Direction` while keeping the outline's hit location. So this can
    /// never make an unpickable block pickable, and never moves the hit point. It
    /// is what makes a hopper's funnel and a cauldron's inner walls report the
    /// face you visually clicked rather than the outer bounding face. Treating it
    /// as a second, independent clip target is the misreading to avoid.
    ///
    /// The default returns `None`.
    fn block_interaction(&self, state_id: u32) -> Option<&'static [BlockAabb]> {
        let _ = state_id;
        None
    }

    /// The built-in **prototype** component values of an item, keyed by its
    /// canonical `minecraft:*` identifier, or `None` for an item this version's
    /// census does not know.
    ///
    /// # Why this cannot be read off the wire
    ///
    /// A clientbound `ItemStack` is an item id plus a `DataComponentPatch` — the
    /// *delta* from the item's built-in prototype component map. Three components
    /// gameplay needs live only in that prototype, so no packet ever mentions
    /// them and no wire decoder can produce them:
    ///
    /// * `minecraft:max_stack_size` — without it every stack looks like 64, so a
    ///   drag distributing buckets or shulker boxes over-fills the prediction and
    ///   is corrected by the server;
    /// * `minecraft:max_damage` — without it `isDamageableItem` is false, so
    ///   `isStackable` is true and two identically-componented swords merge;
    /// * `minecraft:equippable` — without it `ArmorSlot.mayPlace` (which is
    ///   `slot == equippable.slot() && …`) is false for every item, so **no
    ///   armour can be equipped by any click type**.
    ///
    /// This is the same shape of problem as `minecraft:tool`, which
    /// [`tool_mining`](VersionAdapter::tool_mining) exists for: the prototype is
    /// version data, so a version-free consumer reaches it here.
    ///
    /// Adapters also fold these into
    /// [`ItemComponents`](crate::ItemComponents)' effective fields at decode
    /// time, which is the route a consumer holding a stack should use. This seam
    /// is for the case where there is no stack — a creative-menu entry, a recipe
    /// output, a slot cap computed before anything is in the slot.
    ///
    /// The default returns `None`: a version with no item census reports
    /// "unknown", never a guessed 64.
    fn item_prototype(&self, item: &str) -> Option<ItemPrototype> {
        let _ = item;
        None
    }

    /// Whether a block state **blocks motion** — vanilla
    /// `BlockState.blocksMotion()` — or `None` if this version does not know the
    /// state.
    ///
    /// # Why this cannot be derived from [`block_collision`](VersionAdapter::block_collision)
    ///
    /// `blocksMotion()` is
    /// `block != COBWEB && block != BAMBOO_SAPLING && isSolid()`, and `isSolid()`
    /// reads a cached `legacySolid` flag computed once per state by
    /// `BlockBehaviour.BlockStateBase.calculateSolid()`. Only the **last** of that
    /// method's five branches is geometry:
    ///
    /// ```text
    /// forceSolidOn  -> true          // 237 blocks in 26.2, no getter, not in blocks.json
    /// forceSolidOff -> false         //   8 blocks
    /// cache == null -> false         //  23 `dynamicShape()` blocks
    /// shape empty   -> false
    /// else mean bbox extent >= 0.7291666666666666 || Ysize >= 1.0
    /// ```
    ///
    /// Deriving the answer from a shape table alone therefore gets **2,618 of
    /// 26.2's 32,366 states, across 202 blocks**, wrong — measured, not estimated,
    /// in `crates/protocol/v770/tests/block_physics.rs`. Every sign, hanging sign,
    /// banner, wall, pressure plate, chain, lantern, lightning rod, dead coral,
    /// *open* fence gate, cake, bell, conduit and turtle egg reads as
    /// non-blocking; azalea, big dripleaf, chorus plant/flower, end rod, snow and
    /// scaffolding read as blocking.
    ///
    /// `0.7291666666666666` is exactly `(1 + 1 + 3/16) / 3` — the mean extent of a
    /// ladder's collision box. The threshold has that value *because* a ladder
    /// lands on it, and `Blocks.LADDER` calls `forceSolidOff()` precisely because
    /// landing on it produces the wrong answer.
    ///
    /// The default returns `None`: a version with no solidity census reports
    /// "unknown". A consumer falling back to the geometry derivation should say so
    /// — that fallback is wrong for the 202 blocks above.
    fn block_blocks_motion(&self, state_id: u32) -> Option<bool> {
        let _ = state_id;
        None
    }

    /// `BubbleColumnBlock`'s `DRAG_DOWN` property for `state_id`, or `None` when the
    /// state is not a bubble column (which is every state but two).
    ///
    /// `Some(true)` is the magma-block drain, `Some(false)` the soul-sand lift. The
    /// consumer is [`lodestone_physics::CollisionView::bubble_column`], which drives
    /// the vertical impulse in `apply_bubble_column`. Issue #199.
    ///
    /// # Why this is a seam and not a name lookup
    ///
    /// Every other block-physics constant in this engine is keyed by block *name*
    /// (`block_physics`), because friction and speed factors are per-block. This one
    /// cannot be: both bubble-column states carry the same name and differ only in a
    /// property, and the property is the entire payload. `drag=true` versus
    /// `drag=false` is the difference between an elevator and a drowning trap.
    ///
    /// Vanilla resolves the base block (soul sand versus magma) into this boolean
    /// **once, at block-update time**, in `BubbleColumnBlock.getColumnState`. The
    /// entity-side code reads only the boolean and never looks below the column, so a
    /// consumer needs nothing else — in particular there is no "doubled for magma"
    /// term, despite that being a natural guess.
    ///
    /// The default returns `None` for every state: a version with no bubble-column
    /// data reports "no column anywhere", which degrades to the pre-#199 behaviour
    /// (a bubble column moves you like the plain water it already classifies as)
    /// rather than to a wrong impulse.
    fn block_bubble_column_drag(&self, state_id: u32) -> Option<bool> {
        let _ = state_id;
        None
    }
}
