//! Session UI payloads carried by client events.

use uuid::Uuid;

use crate::*;

/// One unlocked recipe, from the recipe book add packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeBookEntry {
    /// The server's `RecipeDisplayId` — the handle
    /// [`ClientEvent::RecipeBookRemoved`] and
    /// [`crate::ClientAction::PlaceRecipe`] both use. **Not** a recipe
    /// `Identifier`: 26.x replaced the name with a per-session index.
    pub display_id: i32,
    /// Item ids the recipe's result slot can display, retaining registry
    /// provenance. Usually one; a display can
    /// legitimately offer several (a `composite`, or a tag-driven slot).
    pub result_items: Vec<ItemId>,
    /// Item ids the display's trailing crafting-station/furnace slot-display
    /// can show — the small corner icon a recipe-unlock toast draws (a crafting
    /// table, furnace, etc.). Every `RecipeDisplay` variant carries this as its
    /// final `SlotDisplay`. Usually one entry; empty for a display whose station
    /// slot is itself `empty` or unresolved.
    pub station_items: Vec<ItemId>,
    /// The recipe-book group this entry shares a stacked button with, or `None`
    /// when the entry stands alone.
    ///
    /// A group is what makes the four wood-plank recipes collapse into one
    /// button that cycles. The wire encoding is an optional VarInt where `0`
    /// means absent and a present value `v` is written `v + 1`; the offset is
    /// already removed here, so `Some(0)` is group zero.
    pub group: Option<i32>,
    /// Which recipe-book tab the entry belongs to — the book category index,
    /// not the crafting-book *type*.
    pub category: i32,
    /// The ingredient sets a player must have already unlocked before this
    /// entry is shown, in wire order, or `None` when the entry states no
    /// requirement.
    ///
    /// This is the recipe book's own progressive-reveal gate, not the recipe's
    /// inputs: the display's inputs are what
    /// [`result_items`](Self::result_items) and the display walk cover. A
    /// [`RegistrySet::Tag`](crate::RegistrySet::Tag) arm names a tag whose
    /// membership is not on the wire.
    pub crafting_requirements: Option<Vec<crate::RegistrySet>>,
    /// Whether this unlock should raise a toast (`flags` bit 0).
    pub notification: bool,
    /// Whether its recipe-book tab should highlight (`flags` bit 1).
    pub highlight: bool,
}

/// One villager trade, from the merchant offers packet.
///
/// Note the arithmetic fields are **big-endian `i32`s on the wire, not VarInts** —
/// vanilla's own merchant-offer codec writes a fixed-width int for `uses`, its
/// own max-uses field, `xp`,
/// its own special-price-diff field, and `demand`, which is unusual enough in this protocol that
/// a VarInt-by-default encoder or decoder gets all five wrong at once.
#[derive(Debug, Clone, PartialEq)]
pub struct MerchantOffer {
    /// First input: `(item registry id, count)`.
    pub cost_a: (i32, i32),
    /// Optional second input.
    pub cost_b: Option<(i32, i32)>,
    /// What the trade produces.
    pub result: Option<ItemStack>,
    /// Whether the trade is currently exhausted.
    pub out_of_stock: bool,
    /// Times used since the last restock.
    pub uses: i32,
    /// Uses before it locks.
    pub max_uses: i32,
    /// Villager xp granted.
    pub xp: i32,
    /// Demand/reputation price adjustment, in items.
    pub special_price_diff: i32,
    /// Demand price multiplier.
    pub price_multiplier: f32,
    /// Accumulated demand.
    pub demand: i32,
}

/// One statistic the server reported, from the award-stats packet.
///
/// The wire carries two registry ids — a `stat_type` and a value id whose
/// registry *depends on that type*. The
/// adapter resolves both, and `value` is `None` when the value registry is one
/// this build has no table for. That is not an error: the count is still usable
/// and a screen keyed on `stat_type` alone (the game's "General" tab is entirely
/// `minecraft:custom`) does not need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatAward {
    /// The `minecraft:stat_type`, e.g. `minecraft:custom` or `minecraft:mined`.
    pub stat_type: Identifier,
    /// The statistic's value key, e.g. `minecraft:bell_ring` under
    /// `minecraft:custom` or `minecraft:stone` under `minecraft:mined`.
    pub value: Option<Identifier>,
    /// The absolute count, not a delta.
    pub count: i32,
}

/// What a `custom_chat_completions` update does to the current set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatCompletionsAction {
    /// Add these entries.
    Add,
    /// Remove these entries.
    Remove,
    /// Replace the whole set with these entries.
    Set,
}

/// Which server sample series a `debug_sample` batch belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebugSampleKind {
    /// Tick-time sampling — the only kind 26.2 defines.
    TickTime,
}

/// One entry of the server-links packet.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerLink {
    /// What kind of link this is.
    pub kind: ServerLinkKind,
    /// The URL, validated at packet or chat-component ingress.
    pub url: ServerLinkUrl,
}

/// A syntactically valid URL supplied by a server or interactive chat component.
///
/// The underlying [`url::Url`] stays private so downstream crates cannot replace
/// the validated value with an arbitrary string between confirmation and the
/// platform browser handoff.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerLinkUrl(url::Url);

/// Why an untrusted server/chat URL could not become a [`ServerLinkUrl`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseServerLinkUrlError {
    /// The value is not an absolute syntactically valid URL.
    #[error("invalid URL: {0}")]
    Invalid(#[from] url::ParseError),
    /// Browser handoff is limited to web links; executable and local-resource
    /// schemes must never cross this boundary.
    #[error("unsupported URL scheme {0}")]
    UnsupportedScheme(String),
}

impl ServerLinkUrl {
    /// Parses and validates an untrusted URL at its ingress boundary.
    pub fn parse(value: &str) -> Result<Self, ParseServerLinkUrlError> {
        let url = url::Url::parse(value)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ParseServerLinkUrlError::UnsupportedScheme(
                url.scheme().to_owned(),
            ));
        }
        Ok(Self(url))
    }

    /// The normalized URL spelling for display or final platform handoff.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for ServerLinkUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod server_link_url_tests {
    use super::ServerLinkUrl;

    #[test]
    fn absolute_urls_validate_and_relative_or_malformed_values_do_not() {
        let url = ServerLinkUrl::parse("https://example.invalid/path?q=one")
            .expect("absolute URL is valid");
        assert_eq!(url.as_str(), "https://example.invalid/path?q=one");
        assert!(ServerLinkUrl::parse("/relative/path").is_err());
        assert!(ServerLinkUrl::parse("not a URL").is_err());
        assert!(ServerLinkUrl::parse("javascript:alert(1)").is_err());
        assert!(ServerLinkUrl::parse("file:///private/etc/passwd").is_err());
    }
}

/// A server link's label: one of vanilla's known kinds, or a custom component.
///
/// The wire is `ByteBufCodecs.either`, a boolean where `true` means *Left* — and
/// Left is the **known** id, not the custom label. Getting that polarity
/// backwards produces a plausible-looking decode of the wrong half.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerLinkKind {
    /// One of vanilla's ten `KnownLinkType`s, by id.
    Known(i32),
    /// A server-authored label.
    Custom(Text),
}

/// Whether a `waypoint` packet starts tracking, stops tracking, or updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointOperation {
    /// Start tracking.
    Track,
    /// Stop tracking.
    Untrack,
    /// Update an already-tracked waypoint.
    Update,
}

/// One tracked waypoint, from the tracked waypoint packet.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedWaypoint {
    /// The waypoint's identity: a player's UUID, or a free-form string for a
    /// non-entity waypoint. The wire is a boolean discriminant, `true` for UUID.
    pub id: WaypointId,
    /// The icon style, a `minecraft:waypoint_style` key.
    pub style: Identifier,
    /// Packed RGB tint, when the server overrode the style's own colour.
    pub color: Option<u32>,
    /// Where the waypoint is, at whatever precision the server chose to send.
    pub position: WaypointPosition,
}

/// A waypoint's identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WaypointId {
    /// An entity's UUID — vanilla's own locator-bar waypoints.
    Entity(Uuid),
    /// A free-form name.
    Named(String),
}

/// How precisely a waypoint's position is known.
///
/// Vanilla degrades deliberately with distance: a nearby waypoint sends exact
/// coordinates, a distant one only its chunk, and one past the tracking range
/// only a compass bearing. A consumer must render all four — treating
/// [`Self::Empty`] or [`Self::Azimuth`] as "no position" would make the locator
/// bar go blank exactly when it is most useful.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaypointPosition {
    /// No position at all.
    Empty,
    /// Exact block position.
    Exact(BlockPos),
    /// Chunk position only.
    Chunk(ChunkPos),
    /// Compass bearing in radians only.
    Azimuth(f32),
}

/// One icon drawn over a filled map, from vanilla's `MapDecoration`.
#[derive(Debug, Clone, PartialEq)]
pub struct MapDecoration {
    /// The `minecraft:map_decoration_type` registry key (e.g.
    /// `minecraft:player`, `minecraft:banner_red`), resolved from the wire's
    /// numeric id.
    pub kind: Identifier,
    /// Position across the map, as vanilla's signed byte in the ±127 space that
    /// spans the whole 128-pixel width (so 2 wire units ≈ 1 pixel).
    pub x: i8,
    /// Position down the map, same space as [`Self::x`].
    pub y: i8,
    /// Facing, 0–15 in sixteenths of a turn. Vanilla masks the wire byte with
    /// `& 15`, so this is always in range.
    pub rotation: u8,
    /// Custom label (a named banner), if any.
    pub name: Option<Text>,
}

/// A rectangular sub-region of a map's 128×128 colour grid, from vanilla's
/// `MapItemSavedData.MapPatch`.
///
/// **This is a sub-rectangle, not the whole frame.** Vanilla only ever sends the
/// dirty columns, so a moving player produces a tall 1-or-2-column-wide patch,
/// and treating `colors` as a full 16 384-byte image reads garbage. Index it as
/// `colors[x + y * width]` and offset by `start_x`/`start_y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapPatch {
    /// Left edge of the patch within the 128-wide grid.
    pub start_x: u8,
    /// Top edge of the patch within the 128-tall grid.
    pub start_y: u8,
    /// Patch width in pixels, always ≥ 1 (a zero width is how the wire spells
    /// "no patch", which decodes to `None` instead).
    pub width: u8,
    /// Patch height in pixels.
    pub height: u8,
    /// `width * height` map-palette colour indices, row-major.
    pub colors: Vec<u8>,
}

/// Which frame vanilla draws around an advancement's icon — the wire ordinal
/// order of `AdvancementType`.
///
/// **The ordinals are `TASK`, `CHALLENGE`, `GOAL`**, which is not the order the
/// three are usually listed in; reading it as task/goal/challenge swaps the two
/// rarest frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdvancementFrame {
    /// Ordinal 0 — the plain square frame.
    Task,
    /// Ordinal 1 — the spiked frame.
    Challenge,
    /// Ordinal 2 — the rounded frame.
    Goal,
}

impl AdvancementFrame {
    /// From the wire ordinal (`FriendlyByteBuf::readEnum`, a VarInt).
    #[must_use]
    pub const fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => Self::Task,
            1 => Self::Challenge,
            2 => Self::Goal,
            _ => return None,
        })
    }
}

/// The presentation half of an advancement, from vanilla's `DisplayInfo`.
///
/// # `x`/`y` exist only here
///
/// 26.2's advancement JSON on disk carries no position — vanilla computes the
/// tidy-tree layout server-side in `TreeNodePosition` and writes the result to
/// the wire. So these two floats are the *only* source of vanilla's own layout,
/// which is what makes this decode load-bearing rather than cosmetic.
///
/// # Field order is not the datapack's
///
/// Vanilla's own display-info network serializer writes title, description,
/// icon, frame, an
/// `int` flag word, the optional background, then x and y. Its own
/// "announce chat" field is
/// **not on the wire at all** (vanilla's reader hardcodes `false`), and the flag
/// word is a raw big-endian `int`, not a byte.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementDisplay {
    /// Title component.
    pub title: Text,
    /// Description component.
    pub description: Text,
    /// The icon stack (`ItemStackTemplate`: item, count, components).
    pub icon: ItemStack,
    /// Frame shape.
    pub frame: AdvancementFrame,
    /// Tab background texture, present on root advancements only.
    pub background: Option<Identifier>,
    /// Whether completing it pops a toast.
    pub show_toast: bool,
    /// Whether it is hidden until obtained.
    pub hidden: bool,
    /// Server-computed tree column, in advancement-grid units.
    pub x: f32,
    /// Server-computed tree row, in advancement-grid units.
    pub y: f32,
}

/// One node of the advancement tree, from vanilla's `AdvancementHolder`.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvancementEntry {
    /// The advancement id, e.g. `minecraft:story/mine_stone`.
    pub id: Identifier,
    /// Parent id; `None` makes this a root (a tab).
    pub parent: Option<Identifier>,
    /// Presentation, absent for an advancement vanilla does not draw (recipe
    /// unlocks). A node without display is hidden by vanilla's own screen.
    pub display: Option<AdvancementDisplay>,
    /// AND-of-ORs completion shape: done when every group has one obtained
    /// criterion.
    pub requirements: Vec<Vec<String>>,
    /// Vanilla's own "sends telemetry event" bit, carried because it is on the wire.
    pub sends_telemetry_event: bool,
}

/// Which of the client's event routers claim a [`ClientEvent`].
///
/// # Why this lives in `lodestone-model` and not next to the routers
///
/// [`ClientEvent`] is `#[non_exhaustive]`, which means **no downstream crate can
/// write an exhaustive match over it** — every consumer is *forced* to end in a
/// `_ =>` arm, and a terminal wildcard is indistinguishable from a decision. That
/// attribute is exactly why a new variant used to compile with zero routing arms
/// anywhere and reach nothing. Inside the defining crate the attribute does not
/// bind, so [`route`] can be exhaustive here while the attribute keeps protecting
/// external plugin code. The layering cost is real and accepted: the leaf model
/// crate names its consumers. It buys the one property nothing else can — a
/// **compile error** when a variant is added and not routed.
///
/// # Why booleans and not an enum
///
/// The claims are **not exclusive**, so an enum would force a false choice and
/// the table would begin by losing information:
///
/// * [`ClientEvent::Login`] is folded by `lodestone_ecs::ingest` (the entity id
///   and the `EntityIndex` entry), *and* by `lodestone_ecs::session` (the session
///   scalars), *and* forwarded to the shell as `NetUpdate::LoggedIn`.
/// * [`ClientEvent::EntityPassengersChanged`] is `ingest` (the
///   `Passengers`/`Vehicle` component pair) *and* `session` (the local player's
///   own `Riding` scalar).
///
/// Three disjoint writes off one event is normal here. A double *fold* of the
/// same state is the thing to avoid, and no boolean can tell you that — only
/// reading the two systems can.
///
/// # What each flag is worth
///
/// | flag | enforced by |
/// |---|---|
/// | [`ingest`](Route::ingest) | `lodestone_ecs::ingest::handles_event` *is* this flag — a live derivation |
/// | [`session`](Route::session) | `lodestone_ecs::session::handles_event` *is* this flag — a live derivation |
/// | [`shell`](Route::shell) | a `debug_assert!` on the catch-all of `lodestone_shell::net::forward` |
/// | [`shell_conditional`](Route::shell_conditional) | nothing; it exists only to keep that assert correct |
/// | [`client`](Route::client) | nothing; documentation, so that [`Route::NOWHERE`] means what it says |
/// One recipe book's stored UI state, from `RecipeBookSettings.TypeSettings`.
///
/// Both fields default to `false`, which is vanilla's own default for a book no
/// server has reported: closed, unfiltered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecipeBookTypeSettings {
    /// Whether this book is open.
    pub open: bool,
    /// Whether this book's "only show craftable" filter is active.
    pub filtering: bool,
}
