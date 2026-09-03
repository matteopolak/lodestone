use crate::event::{EquipmentSlot, ProfileProperty};
use crate::ids::ResourceKey;
use crate::text::Text;

/// A canonical item stack.
///
/// Carries the stable item key, count, and the subset of the item's data
/// components this client models ([`ItemComponents`]). Components a build does
/// not understand are not represented field-by-field; their presence is instead
/// summarised by [`ItemComponents::has_unmodeled`], so an item that carries an
/// unrecognised component still yields a usable stack rather than tearing down
/// the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemStack {
    /// Canonical item key, for example `minecraft:stone`.
    pub item: ResourceKey,
    /// Number of items in the stack.
    pub count: u32,
    /// The modeled subset of the stack's data-component patch.
    pub components: ItemComponents,
}

impl ItemStack {
    /// Creates a stack of `item` × `count` with no components.
    #[must_use]
    pub fn new(item: ResourceKey, count: u32) -> Self {
        Self {
            item,
            count,
            components: ItemComponents::default(),
        }
    }
}

/// The subset of an item stack's data-component patch this client models.
///
/// Modern item stacks carry an open-ended, versioned set of data components.
/// This models only the fields that drive gameplay surfaces — the HUD, tooltips,
/// and tool behaviour — and deliberately does not attempt to represent every
/// component. [`has_unmodeled`](Self::has_unmodeled) records whether the wire
/// carried at least one component this build could not decode, so a consumer can
/// distinguish a genuinely bare stack from one that was only partially understood.
///
/// # Two kinds of field live here: *patch* fields and *effective* fields
///
/// Most fields are the raw patch — what the wire said, and nothing else.
/// [`tool`](Self::tool) is explicitly patch-shaped ([`ToolPatch::Inherited`]
/// means "the wire said nothing"), because evaluating a tool needs the version's
/// block tags and block-registry ids and so cannot happen at decode time; that
/// lives behind `VersionAdapter::tool_mining`.
///
/// [`max_stack_size`](Self::max_stack_size), [`max_damage`](Self::max_damage)
/// and [`equippable`](Self::equippable) are different: they are **effective**
/// values, the item's built-in prototype component already folded with the
/// patch, resolved by the adapter at decode time. They can be, because each is a
/// plain scalar needing no tag or state lookup to interpret. `None` means "this
/// adapter has no prototype census for this item", never a guessed default —
/// see each field's docs for why guessing is the trap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemComponents {
    /// `minecraft:item_model`'s item-definition id. When present, this replaces
    /// the stack's base item id for client-side `assets/<namespace>/items/*.json`
    /// lookup; it does not change the item used for gameplay.
    pub item_model: Option<ResourceKey>,
    /// `minecraft:custom_model_data`'s numeric selector list, stored as raw
    /// IEEE-754 bits so this version-free, equality-bearing model never has to
    /// pretend `f32` is `Eq`. Item-model `range_dispatch` reads index zero.
    pub custom_model_data: Vec<u32>,
    /// A player- or server-assigned display name overriding the item's default.
    pub custom_name: Option<Text>,
    /// `minecraft:lore`'s authored tooltip lines, in wire order.
    ///
    /// Each entry remains a full [`Text`] tree rather than a flattened string:
    /// nested RGB colours and explicit formatting overrides must survive until
    /// the tooltip applies vanilla's default dark-purple italic parent style.
    /// An empty vector means the patch carries no lore lines.
    pub lore: Vec<Text>,
    /// Accumulated durability damage; the item's remaining durability is its
    /// max damage minus this value. `None` when the stack carries no damage
    /// component (either undamaged or not damageable).
    pub damage: Option<u32>,
    /// Enchantments applied to the stack, in wire order.
    pub enchantments: Vec<ItemEnchantment>,
    /// `minecraft:dyed_color`'s RGB, when the patch carries one — leather
    /// armour (and any other item whose base material takes dye) coloured by
    /// a dye or a dyeing table. Low 24 bits are the colour; vanilla's own
    /// dyed-color network codec is a bare int codec, so this is the raw wire
    /// int, not yet split into RGB bytes — `lodestone_render::entity::armour_layer_tint_with_dye`
    /// does that, matching vanilla's own armour-layer renderer's
    /// `dyeColor & 0x00FFFFFF != 0` "is this dyed" gate.
    /// `None` when the stack carries no dye (an undyed leather item, or any
    /// non-dyeable item) — a different state from a dye that resolves to
    /// black (`Some(0)`), which vanilla also treats as "undyed" downstream
    /// (see `armour_layer_tint_with_dye`'s own doc for that quirk).
    pub dyed_color: Option<u32>,
    /// `minecraft:trim`'s material and pattern, when the patch carries one — a
    /// smithing-table armour trim.
    ///
    /// `None` for untrimmed armour and for every non-armour item. Trim is
    /// **decoded rather than treated as unmodeled** because the component patch's
    /// clientbound codec cannot skip an unknown component (see
    /// [`has_unmodeled`](Self::has_unmodeled)): a trimmed stack used to truncate
    /// decoding of the rest of the packet, not just lose its trim.
    pub trim: Option<ArmorTrim>,
    /// `minecraft:map_id`: which saved map a `filled_map` stack shows.
    ///
    /// `None` for every other item. Decoded for the same reason as
    /// [`trim`](Self::trim) rather than for the picture's sake: the clientbound
    /// component patch cannot skip an unknown component, so a filled map sitting
    /// in any inventory used to truncate decoding of the rest of the packet.
    /// Without it the renderer can only draw the lowest-numbered known map.
    pub map_id: Option<i32>,
    /// `minecraft:pot_decorations`: the four sherds facing out of a
    /// `minecraft:decorated_pot` stack.
    ///
    /// `None` for every other item and for a pot crafted from four plain bricks
    /// (which carries no component at all). Decoded for the same reason as
    /// [`trim`](Self::trim) and [`map_id`](Self::map_id) rather than for the
    /// picture's sake: the clientbound component patch cannot skip an unknown
    /// component, and an advancement whose icon is `minecraft:decorated_pot`
    /// therefore truncated the whole `update_advancements` packet — which is a
    /// **join-blocking** failure, not a cosmetic one, because that packet arrives
    /// during the initial world load.
    pub pot_decorations: Option<PotDecorations>,
    /// **Effective** `minecraft:potion_contents` colour: the opaque ARGB a potion
    /// item's `minecraft:potion` tint source resolves to (vanilla's own
    /// potion-color resolution), already folded with the potion's own built-in
    /// effect list and any custom-effects/custom-color fields the patch carried.
    ///
    /// `None` when the patch carries no `minecraft:potion_contents` at all (a
    /// non-potion item, or a potion stack whose patch is otherwise empty) — the same
    /// "absent means take the tint source's own JSON default" contract
    /// [`dyed_color`](Self::dyed_color) uses, not a guessed colour. Decoded rather
    /// than treated as unmodeled for the same reason as [`trim`](Self::trim) and
    /// [`pot_decorations`](Self::pot_decorations): the clientbound component patch
    /// cannot skip an unknown component, and `minecraft:potion_contents` used to
    /// truncate the rest of the packet from a potion stack onward.
    ///
    /// This is an *effective* value (like [`max_stack_size`](Self::max_stack_size))
    /// rather than the raw patch, because mixing needs the potion registry's own
    /// effect census (`lodestone_data::potion`), which is version data this
    /// version-free type does not own — evaluating at decode time is the same
    /// tradeoff [`max_stack_size`] already makes.
    pub potion_color: Option<u32>,
    /// The raw `minecraft:potion_contents` `potion` field: the network
    /// `minecraft:potion` registry id itself (vanilla's potion network
    /// codec's registry reference), not [`potion_color`](Self::potion_color)'s
    /// already-mixed colour.
    ///
    /// [`potion_color`](Self::potion_color) alone cannot drive a tooltip title or
    /// effect lore: `swiftness`/`long_swiftness`/`strong_swiftness` all mix to the
    /// same colour (vanilla's potion-color resolution only sees the effect list, and all three
    /// share one) but must resolve to three different lore bodies (different
    /// duration, and `strong_swiftness` a different amplifier). `None` when the
    /// patch carries no `minecraft:potion_contents`, or one with no `potion`
    /// holder (a bare custom-effects patch) — the same absent-means-no-component
    /// contract every other patch field here uses.
    pub potion: Option<i32>,
    /// `minecraft:profile`: a `player_head`/`player_wall_head` stack's owner
    /// identity — the same `name`/`id`/`textures` shape
    /// [`crate::event::PlayerListEntry::properties`] already carries for a
    /// remote player's tab-list entry, decoded here for a held or displayed
    /// head instead. `properties` is where the skin comes from: an online-mode
    /// server signs a `minecraft:textures` entry whose value is base64 JSON
    /// (see [`ProfileProperty`]'s own doc for the two traps in that blob).
    ///
    /// `None` for every non-head item and for a head with no owner set (the
    /// plain "Player Head" block/item). Decoded rather than treated as
    /// unmodeled for the same reason as [`trim`](Self::trim),
    /// [`map_id`](Self::map_id) and [`pot_decorations`](Self::pot_decorations):
    /// the clientbound component patch cannot skip an unknown component, so a
    /// player head in any container — not just one placed as a block —
    /// truncated the rest of the packet from that slot onward.
    ///
    /// **Only the identity half of the wire component is kept.** 26.2's
    /// resolvable-profile component also carries a skin patch — an optional
    /// direct resource-id override for the body/cape/elytra texture and rig,
    /// bypassing the Mojang session service entirely. Nothing in this client
    /// resolves a resource-id skin yet, so those bytes are decoded (to keep the
    /// rest of the packet aligned) and discarded rather than modeled; a
    /// consumer that needs them has no field to read here.
    pub profile: Option<ItemProfile>,
    /// An enchantment identity this client itself authored — never decoded off
    /// the wire. See [`AuthoredEnchantment`]'s own doc for what it is, why it
    /// exists, and why it must never be confused with
    /// [`enchantments`](Self::enchantments).
    pub authored_enchantment: Option<AuthoredEnchantment>,
    /// What this stack's component *patch* said about `minecraft:tool`.
    ///
    /// Almost always [`ToolPatch::Inherited`] — see that type's docs; a plain
    /// vanilla pickaxe carries no `minecraft:tool` on the wire at all.
    pub tool: ToolPatch,
    /// **Effective** `minecraft:max_stack_size`: how many of this item fit in one
    /// slot. `None` when the producing adapter has no item-prototype census.
    ///
    /// This is a *prototype* component — vanilla's default item-component set
    /// sets it to `64` for every item and individual items override it — so a
    /// clientbound patch essentially never mentions it and it cannot be
    /// recovered from the wire. Guessing `64` is wrong for a great many items
    /// (`minecraft:water_bucket` and every shulker box are `1`,
    /// `minecraft:egg` is `16`), and guessing `1` — vanilla's own fallback when
    /// the component is genuinely absent — is
    /// wrong for almost everything else. A consumer that gets `None` should
    /// treat the cap as unknown rather than substituting either.
    pub max_stack_size: Option<u32>,
    /// **Effective** `minecraft:max_damage`: the item's durability, or `None`
    /// both when the item is not damageable *and* when the adapter has no
    /// prototype census (the two are indistinguishable here by design — an
    /// undamageable item and an unknown one both have no durability to show).
    ///
    /// Also a prototype component, and the gate on vanilla's own
    /// damageable-item check and therefore its stackability check: while
    /// this is absent, two identically-componented swords look stackable and
    /// merge into a stack of two.
    pub max_damage: Option<u32>,
    /// **Effective** `minecraft:equippable` slot: where this item is worn, or
    /// `None` for an item that is not equippable (or an adapter with no
    /// prototype census).
    ///
    /// Also a prototype component. Vanilla's armour-slot placement check
    /// requires the target slot to equal the item's own equip slot, so
    /// while this is `None` **no item can
    /// be placed in any armour slot by any click type**.
    ///
    /// Only the slot is carried. Vanilla's equippable component also has an
    /// allowed-entities set — `minecraft:wolf_armor` is wolves only,
    /// `minecraft:saddle` is `#minecraft:can_equip_saddle` — which
    /// vanilla's own equip-eligibility check additionally requires. Every restricted item
    /// in 26.2 is in a non-humanoid slot ([`EquipmentSlot::Body`] or
    /// [`EquipmentSlot::Saddle`]) and so cannot reach a player armour slot on
    /// the slot check alone, but a consumer wanting the restriction itself must
    /// ask the version seam (`VersionAdapter::item_prototype`), not this field.
    ///
    /// **[`EquipmentSlot::Body`] is not chest armour.** Vanilla gates humanoid
    /// armour on a distinct "humanoid armour" slot-type grouping, which covers
    /// [`Feet`](EquipmentSlot::Feet)/[`Legs`](EquipmentSlot::Legs)/[`Chest`](EquipmentSlot::Chest)/[`Head`](EquipmentSlot::Head)
    /// and deliberately **excludes** the body slot (animal armour). Folding
    /// `"body"` into `Chest` makes wolf armour and horse armour placeable in a
    /// player's chestplate slot.
    pub equippable: Option<EquipmentSlot>,
    /// `minecraft:custom_data`: the plugin/datapack NBT blob, kept **opaque** as
    /// the raw network-NBT bytes (root tag id then payload, exactly as
    /// vanilla's own network-NBT writer wrote them).
    ///
    /// Nothing in this client interprets it, and nothing should: it is arbitrary
    /// server-defined data. It is carried rather than discarded only so a
    /// consumer that wants to inspect or re-emit it can, and it is decoded at all
    /// for a much sharper reason — the clientbound component patch cannot skip an
    /// unknown component, so this was a **decode cliff on the most-stamped
    /// component in the game**. Every Bukkit/Paper plugin that marks a GUI item
    /// sets it, so a lobby hotbar truncated the rest of whatever packet carried
    /// it. `None` means the patch did not mention it.
    ///
    /// Stored as bytes rather than a parsed `Nbt` so [`ItemComponents`] keeps its
    /// `Eq`: NBT carries floats, which are not `Eq`.
    pub custom_data: Option<Vec<u8>>,
    /// `minecraft:repair_cost`: the anvil's "prior work penalty" counter —
    /// vanilla's anvil-menu repair-cost formula doubles-and-adds-one
    /// each time an item is worked, and the anvil's XP cost sums both
    /// operands' resulting values.
    ///
    /// **Server-side bookkeeping only.** The wire component exists
    /// (`minecraft:repair_cost`, a bare VarInt) but this build's protocol
    /// decoder currently only consumes it for byte-alignment and does not
    /// surface it (see `crates/versions/26.2/src/adapter/inventory.rs`'s
    /// "consumed for alignment" component group) — so a stack that arrived
    /// over the wire always reports `0` here even if a real client sent a
    /// worked item. Every stack this server itself produces (anvil/grindstone
    /// output) sets this field directly in Rust, never through a decode, so
    /// the anvil economy is internally consistent even though the value does
    /// not yet round-trip through a real client. Defaults to `0`, matching
    /// vanilla's own default of `0` when the repair-cost component is
    /// absent.
    pub repair_cost: u32,
    /// `minecraft:writable_book_content`: an unsigned book-and-quill's draft
    /// pages, in order — vanilla's writable-book-content component, a list
    /// of up to 100 filterable-string pages capped at 1024 characters each.
    ///
    /// Only the *raw* half of each `Filterable` is kept — the *filtered*
    /// alternate exists for a chat-filtering service this crate does not run,
    /// the same "no filtering service, so raw is the only value that matters"
    /// call [`crate::text`]'s own chat handling already makes. `None` for
    /// every item but a `minecraft:writable_book` that has been edited at
    /// least once; a freshly crafted one carries no component at all.
    ///
    /// Decoded rather than left unmodeled for the same reason as
    /// [`trim`](Self::trim): vanilla's writable-book-content network codec
    /// has no length prefix, so a writable book sitting in *any* container
    /// used to truncate the rest of that packet.
    pub writable_book_content: Option<Vec<String>>,
    /// `minecraft:written_book_content`: a signed book's title, author,
    /// generation and page text — vanilla's written-book-content component.
    /// `None` for every item but a `minecraft:written_book`.
    ///
    /// Decoded for the same reason as
    /// [`writable_book_content`](Self::writable_book_content): its stream
    /// codec is equally unprefixed, so a written book anywhere in an
    /// inventory used to truncate the rest of that packet.
    pub written_book_content: Option<WrittenBookContent>,
    /// `minecraft:bundle_contents`: a bundle's nested items, in slot order (index
    /// 0 is the most-recently-inserted stack — vanilla's bundle-insert
    /// routine always inserts at index 0). Empty for every non-bundle item and for an empty
    /// bundle; the two are indistinguishable here, the same "absent patch field
    /// and an explicitly-empty one collapse to the same value" convention
    /// [`enchantments`](Self::enchantments) already uses.
    ///
    /// Each entry is a **full nested `ItemStack`**, not a display-only summary —
    /// vanilla's bundle-contents network codec wire-carries a whole nested
    /// item-stack template (item, count, and its own recursive component
    /// patch) per contained item, and a bundle can legally contain another
    /// bundle (vanilla weights that nesting to discourage it, but never
    /// forbids it), which is why the nesting is real rather than flattened
    /// to one level.
    ///
    /// Decoded rather than treated as unmodeled for the same reason as
    /// [`trim`](Self::trim) and the rest of that group: vanilla's nested
    /// item-stack-template codec carries no length prefix, so a filled
    /// bundle sitting in any inventory used to truncate the rest of the
    /// packet from that slot onward.
    ///
    /// **Vanilla's own bundle-contents component tracks a selected item that
    /// never reaches the wire** — its network codec maps straight onto a
    /// constructor path that always defaults it to `-1`; the tooltip
    /// highlight vanilla's client shows is derived from local mouse/scroll
    /// state, never from this component. So there is no `selected_index`
    /// field here to carry, and there should not be one — a field for it
    /// would always read as unset from a real server.
    pub bundle_contents: Vec<ItemStack>,
    /// `minecraft:banner_patterns`: a banner or shield stack's loom-applied
    /// pattern layers, in the stack's own stored order — vanilla's banner
    /// renderer draws them in exactly that order and no other. Empty for every non-banner,
    /// non-shield item and for a plain banner carrying no patterns; the two
    /// are indistinguishable here, the same absent-patch-field-and-explicitly-
    /// empty convention [`enchantments`](Self::enchantments) already uses.
    ///
    /// Decoded rather than treated as unmodeled for the same reason as
    /// [`trim`](Self::trim) and the rest of that group:
    /// vanilla's banner-pattern-layers network codec's per-layer entry
    /// carries no length prefix,
    /// so a banner or shield sitting in *any* container — inventory, chest,
    /// shulker box, a loom's own input slot — used to truncate the rest of
    /// the packet from that slot onward.
    pub banner_patterns: Vec<BannerPatternLayer>,
    /// `minecraft:base_color`: a shield's own dye tint, independent of any
    /// [`Self::banner_patterns`] layer — vanilla's own base-color component.
    /// `None` for a
    /// never-dyed shield and for every non-shield item; stored by vanilla's
    /// own snake_case dye name, matching [`BannerPatternLayer::color`]'s
    /// convention (and, like it, the field a plain banner's own base-colour
    /// mask is derived from the *item id* rather than this component —
    /// `crate::banner_pattern` in `lodestone-render`, not here).
    pub base_color: Option<String>,
    /// `minecraft:charged_projectiles`: a crossbow's loaded arrow(s) or firework,
    /// in load order. Empty for every non-crossbow item and for an unloaded
    /// crossbow; the two are indistinguishable here, the same absent-patch-field-
    /// and-explicitly-empty convention [`enchantments`](Self::enchantments)
    /// already uses.
    ///
    /// Each entry is a **full nested `ItemStack`**, the same
    /// item-then-count-then-recursive-patch shape
    /// [`bundle_contents`](Self::bundle_contents) carries, and for the same
    /// reason: the per-entry payload has no length prefix, so a loaded crossbow
    /// sitting in any container used to truncate the rest of the packet from
    /// that slot onward.
    pub charged_projectiles: Vec<ItemStack>,
    /// `minecraft:attack_range`: the melee reach an item grants, both in and out
    /// of creative, plus the hitbox margin and the mob-wielded scale factor.
    /// `None` for every item that does not override the entity's default reach
    /// (most items — a player's own base interaction range is entity state, not
    /// an item component, so an ordinary sword carries no `minecraft:attack_range`
    /// at all).
    ///
    /// Decoded rather than treated as unmodeled for the same reason as
    /// [`trim`](Self::trim) and the rest of that group: the payload is six
    /// fixed-width floats with no length prefix, so a spear-family item in any
    /// container used to truncate the rest of the packet from that slot onward.
    pub attack_range: Option<AttackRange>,
    /// True when the stack's patch carried at least one component this build
    /// does not model, so decoding stopped early and the modeled fields above
    /// may be incomplete. The modeled fields that were decoded remain valid.
    ///
    /// For the three *effective* fields this additionally means "the patch may
    /// have overridden one of these and we could not see it": they still hold
    /// the item's prototype value, which is the best available answer, but is not
    /// guaranteed to be the effective one.
    pub has_unmodeled: bool,
}

/// `minecraft:written_book_content`'s modeled shape — vanilla's own
/// written-book-content record (`title`, `author`, `generation`, `pages`,
/// `resolved`), with each filterable field collapsed to its raw value for the
/// same reason [`ItemComponents::writable_book_content`] is: this crate runs
/// no chat-filtering service, so the *filtered* alternate is never the value
/// a consumer wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenBookContent {
    /// The book's title, as typed at signing time (≤32 characters).
    pub title: String,
    /// The signing player's plain-text display name
    /// (vanilla's book-signing handler's plain-text-name accessor), not the uuid.
    pub author: String,
    /// Copy generation: `0` original, `1` copy, `2` copy of a copy, `3`
    /// tattered — vanilla's own maximum-generation constant. Every book
    /// this crate itself signs starts at `0`; a build with no book-cloning
    /// item yet never produces `1..=3`.
    pub generation: u8,
    /// Page contents, in order, as chat components — signing turns each raw
    /// page string into a literal text component (vanilla's book-signing
    /// handler), so this is never anything
    /// richer than a literal for a book this crate produces, but the field is
    /// typed as [`Text`] because a page decoded off the wire (from a real
    /// vanilla server, or a future click/hover-bearing book) is not
    /// guaranteed to be one.
    pub pages: Vec<Text>,
    /// Whether this book's pages have finished click/hover-event resolution
    /// (vanilla's own resolved flag). Always `true` for a book this crate
    /// signs — its pages are plain literals with nothing left to resolve.
    pub resolved: bool,
}

/// The four sherds of a `minecraft:decorated_pot` — vanilla's own
/// decorated-pot record (four optional-item fields in the order `back`,
/// `left`, `right`, `front`).
///
/// # `None` means a plain brick face, not "unknown"
///
/// Vanilla's own sherd accessor maps a plain brick to an empty optional on
/// the way in, and its reverse accessor maps an empty optional back to a
/// plain brick on the way out, so a brick and a blank face are the same state
/// by construction. This type mirrors that: a `None` side is an undecorated
/// side, and it is what a pot crafted from four plain bricks decodes to.
///
/// The wire list is a fixed-length-4 codec, so a shorter list is legal and
/// its missing tail is `None` — that is vanilla's own out-of-range
/// sherd-index fallback. In practice a vanilla server always writes exactly
/// four, because its ordering helper builds a four-element list
/// unconditionally.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PotDecorations {
    /// The sherd on the pot's back face, or `None` for a plain brick.
    pub back: Option<ResourceKey>,
    /// The sherd on the pot's left face, or `None` for a plain brick.
    pub left: Option<ResourceKey>,
    /// The sherd on the pot's right face, or `None` for a plain brick.
    pub right: Option<ResourceKey>,
    /// The sherd on the pot's front face, or `None` for a plain brick.
    pub front: Option<ResourceKey>,
}

/// One stored layer of `minecraft:banner_patterns` — vanilla's own
/// banner-pattern-layer record (a registry reference to a banner pattern
/// plus a dye colour), carried the same way [`ArmorTrim`] carries its two
/// registry references:
/// as bare asset/name strings rather than the registry's own value, since
/// that is the form a renderer actually keys sprites by
/// (`lodestone_render::banner_pattern`'s `PatternLayer`/`StoredPatternLayer`).
///
/// `color` is a vanilla-style `DyeColor` snake_case name (matching vanilla's
/// own name accessor, e.g. `"light_blue"`) rather than a typed enum: this crate is the base of
/// the model/game/render layering and defines no `DyeColor` of its own —
/// `lodestone_render::banner_pattern::DyeColor` is the canonical type, and a
/// consumer there parses this string back with `DyeColor::from_name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerPatternLayer {
    /// The pattern's bare asset id (e.g. `"creeper"`), matching vanilla's
    /// own asset-id accessor for the pattern — never a full
    /// `minecraft:`-namespaced identifier, and never the numeric registry
    /// id the wire itself sends for a non-inline registry reference.
    pub pattern_asset_id: String,
    /// The layer's dye colour, by vanilla's own snake_case name.
    pub color: String,
}

/// `minecraft:profile`'s identity half — vanilla's resolvable-profile
/// component, which is either a full game profile (uuid + name +
/// properties, all present) or a partial one (each of name/id independently
/// optional, properties always present but possibly empty).
///
/// This type folds both wire shapes into one: `name` and `id` are `None`
/// exactly when the *partial* form omitted them (the full-profile form always
/// carries both), and `properties` is the property multimap either way —
/// `Vec::new()` for a partial profile that declared none, never a signal of
/// "full vs. partial" on its own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemProfile {
    /// The profile name, when the wire carried one (always present for a full
    /// profile; optional for a partial one, e.g. a head placed by uuid alone).
    pub name: Option<String>,
    /// The profile uuid, when the wire carried one (always present for a full
    /// profile; optional for a partial one, e.g. a head placed by name alone
    /// before the server resolves it).
    pub id: Option<uuid::Uuid>,
    /// The profile's property multimap — on an online-mode server, this is
    /// where `minecraft:textures` (the base64-JSON skin declaration) lives.
    /// Empty for an offline-mode server or a profile with no skin set, not a
    /// distinct state from "no properties field at all": the wire component
    /// has no such distinction either.
    pub properties: Vec<ProfileProperty>,
}

/// The melee reach a `minecraft:attack_range` component grants: minimum and
/// maximum reach, the creative-mode alternates of each, a hitbox margin and a
/// mob-wielded scale factor — six independent floats, in that wire order.
///
/// Stored as raw IEEE-754 bits, the same convention [`ItemTool::default_mining_speed`]
/// documents, so [`ItemComponents`] keeps its `Eq` impl (`f32` is not `Eq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttackRange {
    min_reach_bits: u32,
    max_reach_bits: u32,
    min_creative_reach_bits: u32,
    max_creative_reach_bits: u32,
    hitbox_margin_bits: u32,
    mob_factor_bits: u32,
}

impl AttackRange {
    /// Builds an attack-range component from its six decoded fields, in wire
    /// order.
    #[must_use]
    pub fn new(
        min_reach: f32,
        max_reach: f32,
        min_creative_reach: f32,
        max_creative_reach: f32,
        hitbox_margin: f32,
        mob_factor: f32,
    ) -> Self {
        Self {
            min_reach_bits: min_reach.to_bits(),
            max_reach_bits: max_reach.to_bits(),
            min_creative_reach_bits: min_creative_reach.to_bits(),
            max_creative_reach_bits: max_creative_reach.to_bits(),
            hitbox_margin_bits: hitbox_margin.to_bits(),
            mob_factor_bits: mob_factor.to_bits(),
        }
    }

    /// The minimum survival-mode reach, in blocks.
    #[must_use]
    pub fn min_reach(&self) -> f32 {
        f32::from_bits(self.min_reach_bits)
    }

    /// The maximum survival-mode reach, in blocks.
    #[must_use]
    pub fn max_reach(&self) -> f32 {
        f32::from_bits(self.max_reach_bits)
    }

    /// The minimum creative-mode reach, in blocks.
    #[must_use]
    pub fn min_creative_reach(&self) -> f32 {
        f32::from_bits(self.min_creative_reach_bits)
    }

    /// The maximum creative-mode reach, in blocks.
    #[must_use]
    pub fn max_creative_reach(&self) -> f32 {
        f32::from_bits(self.max_creative_reach_bits)
    }

    /// The extra distance a target's own hitbox extends the reach by.
    #[must_use]
    pub fn hitbox_margin(&self) -> f32 {
        f32::from_bits(self.hitbox_margin_bits)
    }

    /// The multiplier applied to both reaches when a non-player entity wields
    /// this item.
    #[must_use]
    pub fn mob_factor(&self) -> f32 {
        f32::from_bits(self.mob_factor_bits)
    }
}

/// A smithing-table armour trim — vanilla's own armour-trim record, which
/// holds a registry reference to a trim material plus a registry reference
/// to a trim pattern.
///
/// Both are carried as bare registry **paths** (`"iron"`, `"sentry"`), the form
/// `lodestone_assets::trim::{trim_material, trim_pattern}` keys its sprite tables
/// by, so a renderer can go straight from this to a trim sprite. Neither
/// registry reference's *value* is kept: a trim material is an asset-suffix
/// group plus a description component and a trim pattern is an asset id
/// plus a description and a `decal` flag, all of which the asset layer
/// already has statically for the eleven materials and eighteen patterns
/// 26.2 ships.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArmorTrim {
    /// Trim material registry path, e.g. `"netherite"`.
    pub material: String,
    /// Trim pattern registry path, e.g. `"silence"`.
    pub pattern: String,
}

/// What a stack's component patch said about `minecraft:tool`.
///
/// # This is a *patch*, not the effective component
///
/// A clientbound stack carries only the **delta** from the item's built-in
/// prototype component map, and vanilla puts a tool's `minecraft:tool` in that
/// prototype (set by the tool-material's own property-application step). So
/// an ordinary diamond
/// pickaxe arrives with an *empty* patch and this field is
/// [`Inherited`](Self::Inherited) — the mining speed still has to come from
/// somewhere, and that somewhere is version data the protocol crate owns.
///
/// Read this only through a version adapter's mining seam
/// (`VersionAdapter::tool_mining`), which folds the prototype and this data
/// patch together the way vanilla's own component lookup does. Treating
/// `Inherited` as "no tool" is the trap: it makes every real pickaxe mine at
/// bare-hand speed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolPatch {
    /// The patch neither set nor removed `minecraft:tool`; the item's built-in
    /// prototype tool component (if any) applies unchanged. This is the case
    /// for every ordinary vanilla tool.
    #[default]
    Inherited,
    /// The patch set an explicit `minecraft:tool`, replacing the prototype
    /// wholesale (`/give …[minecraft:tool={…}]`, datapack-authored items).
    Set(ItemTool),
    /// The patch removed `minecraft:tool` (`/give …[!minecraft:tool]`), so the
    /// stack mines like a bare hand regardless of what item it is.
    ///
    /// Removals are the tail of the patch, so this is only observable when the
    /// patch decoded to completion — an unmodeled component
    /// ([`ItemComponents::has_unmodeled`]) stops decoding before the removal
    /// list and leaves this as [`Inherited`](Self::Inherited).
    Removed,
}

/// A decoded `minecraft:tool` data component.
///
/// The mining speed for a block state is the first rule whose block set matches
/// **and** carries a speed, else [`default_mining_speed`](Self::default_mining_speed);
/// correctness-for-drops is the first rule whose block set matches **and**
/// carries a `correct_for_drops`, else `false`. Rule order is therefore
/// load-bearing and is preserved exactly as it arrived.
///
/// Evaluating a rule needs to know which blocks are in a tag, which is
/// version/session data — so the evaluation itself lives behind
/// `VersionAdapter::tool_mining`, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemTool {
    /// Match rules, in wire order. First match wins.
    pub rules: Vec<ToolRule>,
    /// Raw IEEE-754 bits of vanilla's own default-mining-speed field
    /// (vanilla default `1.0`).
    /// See [`ItemTool::default_mining_speed`] for why this is stored as bits.
    default_mining_speed_bits: u32,
    /// Vanilla's own damage-per-block field: durability spent per block broken.
    pub damage_per_block: u32,
    /// Vanilla's own creative-destroy-blocks flag. Creative-mode policy only; it does not
    /// enter the break-time formula.
    pub can_destroy_blocks_in_creative: bool,
}

impl ItemTool {
    /// Builds a tool component from its decoded fields.
    #[must_use]
    pub fn new(
        rules: Vec<ToolRule>,
        default_mining_speed: f32,
        damage_per_block: u32,
        can_destroy_blocks_in_creative: bool,
    ) -> Self {
        Self {
            rules,
            default_mining_speed_bits: default_mining_speed.to_bits(),
            damage_per_block,
            can_destroy_blocks_in_creative,
        }
    }

    /// Vanilla's own default-mining-speed field: the speed used when no rule matches.
    ///
    /// Stored as raw bits so [`ItemComponents`] — and therefore [`ItemStack`],
    /// which the client compares for equality in inventory reconciliation and
    /// equipment diffing — keeps its `Eq` impl. `f32` is not `Eq`; the bits are,
    /// and comparing them is *stricter* than comparing floats, which is the
    /// right default for "did the server send us the same stack?".
    #[must_use]
    pub fn default_mining_speed(&self) -> f32 {
        f32::from_bits(self.default_mining_speed_bits)
    }
}

/// One vanilla tool rule: a block set, an optional speed override, and an optional
/// correct-for-drops verdict. Either optional field may be absent — a rule that
/// only denies drops carries no speed, and a rule that only overrides speed
/// carries no verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRule {
    /// The blocks this rule applies to.
    pub blocks: ToolBlocks,
    /// Raw IEEE-754 bits of the optional speed (see
    /// [`ItemTool::default_mining_speed`] for why bits).
    speed_bits: Option<u32>,
    /// Whether a match makes this tool correct for the block's drops. `None`
    /// means the rule is silent and later rules decide.
    pub correct_for_drops: Option<bool>,
}

impl ToolRule {
    /// Builds a rule from its decoded fields.
    #[must_use]
    pub fn new(blocks: ToolBlocks, speed: Option<f32>, correct_for_drops: Option<bool>) -> Self {
        Self {
            blocks,
            speed_bits: speed.map(f32::to_bits),
            correct_for_drops,
        }
    }

    /// The rule's mining-speed override, if it has one.
    #[must_use]
    pub fn speed(&self) -> Option<f32> {
        self.speed_bits.map(f32::from_bits)
    }
}

/// The block set a [`ToolRule`] matches against (26.2 `HolderSet<Block>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolBlocks {
    /// A block tag, for example `minecraft:mineable/pickaxe`. Written on the
    /// wire without the leading `#`.
    Tag(ResourceKey),
    /// An explicit list of blocks, as **network `minecraft:block` registry
    /// ids**. Like [`ItemEnchantment::id`], these are version-scoped numbers
    /// that only the protocol crate that decoded them can resolve; they are
    /// deliberately not lifted to names here, because doing so would need the
    /// version's block registry and this type is version-free.
    Blocks(Vec<i32>),
}

/// A single enchantment entry on an item stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemEnchantment {
    /// Network `minecraft:enchantment` registry id. The enchantment registry is
    /// data-driven and synced at configuration time, so this id is scoped to the
    /// current session rather than stable across versions.
    pub id: i32,
    /// Enchantment level (for example, 4 for Efficiency IV).
    pub level: u32,
}

/// An enchantment identity a **client itself authored** for a stack it built out
/// of band — never produced by decoding the wire, and never comparable to
/// [`ItemEnchantment::id`] (a real, session-scoped network id assigned by the
/// server's own registry sync).
///
/// `path` is the enchantment's bare registry path (`"sharpness"`, no
/// `minecraft:` prefix), resolvable through `lodestone_data::enchantment` with
/// **no session in hand** — that crate's census is session-independent by
/// construction (name and level range only, see its own module doc for why it
/// carries no network id). `level` is a plain enchantment level, not a network
/// anything.
///
/// The one current producer is the creative menu's enchanted-book entries: each
/// one's enchantment identity and level are known statically (the creative table
/// is a fixed list, always built at each enchantment's own max level) but the
/// stack's *network* `minecraft:enchantment` id is session-scoped and that list
/// has no session in hand. A consumer may render this directly; it must never be
/// confused with, merged into, or compared against
/// [`ItemComponents::enchantments`] — doing so would let a locally-meaningful
/// path collide with a real, differently-ordered session id and silently name
/// the wrong enchantment, exactly the hazard `ItemEnchantment::id`'s own doc
/// warns about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthoredEnchantment {
    /// Bare registry path, e.g. `"sharpness"`.
    pub path: &'static str,
    /// Enchantment level.
    pub level: u8,
}
