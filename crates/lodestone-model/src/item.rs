use crate::event::EquipmentSlot;
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
    /// A player- or server-assigned display name overriding the item's default.
    pub custom_name: Option<Text>,
    /// Accumulated durability damage; the item's remaining durability is its
    /// max damage minus this value. `None` when the stack carries no damage
    /// component (either undamaged or not damageable).
    pub damage: Option<u32>,
    /// Enchantments applied to the stack, in wire order.
    pub enchantments: Vec<ItemEnchantment>,
    /// `minecraft:dyed_color`'s RGB, when the patch carries one — leather
    /// armour (and any other item whose base material takes dye) coloured by
    /// a dye or a dyeing table. Low 24 bits are the colour; vanilla's own
    /// `DyedItemColor.STREAM_CODEC` is a bare `ByteBufCodecs.INT`
    /// (`DyedItemColor.java`), so this is the raw wire int, not yet split
    /// into RGB bytes — `lodestone_render::entity::armour_layer_tint_with_dye`
    /// does that, matching `ArmorMaterial`/`EquipmentLayerRenderer`'s own
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
    /// item's `minecraft:potion` tint source resolves to (`Potion.calculate` /
    /// `PotionContents.getColorOr`), already folded with the potion's own built-in
    /// effect list and any `customEffects`/`customColor` the patch carried.
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
    /// `minecraft:potion` registry id itself (`Potion.STREAM_CODEC`'s
    /// `Holder<Potion>`), not [`potion_color`](Self::potion_color)'s already-mixed
    /// colour.
    ///
    /// [`potion_color`](Self::potion_color) alone cannot drive a tooltip title or
    /// effect lore: `swiftness`/`long_swiftness`/`strong_swiftness` all mix to the
    /// same colour (`Potion.calculate` only sees the effect list, and all three
    /// share one) but must resolve to three different lore bodies (different
    /// duration, and `strong_swiftness` a different amplifier). `None` when the
    /// patch carries no `minecraft:potion_contents`, or one with no `potion`
    /// holder (a bare custom-effects patch) — the same absent-means-no-component
    /// contract every other patch field here uses.
    pub potion: Option<i32>,
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
    /// This is a *prototype* component — vanilla's `COMMON_ITEM_COMPONENTS` sets
    /// it to `64` for every item and individual items override it — so a
    /// clientbound patch essentially never mentions it and it cannot be
    /// recovered from the wire. Guessing `64` is wrong for a great many items
    /// (`minecraft:water_bucket` and every shulker box are `1`,
    /// `minecraft:egg` is `16`), and guessing `1` — vanilla's own fallback when
    /// the component is genuinely absent, `ItemInstance.getMaxStackSize` — is
    /// wrong for almost everything else. A consumer that gets `None` should
    /// treat the cap as unknown rather than substituting either.
    pub max_stack_size: Option<u32>,
    /// **Effective** `minecraft:max_damage`: the item's durability, or `None`
    /// both when the item is not damageable *and* when the adapter has no
    /// prototype census (the two are indistinguishable here by design — an
    /// undamageable item and an unknown one both have no durability to show).
    ///
    /// Also a prototype component, and the gate on vanilla
    /// `ItemStack.isDamageableItem` and therefore `ItemStack.isStackable`: while
    /// this is absent, two identically-componented swords look stackable and
    /// merge into a stack of two.
    pub max_damage: Option<u32>,
    /// **Effective** `minecraft:equippable` slot: where this item is worn, or
    /// `None` for an item that is not equippable (or an adapter with no
    /// prototype census).
    ///
    /// Also a prototype component. `ArmorSlot.mayPlace` is
    /// `owner.isEquippableInSlot(stack, slot)`, which is
    /// `slot == equippable.slot() && …`, so while this is `None` **no item can
    /// be placed in any armour slot by any click type**.
    ///
    /// Only the slot is carried. Vanilla's `Equippable` also has an
    /// `allowedEntities` set — `minecraft:wolf_armor` is wolves only,
    /// `minecraft:saddle` is `#minecraft:can_equip_saddle` — which
    /// `Equippable.canBeEquippedBy` additionally requires. Every restricted item
    /// in 26.2 is in a non-humanoid slot ([`EquipmentSlot::Body`] or
    /// [`EquipmentSlot::Saddle`]) and so cannot reach a player armour slot on
    /// the slot check alone, but a consumer wanting the restriction itself must
    /// ask the version seam (`VersionAdapter::item_prototype`), not this field.
    ///
    /// **[`EquipmentSlot::Body`] is not chest armour.** Vanilla gates humanoid
    /// armour on `EquipmentSlot.Type.HUMANOID_ARMOR`, which covers
    /// [`Feet`](EquipmentSlot::Feet)/[`Legs`](EquipmentSlot::Legs)/[`Chest`](EquipmentSlot::Chest)/[`Head`](EquipmentSlot::Head)
    /// and deliberately **excludes** `BODY` (animal armour). Folding `"body"`
    /// into `Chest` makes wolf armour and horse armour placeable in a player's
    /// chestplate slot.
    pub equippable: Option<EquipmentSlot>,
    /// `minecraft:custom_data`: the plugin/datapack NBT blob, kept **opaque** as
    /// the raw network-NBT bytes (root tag id then payload, exactly as
    /// `FriendlyByteBuf.writeNbt` wrote them).
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
    /// vanilla's `AnvilMenu.calculateIncreasedRepairCost` doubles-and-adds-one
    /// each time an item is worked, and the anvil's XP cost sums both
    /// operands' values (`AnvilMenu.createResult`'s `tax`).
    ///
    /// **Server-side bookkeeping only.** The wire component exists
    /// (`minecraft:repair_cost`, a bare VarInt) but this build's protocol
    /// decoder currently only consumes it for byte-alignment and does not
    /// surface it (see `crates/protocol/v770/src/adapter/inventory.rs`'s
    /// "consumed for alignment" component group) — so a stack that arrived
    /// over the wire always reports `0` here even if a real client sent a
    /// worked item. Every stack this server itself produces (anvil/grindstone
    /// output) sets this field directly in Rust, never through a decode, so
    /// the anvil economy is internally consistent even though the value does
    /// not yet round-trip through a real client. Defaults to `0`, matching
    /// vanilla's own `getOrDefault(DataComponents.REPAIR_COST, 0)`.
    pub repair_cost: u32,
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

/// The four sherds of a `minecraft:decorated_pot` — vanilla's `PotDecorations`
/// record (`PotDecorations`, four `Optional<Item>` fields in the order `back`,
/// `left`, `right`, `front`).
///
/// # `None` means a plain brick face, not "unknown"
///
/// Vanilla's own `PotDecorations::getItem` maps `Items.BRICK` to
/// `Optional.empty()` on the way in and `ordered()` maps empty back to
/// `Items.BRICK` on the way out, so a brick and a blank face are the same state
/// by construction. This type mirrors that: a `None` side is an undecorated
/// side, and it is what a pot crafted from four plain bricks decodes to.
///
/// The wire list is `ByteBufCodecs.list(4)`, so a shorter list is legal and its
/// missing tail is `None` — that is `getItem`'s `i >= sherds.size()` arm. In
/// practice a vanilla server always writes exactly four, because `ordered()`
/// builds a four-element list unconditionally.
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

/// A smithing-table armour trim — vanilla's `ArmorTrim` record
/// (`world/item/equipment/trim/ArmorTrim.java`), which is a
/// `Holder<TrimMaterial>` plus a `Holder<TrimPattern>`.
///
/// Both are carried as bare registry **paths** (`"iron"`, `"sentry"`), the form
/// `lodestone_assets::trim::{trim_material, trim_pattern}` keys its sprite tables
/// by, so a renderer can go straight from this to a trim sprite. Neither holder's
/// *value* is kept: `TrimMaterial` is an asset-suffix group plus a description
/// component and `TrimPattern` is an asset id plus a description and a `decal`
/// flag, all of which the asset layer already has statically for the eleven
/// materials and eighteen patterns 26.2 ships.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArmorTrim {
    /// Trim material registry path, e.g. `"netherite"`.
    pub material: String,
    /// Trim pattern registry path, e.g. `"silence"`.
    pub pattern: String,
}

/// What a stack's `DataComponentPatch` said about `minecraft:tool`.
///
/// # This is a *patch*, not the effective component
///
/// A clientbound stack carries only the **delta** from the item's built-in
/// prototype component map, and vanilla puts a tool's `minecraft:tool` in that
/// prototype (`ToolMaterial.applyToolProperties`). So an ordinary diamond
/// pickaxe arrives with an *empty* patch and this field is
/// [`Inherited`](Self::Inherited) — the mining speed still has to come from
/// somewhere, and that somewhere is version data the protocol crate owns.
///
/// Read this only through a version adapter's mining seam
/// (`VersionAdapter::tool_mining`), which folds the prototype and this patch
/// together the way `ItemStack.get(DataComponents.TOOL)` does. Treating
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

/// A decoded `minecraft:tool` data component (26.2
/// `net.minecraft.world.item.component.Tool`).
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
    /// Raw IEEE-754 bits of `Tool.defaultMiningSpeed` (vanilla default `1.0`).
    /// See [`ItemTool::default_mining_speed`] for why this is stored as bits.
    default_mining_speed_bits: u32,
    /// `Tool.damagePerBlock`: durability spent per block broken.
    pub damage_per_block: u32,
    /// `Tool.canDestroyBlocksInCreative`. Creative-mode policy only; it does not
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

    /// `Tool.defaultMiningSpeed`: the speed used when no rule matches.
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

/// One `Tool.Rule`: a block set, an optional speed override, and an optional
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
